use std::sync::Arc;

use chronoscope_api::jwt::JwtConfig;
use chronoscope_api::state::{AppState, Config, ServerFactStore, default_dns_resolver};
use chronoscope_db::{Database, FactStoreLocations};
use dropshot::{
    ApiDescription, ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpServerStarter,
};
use slog::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Set up logging first so we can use it throughout startup
    let config_logging = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Info,
    };
    let log = config_logging.to_logger("chronoscope-api")?;

    // Load configuration
    let config = Config::from_env()?;

    info!(log, "Starting Chronoscope API server";
        "database" => &config.database_url,
        "rp_id" => &config.rp_id,
        "rp_origin" => &config.rp_origin,
        "bind_addr" => %config.bind_addr,
        "ios_app" => config.ios_app_id.as_deref().unwrap_or("not configured"),
    );

    // Initialize app state (includes database setup and migrations)
    let db = Database::new(&config.database_url).await?;
    let jwt = JwtConfig::from_env()?;
    let dns_resolver = default_dns_resolver()?;

    // The fact store owns its own pool: a throwaway in-memory `main` with the
    // fact-store layers attached. The configured facts DB pins as the frozen
    // read-only `base` — `open` validates it (exists, SQLite, current codec
    // stamp) rather than fabricating an empty one, so a missing, partial, or
    // stale file fails loud at boot — beneath a fresh writable overlay scratch
    // that takes submissions. No boot-time resolver runs here, so the media map
    // starts empty. The scratch dir lives for the server's lifetime.
    let facts_overlay_dir = tempfile::TempDir::with_prefix("chronoscope-facts-overlay-")?;
    let facts = ServerFactStore::open(FactStoreLocations::mounted(
        config.facts_database.clone(),
        facts_overlay_dir
            .path()
            .join("overlay.db")
            .display()
            .to_string(),
    ))
    .await?;
    let image_media = Arc::new(std::collections::HashMap::new());

    // Keep a handle to the fact store for graceful shutdown; the copy handed to
    // the server shares the same `Arc`-backed pool.
    let facts_for_shutdown = facts.clone();

    // When embedded-media feature is enabled (e.g., dev builds), we need a media store.
    // Production builds without the feature don't need one.
    #[cfg(feature = "embedded-media")]
    let app_state = {
        use chronoscope_db::media_store::InMemoryMediaStore;
        Arc::new(
            AppState::new(
                db.clone(),
                config,
                jwt,
                dns_resolver,
                std::sync::Arc::new(InMemoryMediaStore::new()),
                facts,
                image_media,
            )
            .await?,
        )
    };
    #[cfg(not(feature = "embedded-media"))]
    let app_state =
        Arc::new(AppState::new(db.clone(), config, jwt, dns_resolver, facts, image_media).await?);

    // Configure Dropshot
    let config_dropshot = ConfigDropshot {
        bind_address: app_state.config.bind_addr,
        default_request_body_max_bytes: 1024 * 1024, // 1MB
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };

    // Build API
    let mut api = ApiDescription::new();
    chronoscope_api::register_api(&mut api)?;

    // Start server
    let server = HttpServerStarter::new(&config_dropshot, api, app_state, &log)?.start();
    let result = server.await;
    // Close both SpatiaLite-loaded pools — the app pool and the fact store's
    // own — inside the live runtime, so each connection's dlclose completes
    // before process exit rather than on an ungraceful drop.
    db.close().await;
    facts_for_shutdown.close().await;
    result.map_err(Into::into)
}
