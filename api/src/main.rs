use std::sync::Arc;

use chronoscope_api::jwt::JwtConfig;
use chronoscope_api::state::{AppState, Config, default_dns_resolver};
use chronoscope_core::facts::memory::MemoryFactStore;
use chronoscope_db::Database;
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
    let db = Database::new(&config.database_url, &chronoscope_db::resolve_regions_db()?).await?;
    let jwt = JwtConfig::from_env()?;
    let dns_resolver = default_dns_resolver()?;

    // No production data source feeds the fact store yet — it starts empty.
    // The SQLite-backed store still serves reads; this will populate once a
    // fact-store-backed read path lands.
    let facts = MemoryFactStore::new();

    // When embedded-media feature is enabled (e.g., dev builds), we need a media store.
    // Production builds without the feature don't need one.
    #[cfg(feature = "embedded-media")]
    let app_state = {
        use chronoscope_db::media_store::InMemoryMediaStore;
        Arc::new(
            AppState::new(
                db,
                config,
                jwt,
                dns_resolver,
                std::sync::Arc::new(InMemoryMediaStore::new()),
                facts,
            )
            .await?,
        )
    };
    #[cfg(not(feature = "embedded-media"))]
    let app_state = Arc::new(AppState::new(db, config, jwt, dns_resolver, facts).await?);

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

    server.await.map_err(Into::into)
}
