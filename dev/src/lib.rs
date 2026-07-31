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
use chronoscope_api::state::{AppState, Config, FactsDatabase, ServerFactStore, ServerIds};
use chronoscope_core::submit::{Commit, commit_facts};
#[cfg(not(feature = "postgres"))]
use chronoscope_db::FactStoreLocations;
use chronoscope_db::media_store::{InMemoryMediaStore, MediaStore};
use chronoscope_db::{Database, Email, Queue, ResearchUrl, UserId};
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

mod factstore;
mod image_resolve;

pub use factstore::{LoadError, load_curated_fact_store};
pub use image_resolve::{ImageResolveMode, resolve_fact_store_images};

/// The pixel dimension of the shared placeholder JPEG — an 8x8 solid-copper tile.
const PLACEHOLDER_DIM: u32 = 8;

/// Encode the shared placeholder image: a small solid-copper JPEG. One image
/// stands in for every fact-store image under [`ImageResolveMode::Placeholder`].
fn placeholder_jpeg() -> Result<bytes::Bytes, Box<dyn std::error::Error + Send + Sync>> {
    use image::ImageEncoder;

    let rgb_data: Vec<u8> =
        [0x8Bu8, 0x5E, 0x3C].repeat((PLACEHOLDER_DIM * PLACEHOLDER_DIM) as usize);
    let mut jpeg_buf = std::io::Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new(&mut jpeg_buf).write_image(
        &rgb_data,
        PLACEHOLDER_DIM,
        PLACEHOLDER_DIM,
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(bytes::Bytes::from(jpeg_buf.into_inner()))
}

/// The env var naming the read-only facts database to mount: `just web-dev`
/// resolves the pinned `fetch-wikidata-db` output into it, and the hermetic
/// test environment points it at the curated build.
pub const FACTS_DB_ENV: &str = "CHRONOSCOPE_FACTS_DB";

/// Resolve [`FACTS_DB_ENV`] and validate the named pre-built facts DB (via the
/// shared [`validate_facts_file`](chronoscope_db::validate_facts_file) — the
/// one definition of "valid, current facts DB", also used by the fact store's
/// mount), returning its path for [`FactsDbSource::Mounted`]. The dev server
/// pins it as the frozen read-only base, immutable, so a 0444 nix-store
/// artifact serves in place — no clone, no writable copy. `subset` names the
/// fetch to run in every refusal, so a missing, stale, or garbage artifact
/// fails with the exact command.
pub fn mount_facts_db(subset: &str) -> Result<String, DevServerError> {
    let source = std::env::var(FACTS_DB_ENV).map_err(|_| {
        DevServerError(format!(
            "{FACTS_DB_ENV} not set — run inside the web shell (`nix develop .#web`; \
             `just web-dev` enters it for you), or fetch a facts DB with \
             `just fetch-wikidata-db {subset}` and export the path"
        ))
    })?;
    // The shared validator names the specific fault (missing / not SQLite /
    // stale codec); the mount adds the fetch remedy around it.
    chronoscope_db::validate_facts_file(std::path::Path::new(&source)).map_err(|e| {
        DevServerError(format!(
            "{e} — re-fetch it with `just fetch-wikidata-db {subset}`"
        ))
    })?;
    Ok(source)
}

/// The facts-DB subset in play: `CHRONOSCOPE_FACTS_DB_SUBSET` when set (the
/// `just web-dev` recipe exports it beside the DB path), else `curated`.
/// Only used to name the right `just fetch-wikidata-db <subset>` command in
/// mount refusals.
pub fn facts_db_subset() -> String {
    std::env::var("CHRONOSCOPE_FACTS_DB_SUBSET").unwrap_or_else(|_| "curated".to_owned())
}

/// How long workers get to notice the shutdown signal before the pools close
/// without them.
///
/// The runner observes shutdown between batches, so a worker wedged inside one
/// holds this join open for as long as that batch takes. Waiting it out forever
/// costs the pool closes, which are the reason [`RunningDevServer::shutdown`]
/// exists; naming whoever is still running turns a wedge into something a test
/// log can point at.
const WORKER_STOP_DEADLINE: Duration = Duration::from_secs(10);

/// Error type for dev server setup.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct DevServerError(pub String);

impl From<String> for DevServerError {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// A spawned worker task alongside the id it logs under, so a shutdown that
/// runs out of patience can say which worker is still going.
struct WorkerTask {
    worker_id: String,
    handle: JoinHandle<()>,
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

    /// Database pool for direct access in tests
    db: Arc<Database>,

    /// The fact store, kept for its graceful shutdown. It owns a separate
    /// SpatiaLite-loaded pool (the `Arc`-backed pool is shared with the copy
    /// handed to the server), and its close must run on the live runtime for
    /// the same reason `db.close()` does.
    facts: ServerFactStore,

    /// Send `true` to trigger graceful shutdown of workers
    shutdown_tx: watch::Sender<bool>,

    /// Handles to spawned worker tasks for graceful shutdown
    worker_handles: Vec<WorkerTask>,

    /// The server's logger, kept so shutdown can report workers that overran
    /// [`WORKER_STOP_DEADLINE`].
    log: slog::Logger,
}

impl RunningDevServer {
    /// Get the database for direct access.
    ///
    /// Used by integration tests to query DB state directly, bypassing the
    /// HTTP API layer.
    #[must_use]
    pub fn db(&self) -> &Database {
        &self.db
    }

    /// Gracefully shut down the server and wait for all workers to stop.
    ///
    /// The wait is bounded; workers still running when it expires are named in
    /// a warning and left detached, exactly as a plain drop leaves them.
    pub async fn shutdown(mut self) {
        // Signal workers to stop
        let _ = self.shutdown_tx.send(true);

        // Take ownership of handles (leaving empty vec) so we can await them
        // despite implementing Drop
        let workers = std::mem::take(&mut self.worker_handles);
        // One deadline across the whole join, so a worker that hogs it can't
        // hand the next one a fresh budget. `timeout_at` polls the handle
        // before the deadline, so a worker that already finished still reports
        // as stopped once the budget is spent.
        let deadline = tokio::time::Instant::now() + WORKER_STOP_DEADLINE;
        let mut still_running = Vec::new();
        for worker in workers {
            if tokio::time::timeout_at(deadline, worker.handle)
                .await
                .is_err()
            {
                still_running.push(worker.worker_id);
            }
        }
        if !still_running.is_empty() {
            slog::warn!(self.log, "workers did not stop before the deadline; closing pools anyway";
                "workers" => still_running.join(", "),
                "deadline_secs" => WORKER_STOP_DEADLINE.as_secs());
        }

        // Close both SpatiaLite-loaded pools — the app pool and the fact
        // store's own — while the runtime is alive, so each connection's
        // `dlclose` completes before process exit instead of on an ungraceful
        // drop at teardown.
        self.db.close().await;
        self.facts.close().await;
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

/// Where the dev server's fact store comes from.
pub enum FactsDbSource {
    /// A pre-built, codec-stamped facts DB pinned as the frozen `base` from
    /// [`mount_facts_db`], read beneath a fresh writable `overlay` scratch.
    /// The base attaches `mode=ro&immutable=1`, so a 0444 nix-store pin serves
    /// with no clone; submissions land in the overlay and are discarded on
    /// relaunch. `overlay` is a per-launch scratch path in the caller's tempdir.
    Mounted {
        /// The read-only base pin.
        base: String,
        /// The writable scratch overlay path.
        overlay: String,
    },
    /// A writable overlay only, no base — created and migrated if absent, the
    /// URL-fetch harness's empty scratch file. Nothing stages facts into it; it
    /// only has to exist and carry the fact schema.
    Writable(String),
}

/// Open the dev server's fact store from its configured source. The store owns
/// its pool (a throwaway in-memory `main` plus the fact-store layers): a
/// mounted source pins the base read-only+immutable beneath a fresh writable
/// overlay scratch, and a writable source is that overlay alone, created and
/// migrated if absent.
#[cfg(not(feature = "postgres"))]
async fn open_facts(source: &FactsDbSource) -> chronoscope_db::DbResult<ServerFactStore> {
    match source {
        FactsDbSource::Mounted { base, overlay } => {
            ServerFactStore::open(FactStoreLocations::mounted(base.clone(), overlay.clone())).await
        }
        FactsDbSource::Writable(url) => {
            ServerFactStore::open(FactStoreLocations::standalone(url.clone())).await
        }
    }
}

/// The Postgres twin, compiled when a workspace build unifies the api crate's
/// `postgres` feature on. [`FactsDbSource::Writable`] carries a connection URL,
/// which doubles as the way to point the dev server at a real database and
/// reproduce something production-shaped; base+overlay layering is a SQLite
/// construction, so a mounted source fails with a message saying as much.
///
/// A writable source migrates, matching the SQLite twin, whose scratch overlay
/// is created and migrated on open: the dev server is handed a scratch database
/// it owns, and requiring a separate loader run before it could serve would be
/// friction with nothing to protect.
#[cfg(feature = "postgres")]
async fn open_facts(source: &FactsDbSource) -> chronoscope_db::DbResult<ServerFactStore> {
    match source {
        FactsDbSource::Mounted { base, .. } => Err(chronoscope_db::DbError::Config(format!(
            "the Postgres fact store connects to a URL, so the mounted SQLite artifact at \
             {base} has no meaning here; pass FactsDbSource::Writable with a connection URL"
        ))),
        FactsDbSource::Writable(url) => ServerFactStore::connect_and_migrate(url).await,
    }
}

/// Configuration for starting the dev server.
pub struct DevServerConfig {
    /// Optional app database URL (auth/queues/media). If `None`, uses
    /// `sqlite::memory:`. Distinct from the facts overlay ([`facts`](Self::facts)),
    /// which holds the fact tables.
    pub database_url: Option<String>,

    /// The fact store to serve: a read-only pre-built pin (`web-dev`, the
    /// browser tests, the ngrok dev server) or a writable scratch file created
    /// on demand (the URL-fetch harnesses). See [`FactsDbSource`].
    pub facts: FactsDbSource,

    /// Commits to write into the fact store before serving. Browser tests use
    /// this to place entities at chosen coordinates on top of the mounted store;
    /// submissions land in the writable overlay. Committed after the store opens
    /// and before image resolution, so a seeded image resolves into the media
    /// store like any other.
    pub seed_commits: Vec<Commit<ServerIds>>,

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

    /// How to resolve fact-store images into the media store at startup.
    /// [`ImageResolveMode::Placeholder`] gives browser tests deterministic,
    /// same-origin images (CORS-safe canvas draws, no network);
    /// [`ImageResolveMode::Fetch`] downloads the real Commons originals for
    /// interactive `web-dev` / ngrok runs.
    pub image_resolve: ImageResolveMode,

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
) -> WorkerTask {
    use chronoscope_workers::url_fetcher::FetchContext;

    let worker_id = name.to_string();

    let queue: Arc<Queue<ResearchUrl>> = match affinity {
        None => ctx.db.url_queue_generic.clone(),
        Some(integration) => ctx.db.make_url_queue(integration),
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

    let handle = tokio::spawn({
        let worker_id = worker_id.clone();
        async move {
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
        }
    });
    WorkerTask { worker_id, handle }
}

/// Spawn an analysis worker in a background task.
fn spawn_analysis_worker(
    worker_id: &str,
    triton: Arc<dyn TritonService>,
    ctx: &WorkerContext,
) -> WorkerTask {
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

    let handle = tokio::spawn({
        let worker_id = worker_id.clone();
        async move {
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
        }
    });
    WorkerTask { worker_id, handle }
}

/// Start the development server with the given configuration.
///
/// This sets up:
/// - The SQLite database (facts + app tables in one file)
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
        Database::new(db_url)
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
        // The fact store is built below from `config.facts` (a `FactsDbSource`)
        // and handed to `AppState` directly, so this app `Config` field goes
        // unused here.
        facts_database: FactsDatabase::new("sqlite::memory:")
            .map_err(|e| DevServerError(format!("{e}")))?,
        rp_id,
        rp_origin,
        bind_addr: format!("127.0.0.1:{port}")
            .parse()
            .map_err(|e| format!("Invalid bind address: {e}"))?,
        ios_app_id: config.ios_app_id,
        cdn_base_url: url::Url::parse(&config.cdn_base_url)
            .map_err(|e| format!("Invalid CDN base URL: {e}"))?,
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
        "cdn_base_url" => %api_config.cdn_base_url,
    );

    // Configure Dropshot (before api_config is moved)
    let config_dropshot = ConfigDropshot {
        bind_address: api_config.bind_addr,
        default_request_body_max_bytes: 1024 * 1024,
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };

    // No boot-time ingest: whatever the source names is served as it stands.
    let facts = open_facts(&config.facts)
        .await
        .map_err(|e| format!("opening fact store: {e}"))?;

    // Write any test-provided seed commits before serving, so seeded entities
    // read back through the same projection as the mounted store and their
    // images resolve in the pass below. Submissions land in the writable overlay.
    for commit in config.seed_commits {
        commit_facts(&facts, commit)
            .await
            .map_err(|e| DevServerError(format!("seeding fact store: {e:?}")))?;
    }

    // Resolve every fact-store image into the media store, so the read path
    // serves thumbnails and detail images from our own `/media/{key}` rather
    // than pointing browsers at upstream Commons.
    let image_media = Arc::new(
        resolve_fact_store_images(
            &facts,
            &media_store,
            &config.http_client,
            config.image_resolve,
        )
        .await,
    );
    info!(log, "Resolved fact-store images"; "count" => image_media.len());

    // Keep a handle to the fact store for graceful shutdown; the copy handed to
    // the server shares the same `Arc`-backed pool.
    let facts_for_shutdown = facts.clone();

    // Create AppState with our shared database, media store, and fact store
    let app_state = AppState::new(
        db.as_ref().clone(),
        api_config,
        jwt_config,
        config.dns_resolver,
        media_store,
        facts,
        image_media,
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

    // `HttpServerStarter::new` already bound the listener synchronously, so
    // the kernel's listen backlog is accepting connections from this point
    // on — clients can connect even before this spawned task is scheduled
    // to call `accept`. No readiness sleep is needed.
    tokio::spawn(async move {
        if let Err(e) = server.await {
            eprintln!("Server error: {e}");
        }
    });

    info!(log, "Dev server ready"; "base_url" => &base_url);

    Ok(RunningDevServer {
        base_url,
        port,
        auth_token,
        test_user_id,
        db,
        facts: facts_for_shutdown,
        shutdown_tx,
        worker_handles,
        log: log.clone(),
    })
}
