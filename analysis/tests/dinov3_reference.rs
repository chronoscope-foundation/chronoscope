//! The Rust chain against the checkpoint's own Python.
//!
//! Decode, `embed`, read the CLS — reproduced from the embeddings
//! `nix/scripts/dinov3-fixture.py` recorded by calling `AutoImageProcessor` and
//! `AutoModel` directly. Every other comparison in this pipeline holds two
//! readings of `preprocessor_config.json` against each other, and they share an
//! author; this one spans two languages and two ecosystems, so a misread
//! rescale convention, a transposed layout or a swapped channel order has
//! somewhere to show.
//!
//! Ignored by default: it needs a multi-hundred-megabyte export hanging off the
//! gated weight fetches, which the commit gate cannot realize.

mod support;

use std::{
    env, error, fs,
    path::{Path, PathBuf},
};

use chronoscope_analysis::dinov3::Dinov3;
use serde::Deserialize;
use support::{accel, coreml_cache_root, describe};

/// Fixture directories, `PATH`-style, one per exported resolution.
/// `just model-test` realizes them and sets this.
const FIXTURES: &str = "DINOV3_FIXTURES";

/// The most a CLS embedding may drift from the reference before the comparison
/// fails. The faults it exists to catch — a channel swap, the wrong resample
/// kernel, a transposed layout — move CLS by 1e-2 or more; a correct pipeline
/// drifts only ~1e-4, the JPEG decoder and float rounding disagreeing between
/// the two ecosystems. 1e-3 sits ~10x above that noise and ~10x below any real
/// bug. Loosen it if a decoder swap ever pushes the honest drift up.
const CLS_DRIFT_MAX: f64 = 1e-3;

/// The most a single patch may drift, kept separate from the CLS bound: CLS
/// averages the decode/resample disagreement across the whole frame, while one
/// patch sees a single 16x16 window where that disagreement does not average
/// out. The first `model-test` run put the worst honest patch at 5.8e-3
/// (shekar-dzong-1921 at 224px, near-monochrome at a modest downscale); every
/// patch carries a norm near 8, so cosine has no low-norm tail to inflate. This
/// sits ~2.6x above that worst and well below the ~1e-1 a swapped channel order
/// or transposed layout drives every patch to.
const PATCH_DRIFT_MAX: f64 = 1.5e-2;

#[derive(Deserialize)]
struct Reference {
    /// The export these embeddings were measured against, which is also in this
    /// fixture's closure.
    export: PathBuf,
    images: Vec<ReferenceImage>,
}

#[derive(Deserialize)]
struct ReferenceImage {
    id: String,
    file: PathBuf,
    /// The raw patch grid, `patch_count * hidden_size` little-endian f32.
    patches: PathBuf,
    cls: Vec<f32>,
}

/// Cosine distance, accumulated in f64 so the subtraction from 1.0 keeps the
/// digits that distinguish rounding from a fault.
fn cosine_distance(reference: &[f32], produced: &[f32]) -> f64 {
    let mut dot = 0.0_f64;
    let mut reference_norm = 0.0_f64;
    let mut produced_norm = 0.0_f64;
    for (&left, &right) in reference.iter().zip(produced) {
        let (left, right) = (f64::from(left), f64::from(right));
        dot += left * right;
        reference_norm += left * left;
        produced_norm += right * right;
    }
    1.0 - dot / (reference_norm.sqrt() * produced_norm.sqrt())
}

fn compare(fixture: &Path) -> Result<(), Box<dyn error::Error>> {
    let path = fixture.join("reference.json");
    let bytes = fs::read(&path).map_err(|source| format!("could not read {path:?}: {source}"))?;
    let reference: Reference = serde_json::from_slice(&bytes).map_err(|source| {
        format!("{path:?} is not shaped the way this comparison reads it: {source}")
    })?;

    let cache_root = coreml_cache_root();
    let mut model =
        Dinov3::open(&reference.export, accel(cache_root.as_deref())).map_err(|source| {
            format!(
                "could not open the model under {:?}: {}",
                reference.export,
                describe(&source)
            )
        })?;
    // The one shape fact the messages need, off the opened model: the fixture no
    // longer states it.
    let resolution = model.resolution();

    for entry in &reference.images {
        let path = fixture.join(&entry.file);
        let bytes =
            fs::read(&path).map_err(|source| format!("could not read {path:?}: {source}"))?;
        let image = image::load_from_memory(&bytes)
            .map_err(|source| format!("could not decode {path:?}: {source}"))?;
        let features = model.embed(&image).map_err(|source| {
            format!(
                "{} at {resolution}px: could not embed the image: {}",
                entry.id,
                describe(&source)
            )
        })?;

        let cls = features.cls.as_slice();
        assert_eq!(
            cls.len(),
            entry.cls.len(),
            "{} at {resolution}px: the reference CLS has {} components, the model produced {}",
            entry.id,
            entry.cls.len(),
            cls.len(),
        );

        let cls_drift = cosine_distance(&entry.cls, cls);
        assert!(
            cls_drift <= CLS_DRIFT_MAX,
            "{} at {resolution}px: CLS drifted {cls_drift:.3e} from the reference, past the \
             {CLS_DRIFT_MAX:.0e} a correct pipeline stays under. A drift this large is a wiring \
             fault: a swapped channel order, the wrong resample kernel, or a transposed layout.",
            entry.id,
        );

        // The patch count and token width come off the model's own output, not
        // the fixture: the reference sidecar must reproduce what this model
        // produces, so it is measured against that.
        let expected_patches = features.patches.len();
        let hidden = features
            .patches
            .first()
            .map(|patch| patch.as_slice().len())
            .ok_or_else(|| {
                format!(
                    "{} at {resolution}px: the model produced no patches",
                    entry.id
                )
            })?;

        let sidecar_path = fixture.join(&entry.patches);
        let sidecar = fs::read(&sidecar_path)
            .map_err(|source| format!("could not read {sidecar_path:?}: {source}"))?;
        assert_eq!(
            sidecar.len(),
            expected_patches * hidden * 4,
            "{} at {resolution}px: the reference patch sidecar is {} bytes, not the \
             {expected_patches} x {hidden} x 4 the model produces",
            entry.id,
            sidecar.len(),
        );

        let (quads, _rest) = sidecar.as_chunks::<4>();
        let reference_patches: Vec<f32> =
            quads.iter().map(|quad| f32::from_le_bytes(*quad)).collect();

        let mut max_drift = 0.0_f64;
        for (reference_patch, produced_patch) in reference_patches
            .chunks_exact(hidden)
            .zip(&features.patches)
        {
            assert_eq!(
                produced_patch.as_slice().len(),
                reference_patch.len(),
                "{} at {resolution}px: the model produced {}-wide patches, the reference is \
                 {}-wide",
                entry.id,
                produced_patch.as_slice().len(),
                reference_patch.len(),
            );
            max_drift = max_drift.max(cosine_distance(reference_patch, produced_patch.as_slice()));
        }
        assert!(
            max_drift <= PATCH_DRIFT_MAX,
            "{} at {resolution}px: a patch drifted {max_drift:.3e} from the reference, past the \
             {PATCH_DRIFT_MAX:.1e} a correct pipeline stays under. The CLS bound averages this \
             disagreement over the frame, so a per-patch drift this large is a wiring fault \
             reaching the patch grid.",
            entry.id,
        );
    }

    Ok(())
}

#[test]
#[ignore = "needs the DINOv3 export and its fixture; run `just model-test`"]
fn cls_embeddings_reproduce_the_checkpoints_own_processor() -> Result<(), Box<dyn error::Error>> {
    let fixtures = env::var_os(FIXTURES)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!(
                "{FIXTURES} is unset. `just model-test` realizes the fixtures and sets it; \
             the ignored tests cannot find them any other way."
            )
        })?;

    for fixture in env::split_paths(&fixtures) {
        compare(&fixture)?;
    }
    Ok(())
}
