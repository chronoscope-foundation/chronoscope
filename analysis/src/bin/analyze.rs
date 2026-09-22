//! The analysis pipeline over a batch of images.
//!
//! Loads the three models once and reads each image path given, writing a JSON
//! result and a panel-overlay PNG beside each input. Loading once is the point
//! of taking a batch: the model loads are the fixed cost.
//!
//! The handles come from the environment: `SAM3_EXPORT`, `DINOV3_EXPORT` and
//! `QWEN_MODEL_FIRST_SHARD` for the models, and `ANNOTATE_FONT` for the marker
//! font, which is the only one the `analysis` shell brings. The exports are
//! realized on demand rather than by the shell, since each is gigabytes.
//! `just model-test` realizes the same three for the comparisons, so its recipe
//! is the worked example.
//!
//! `COREML_CACHE` is optional: any writable directory, which runs the ONNX
//! graphs on CoreML and keeps the compile across runs. Unset runs them on the
//! CPU.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chronoscope_analysis::onnx::Accel;
use chronoscope_analysis::pipeline::Pipeline;
use chronoscope_analysis::pipeline::passes::draw_subimages;
use chronoscope_analysis::qwen3::{MODEL_SHARD_ENV, Qwen3};
use chronoscope_analysis::sam3::Sam3;
use chronoscope_analysis::setofmark::font_from_env;
use chronoscope_analysis::{dinov3::Dinov3, pipeline::Analysis};
use chronoscope_core::grammar::composites::SubimageRegion;

/// The SAM 3 export directory, as `just model-test` names it.
const SAM3_EXPORT_ENV: &str = "SAM3_EXPORT";

/// The DINOv3 export directory. One resolution per export, so this names which.
const DINOV3_EXPORT_ENV: &str = "DINOV3_EXPORT";

/// The CoreML compiled-model cache root. Unset runs the graphs on the CPU, so a
/// platform without CoreML needs no flag.
const COREML_CACHE_ENV: &str = "COREML_CACHE";

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

/// A required path from the environment, named in the error so a missing handle
/// says which one it was, and how to get it.
///
/// An empty value carries no path to open, so it earns the same error as an
/// unset one rather than reaching a model loader as the current directory.
fn required_path(var: &str) -> Result<PathBuf, String> {
    std::env::var_os(var)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("{var} is unset. {REALIZED_BY}"))
}

/// Where the model handles come from, for an error to point at.
///
/// One pointer rather than a command per handle: the recipe is what realizes
/// them in every standard path, and a copy of its commands here would go stale
/// silently, since nothing reads this message on the paths that work.
const REALIZED_BY: &str = "`just model-test` realizes the model handles; its recipe is the worked \
                           example of what each one needs.";

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

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("could not start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(run(&paths))
}

/// Loads the models once and reads every path, reporting per image so one
/// failure does not abandon the batch.
async fn run(paths: &[PathBuf]) -> ExitCode {
    let pipeline = match load().await {
        Ok(pipeline) => pipeline,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    let mut failures = 0_usize;
    for path in paths {
        if let Err(error) = analyze_one(&pipeline, path).await {
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

/// Opens the three models and the marker font from the environment.
async fn load() -> Result<Pipeline, Box<dyn std::error::Error>> {
    let shard = required_path(MODEL_SHARD_ENV)?;
    let sam3_export = required_path(SAM3_EXPORT_ENV)?;
    let dinov3_export = required_path(DINOV3_EXPORT_ENV)?;
    let cache_root = std::env::var_os(COREML_CACHE_ENV).map(PathBuf::from);
    let accel = cache_root
        .as_deref()
        .map_or(Accel::Cpu, |root| Accel::CoreML { cache_root: root });

    let qwen = Qwen3::open(&shard).await?;
    let sam3 = Sam3::open(&sam3_export, accel)?;
    let dinov3 = Dinov3::open(&dinov3_export, accel)?;
    let font = font_from_env()?;
    Ok(Pipeline::new(qwen, sam3, dinov3, font))
}

/// Reads one image and writes its analysis and panel overlay.
async fn analyze_one(
    pipeline: &Pipeline,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let image = image::open(path)?;
    let analysis = pipeline.analyze_image(&image).await?;
    report(path, &image, &analysis)?;
    Ok(())
}

/// Writes the analysis JSON and the panel overlay, and says what was found.
fn report(
    path: &Path,
    image: &image::DynamicImage,
    analysis: &Analysis,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let json_path = output_path(path, ".analysis.json");
    std::fs::write(&json_path, serde_json::to_string_pretty(analysis)?)?;

    let rects: Vec<SubimageRegion> = analysis
        .panels
        .iter()
        .map(|panel| SubimageRegion::Rect { rect: panel.rect })
        .collect();
    let png_path = output_path(path, ".panels.png");
    draw_subimages(image, &rects).save(&png_path)?;

    println!(
        "{}: {} panel(s) -> {}",
        path.display(),
        analysis.panels.len(),
        json_path.display()
    );
    Ok(())
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
