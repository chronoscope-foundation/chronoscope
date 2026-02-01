//! CLI for analyzing images via Triton.
//!
//! Usage:
//!   analyze [--endpoint URL] <image>
//!
//! Examples:
//!   # Analyze a local image
//!   analyze --endpoint <http://localhost:8000> photo.jpg
//!
//!   # With port forwarding active:
//!   # Terminal 1: northflank forward service --projectId chronoscope-analysis --serviceId triton --port 8000
//!   # Terminal 2: analyze photo.jpg

use std::path::PathBuf;
use std::sync::Arc;

use chronoscope_integrations::ReqwestClient;
use clap::Parser;
use tracing_subscriber::EnvFilter;
use url::Url;

use chronoscope_analysis::{AnalysisError, TritonClient};

#[derive(Parser)]
#[command(name = "analyze")]
#[command(about = "Analyze images for historical building research")]
struct Args {
    /// Triton server HTTP endpoint
    #[arg(long, default_value = "http://localhost:8000")]
    endpoint: String,

    /// Image file to analyze
    image: PathBuf,

    /// Save annotated image to this path (optional)
    #[arg(long)]
    save_annotated: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    // Parse endpoint URL and create HTTP client
    let endpoint = Url::parse(&args.endpoint)?;
    let http_client = Arc::new(ReqwestClient::new()?);
    let client = TritonClient::new(endpoint, http_client);

    // Check server health
    tracing::info!("Checking Triton server at {}...", args.endpoint);
    client
        .is_server_ready()
        .await
        .map_err(|e| AnalysisError::Connection(format!("Health check failed: {e}")))?;
    tracing::info!("Server ready");

    // Read image
    tracing::info!("Reading image: {}", args.image.display());
    let image_bytes = std::fs::read(&args.image)?;
    tracing::info!("Image size: {} bytes", image_bytes.len());

    // Analyze
    tracing::info!("Analyzing...");
    let result = client.analyze(&image_bytes).await?;

    // Save annotated image if requested
    if let Some(ref path) = args.save_annotated {
        if let Some(ref annotated_b64) = result.annotated_image {
            use base64::Engine;
            let annotated_bytes = base64::engine::general_purpose::STANDARD
                .decode(annotated_b64)
                .map_err(|e| {
                    AnalysisError::ResponseParsing(format!("Failed to decode annotated image: {e}"))
                })?;
            std::fs::write(path, &annotated_bytes)?;
            tracing::info!("Saved annotated image to {}", path.display());
        } else {
            tracing::warn!("No annotated image in response");
        }
    }

    // Output result as JSON (without the bulky base64 image)
    let mut output = result;
    output.annotated_image = None;
    let json = serde_json::to_string_pretty(&output)?;
    println!("{json}");

    Ok(())
}
