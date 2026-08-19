//! The analysis pipeline over a batch of images.
//!
//! Loads Qwen once and runs the pipeline over each image path given, writing a
//! JSON result and a panel-overlay PNG beside each input. Loading once is the
//! point of taking a batch: the model load is the fixed cost. The model shard
//! comes from `QWEN_MODEL_FIRST_SHARD`, the handle the reference tests use. Today
//! the pipeline reaches pass 1 (composite detection); later units extend it.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::Serialize;

use chronoscope_analysis::ask::{CompositeOutcome, Outcome};
use chronoscope_analysis::pipeline::{detect_composite, draw_subimages, subimages};
use chronoscope_analysis::qwen3::{MODEL_SHARD_ENV, Qwen3};
use chronoscope_core::grammar::composites::SubimageRegion;

/// The pass-1 result written per image: the raw detection and the subimages it
/// resolves to.
#[derive(Serialize)]
struct Analysis {
    composite: CompositeOutcome,
    subimages: Vec<SubimageRegion>,
}

/// An output path beside `input`, its full file name plus `suffix`. Appending to
/// the whole name rather than replacing the extension keeps two inputs that share
/// a stem but differ in extension (`photo.jpg`, `photo.png`) from colliding.
fn output_path(input: &Path, suffix: &str) -> PathBuf {
    let mut name = input
        .file_name()
        .unwrap_or(input.as_os_str())
        .to_os_string();
    name.push(suffix);
    input.with_file_name(name)
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::level_filters::LevelFilter::INFO.into()),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if paths.is_empty() {
        eprintln!("usage: analyze <image>...");
        return ExitCode::FAILURE;
    }

    let shard = match std::env::var_os(MODEL_SHARD_ENV) {
        Some(value) => PathBuf::from(value),
        None => {
            eprintln!("set {MODEL_SHARD_ENV} to the model's afq4-0.uqff");
            return ExitCode::FAILURE;
        }
    };

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(run(&shard, &paths))
}

/// Loads the model once and runs the pipeline over every path, reporting per
/// image so one failure does not abandon the batch.
async fn run(shard: &Path, paths: &[PathBuf]) -> ExitCode {
    let qwen = match Qwen3::open(shard).await {
        Ok(qwen) => qwen,
        Err(error) => {
            eprintln!("could not load the model: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut failures = 0_usize;
    for path in paths {
        if let Err(error) = analyze_one(&qwen, path).await {
            eprintln!("{}: {error}", path.display());
            failures += 1;
        }
    }
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Runs pass 1 over one image and writes its result and overlay.
async fn analyze_one(qwen: &Qwen3, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let image = image::open(path)?;
    match detect_composite(qwen, image.clone()).await? {
        Outcome::Parsed(composite) => {
            let regions = subimages(&composite)?;
            let overlay = draw_subimages(&image, &regions);
            let count = regions.len();

            let json_path = output_path(path, ".analysis.json");
            let json = serde_json::to_string_pretty(&Analysis {
                composite,
                subimages: regions,
            })?;
            std::fs::write(&json_path, json)?;

            let png_path = output_path(path, ".panels.png");
            overlay.save(&png_path)?;

            println!(
                "{}: {count} subimage(s) -> {}",
                path.display(),
                png_path.display()
            );
            Ok(())
        }
        Outcome::Incomplete { finish, raw_prefix } => {
            Err(format!("pass 1 stopped early ({finish}); partial output: {raw_prefix}").into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_path_keeps_the_extension_so_shared_stems_do_not_collide() {
        // Replacing the extension would map both inputs to `photo.analysis.json`
        // and silently clobber the first; appending keeps them distinct.
        let jpg = output_path(Path::new("/dir/photo.jpg"), ".analysis.json");
        let png = output_path(Path::new("/dir/photo.png"), ".analysis.json");
        assert_eq!(jpg, PathBuf::from("/dir/photo.jpg.analysis.json"));
        assert_ne!(jpg, png);
    }
}
