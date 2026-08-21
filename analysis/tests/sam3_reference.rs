//! The Rust interactive and concept paths against the torch model they were
//! traced from.
//!
//! Both reproduce masks `nix/scripts/sam3-fixture.py` recorded from the torch
//! model directly — `predict_inst` for the box path, `set_text_prompt` for the
//! grounding path. Every other check on these paths is internal to the crate;
//! these span the ONNX graphs the crate runs and the torch model behind them, so
//! a wrong box-to-model-frame mapping, a feature-prep divergence, a mask upscale
//! off by a convention, or a bad tokenization has somewhere to show.
//!
//! Ignored by default: they need a multi-gigabyte export and fixture hanging off
//! the gated weight fetches, which the commit gate cannot realize.

mod support;

use std::{
    env, error, fs,
    path::{Path, PathBuf},
};

use chronoscope_analysis::sam3::{Sam3, ScoredRegion};
use chronoscope_core::grammar::geometry::{Dimensions, ProportionalRect, Region};
use serde::Deserialize;
use support::{accel, coreml_cache_root, describe};

/// The fixture directory. `just model-test` realizes it and sets this.
const FIXTURE: &str = "SAM3_FIXTURE";

/// The least a Rust mask may overlap the torch reference before the comparison
/// fails. A correct pipeline matches the reference on every pixel but a thin
/// boundary rim, so honest IoU sits near 1. The first `model-test` run put the
/// worst honest case at 0.9906 (itsukushima-torii, a 4200px frame whose 288px
/// mask upscales most); the rest were 0.997+. This floor sits below that and far
/// above the collapse a wiring fault brings — a squashed box, the wrong feature
/// maps, or an upscale in the wrong frame moves the mask bodily. Loosen it only
/// if the honest worst case ever climbs.
const IOU_FLOOR: f64 = 0.98;

/// The least a Rust concept union may overlap the torch reference, calibrated
/// from the `model-test` run the way [`IOU_FLOOR`] is. The first run put the worst
/// honest case at 0.9961 (st-basils-wide, five grounded buildings) with the rest
/// 0.997+; this floor sits below that and far above the collapse a wiring fault
/// brings.
const CONCEPT_IOU_FLOOR: f64 = 0.99;

#[derive(Deserialize)]
struct Reference {
    /// The export these masks were measured against, also in this fixture's
    /// closure.
    export: PathBuf,
    entries: Vec<Entry>,
    concept: Vec<ConceptEntry>,
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

/// One text-prompt reference: the prompt both sides ground, and the union of the
/// masks the torch head kept. The union IoU is the assertion — it proves the two
/// sides find the same pixels. The count and scores ride along as a printed
/// diagnostic (see `compare_concept`).
#[derive(Deserialize)]
struct ConceptEntry {
    id: String,
    file: PathBuf,
    prompt: String,
    count: usize,
    /// The torch head's per-instance scores, printed beside the Rust scores so a
    /// count difference at the 0.5 cutoff is visible as the marginal instance it
    /// is.
    scores: Vec<f64>,
    union_mask: PathBuf,
    source: Source,
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

/// Reads the fixture's `reference.json` and opens the export it names.
fn open_reference(fixture: &Path) -> Result<(Reference, Sam3), Box<dyn error::Error>> {
    let path = fixture.join("reference.json");
    let bytes = fs::read(&path).map_err(|source| format!("could not read {path:?}: {source}"))?;
    let reference: Reference = serde_json::from_slice(&bytes).map_err(|source| {
        format!("{path:?} is not shaped the way this comparison reads it: {source}")
    })?;

    let cache_root = coreml_cache_root();
    let sam = Sam3::open(&reference.export, accel(cache_root.as_deref())).map_err(|source| {
        format!(
            "could not open the model under {:?}: {}",
            reference.export,
            describe(&source)
        )
    })?;
    Ok((reference, sam))
}

fn compare(
    reference: &Reference,
    sam: &mut Sam3,
    fixture: &Path,
) -> Result<(), Box<dyn error::Error>> {
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

/// The union of every region's pixels on the source grid, the shape the concept
/// comparison reads: the torch reference records one union mask per prompt, so
/// the Rust instances are OR'd to match it.
fn union(
    regions: &[ScoredRegion],
    dimensions: Dimensions,
) -> Result<Region, Box<dyn error::Error>> {
    let extent = (dimensions.width() as usize) * (dimensions.height() as usize);
    let mut dense = vec![false; extent];
    for scored in regions {
        let plane = scored.region.to_dense();
        // The regions are on the decoded image's grid and `dimensions` is the
        // fixture's recorded source; a `zip` would silently truncate if they ever
        // disagreed, so reject the mismatch rather than union a partial mask.
        if plane.len() != extent {
            return Err(format!(
                "a grounded region carries {} pixels, not the {extent} of the {}x{} source grid",
                plane.len(),
                dimensions.width(),
                dimensions.height(),
            )
            .into());
        }
        for (slot, pixel) in dense.iter_mut().zip(plane) {
            *slot |= pixel;
        }
    }
    Ok(Region::from_dense(dimensions, &dense)?)
}

/// A compact fixed-precision list of instance scores, for the diagnostic print.
fn scores_line(scores: &[f64]) -> String {
    scores
        .iter()
        .map(|score| format!("{score:.3}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn compare_concept(
    reference: &Reference,
    sam: &mut Sam3,
    fixture: &Path,
) -> Result<(), Box<dyn error::Error>> {
    let mut worst: Option<(String, f64)> = None;
    for entry in &reference.concept {
        let image_path = fixture.join(&entry.file);
        let image_bytes = fs::read(&image_path)
            .map_err(|source| format!("could not read {image_path:?}: {source}"))?;
        let image = image::load_from_memory(&image_bytes)
            .map_err(|source| format!("could not decode {image_path:?}: {source}"))?;

        let encoded = sam
            .encode(&image)
            .map_err(|source| format!("{}: could not encode: {}", entry.id, describe(&source)))?;
        let regions = sam
            .segment_concept(&encoded, &entry.prompt)
            .map_err(|source| {
                format!(
                    "{}: could not segment concept: {}",
                    entry.id,
                    describe(&source)
                )
            })?;

        let dimensions = Dimensions::new(entry.source.width, entry.source.height)?;
        let ours = union(&regions, dimensions)?;
        let torch = reference_region(
            &fixture.join(&entry.union_mask),
            entry.source.width,
            entry.source.height,
        )?;

        // Two empty unions agree perfectly on "nothing here"; the IoU of a pair of
        // empty masks is otherwise undefined.
        let iou = if ours.is_empty() && torch.is_empty() {
            1.0
        } else {
            ours.intersection_over_union(&torch)
                .map_err(|source| format!("{}: {source}", entry.id))?
        };

        let mut ours_scores: Vec<f64> = regions
            .iter()
            .map(|scored| f64::from(scored.score))
            .collect();
        ours_scores.sort_by(|a, b| b.total_cmp(a));
        // Count is a diagnostic, not an assertion. The grounding graph emits every
        // instance over its baked 0.5 cutoff, so one sitting within a hair of 0.5
        // lands on either side of it between the torch model and the ONNX graph.
        // `segment_concept` returns those instances raw, so a count can differ only
        // by such a marginal instance, never a split or a merge — the union IoU is
        // what proves the two agree on the pixels. Commit 2's post-processing is
        // what makes instance structure worth asserting.
        println!(
            "{:24} {:14} iou {iou:.4}  count ours {} torch {}  ours [{}]  torch [{}]",
            entry.id,
            entry.prompt,
            regions.len(),
            entry.count,
            scores_line(&ours_scores),
            scores_line(&entry.scores),
        );
        if worst.as_ref().is_none_or(|(_, value)| iou < *value) {
            worst = Some((entry.id.clone(), iou));
        }
    }

    if let Some((id, iou)) = worst {
        assert!(
            iou >= CONCEPT_IOU_FLOOR,
            "{id}: the Rust concept union overlapped the torch reference by only {iou:.4}, under \
             the {CONCEPT_IOU_FLOOR} a matched grounding path stays above. An IoU this low is a \
             wiring fault: a wrong tokenization, the folded grounding inputs, or the box-exemplar \
             convention."
        );
    }
    Ok(())
}

/// The fixture directory `just model-test` realizes and names, or the reason the
/// ignored tests cannot find it.
fn fixture_dir() -> Result<PathBuf, Box<dyn error::Error>> {
    let fixture = env::var_os(FIXTURE)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!(
                "{FIXTURE} is unset. `just model-test` realizes the fixture and sets it; the \
                 ignored test cannot find it any other way."
            )
        })?;
    Ok(PathBuf::from(fixture))
}

/// Both reference comparisons share one open model: SAM 3's CoreML cache keys a
/// compiled graph by path, so two tests opening the same export at once collide
/// creating the same package. Opening once also loads the multi-gigabyte export
/// a single time.
#[test]
#[ignore = "needs the SAM 3 export and its fixture; run `just model-test`"]
fn reference_paths_reproduce_torch() -> Result<(), Box<dyn error::Error>> {
    let fixture = fixture_dir()?;
    let (reference, mut sam) = open_reference(&fixture)?;
    // An empty reference set would let every assertion below be vacuously skipped,
    // so a fixture that recorded nothing passes as loudly as the model being wrong.
    assert!(
        !reference.entries.is_empty() && !reference.concept.is_empty(),
        "the fixture recorded no reference entries ({} interactive, {} concept); the \
         comparison would pass without exercising the model",
        reference.entries.len(),
        reference.concept.len(),
    );
    compare(&reference, &mut sam, &fixture)?;
    compare_concept(&reference, &mut sam, &fixture)?;
    Ok(())
}
