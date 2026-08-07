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
    DevServerConfig, FactsDbSource, ImageResolveMode, facts_db_subset, find_available_port,
    mount_facts_db, start_dev_server,
};
use chronoscope_workers::{ReqwestClient, RetryConfig};
use dropshot::{ConfigLogging, ConfigLoggingLevel};
use slog::info;
use tokio::signal;

/// The image-resolve mode for this run. `CHRONOSCOPE_IMAGE_RESOLVE=placeholder`
/// stores a deterministic placeholder per image with no network; anything else
/// (including unset) fetches the real source images.
fn image_resolve_mode() -> ImageResolveMode {
    match std::env::var("CHRONOSCOPE_IMAGE_RESOLVE").as_deref() {
        Ok("placeholder") => ImageResolveMode::Placeholder,
        _ => ImageResolveMode::Fetch,
    }
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

    // 2. Mount the pre-built facts DB: the read-only artifact
    // `CHRONOSCOPE_FACTS_DB` names (resolved by `just web-dev [subset]` from the
    // pinned fetch), pinned as the frozen `base` (attached `mode=ro&immutable=1`,
    // no clone) beneath a fresh writable overlay. The app tables
    // (auth/queues/media) and the overlay scratch get sibling files in a TempDir,
    // whose -wal/-shm siblings are cleaned up when the guard drops on every exit
    // path, early errors included.
    let db_dir = tempfile::TempDir::with_prefix("chronoscope-web-dev-")?;
    let facts_pin = mount_facts_db(&facts_db_subset())?;
    let database_url = format!("sqlite:{}", db_dir.path().join("app.db").display());
    let facts_overlay = db_dir.path().join("facts-overlay.db").display().to_string();
    info!(log, "Serving facts DB (read-only base) from {facts_pin}");

    // 4. Start the API server
    let http_client =
        Arc::new(ReqwestClient::new().map_err(|e| format!("Failed to create HTTP client: {e}"))?);

    // Interactive dev serves the real Commons images by default; a dense subset's
    // thousand-image fetch gates startup for many minutes, so
    // CHRONOSCOPE_IMAGE_RESOLVE=placeholder swaps in the no-network placeholder.
    let image_resolve = image_resolve_mode();
    info!(log, "Image resolve mode: {image_resolve:?}");

    let server = start_dev_server(DevServerConfig {
        database_url: Some(database_url),
        facts: FactsDbSource::Mounted {
            base: facts_pin,
            overlay: facts_overlay,
        },
        seed_commits: Vec::new(),
        http_client,
        worker_idle_backoff: Duration::from_secs(60),
        retry_config: RetryConfig::default(),
        log: log.clone(),
        port: api_port,
        // Thumbnails resolve to the Trunk front door's `/api` proxy, so the
        // browser fetches them same-origin and Trunk forwards to `/media`.
        cdn_base_url: format!("http://127.0.0.1:{trunk_port}/api"),
        image_resolve,
        rp_id: None,
        rp_origin: None,
        ios_app_id: None,
        apify_config: None,
        dns_resolver: permissive_dns_resolver(),
    })
    .await
    .map_err(|e| format!("Failed to start API server: {e}"))?;

    // 5. Start Trunk, serving the app and reverse-proxying `/api/*` to the API.
    // `--proxy-rewrite=/api/` strips the mount prefix, so `/api/entities` reaches
    // the root `/entities` route — same-origin, so the browser never sees CORS.
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

    // 6. Serve until something tells us to stop, or nothing can any more.
    let reason = shutdown_reason().await;
    info!(log, "Shutting down"; "reason" => reason);
    trunk.kill().ok();
    server.shutdown().await;
    drop(db_dir);

    Ok(())
}

/// Resolves when this server should stop, naming why.
///
/// `just web-dev` runs the server three processes deep (`nix develop`, then
/// `cargo run`), so a launcher that goes away signals nothing down the chain and
/// the server is left holding its ports and its Trunk child. Reparenting to
/// `init` is what that looks like from here, and polling for it is what lets the
/// shutdown below run at all: for a long time these servers outlived the
/// sessions that started them, sixteen deep across two worktrees at one point.
///
/// `SIGTERM` and `SIGHUP` matter for the same reason. A plain `kill` used to
/// take the server down without ever reaching Trunk, which then outlived it.
async fn shutdown_reason() -> &'static str {
    let mut terminate = signal::unix::signal(signal::unix::SignalKind::terminate()).ok();
    let mut hangup = signal::unix::signal(signal::unix::SignalKind::hangup()).ok();

    // A signal we could not register for is one that will never arrive.
    async fn on(source: Option<&mut signal::unix::Signal>) {
        match source {
            Some(signal) => {
                signal.recv().await;
            }
            None => std::future::pending().await,
        }
    }

    async fn orphaned() {
        // Slow enough to cost nothing, quick enough that a session's servers are
        // gone before the next one starts.
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tick.tick().await;
            if std::os::unix::process::parent_id() == 1 {
                return;
            }
        }
    }

    tokio::select! {
        _ = signal::ctrl_c() => "interrupt",
        () = on(terminate.as_mut()) => "terminate",
        () = on(hangup.as_mut()) => "hangup",
        () = orphaned() => "launcher exited",
    }
}
