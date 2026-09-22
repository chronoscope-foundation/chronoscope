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
//!
//! In the crate rather than `tests/`, unlike its SAM and orchestration
//! siblings, because it compares against `raw_patches`, `Embedding::cosine` and
//! `Embedding::from_raw`, which are crate-visible so the public surface carries
//! no test-only accessors.

use std::{
    env, error, fs,
    path::{Path, PathBuf},
};

use super::{Dinov3, Embedding, manifest::EMBEDDING_DIM};
use crate::test_support::{accel, coreml_cache_root, describe};
use serde::Deserialize;

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

/// The reference vector as an [`Embedding`], so the comparison runs between two
/// directions: the fixture records raw model output, which carries a norm near 8.
fn reference_embedding(components: &[f32], what: &str) -> Result<Embedding, Box<dyn error::Error>> {
    let raw = <&[f32; EMBEDDING_DIM]>::try_from(components).map_err(|_| {
        format!(
            "{what}: the reference is {}-wide, not the {EMBEDDING_DIM} the model produces",
            components.len()
        )
    })?;
    Ok(Embedding::from_raw(raw)?)
}

async fn compare(fixture: &Path) -> Result<(), Box<dyn error::Error>> {
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
        let features = model.embed(&image).await.map_err(|source| {
            format!(
                "{} at {resolution}px: could not embed the image: {}",
                entry.id,
                describe(&source)
            )
        })?;

        let produced = features.image().map_err(|source| {
            format!(
                "{} at {resolution}px: the CLS has no direction: {}",
                entry.id,
                describe(&source)
            )
        })?;
        let reference_cls =
            reference_embedding(&entry.cls, &format!("{} at {resolution}px CLS", entry.id))?;

        let cls_drift = 1.0 - reference_cls.cosine(&produced);
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
        let patches = features.raw_patches();
        let expected_patches = patches.len();
        let hidden = patches.first().map(|patch| patch.len()).ok_or_else(|| {
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
        for (reference_patch, produced_patch) in reference_patches.chunks_exact(hidden).zip(patches)
        {
            let reference_patch =
                reference_embedding(reference_patch, &format!("{} at {resolution}px", entry.id))?;
            let produced_patch = Embedding::from_raw(produced_patch)?;
            max_drift = max_drift.max(1.0 - reference_patch.cosine(&produced_patch));
        }
        assert!(
            max_drift <= PATCH_DRIFT_MAX,
            "{} at {resolution}px: a patch drifted {max_drift:.3e} from the reference, past the \
             {PATCH_DRIFT_MAX:.1e} a correct pipeline stays under. The CLS bound averages this \
             disagreement over the frame, so a per-patch drift this large is a wiring fault \
             reaching the patch grid.",
            entry.id,
        );

        // The headroom under each bound, which is what says whether a session
        // setting has moved the comparison rather than merely passed it.
        let id = &entry.id;
        println!("{id:28} at {resolution}px  cls {cls_drift:.3e}  worst patch {max_drift:.3e}");
    }

    Ok(())
}

#[tokio::test]
#[ignore = "needs the DINOv3 export and its fixture; run `just model-test`"]
async fn cls_embeddings_reproduce_the_checkpoints_own_processor()
-> Result<(), Box<dyn error::Error>> {
    let fixtures = env::var_os(FIXTURES)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!(
                "{FIXTURES} is unset. `just model-test` realizes the fixtures and sets it; \
             the ignored tests cannot find them any other way."
            )
        })?;

    for fixture in env::split_paths(&fixtures) {
        compare(&fixture).await?;
    }
    Ok(())
}
