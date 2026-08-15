//! Qwen 3.6 through mistral.rs, end to end: an image in, a schema-shaped value
//! out.
//!
//! Unlike the SAM 3 and DINOv3 reference tests, this one reproduces no numeric
//! reference: it proves the mechanism, not a match. The model loads the
//! prequantized AFQ4 UQFF and answers a prompt about an image as a
//! caller-defined type, so load, constrain, decode, and deserialize each have
//! somewhere to fail. A non-empty field on the parsed value signals the round
//! trip closed.
//!
//! Ignored by default: it loads a multi-gigabyte UQFF the commit gate cannot
//! realize. `QWEN_MODEL_DIR` names the UQFF directory the `qwen-quantize` bin
//! wrote; `just model-test` is where that gets wired.

use std::{env, error, path::Path};

use chronoscope_analysis::qwen3::Qwen3;
use image::{Rgb, RgbImage};

/// The env var naming the model's first UQFF shard (its `afq4-0.uqff`), as the
/// `qwen-vlm-uqff` derivation's `firstShard` exposes it.
const FIRST_SHARD_ENV: &str = "QWEN_MODEL_FIRST_SHARD";

/// The tiny type the model must fill. That a caller-defined Rust value comes back
/// populated is the whole proof: schema-constrained decoding parsed into it.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct Described {
    description: String,
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

fn run(first_shard: &Path) -> Result<(), Box<dyn error::Error>> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let model = Qwen3::open(first_shard).await.map_err(|source| {
            format!(
                "could not open the model from {first_shard:?}: {}",
                describe(&source)
            )
        })?;
        let answer: Described = model
            .ask(
                red_disk(),
                "Describe what is in this image in one sentence.",
            )
            .await
            .map_err(|source| format!("could not answer the prompt: {}", describe(&source)))?;

        println!("description: {}", answer.description);
        assert!(
            !answer.description.trim().is_empty(),
            "the model returned an empty description, so the schema-constrained \
             decode produced nothing to parse"
        );
        Ok::<(), Box<dyn error::Error>>(())
    })
}

#[test]
#[ignore = "needs the Qwen 3.6 weights; run `just model-test`"]
fn structured_answer_round_trips_from_an_image() -> Result<(), Box<dyn error::Error>> {
    let first_shard = env::var_os(FIRST_SHARD_ENV)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!(
                "{FIRST_SHARD_ENV} is unset. Set it to the model's `afq4-0.uqff` shard \
                 path (the `qwen-vlm-uqff` derivation's `firstShard`); the ignored test \
                 cannot find the weights any other way."
            )
        })?;
    run(Path::new(&first_shard))
}
