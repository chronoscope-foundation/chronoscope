//! Corpus test CLI.
//!
//! Requires the `corpus-test` feature:
//!   cargo run -p chronoscope-analysis --features corpus-test --bin corpus -- <command>

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

use chronoscope_analysis::corpus::{self, CorpusError, runner};

#[derive(Parser)]
#[command(name = "corpus", about = "Corpus image test suite tools")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download all corpus images (no models needed).
    Download,
    /// Download images and run the full pipeline, caching results.
    Run,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(LevelFilter::INFO.into()))
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let analysis_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    match run(cli, &analysis_dir).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli, analysis_dir: &std::path::Path) -> Result<(), CorpusError> {
    match cli.command {
        Command::Download => {
            let images = corpus::download_all(analysis_dir).await?;
            eprintln!("Downloaded {} images", images.len());
        }
        Command::Run => {
            let pipeline_hash = runner::compute_pipeline_hash(analysis_dir)?;
            eprintln!("Pipeline hash: {pipeline_hash}");

            let image_paths = corpus::download_all(analysis_dir).await?;
            eprintln!("Downloaded {} images", image_paths.len());

            let results = runner::run_pipeline(analysis_dir, &pipeline_hash, &image_paths)?;
            eprintln!("Processed {} images", results.len());
        }
    }

    Ok(())
}
