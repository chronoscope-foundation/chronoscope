//! The Rust interactive path against the torch model it was traced from.
//!
//! Encode, box prompt, decode, upscale, threshold — reproduced from the masks
//! `nix/scripts/sam3-fixture.py` recorded by calling `Sam3Image.predict_inst`
//! directly. Every other check on this path is internal to the crate; this one
//! spans the ONNX graph the crate runs and the torch model behind it, so a wrong
//! box-to-model-frame mapping, a feature-prep divergence, or a mask upscale off
//! by a convention has somewhere to show.
//!
//! Ignored by default: it needs a multi-gigabyte export and fixture hanging off
//! the gated weight fetches, which the commit gate cannot realize.

mod support;

use std::{
    env, error, fs,
    path::{Path, PathBuf},
};

use chronoscope_analysis::sam3::Sam3;
use chronoscope_core::grammar::geometry::{Dimensions, ProportionalRect, Region};
use serde::Deserialize;
use support::{accel, coreml_cache_root, describe};

/// The fixture directory. `just model-test` realizes it and sets this.
const FIXTURE: &str = "SAM3_FIXTURE";

/// The least a Rust mask may overlap the torch reference before the comparison
/// fails. A matched pipeline drifts only where the two decoders disagree on a
/// JPEG and where the bilinear upscale rounds a boundary pixel, both of which
/// touch a thin rim of a mask whose interior is thousands of pixels, so honest
/// `IoU` sits near 1. The first `model-test` run put the worst honest case at
/// 0.9906 (itsukushima-torii, a 4200px frame where the 288px mask upscales
/// most and the rim is longest); the rest were 0.997+. This floor sits ~2x that
/// rim disagreement and far above the collapse a wiring fault brings — a
/// squashed box, the wrong feature maps, or an upscale in the wrong frame moves
/// the mask bodily. Loosen it only if a decoder swap pushes the honest rim up.
const IOU_FLOOR: f64 = 0.98;

#[derive(Deserialize)]
struct Reference {
    /// The export these masks were measured against, also in this fixture's
    /// closure.
    export: PathBuf,
    entries: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    id: String,
    file: PathBuf,
    mask: PathBuf,
    /// The box prompt both sides segment, normalized min/max corners.
    box_xyxy_norm: [f64; 4],
    /// The torch predictor's own quality estimate for its chosen mask, printed
    /// beside the Rust score so a candidate-selection divergence is visible.
    predicted_iou: f64,
    source: Source,
}

#[derive(Deserialize)]
struct Source {
    width: u32,
    height: u32,
}

/// The torch reference mask as a [`Region`] on the source grid: a grayscale PNG
/// whose foreground pixels the fixture wrote as 255.
fn reference_region(path: &Path, width: u32, height: u32) -> Result<Region, Box<dyn error::Error>> {
    let bytes = fs::read(path).map_err(|source| format!("could not read {path:?}: {source}"))?;
    let luma = image::load_from_memory(&bytes)
        .map_err(|source| format!("could not decode {path:?}: {source}"))?
        .to_luma8();
    if luma.dimensions() != (width, height) {
        return Err(format!(
            "{path:?} is {:?}, not the {width}x{height} its source declares",
            luma.dimensions()
        )
        .into());
    }
    let dense: Vec<bool> = luma.pixels().map(|pixel| pixel.0[0] > 127).collect();
    Ok(Region::from_dense(Dimensions::new(width, height)?, &dense)?)
}

fn compare(fixture: &Path) -> Result<(), Box<dyn error::Error>> {
    let path = fixture.join("reference.json");
    let bytes = fs::read(&path).map_err(|source| format!("could not read {path:?}: {source}"))?;
    let reference: Reference = serde_json::from_slice(&bytes).map_err(|source| {
        format!("{path:?} is not shaped the way this comparison reads it: {source}")
    })?;

    let cache_root = coreml_cache_root();
    let mut sam =
        Sam3::open(&reference.export, accel(cache_root.as_deref())).map_err(|source| {
            format!(
                "could not open the model under {:?}: {}",
                reference.export,
                describe(&source)
            )
        })?;

    let mut worst: Option<(String, f64)> = None;
    for entry in &reference.entries {
        let image_path = fixture.join(&entry.file);
        let image_bytes = fs::read(&image_path)
            .map_err(|source| format!("could not read {image_path:?}: {source}"))?;
        let image = image::load_from_memory(&image_bytes)
            .map_err(|source| format!("could not decode {image_path:?}: {source}"))?;

        let encoded = sam
            .encode(&image)
            .map_err(|source| format!("{}: could not encode: {}", entry.id, describe(&source)))?;
        let [x0, y0, x1, y1] = entry.box_xyxy_norm;
        let rect = ProportionalRect::new(x0, y0, x1, y1)?;
        let scored = sam
            .segment_rect(&encoded, &rect)
            .map_err(|source| format!("{}: could not segment: {}", entry.id, describe(&source)))?
            .ok_or_else(|| format!("{}: the box produced an empty mask", entry.id))?;

        let torch = reference_region(
            &fixture.join(&entry.mask),
            entry.source.width,
            entry.source.height,
        )?;
        let iou = scored
            .region
            .intersection_over_union(&torch)
            .map_err(|source| format!("{}: {source}", entry.id))?;

        let id = &entry.id;
        let ours_area = scored.region.area();
        let torch_area = torch.area();
        let score = scored.score;
        let pred = entry.predicted_iou;
        println!(
            "{id:28} iou {iou:.4}  ours {ours_area}px  torch {torch_area}px  score {score:.4} vs pred {pred:.4}"
        );
        if worst.as_ref().is_none_or(|(_, value)| iou < *value) {
            worst = Some((entry.id.clone(), iou));
        }
    }

    if let Some((id, iou)) = worst {
        assert!(
            iou >= IOU_FLOOR,
            "{id}: Rust mask overlapped the torch reference by only {iou:.4}, under the \
             {IOU_FLOOR} a matched pipeline stays above. An IoU this low is a wiring fault: a \
             squashed box, the wrong feature maps, or a mask upscale in the wrong frame."
        );
    }
    Ok(())
}

#[test]
#[ignore = "needs the SAM 3 export and its fixture; run `just model-test`"]
fn interactive_masks_reproduce_predict_inst() -> Result<(), Box<dyn error::Error>> {
    let fixture = env::var_os(FIXTURE)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!(
                "{FIXTURE} is unset. `just model-test` realizes the fixture and sets it; the \
                 ignored test cannot find it any other way."
            )
        })?;
    compare(Path::new(&fixture))
}
