//! Qwen 3.6 through mistral.rs, end to end: an image in, a schema-shaped value
//! out.
//!
//! Unlike the SAM 3 and DINOv3 reference tests, this one reproduces no numeric
//! reference: it proves the mechanism, not a match. The model loads the
//! prequantized AFQ4 UQFF and answers a prompt about an image as a
//! caller-defined type, so load, constrain, decode, and deserialize each have
//! somewhere to fail. A parsed value that names the red circle signals the round
//! trip closed and the constrained decode did not degrade.
//!
//! The second test is the coordinate probe: it instructs the model to return
//! labeled `upper_left`/`lower_right` corners on a 0-1000 grid and prints the box
//! it draws for two synthetic rectangles at two resolutions. Print-only, a way to
//! eyeball box quality against a new image rather than an assertion.
//!
//! Both are ignored by default: they load a multi-gigabyte UQFF the commit gate
//! cannot realize. `QWEN_MODEL_FIRST_SHARD` names the UQFF's `afq4-0.uqff`;
//! `just model-test` is where that gets wired.

use std::{error, path::Path};

use chronoscope_analysis::{
    ask::{Outcome, Prompt, Rect},
    qwen3::Qwen3,
};
use image::{Rgb, RgbImage};

mod common;
use common::{describe, first_shard};

/// The tiny type the model must fill. A caller-defined Rust value coming back
/// populated is the whole proof: schema-constrained decoding parsed into it.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct Described {
    description: String,
}

/// A single box the model localizes, in the ask's labeled [`Rect`] shape, so the
/// probe exercises the exact type the pipeline decodes.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct Boxed {
    #[serde(rename = "box")]
    bbox: Rect,
}

/// A recognizable synthetic image so the description has something to name: a red
/// disk on white. Synthesizing it keeps this check off the Nix fixtures the
/// numeric model tests depend on.
fn red_disk() -> image::DynamicImage {
    let side: u32 = 256;
    let center = side as f32 / 2.0;
    let radius = side as f32 / 3.0;
    let pixels = RgbImage::from_fn(side, side, |x, y| {
        let dx = x as f32 - center;
        let dy = y as f32 - center;
        if dx * dx + dy * dy <= radius * radius {
            Rgb([220, 30, 30])
        } else {
            Rgb([255, 255, 255])
        }
    });
    image::DynamicImage::ImageRgb8(pixels)
}

/// A red rectangle on white with proportional corners `x0..x1` by `y0..y1`,
/// rendered at `width` x `height`.
///
/// The corners are asymmetric in both axes so a swapped axis order or a scale
/// that only differs by resolution cannot be mistaken for the intended box, so
/// the readout disentangles order from scale from normalization.
fn red_rectangle(
    width: u32,
    height: u32,
    x0: f64,
    x1: f64,
    y0: f64,
    y1: f64,
) -> image::DynamicImage {
    let pixels = RgbImage::from_fn(width, height, |x, y| {
        let fx = x as f64 / width as f64;
        let fy = y as f64 / height as f64;
        if (x0..x1).contains(&fx) && (y0..y1).contains(&fy) {
            Rgb([220, 30, 30])
        } else {
            Rgb([255, 255, 255])
        }
    });
    image::DynamicImage::ImageRgb8(pixels)
}

fn run_round_trip(first_shard: &Path) -> Result<(), Box<dyn error::Error>> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let model = Qwen3::open(first_shard).await.map_err(|source| {
            format!(
                "could not open the model from {first_shard:?}: {}",
                describe(&source)
            )
        })?;
        let prompt = Prompt {
            preamble: "Describe what is in this image in one sentence.".to_owned(),
            images: vec![red_disk()],
            postamble: "Describe the image.".to_owned(),
        };
        let outcome: Outcome<Described> = model
            .ask(prompt)
            .await
            .map_err(|source| format!("could not answer the prompt: {}", describe(&source)))?;

        match outcome {
            Outcome::Parsed(answer) => {
                println!("description: {}", answer.description);
                // Any functional VLM describes a red disk as a red circle, so this
                // survives a model swap while still catching a degraded decode: the
                // x-guidance regression returned `": "` and `}`, which a bare
                // non-empty check would have passed.
                let described = answer.description.to_lowercase();
                assert!(
                    described.contains("red") && described.contains("circle"),
                    "expected a red-circle description; got {:?}, so the \
                     schema-constrained decode degraded the output",
                    answer.description
                );
            }
            Outcome::Incomplete { finish, raw_prefix } => {
                return Err(format!(
                    "the model stopped early ({finish}) with prefix: {raw_prefix}"
                )
                .into());
            }
        }
        Ok::<(), Box<dyn error::Error>>(())
    })
}

fn run_coordinate_probe(first_shard: &Path) -> Result<(), Box<dyn error::Error>> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let model = Qwen3::open(first_shard).await.map_err(|source| {
            format!(
                "could not open the model from {first_shard:?}: {}",
                describe(&source)
            )
        })?;

        // Both sizes sit inside Qwen's pixel budget (min 65,536, max 16,777,216)
        // and produce different internal grids, so a smart-resize clamp cannot
        // make absolute coordinates look resolution-invariant. Two positions: one
        // near the left edge, one pushed right so absolute and 0-1000 diverge in x
        // at both resolutions, not just at 512 wide.
        let instruction = "Locate the red rectangle and give its bounding box as an \
             upper_left corner and a lower_right corner, each with an x and a y on \
             a 0 to 1000 grid where 0 is the left or top edge and 1000 is the right \
             or bottom edge.";
        for (x0, x1, y0, y1) in [(0.10, 0.45, 0.25, 0.70), (0.55, 0.85, 0.20, 0.65)] {
            println!(
                "rectangle x [{x0}..{x1}] y [{y0}..{y1}]  (0-1000: [{}, {}, {}, {}])",
                (x0 * 1000.0) as i32,
                (y0 * 1000.0) as i32,
                (x1 * 1000.0) as i32,
                (y1 * 1000.0) as i32,
            );
            for (width, height) in [(512u32, 768u32), (1024u32, 1536u32)] {
                let prompt = Prompt {
                    preamble: instruction.to_owned(),
                    images: vec![red_rectangle(width, height, x0, x1, y0, y1)],
                    postamble: "Give the bounding box of the red rectangle.".to_owned(),
                };
                let outcome: Outcome<Boxed> = model.ask(prompt).await.map_err(|source| {
                    format!("could not answer the probe: {}", describe(&source))
                })?;
                match outcome {
                    Outcome::Parsed(boxed) => {
                        println!("  {width}x{height} -> box {:?}", boxed.bbox);
                    }
                    Outcome::Incomplete { finish, raw_prefix } => {
                        return Err(format!(
                            "the model stopped early ({finish}) with prefix: {raw_prefix}"
                        )
                        .into());
                    }
                }
            }
        }
        Ok::<(), Box<dyn error::Error>>(())
    })
}

#[test]
#[ignore = "needs the Qwen 3.6 weights; run `just model-test`"]
fn structured_answer_round_trips_from_an_image() -> Result<(), Box<dyn error::Error>> {
    run_round_trip(&first_shard()?)
}

#[test]
#[ignore = "coordinate probe; prints Qwen's box numbers, run `just model-test`"]
fn box_coordinate_convention_probe() -> Result<(), Box<dyn error::Error>> {
    run_coordinate_probe(&first_shard()?)
}
