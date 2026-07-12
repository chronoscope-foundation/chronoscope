use std::sync::Arc;

use chronoscope_api::jwt::JwtConfig;
use chronoscope_api::state::{AppState, Config, ServerFactStore, default_dns_resolver};
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
    let db = Database::new(&config.database_url).await?;
    let jwt = JwtConfig::from_env()?;
    let dns_resolver = default_dns_resolver()?;

    // The fact store rides the same pool as everything else (auth, research,
    // media) — its tables are part of the one migrated schema, so whatever
    // facts the database file holds persist across restarts. No boot-time
    // resolver runs here, so the media map starts empty.
    let facts = ServerFactStore::new(db.pool_ref().clone());
    let image_media = Arc::new(std::collections::HashMap::new());

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
    // Close the SpatiaLite-loaded pool inside the live runtime, so each
    // connection's dlclose completes before process exit.
    db.close().await;
    result.map_err(Into::into)
}
