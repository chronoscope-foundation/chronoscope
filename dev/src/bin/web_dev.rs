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

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use chronoscope_api::state::permissive_dns_resolver;
use chronoscope_dev::{
    DevServerConfig, ImageResolveMode, facts_db_subset, find_available_port, mount_facts_db,
    start_dev_server,
};
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

    // 2. Mount the pre-built facts DB: a fresh copy-on-write clone of the
    // read-only artifact `CHRONOSCOPE_FACTS_DB` names (resolved by
    // `just web-dev [subset]` from the pinned fetch), opened read-write as
    // this run's whole database. A TempDir guard because SQLite writes
    // -wal/-shm siblings next to the .db; dropping it cleans up on every
    // exit path, early errors included.
    let db_dir = tempfile::TempDir::with_prefix("chronoscope-web-dev-")?;
    let database_url = mount_facts_db(db_dir.path(), &facts_db_subset())?;
    info!(log, "Facts DB clone at {database_url}");

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
        // Thumbnails resolve to the Trunk front door's `/api` proxy, so the
        // browser fetches them same-origin and Trunk forwards to `/media`.
        cdn_base_url: format!("http://127.0.0.1:{trunk_port}/api"),
        // Interactive dev serves the real Commons images from our media store.
        image_resolve: ImageResolveMode::Fetch,
        rp_id: None,
        rp_origin: None,
        ios_app_id: None,
        apify_config: None,
        triton: None,
        dns_resolver: permissive_dns_resolver(),
    })
    .await
    .map_err(|e| format!("Failed to start API server: {e}"))?;

    // 5. Start Trunk, serving the app and reverse-proxying `/api/*` to the API.
    // `--proxy-rewrite=/api/` strips the mount prefix, so `/api/markers` reaches
    // the root `/markers` route — same-origin, so the browser never sees CORS.
    let web_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("no parent")?
        .join("web");
    info!(log, "Starting Trunk live-reload server...");
    let trunk_port_str = trunk_port.to_string();
    let proxy_backend = format!("--proxy-backend=http://127.0.0.1:{api_port}/");
    let mut trunk = Command::new("trunk")
        .args([
            "serve",
            "--port",
            &trunk_port_str,
            &proxy_backend,
            "--proxy-rewrite=/api/",
        ])
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
    drop(db_dir);

    Ok(())
}
