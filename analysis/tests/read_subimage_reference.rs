//! The image gate through the real Qwen 3.6: scenes it should reject.
//!
//! A synthetic red disk and four curated corpus images (food, a landscape, a
//! portrait, an architectural scale model) are all things the gate should judge
//! `Irrelevant`: none is a real built structure, and the scale model exercises
//! the gate's scale-model clause specifically. Together they exercise the
//! relevance short-circuit end to end against the live model, the way
//! `qwen3_reference` exercises the bare `ask`. The relevant range (a map, a
//! pictorial map, an exterior building, an interior) still wants its own fixtures.
//!
//! Ignored by default: it loads the multi-gigabyte UQFF the commit gate cannot
//! realize. `just model-test` wires it.

use std::error;

use chronoscope_analysis::{ask::ImageOutcome, pipeline::passes::gate, qwen3::Qwen3};
use image::{DynamicImage, Rgb, RgbImage};

mod common;
use common::{describe, first_shard};
mod corpus;
use corpus::{corpus_dir, load_corpus};

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

#[test]
#[ignore = "needs the Qwen 3.6 weights; run `just model-test`"]
fn the_gate_rejects_a_synthetic_non_structure() -> Result<(), Box<dyn error::Error>> {
    let first_shard = first_shard()?;
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let model = Qwen3::open(&first_shard).await.map_err(|source| {
            format!(
                "could not open the model from {first_shard:?}: {}",
                describe(&source)
            )
        })?;
        let scene = red_disk_scene(256);

        let reading = gate(&model, &scene)
            .await
            .map_err(|source| format!("could not gate the subimage: {}", describe(&source)))?;

        match reading {
            ImageOutcome::Irrelevant { reason } => {
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

/// Curated corpus negatives the gate must reject, each a scene with no real
/// built structure. `scale-model-irrelevant` is the pointed one: it depicts real
/// buildings, so it exercises the gate's scale-model clause specifically.
const IRRELEVANT_CORPUS: &[&str] = &[
    "food-irrelevant",
    "landscape-irrelevant",
    "portrait-irrelevant",
    "scale-model-irrelevant",
];

#[test]
#[ignore = "needs the Qwen 3.6 weights and the corpus; run `just model-test`"]
fn the_gate_rejects_real_irrelevant_images() -> Result<(), Box<dyn error::Error>> {
    let first_shard = first_shard()?;
    let corpus = corpus_dir()?;
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        // One model, reused across the images: the load dominates, and the gate
        // is the only call per image.
        let model = Qwen3::open(&first_shard).await.map_err(|source| {
            format!(
                "could not open the model from {first_shard:?}: {}",
                describe(&source)
            )
        })?;
        for id in IRRELEVANT_CORPUS {
            let image = load_corpus(&corpus, id)?;
            let reading = gate(&model, &image)
                .await
                .map_err(|source| format!("could not gate {id}: {}", describe(&source)))?;
            match reading {
                ImageOutcome::Irrelevant { reason } => {
                    println!("gate rejected {id}: {}", reason.as_str());
                }
                other => {
                    return Err(format!(
                        "expected the gate to reject {id} as irrelevant; got {other:?}"
                    )
                    .into());
                }
            }
        }
        Ok(())
    })
}
