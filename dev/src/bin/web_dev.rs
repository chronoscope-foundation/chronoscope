//! Lightweight web development server.
//!
//! Starts the API server (with a fresh throwaway SQLite DB) and Trunk
//! live-reload server. No ngrok, no workers, no iOS config — just what's
//! needed for web frontend dev.
//!
//! Usage:
//!   cargo run -p chronoscope-dev --bin web-dev
//!
//! Or via justfile:
//!   just web-dev

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use chronoscope_api::state::permissive_dns_resolver;
use chronoscope_dev::{DevServerConfig, ImageResolveMode, find_available_port, start_dev_server};
use chronoscope_workers::{ReqwestClient, RetryConfig};
use dropshot::{ConfigLogging, ConfigLoggingLevel};
use slog::info;
use tokio::signal;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Workers and the fact-store image resolver report via `tracing`; without a
    // subscriber their warnings (e.g. a skipped image fetch) vanish silently.
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    let config_logging = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Info,
    };
    let log = config_logging.to_logger("web-dev")?;

    // 1. Find available ports for the API server and Trunk
    let api_port = find_available_port()?;
    let trunk_port = find_available_port()?;
    info!(log, "API server will bind to port {}", api_port);
    info!(log, "Trunk will bind to port {}", trunk_port);

    // 2. Curated Wikidata entities.jsonl for the fact store.
    let wikidata_entities_jsonl = PathBuf::from(
        std::env::var("WIKIDATA_ENTITIES_JSONL")
            .map_err(|_| "WIKIDATA_ENTITIES_JSONL not set — run inside nix develop")?,
    );

    // 3. Fresh file-backed SQLite DB for this run — the pool reaps idle
    // connections, and a shared-cache in-memory DB would vanish with its last
    // one during a quiet stretch. A dedicated directory because SQLite writes
    // -wal/-shm siblings next to the .db; removed on shutdown.
    let db_dir = std::env::temp_dir().join(format!("chronoscope-web-dev-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&db_dir)?;
    let database_url = format!("sqlite:{}", db_dir.join("web-dev.db").display());
    info!(log, "Database at {}", db_dir.join("web-dev.db").display());

    // 4. Start the API server
    let http_client =
        Arc::new(ReqwestClient::new().map_err(|e| format!("Failed to create HTTP client: {e}"))?);

    let server = start_dev_server(DevServerConfig {
        database_url: Some(database_url),
        http_client,
        worker_idle_backoff: Duration::from_secs(60),
        retry_config: RetryConfig::default(),
        log: log.clone(),
        port: api_port,
        cdn_base_url: format!("http://127.0.0.1:{api_port}"),
        // Interactive dev serves the real Commons images from our media store.
        image_resolve: ImageResolveMode::Fetch,
        rp_id: None,
        rp_origin: None,
        ios_app_id: None,
        apify_config: None,
        triton: None,
        dns_resolver: permissive_dns_resolver(),
        wikidata_entities_jsonl: Some(wikidata_entities_jsonl),
    })
    .await
    .map_err(|e| format!("Failed to start API server: {e}"))?;

    // 5. Start Trunk with CHRONOSCOPE_API_URL set.
    // Trunk's post_build hook writes config.json to dist/ using this env var.
    let web_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("no parent")?
        .join("web");
    info!(log, "Starting Trunk live-reload server...");
    let trunk_port_str = trunk_port.to_string();
    let mut trunk = Command::new("trunk")
        .args(["serve", "--port", &trunk_port_str])
        .env("CHRONOSCOPE_API_URL", &server.base_url)
        .current_dir(&web_dir)
        .spawn()
        .map_err(|e| format!("Failed to start trunk: {e}"))?;

    info!(log, "");
    info!(log, "========================================");
    info!(log, "Web development server ready!");
    info!(log, "");
    info!(log, "  Frontend: http://127.0.0.1:{}", trunk_port);
    info!(log, "  API:      {}", server.base_url);
    info!(log, "========================================");
    info!(log, "");

    // 6. Wait for Ctrl+C
    signal::ctrl_c().await.ok();
    info!(log, "Shutting down...");
    trunk.kill().ok();
    server.shutdown().await;
    std::fs::remove_dir_all(&db_dir).ok();

    Ok(())
}
