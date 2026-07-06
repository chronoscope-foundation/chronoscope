//! API client integration for the web frontend.
//!
//! Uses the shared [`Client`] from `chronoscope-api-client` for typed
//! API access. Handles runtime configuration discovery from `/config.json`.

use wasm_bindgen::JsCast;

pub use chronoscope_api_client::{ClickAction, Client, EntityDetail};

// ==================== Runtime Configuration ====================

/// Runtime configuration loaded from `/config.json`.
#[derive(serde::Deserialize)]
struct AppConfig {
    api_url: String,
}

/// Discover the API URL from `/config.json` and create a client.
///
/// Uses the browser's native fetch API (via `web_sys`) instead of reqwest,
/// because reqwest's WASM backend requires runtime feature configuration
/// that doesn't work reliably with `default-features = false`.
///
/// Returns `None` if the config fetch fails (e.g., no API server running).
pub async fn client_from_config() -> Option<Client> {
    let window = web_sys::window()?;
    let resp_value = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str("/config.json"))
        .await
        .map_err(|e| {
            web_sys::console::warn_1(&format!("fetch /config.json failed: {e:?}").into());
        })
        .ok()?;
    let resp: web_sys::Response = resp_value.dyn_into().ok()?;
    if !resp.ok() {
        web_sys::console::warn_1(&"Failed to load /config.json — API features disabled".into());
        return None;
    }
    let json = wasm_bindgen_futures::JsFuture::from(resp.json().ok()?)
        .await
        .map_err(|e| {
            web_sys::console::warn_1(&format!("Failed to read /config.json body: {e:?}").into());
        })
        .ok()?;
    let config: AppConfig = serde_wasm_bindgen::from_value(json)
        .map_err(|e| {
            web_sys::console::warn_1(&format!("Failed to parse /config.json: {e}").into());
        })
        .ok()?;
    Some(Client::new(config.api_url))
}

// ==================== Lazy initialization ====================

/// Lazily initialize the API client, fetching `/config.json` on first call.
///
/// Returns a clone of the initialized client, or `None` if config loading
/// fails. [`Client`] is cheap to clone (just a URL string + `reqwest::Client`).
pub async fn get_or_init_client(
    handle: &std::rc::Rc<std::cell::RefCell<Option<Client>>>,
) -> Option<Client> {
    let needs_init = handle.borrow().is_none();
    if needs_init {
        let client = client_from_config().await?;
        *handle.borrow_mut() = Some(client);
    }
    handle.borrow().clone()
}

// ==================== Constants ====================
