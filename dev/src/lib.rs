//! Shared setup logic for the development server.
//!
//! This module provides `start_dev_server` which sets up all the shared
//! infrastructure (database, media store, workers, API server) and returns
//! a handle for interacting with the running server.
//!
//! Used by:
//! - The dev binary (with real HTTP client and ngrok)
//! - Integration tests (with VCR HTTP client and localhost)

use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use chronoscope_analysis::TritonService;
use chronoscope_api::jwt::JwtConfig;
use chronoscope_api::state::{AppState, Config};
use chronoscope_db::media_store::{InMemoryMediaStore, MediaStore};
use chronoscope_db::{Database, Email, Queue, ResearchUrl, UserId, url_queue_config};
use chronoscope_workers::analysis::AnalysisWorker;
use chronoscope_workers::url_fetcher::{FetcherConfig, UrlFetcherWorker};
use chronoscope_workers::{
    ApifyConfig, HttpClient, IntegrationName, IntegrationRegistry, NoOpEnqueuer, RetryConfig,
    UrlEnqueuer, WorkerConfig, create_registry, run,
};
use dropshot::{ApiDescription, ConfigDropshot, HttpServerStarter};
use slog::info;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Error type for dev server setup.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct DevServerError(pub String);

impl From<String> for DevServerError {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Handle to a running development server.
pub struct RunningDevServer {
    /// Base URL of the server (e.g., `http://127.0.0.1:8080`)
    pub base_url: String,

    /// Port the server is listening on
    pub port: u16,

    /// JWT auth token for a test user (for making authenticated API requests)
    pub auth_token: String,

    /// User ID of the test user
    pub test_user_id: UserId,

    /// Database pool for direct access (e.g., loading ingestion bundles in tests)
    db: Arc<Database>,

    /// Media store for seeding test images.
    media_store: Arc<dyn MediaStore>,

    /// Send `true` to trigger graceful shutdown of workers
    shutdown_tx: watch::Sender<bool>,

    /// Handles to spawned worker tasks for graceful shutdown
    worker_handles: Vec<JoinHandle<()>>,
}

impl RunningDevServer {
    /// Get the database for direct access.
    ///
    /// Used by integration tests to load ingestion bundles or query DB state
    /// directly, bypassing the HTTP API layer.
    #[must_use]
    pub fn db(&self) -> &Database {
        &self.db
    }

    /// Seed test media by resolving all pending research URLs with placeholder images.
    ///
    /// For each pending URL that has an annotation, generates a small solid-color
    /// JPEG, stores it in the media store, and marks the URL as resolved. This
    /// makes image-dependent features (detail panel grid, map thumbnails,
    /// lightbox) testable without running actual fetch workers.
    ///
    /// Returns the number of media items created.
    pub async fn seed_test_media(&self) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        use image::ImageEncoder;
        use sha2::{Digest, Sha256};

        // Generate a small placeholder JPEG (8x8 solid copper).
        let (w, h) = (8u32, 8u32);
        let rgb_data: Vec<u8> = [0x8Bu8, 0x5E, 0x3C].repeat((w * h) as usize);
        let mut jpeg_buf = std::io::Cursor::new(Vec::new());
        image::codecs::jpeg::JpegEncoder::new(&mut jpeg_buf).write_image(
            &rgb_data,
            w,
            h,
            image::ExtendedColorType::Rgb8,
        )?;
        let jpeg_bytes = bytes::Bytes::from(jpeg_buf.into_inner());

        // Find all unresolved research URLs that have annotations.
        // URLs may be 'pending' or 'processing' (workers can claim them
        // before we seed, even with a long idle backoff).
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT r.id, r.url FROM research_urls r \
             JOIN annotations a ON a.url_id = r.id \
             WHERE r.media_id IS NULL \
             GROUP BY r.id",
        )
        .fetch_all(self.db.pool_ref())
        .await?;

        let now = chrono::Utc::now().naive_utc();
        let mut count = 0;

        for (url_id, source_url) in &rows {
            let exact_hash = Sha256::digest(source_url.as_bytes()).to_vec();
            let url_id = chronoscope_db::ResearchUrlId::new(url_id.clone());
            let storage_key = format!("media/{}.jpg", uuid::Uuid::now_v7());

            // Store full image + thumbnail variant in the media store
            self.media_store
                .put(&storage_key, jpeg_bytes.clone(), "image/jpeg")
                .await?;
            self.media_store
                .put(
                    &storage_key.replace(".jpg", "_thumb.jpg"),
                    jpeg_bytes.clone(),
                    "image/jpeg",
                )
                .await?;

            let media_data = chronoscope_db::MediaData {
                exact_hash,
                perceptual_hash: None,
                storage_key,
                media_type: chronoscope_db::MediaType::Image,
                width: w as i32,
                height: h as i32,
                duration_seconds: None,
                captured: None,
                location: None,
                source_metadata: None,
                fetched_at: now,
            };
            let media_id = self.db.get_or_create_media(&media_data).await?;
            self.db
                .mark_url_resolved_to_media(&url_id, &media_id)
                .await?;

            count += 1;
        }

        Ok(count)
    }

    /// Gracefully shut down the server and wait for all workers to stop.
    pub async fn shutdown(mut self) {
        // Signal workers to stop
        let _ = self.shutdown_tx.send(true);

        // Take ownership of handles (leaving empty vec) so we can await them
        // despite implementing Drop
        let handles = std::mem::take(&mut self.worker_handles);
        for handle in handles {
            let _ = handle.await;
        }
    }
}

impl Drop for RunningDevServer {
    fn drop(&mut self) {
        // Signal workers to stop. We can't await here, but signaling is enough
        // to prevent workers from picking up new tasks. The handles will be
        // dropped, which is fine since workers are detached tasks.
        let _ = self.shutdown_tx.send(true);
    }
}

/// Configuration for starting the dev server.
pub struct DevServerConfig {
    /// Optional database URL. If `None`, uses `sqlite::memory:`.
    /// The server creates a writable copy if the source is read-only.
    pub database_url: Option<String>,

    /// HTTP client for workers to use (real or VCR)
    pub http_client: Arc<dyn HttpClient>,

    /// How long workers wait when idle before polling again
    pub worker_idle_backoff: Duration,

    /// Retry configuration for workers
    pub retry_config: RetryConfig,

    /// Logger for the server
    pub log: slog::Logger,

    /// Port to bind to (use `find_available_port()` to get one)
    pub port: u16,

    /// Base URL for CDN/media assets (e.g., ngrok URL or localhost)
    pub cdn_base_url: String,

    /// Optional: RP ID for WebAuthn (defaults to "localhost")
    pub rp_id: Option<String>,

    /// Optional: RP Origin for WebAuthn (defaults to "http://localhost:{port}")
    pub rp_origin: Option<String>,

    /// Optional: iOS app ID for AASA
    pub ios_app_id: Option<String>,

    /// Optional: Apify configuration for Instagram integration.
    /// If provided, an Instagram worker will be spawned.
    pub apify_config: Option<ApifyConfig>,

    /// Optional: Triton service for image analysis.
    /// If provided, an analysis worker will be spawned.
    pub triton: Option<Arc<dyn TritonService>>,

    /// DNS resolver for URL security validation.
    /// Use `default_dns_resolver()` for system DNS or `permissive_dns_resolver()`
    /// for offline environments (e.g., tests).
    pub dns_resolver: Box<dyn chronoscope_api::state::DnsResolver>,
}

/// Find an available port by binding to port 0 and reading the assigned port.
///
/// # TOCTOU Note
///
/// There's a small race window between finding the port here and actually binding
/// to it later. We accept this because:
/// 1. This is only used for local development and testing
/// 2. Port collisions are rare on a dev machine in practice
/// 3. The alternative (binding to port 0 in Dropshot) would require knowing the
///    ngrok URL before we know the port, creating a chicken-and-egg problem
///
/// # Errors
///
/// Returns an I/O error if unable to bind to any available port.
pub fn find_available_port() -> std::io::Result<u16> {
    // TcpListener closes on drop, freeing the port for later use
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// Shared context for spawning URL fetcher workers.
struct WorkerContext {
    db: Arc<Database>,
    http_client: Arc<dyn HttpClient>,
    media_store: Arc<dyn MediaStore>,
    user_id: UserId,
    idle_backoff: Duration,
    retry_config: RetryConfig,
    shutdown_rx: watch::Receiver<bool>,
    log: slog::Logger,
    /// Pre-configured integration registry (cloned per worker).
    registry: IntegrationRegistry,
}

/// Spawn a URL fetcher worker in a background task, returning its handle.
///
/// If `affinity` is `None`, spawns a generic worker that claims URLs with no affinity.
/// If `affinity` is `Some(name)`, spawns a specialized worker for that integration.
fn spawn_url_fetcher_worker(
    name: &str,
    affinity: Option<IntegrationName>,
    ctx: &WorkerContext,
) -> JoinHandle<()> {
    use chronoscope_workers::url_fetcher::FetchContext;

    let worker_id = name.to_string();

    // Create queue based on affinity
    let queue: Arc<Queue<ResearchUrl>> = match affinity {
        None => ctx.db.url_queue_generic.clone(),
        Some(integration) => {
            // Create a queue for this specific integration
            let config = url_queue_config(Some(integration));
            Arc::new(Queue::new(ctx.db.pool_ref().clone(), config))
        }
    };

    let fetch_ctx = Arc::new(FetchContext {
        db: ctx.db.clone(),
        http: ctx.http_client.clone(),
        media_store: ctx.media_store.clone(),
        config: FetcherConfig::default(),
    });

    let worker = UrlFetcherWorker::new(ctx.registry.clone(), fetch_ctx);
    let enqueuer = UrlEnqueuer::new(ctx.db.clone(), ctx.user_id.clone());
    let worker_config = WorkerConfig {
        worker_id: worker_id.clone(),
        batch_size: 10,
        stale_after: Duration::from_mins(5),
        idle_backoff: ctx.idle_backoff,
    };

    let retry_config = ctx.retry_config.clone();
    let shutdown_rx = ctx.shutdown_rx.clone();
    let log = ctx.log.clone();

    tokio::spawn(async move {
        info!(log, "Starting worker"; "worker_id" => &worker_id);
        if let Err(e) = run(
            queue,
            worker,
            enqueuer,
            worker_config,
            retry_config,
            shutdown_rx,
        )
        .await
        {
            slog::error!(log, "Worker error"; "worker_id" => &worker_id, "error" => %e);
        }
    })
}

/// Spawn an analysis worker in a background task.
fn spawn_analysis_worker(
    worker_id: &str,
    triton: Arc<dyn TritonService>,
    ctx: &WorkerContext,
) -> JoinHandle<()> {
    let worker_id = worker_id.to_string();
    let queue = ctx.db.analysis_queue.clone();
    let worker = AnalysisWorker::new(triton, ctx.db.clone(), ctx.media_store.clone());
    let enqueuer = NoOpEnqueuer;

    let worker_config = WorkerConfig {
        worker_id: worker_id.clone(),
        batch_size: 1, // Process one at a time (scale via multiple workers)
        stale_after: Duration::from_mins(10), // Analysis can take longer
        idle_backoff: ctx.idle_backoff,
    };

    let retry_config = ctx.retry_config.clone();
    let shutdown_rx = ctx.shutdown_rx.clone();
    let log = ctx.log.clone();

    tokio::spawn(async move {
        info!(log, "Starting analysis worker"; "worker_id" => &worker_id);
        if let Err(e) = run(
            queue,
            worker,
            enqueuer,
            worker_config,
            retry_config,
            shutdown_rx,
        )
        .await
        {
            slog::error!(log, "Analysis worker error"; "worker_id" => &worker_id, "error" => %e);
        }
    })
}

/// Start the development server with the given configuration.
///
/// This sets up:
/// - In-memory SQLite database
/// - In-memory media store
/// - System user for workers
/// - Test user with JWT token (for API authentication)
/// - URL fetcher workers
/// - API server with embedded media serving
///
/// Returns a handle that can be used to interact with the server and shut it down.
///
/// # Errors
///
/// Returns [`DevServerError`] if server initialization fails (database setup,
/// user creation, API server startup, etc.).
pub async fn start_dev_server(config: DevServerConfig) -> Result<RunningDevServer, DevServerError> {
    let log = &config.log;
    let port = config.port;
    let base_url = format!("http://127.0.0.1:{port}");

    info!(log, "Starting dev server"; "port" => port, "base_url" => &base_url);

    // Create shutdown channel for graceful termination
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // ==================== Shared Infrastructure ====================

    // Create shared database (includes default integration registry)
    let db_url = config.database_url.as_deref().unwrap_or("sqlite::memory:");
    let db = Arc::new(
        Database::new(
            db_url,
            &chronoscope_db::resolve_regions_db()
                .map_err(|e| format!("Failed to resolve regions DB path: {e}"))?,
        )
        .await
        .map_err(|e| format!("Failed to create database: {e}"))?,
    );

    // Create shared media store
    let media_store: Arc<dyn MediaStore> = Arc::new(InMemoryMediaStore::new());

    // Create test user (used for both API authentication and worker URL discoveries)
    let test_user_id = UserId::generate();
    db.create_user(
        &test_user_id,
        "test-user",
        &Email::new("test@chronoscope.local"),
    )
    .await
    .map_err(|e| format!("Failed to create test user: {e}"))?;
    info!(log, "Created test user"; "user_id" => %test_user_id);

    // ==================== Start URL Fetcher Workers ====================

    // Create integration registry once, cloned per worker
    let registry = create_registry(config.apify_config.clone())
        .map_err(|e| DevServerError(format!("Failed to create integration registry: {e}")))?;

    let worker_ctx = WorkerContext {
        db: db.clone(),
        http_client: config.http_client.clone(),
        media_store: media_store.clone(),
        user_id: test_user_id.clone(),
        idle_backoff: config.worker_idle_backoff,
        retry_config: config.retry_config.clone(),
        shutdown_rx: shutdown_rx.clone(),
        log: log.clone(),
        registry,
    };

    // Spawn workers per affinity: one for each integration + one generic
    let mut worker_handles = vec![
        // Reddit worker
        spawn_url_fetcher_worker("reddit-worker", Some(IntegrationName::Reddit), &worker_ctx),
        // Generic worker (handles URLs with no specialized integration)
        spawn_url_fetcher_worker("generic-worker", None, &worker_ctx),
    ];

    // Spawn Instagram worker if Apify credentials are provided
    if config.apify_config.is_some() {
        worker_handles.push(spawn_url_fetcher_worker(
            "instagram-worker",
            Some(IntegrationName::Instagram),
            &worker_ctx,
        ));
        info!(log, "Instagram worker enabled with Apify integration");
    }

    // Spawn analysis worker if Triton service is provided
    if let Some(triton) = config.triton {
        worker_handles.push(spawn_analysis_worker(
            "analysis-worker",
            triton,
            &worker_ctx,
        ));
        info!(log, "Analysis worker enabled");
    }

    info!(log, "Started workers"; "count" => worker_handles.len());

    // ==================== Start API Server ====================

    let rp_id = config.rp_id.unwrap_or_else(|| "localhost".to_string());
    let rp_origin = config
        .rp_origin
        .unwrap_or_else(|| format!("http://localhost:{port}"));

    let api_config = Config {
        database_url: "sqlite::memory:".to_string(), // Not used - we pass db directly
        rp_id,
        rp_origin,
        bind_addr: format!("127.0.0.1:{port}")
            .parse()
            .map_err(|e| format!("Invalid bind address: {e}"))?,
        ios_app_id: config.ios_app_id,
        cdn_base_url: config.cdn_base_url,
    };

    // Generate a random JWT secret for this session
    let jwt_secret = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let jwt_config = JwtConfig::new(
        &jwt_secret,
        7 * 24 * 60 * 60, // 7 days session expiry
        2 * 60,           // 2 min challenge expiry
        60,               // 60s leeway
    );

    // Generate auth token for the test user
    let auth_token = jwt_config
        .create_session_token(&test_user_id)
        .map_err(|e| format!("Failed to create auth token: {e}"))?;

    info!(log, "Starting API server";
        "rp_id" => &api_config.rp_id,
        "rp_origin" => &api_config.rp_origin,
        "bind_addr" => %api_config.bind_addr,
        "cdn_base_url" => &api_config.cdn_base_url,
    );

    // Configure Dropshot (before api_config is moved)
    let config_dropshot = ConfigDropshot {
        bind_address: api_config.bind_addr,
        default_request_body_max_bytes: 1024 * 1024,
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };

    // Create AppState with our shared database and media store
    let media_store_for_server = media_store.clone();
    let app_state = AppState::new(
        db.as_ref().clone(),
        api_config,
        jwt_config,
        config.dns_resolver,
        media_store,
    )
    .await
    .map_err(|e| format!("Failed to create app state: {e}"))?;
    let app_state = Arc::new(app_state);

    // Build API
    let mut api = ApiDescription::new();
    chronoscope_api::register_api(&mut api).map_err(|e| format!("Failed to register API: {e}"))?;

    // Start server (spawns in background)
    let server = HttpServerStarter::new(&config_dropshot, api, app_state, log)
        .map_err(|e| format!("Failed to start server: {e}"))?
        .start();

    // Spawn the server task to run in background
    tokio::spawn(async move {
        if let Err(e) = server.await {
            eprintln!("Server error: {e}");
        }
    });

    // Give the server a moment to start listening before returning.
    // This avoids races where callers try to connect before the socket is bound.
    #[allow(clippy::disallowed_methods)]
    tokio::time::sleep(Duration::from_millis(50)).await;

    info!(log, "Dev server ready"; "base_url" => &base_url);

    Ok(RunningDevServer {
        base_url,
        port,
        auth_token,
        test_user_id,
        db,
        media_store: media_store_for_server,
        shutdown_tx,
        worker_handles,
    })
}
