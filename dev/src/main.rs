//! Development server with ngrok tunnel and background workers.
//!
//! This binary:
//! 1. Starts ngrok to create a public HTTPS tunnel (required for passkeys)
//! 2. Updates ios/Local.xcconfig with the tunnel domain
//! 3. Spawns URL fetcher workers to process submitted URLs
//! 4. Runs the API server with embedded media serving at /media/{key}
//!
//! Usage:
//!   cargo run -p chronoscope-dev
//!
//! Prerequisites:
//!   - ngrok installed and authenticated (`ngrok config add-authtoken YOUR_TOKEN`)
//!
//! Once running, open Xcode and build/run the iOS app normally.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use chronoscope_api::state::default_dns_resolver;
use chronoscope_dev::{DevServerConfig, start_dev_server};
use chronoscope_workers::{ApifyConfig, ReqwestClient, RetryConfig};
use dropshot::{ConfigLogging, ConfigLoggingLevel};
use slog::{error, info, warn};
use tokio::signal;
use tokio::time::sleep;
use tracing::Level;

const NGROK_API_URL: &str = "http://localhost:4040/api/tunnels";

// CARGO_MANIFEST_DIR is a compile-time constant that always has a parent directory.
#[allow(clippy::expect_used)]
fn ios_xcconfig_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("CARGO_MANIFEST_DIR has no parent")
        .join("ios/Local.xcconfig")
}

fn read_xcconfig_value(path: &Path, key: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("//") || line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once('=')
            && k.trim() == key
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

#[derive(serde::Deserialize)]
struct NgrokTunnels {
    tunnels: Vec<NgrokTunnel>,
}

#[derive(serde::Deserialize)]
struct NgrokTunnel {
    public_url: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Set up tracing for workers (they use tracing, not slog)
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_target(false)
        .init();

    // Set up slog for API/ngrok logging
    let config_logging = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Info,
    };
    let log = config_logging.to_logger("chronoscope-dev")?;

    info!(log, "Starting development server with ngrok tunnel");

    // Run the main logic with cleanup on Ctrl+C
    let result = tokio::select! {
        result = run_dev_server(&log) => result,
        _ = signal::ctrl_c() => {
            info!(log, "Shutting down...");
            Ok(())
        }
    };

    result
}

fn start_ngrok(
    log: &slog::Logger,
    port: u16,
) -> Result<Child, Box<dyn std::error::Error + Send + Sync>> {
    info!(log, "Starting ngrok tunnel"; "port" => port);

    let child = Command::new("ngrok")
        .args(["http", &port.to_string(), "--log=stdout"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "ngrok not found. Install with: brew install ngrok".into()
            } else {
                format!("Failed to start ngrok: {e}")
            }
        })?;

    Ok(child)
}

// Polling for ngrok tunnel to become available after spawning the process.
#[allow(clippy::disallowed_methods)]
async fn wait_for_ngrok_url(
    log: &slog::Logger,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    info!(log, "Waiting for ngrok tunnel...");

    let client = reqwest::Client::new();

    for attempt in 1..=30 {
        sleep(Duration::from_millis(500)).await;

        match client.get(NGROK_API_URL).send().await {
            Ok(response) if response.status().is_success() => {
                if let Ok(tunnels) = response.json::<NgrokTunnels>().await {
                    // Find the HTTPS tunnel
                    if let Some(tunnel) = tunnels
                        .tunnels
                        .iter()
                        .find(|t| t.public_url.starts_with("https://"))
                    {
                        return Ok(tunnel.public_url.clone());
                    }
                }
            }
            Ok(_) => {}
            Err(_) if attempt < 30 => {}
            Err(e) => {
                return Err(format!("Failed to connect to ngrok API: {e}").into());
            }
        }

        if attempt % 10 == 0 {
            warn!(log, "Still waiting for ngrok... (attempt {})", attempt);
        }
    }

    Err("Timed out waiting for ngrok tunnel. Is ngrok authenticated?".into())
}

fn extract_domain(url_str: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let parsed = url::Url::parse(url_str)?;
    parsed
        .host_str()
        .map(String::from)
        .ok_or_else(|| "ngrok URL has no host".into())
}

fn update_xcconfig(
    log: &slog::Logger,
    domain: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let path = ios_xcconfig_path();

    if !path.exists() {
        return Err(format!(
            "Local.xcconfig not found at {}. Copy from Local.xcconfig.example first.",
            path.display()
        )
        .into());
    }

    let content = std::fs::read_to_string(&path)?;
    let mut lines: Vec<String> = content.lines().map(String::from).collect();
    let mut found = false;

    // Update existing API_SERVER_DOMAIN line or add new one
    for line in &mut lines {
        if line.starts_with("API_SERVER_DOMAIN")
            || line.starts_with("// API_SERVER_DOMAIN")
            || line.starts_with("//API_SERVER_DOMAIN")
        {
            *line = format!("API_SERVER_DOMAIN = {domain}");
            found = true;
            break;
        }
    }

    if !found {
        lines.push(format!("API_SERVER_DOMAIN = {domain}"));
    }

    std::fs::write(&path, lines.join("\n") + "\n")?;

    info!(
        log,
        "Updated {} with API_SERVER_DOMAIN = {}",
        path.display(),
        domain
    );
    Ok(())
}

async fn run_dev_server(
    log: &slog::Logger,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 1. Find an available port
    let port =
        chronoscope_dev::find_available_port().map_err(|e| format!("Failed to find port: {e}"))?;
    info!(log, "Found available port"; "port" => port);

    // 2. Start ngrok pointing to that port
    let mut ngrok = start_ngrok(log, port)?;

    // Spawn a task to log ngrok errors
    if let Some(stderr) = ngrok.stderr.take() {
        let log_clone = log.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if line.contains("ERR") || line.contains("error") {
                    error!(log_clone, "ngrok: {}", line);
                }
            }
        });
    }

    // 3. Wait for ngrok URL
    let ngrok_url = wait_for_ngrok_url(log).await?;
    let ngrok_domain = extract_domain(&ngrok_url)?;

    info!(log, "");
    info!(log, "========================================");
    info!(log, "ngrok tunnel: {}", ngrok_url);
    info!(log, "========================================");
    info!(log, "");

    // 4. Update iOS xcconfig with ngrok domain
    update_xcconfig(log, &ngrok_domain)?;

    // 5. Read team ID for iOS app identifier
    let ios_app_id = read_xcconfig_value(&ios_xcconfig_path(), "DEVELOPMENT_TEAM")
        .map(|team_id| format!("{team_id}.chronoscope.app"));

    // 6. Create HTTP client for workers
    let http_client =
        Arc::new(ReqwestClient::new().map_err(|e| format!("Failed to create HTTP client: {e}"))?);

    // 7. Check for Apify credentials for Instagram integration
    let apify_config = std::env::var("APIFY_API_TOKEN").ok().map(|api_token| {
        info!(
            log,
            "Apify credentials found - Instagram integration enabled"
        );
        ApifyConfig::new(api_token)
    });

    // 8. Start the dev server with ngrok URL as CDN base
    let server = start_dev_server(DevServerConfig {
        database_url: None,
        http_client,
        worker_idle_backoff: Duration::from_secs(5),
        retry_config: RetryConfig::default(),
        log: log.clone(),
        port,
        cdn_base_url: ngrok_url.clone(),
        rp_id: Some(ngrok_domain.clone()),
        rp_origin: Some(ngrok_url.clone()),
        ios_app_id,
        apify_config,
        // Analysis worker is optional - connect via gRPC if TRITON_ENDPOINT is set
        triton: match std::env::var("TRITON_ENDPOINT") {
            Ok(endpoint) => Some(std::sync::Arc::new(
                chronoscope_analysis::GrpcTritonClient::connect(&endpoint)
                    .await
                    .map_err(|e| format!("Failed to connect to Triton: {e}"))?,
            )),
            Err(_) => None,
        },
        dns_resolver: default_dns_resolver()
            .map_err(|e| format!("Failed to create DNS resolver: {e}"))?,
        wikidata_entities_jsonl: std::env::var("WIKIDATA_ENTITIES_JSONL")
            .ok()
            .map(std::path::PathBuf::from),
    })
    .await
    .map_err(|e| format!("Failed to start dev server: {e}"))?;

    // Print ready message
    info!(log, "");
    info!(log, "========================================");
    info!(log, "Development server ready!");
    info!(log, "");
    info!(log, "Running:");
    info!(log, "  - API server: {}", ngrok_url);
    info!(log, "  - Local: {}", server.base_url);
    info!(log, "  - Media served at: {}/media/{{key}}", ngrok_url);
    info!(log, "");
    info!(log, "Next steps:");
    info!(log, "  1. Open ios/Chronoscope.xcodeproj in Xcode");
    info!(log, "  2. Build and run (Cmd+R)");
    info!(log, "");
    info!(log, "The iOS app will connect to: {}", ngrok_url);
    info!(log, "========================================");
    info!(log, "");

    // Wait for shutdown signal
    signal::ctrl_c().await.ok();
    info!(log, "Shutting down...");

    // Gracefully shutdown server and wait for workers
    server.shutdown().await;

    // Kill ngrok
    ngrok.kill().ok();

    Ok(())
}
