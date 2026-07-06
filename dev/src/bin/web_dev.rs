//! Lightweight web development server.
//!
//! Starts the API server (with test DB) and Trunk live-reload server.
//! No ngrok, no workers, no iOS config — just what's needed for web frontend dev.
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

/// Holds the temporary directory containing the writable copy of the Wikidata
/// test database. The `TempDir` is kept alive alongside the database URL so
/// cleanup happens on drop rather than leaking via `mem::forget`.
struct WebDevDb {
    database_url: String,
    _tmp_dir: tempfile::TempDir,
}

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

    // 2. Use Wikidata test DB if available (set by nix develop), else in-memory
    let web_dev_db = match std::env::var("WIKIDATA_TEST_DB") {
        Ok(path) => {
            let db_path = Path::new(&path).join("wikidata.db");
            if db_path.exists() {
                // Copy to a temp dir so we can write to it (the Nix store is read-only).
                // A TempDir (not TempFile) because SQLite creates -wal and -shm
                // sibling files alongside the main .db — they all need to live
                // in the same directory and be cleaned up together.
                let tmp_dir = tempfile::tempdir()?;
                let tmp_db = tmp_dir.path().join("wikidata.db");
                std::fs::copy(&db_path, &tmp_db)?;
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(0o644);
                std::fs::set_permissions(&tmp_db, perms)?;
                let url = format!("sqlite:{}", tmp_db.display());
                info!(log, "Loaded Wikidata test DB from {}", db_path.display());
                Some(WebDevDb {
                    database_url: url,
                    _tmp_dir: tmp_dir,
                })
            } else {
                return Err(format!(
                    "WIKIDATA_TEST_DB is set but {} does not exist",
                    db_path.display()
                )
                .into());
            }
        }
        Err(_) => {
            info!(log, "No WIKIDATA_TEST_DB found, using empty in-memory DB");
            None
        }
    };

    let database_url = web_dev_db.as_ref().map(|db| db.database_url.clone());

    // 2b. Use a curated Wikidata entities.jsonl for the fact store if available.
    let wikidata_entities_jsonl = std::env::var("WIKIDATA_ENTITIES_JSONL")
        .ok()
        .map(PathBuf::from);

    // 3. Start the API server
    let http_client =
        Arc::new(ReqwestClient::new().map_err(|e| format!("Failed to create HTTP client: {e}"))?);

    let server = start_dev_server(DevServerConfig {
        database_url,
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
        wikidata_entities_jsonl,
    })
    .await
    .map_err(|e| format!("Failed to start API server: {e}"))?;

    // 4. Start Trunk with CHRONOSCOPE_API_URL set.
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

    // 5. Wait for Ctrl+C
    signal::ctrl_c().await.ok();
    info!(log, "Shutting down...");
    trunk.kill().ok();
    server.shutdown().await;

    Ok(())
}
