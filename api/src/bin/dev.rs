//! Development server with ngrok tunnel.
//!
//! This binary:
//! 1. Starts ngrok to create a public HTTPS tunnel (required for passkeys)
//! 2. Updates ios/Local.xcconfig with the tunnel domain
//! 3. Runs the API server with appropriate WebAuthn configuration
//!
//! Usage:
//!   cargo run --bin dev
//!
//! Prerequisites:
//!   - ngrok installed and authenticated (`ngrok config add-authtoken YOUR_TOKEN`)
//!
//! Once running, open Xcode and build/run the iOS app normally.

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use chronoscope_api::jwt::JwtConfig;
use chronoscope_api::state::{AppState, Config};
use dropshot::{
    ApiDescription, ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpServerStarter,
};
use slog::{error, info, warn};
use tokio::signal;
use tokio::time::sleep;

const NGROK_API_URL: &str = "http://localhost:4040/api/tunnels";

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

/// Find an available port by binding to port 0 and reading the assigned port.
/// There's a small race window between dropping the listener and using the port,
/// but this is a dev script so we accept that tradeoff over hardcoding a port.
fn find_available_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
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
    // Set up logging
    let config_logging = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Info,
    };
    let log = config_logging.to_logger("chronoscope-dev")?;

    let port = find_available_port()?;
    info!(log, "Starting development server with ngrok tunnel"; "port" => port);

    let mut ngrok = start_ngrok(&log, port)?;

    // Set up cleanup on Ctrl+C
    let cleanup_log = log.clone();
    let cleanup = async move {
        signal::ctrl_c().await.ok();
        info!(cleanup_log, "Shutting down...");
    };

    // Wait for ngrok and run server, with cleanup on interrupt
    let result = tokio::select! {
        result = run_with_ngrok(&log, &mut ngrok, port) => result,
        () = cleanup => {
            ngrok.kill().ok();
            Ok(())
        }
    };

    // Ensure ngrok is killed on exit
    ngrok.kill().ok();

    result
}

fn start_ngrok(
    log: &slog::Logger,
    port: u16,
) -> Result<Child, Box<dyn std::error::Error + Send + Sync>> {
    info!(log, "Starting ngrok tunnel on port {}", port);

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

async fn run_with_ngrok(
    log: &slog::Logger,
    ngrok: &mut Child,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Spawn a task to log ngrok output (helps debug issues)
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

    // Wait for ngrok URL
    let ngrok_url = wait_for_ngrok_url(log).await?;
    let ngrok_domain = extract_domain(&ngrok_url)?;

    info!(log, "");
    info!(log, "========================================");
    info!(log, "ngrok tunnel: {}", ngrok_url);
    info!(log, "========================================");
    info!(log, "");

    // Update iOS xcconfig
    update_xcconfig(log, &ngrok_domain)?;

    // Read team ID from xcconfig to construct iOS app identifier
    let ios_app_id = read_xcconfig_value(&ios_xcconfig_path(), "DEVELOPMENT_TEAM")
        .map(|team_id| format!("{team_id}.chronoscope.app"));

    let config = Config {
        database_url: "sqlite::memory:".to_string(),
        rp_id: ngrok_domain.clone(),
        rp_origin: ngrok_url.clone(),
        bind_addr: format!("0.0.0.0:{port}").parse()?,
        ios_app_id,
        cdn_base_url: "https://cdn.chronoscope.io".to_string(),
    };

    // Generate a random JWT secret for this dev session
    let jwt_secret = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let jwt_config = JwtConfig::new(
        &jwt_secret,
        7 * 24 * 60 * 60, // 7 days session expiry
        2 * 60,           // 2 min challenge expiry
        60,               // 60s leeway
    );

    info!(log, "Starting API server";
        "database" => &config.database_url,
        "rp_id" => &config.rp_id,
        "rp_origin" => &config.rp_origin,
        "bind_addr" => %config.bind_addr,
    );

    let app_state = Arc::new(AppState::new_with_jwt(config, jwt_config).await?);

    // Configure Dropshot
    let config_dropshot = ConfigDropshot {
        bind_address: app_state.config.bind_addr,
        default_request_body_max_bytes: 1024 * 1024,
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };

    // Build API
    let mut api = ApiDescription::new();
    chronoscope_api::register_api(&mut api)?;

    // Start server
    info!(log, "");
    info!(log, "========================================");
    info!(log, "Development server ready!");
    info!(log, "");
    info!(log, "Next steps:");
    info!(log, "  1. Open ios/Chronoscope.xcodeproj in Xcode");
    info!(log, "  2. Build and run (Cmd+R)");
    info!(log, "");
    info!(log, "The iOS app will connect to: {}", ngrok_url);
    info!(log, "========================================");
    info!(log, "");

    let server = HttpServerStarter::new(&config_dropshot, api, app_state, log)?.start();
    server.await.map_err(Into::into)
}
