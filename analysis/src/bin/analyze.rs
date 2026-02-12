//! CLI for analyzing images via Triton.
//!
//! Usage:
//!   analyze [--endpoint URL] <image>
//!
//! Examples:
//!   # Analyze a local image (gRPC endpoint, default port 8001)
//!   analyze --endpoint <http://localhost:8001> photo.jpg

use std::path::PathBuf;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use chronoscope_analysis::{AnalysisError, GrpcTritonClient, TritonService};

#[derive(Parser)]
#[command(name = "analyze")]
#[command(about = "Analyze images for historical building research")]
struct Args {
    /// Triton server gRPC endpoint
    #[arg(long)]
    endpoint: String,

    /// Image file to analyze
    image: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    // Connect to Triton via gRPC
    tracing::info!("Connecting to Triton at {}...", args.endpoint);
    let client = GrpcTritonClient::connect(&args.endpoint).await?;

    // Check server health
    tracing::info!("Checking server readiness...");
    client
        .is_server_ready()
        .await
        .map_err(|e| AnalysisError::Transport(format!("Health check failed: {e}")))?;
    tracing::info!("Server ready");

    // Read image
    tracing::info!("Reading image: {}", args.image.display());
    let image_bytes = std::fs::read(&args.image)?;
    tracing::info!("Image size: {} bytes", image_bytes.len());

    // Analyze
    tracing::info!("Analyzing...");
    let result = client.analyze(&image_bytes).await?;

    // Output result as JSON
    let json = serde_json::to_string_pretty(&result)?;
    println!("{json}");

    Ok(())
}
