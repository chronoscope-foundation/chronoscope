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

use std::{
    env, error, fs,
    path::{Path, PathBuf},
};

use chronoscope_analysis::dinov3::Dinov3;
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
const DRIFT_MAX: f64 = 1e-3;

#[derive(Deserialize)]
struct Reference {
    resolution: usize,
    /// The export these embeddings were measured against, which is also in this
    /// fixture's closure.
    export: PathBuf,
    images: Vec<ReferenceImage>,
}

#[derive(Deserialize)]
struct ReferenceImage {
    id: String,
    file: PathBuf,
    cls: Vec<f32>,
}

/// One line carrying an error's whole cause chain, since the boxed error a
/// failing test prints shows only the outermost message otherwise.
fn describe(error: &dyn error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    message
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
    let resolution = reference.resolution;

    let mut model = Dinov3::open(&reference.export).map_err(|source| {
        format!(
            "could not open the model under {:?}: {}",
            reference.export,
            describe(&source)
        )
    })?;

    for entry in &reference.images {
        let path = fixture.join(&entry.file);
        let bytes =
            fs::read(&path).map_err(|source| format!("could not read {path:?}: {source}"))?;
        let image = image::load_from_memory(&bytes)
            .map_err(|source| format!("could not decode {path:?}: {source}"))?;
        let tokens = model.embed(&image).map_err(|source| {
            format!(
                "{} at {resolution}px: could not embed the image: {}",
                entry.id,
                describe(&source)
            )
        })?;

        let cls = tokens.token(0).ok_or_else(|| {
            format!(
                "{} at {resolution}px: the model produced no prefix token to read a CLS from",
                entry.id
            )
        })?;
        assert_eq!(
            cls.len(),
            entry.cls.len(),
            "{} at {resolution}px: the reference CLS has {} components, the model produced {}",
            entry.id,
            entry.cls.len(),
            cls.len(),
        );

        let drift = cosine_distance(&entry.cls, cls);
        assert!(
            drift <= DRIFT_MAX,
            "{} at {resolution}px: CLS drifted {drift:.3e} from the reference, past the \
             {DRIFT_MAX:.0e} a correct pipeline stays under. A drift this large is a wiring \
             fault: a swapped channel order, the wrong resample kernel, or a transposed layout.",
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
