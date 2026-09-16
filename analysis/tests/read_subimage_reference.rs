//! The image gate through the real Qwen 3.6: a synthetic scene it should reject.
//!
//! A red disk on white is not a real built structure, so the gate should judge
//! it `Irrelevant` — this exercises the relevance short-circuit end to end against
//! the live model, the way `qwen3_reference` exercises the bare `ask`. The
//! describe and triage paths, and the full medium / view / relevance range
//! (a map, a pictorial map, an exterior building, an interior, other
//! irrelevants), need real labeled corpus fixtures — a follow-up, since a
//! synthetic scene cannot stand in for them once a relevance gate fronts the flow.
//!
//! Ignored by default: it loads the multi-gigabyte UQFF the commit gate cannot
//! realize (and `ANNOTATE_FONT` is present under `model-test` for the gated
//! signature). `just model-test` wires it.

use std::{error, path::Path};

use chronoscope_analysis::{
    pipeline::{SubimageReading, read_subimage},
    qwen3::Qwen3,
    setofmark::font_from_env,
};
use image::{DynamicImage, Rgb, RgbImage};

mod common;
use common::{describe, first_shard};

/// A red disk on white — a synthetic non-structure the gate should reject.
fn red_disk_scene(side: u32) -> DynamicImage {
    let center = side as f32 / 2.0;
    let radius = side as f32 / 3.0;
    let pixels = RgbImage::from_fn(side, side, |x, y| {
        let (dx, dy) = (x as f32 - center, y as f32 - center);
        if dx * dx + dy * dy <= radius * radius {
            Rgb([220, 30, 30])
        } else {
            Rgb([255, 255, 255])
        }
    });
    DynamicImage::ImageRgb8(pixels)
}

fn run_gate_rejects(first_shard: &Path) -> Result<(), Box<dyn error::Error>> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let model = Qwen3::open(first_shard).await.map_err(|source| {
            format!(
                "could not open the model from {first_shard:?}: {}",
                describe(&source)
            )
        })?;
        // No regions: the gate runs, judges relevance, and short-circuits before
        // any describe/triage work. The font goes unused on the reject path but
        // the signature asks for one, and `ANNOTATE_FONT` is present under model-test.
        let font = font_from_env()?;
        let scene = red_disk_scene(256);

        let reading = read_subimage(&model, scene, Vec::new(), &font)
            .await
            .map_err(|source| format!("could not read the subimage: {}", describe(&source)))?;

        match reading {
            SubimageReading::Irrelevant { reason } => {
                println!("gate rejected the red disk: {}", reason.as_str());
                Ok(())
            }
            other => Err(format!(
                "expected the gate to reject a synthetic non-structure; got {other:?}"
            )
            .into()),
        }
    })
}

#[test]
#[ignore = "needs the Qwen 3.6 weights; run `just model-test`"]
fn the_gate_rejects_a_synthetic_non_structure() -> Result<(), Box<dyn error::Error>> {
    run_gate_rejects(&first_shard()?)
}
