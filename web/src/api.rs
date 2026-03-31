//! API client integration for the web frontend.
//!
//! Uses the shared `ChronoscopeClient` from `chronoscope-api-client` for typed
//! API access. Handles runtime configuration discovery from `/config.json`.

pub use chronoscope_api_client::{ChronoscopeClient, EntitySummary};

// ==================== Runtime Configuration ====================

/// Runtime configuration loaded from `/config.json`.
#[derive(serde::Deserialize)]
struct AppConfig {
    api_url: String,
}

/// Discover the API URL from `/config.json` and create a client.
///
/// Returns `None` if the config fetch fails (e.g., no API server running).
pub async fn client_from_config() -> Option<ChronoscopeClient> {
    let resp = reqwest::get("/config.json").await.ok()?;
    if !resp.status().is_success() {
        web_sys::console::warn_1(&"Failed to load /config.json — API features disabled".into());
        return None;
    }
    let config: AppConfig = resp.json().await.ok().or_else(|| {
        web_sys::console::warn_1(&"Failed to parse /config.json".into());
        None
    })?;
    Some(ChronoscopeClient::new(config.api_url))
}

// ==================== Lazy initialization ====================

/// Lazily initialize the API client, fetching `/config.json` on first call.
///
/// Returns a clone of the initialized client, or `None` if config loading
/// fails. `ChronoscopeClient` is cheap to clone (just a URL string + `reqwest::Client`).
pub async fn get_or_init_client(
    handle: &std::rc::Rc<std::cell::RefCell<Option<ChronoscopeClient>>>,
) -> Option<ChronoscopeClient> {
    let needs_init = handle.borrow().is_none();
    if needs_init {
        let client = client_from_config().await?;
        *handle.borrow_mut() = Some(client);
    }
    handle.borrow().clone()
}

// ==================== Constants ====================

/// Maximum entities to display on the map at once.
pub const MAX_ENTITIES: usize = 500;
/// Entities fetched per API page.
pub const PAGE_SIZE: u32 = 100;
