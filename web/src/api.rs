//! Minimal HTTP client for the Chronoscope API.
//!
//! Discovers the API base URL from `/config.json` on initialization, then
//! provides typed fetch methods. Provided to the component tree via Leptos
//! context — no global/thread-local state.

use gloo_net::http::Request;
use serde::Deserialize;

// ==================== Client ====================

/// HTTP client for the Chronoscope API.
///
/// Holds the base URL discovered from `/config.json`. Created once at app
/// startup and provided via Leptos context so components can `use_context`.
#[derive(Clone, Debug)]
pub struct ApiClient {
    base_url: String,
}

/// Runtime configuration loaded from `/config.json`.
#[derive(Deserialize)]
struct AppConfig {
    api_url: String,
}

impl ApiClient {
    /// Create an `ApiClient` by fetching `/config.json` to discover the API URL.
    ///
    /// Returns `None` if the config fetch fails (e.g., no API server running).
    pub async fn from_config() -> Option<Self> {
        match Request::get("/config.json").send().await {
            Ok(resp) if resp.ok() => match resp.json::<AppConfig>().await {
                Ok(config) => Some(Self {
                    base_url: config.api_url,
                }),
                Err(e) => {
                    web_sys::console::warn_1(
                        &format!("Failed to parse /config.json: {e}").into(),
                    );
                    None
                }
            },
            _ => {
                web_sys::console::warn_1(
                    &"Failed to load /config.json — API features disabled".into(),
                );
                None
            }
        }
    }

    /// Fetch entities within a map viewport bounding box.
    ///
    /// Paginates through up to `MAX_ENTITIES` results, then stops.
    pub async fn fetch_entities(
        &self,
        min_lat: f64,
        max_lat: f64,
        min_lon: f64,
        max_lon: f64,
    ) -> Result<EntityFetchResult, ApiError> {
        let first_url = format!(
            "{}/entities?min_lat={min_lat}&max_lat={max_lat}\
             &min_lon={min_lon}&max_lon={max_lon}&limit={PAGE_SIZE}",
            self.base_url
        );

        let mut all_entities = Vec::new();
        let mut url = first_url;

        loop {
            let resp = Request::get(&url)
                .send()
                .await
                .map_err(|e| ApiError(format!("fetch failed: {e}")))?;

            if !resp.ok() {
                return Err(ApiError(format!("HTTP {}", resp.status())));
            }

            let page: ResultsPage<EntitySummary> = resp
                .json()
                .await
                .map_err(|e| ApiError(format!("JSON parse failed: {e}")))?;

            all_entities.extend(page.items);

            if all_entities.len() >= MAX_ENTITIES {
                all_entities.truncate(MAX_ENTITIES);
                return Ok(EntityFetchResult {
                    entities: all_entities,
                    truncated: true,
                });
            }

            let Some(next) = page.next_page else {
                return Ok(EntityFetchResult {
                    entities: all_entities,
                    truncated: false,
                });
            };

            url = format!("{}/entities?page_token={next}&limit={PAGE_SIZE}", self.base_url);
        }
    }

    /// Fetch a single entity's full detail by ID.
    pub async fn fetch_entity(&self, id: &str) -> Result<EntityDetailResponse, ApiError> {
        let url = format!("{}/entities/{id}", self.base_url);

        let resp = Request::get(&url)
            .send()
            .await
            .map_err(|e| ApiError(format!("fetch failed: {e}")))?;

        if !resp.ok() {
            return Err(ApiError(format!("HTTP {}", resp.status())));
        }

        resp.json()
            .await
            .map_err(|e| ApiError(format!("JSON parse failed: {e}")))
    }
}

// ==================== Response Types ====================
// Lightweight deserialization types matching the API responses.
// These intentionally duplicate the API types to avoid pulling in
// heavy server-side dependencies (sqlx, dropshot) into the WASM build.
// TODO: Extract shared API types into a thin crate.

/// Entity summary from `GET /entities?bbox=...`
#[derive(Debug, Clone, Deserialize)]
pub struct EntitySummary {
    pub id: String,
    pub entity_type: String,
    pub name: Option<String>,
    pub latitude: f64,
    pub longitude: f64,
}

/// Dropshot pagination wrapper.
#[derive(Debug, Deserialize)]
struct ResultsPage<T> {
    items: Vec<T>,
    next_page: Option<String>,
}

/// Response type for entity detail endpoint.
///
/// Uses `serde_json::Value` until shared API types are extracted into a thin crate.
pub type EntityDetailResponse = serde_json::Value;

// ==================== Lazy initialization ====================

/// Lazily initialize the API client, fetching `/config.json` on first call.
///
/// Returns a clone of the initialized client, or `None` if config loading
/// fails. `ApiClient` is cheap to clone (just a URL string).
pub async fn get_or_init_api_client(
    handle: &std::rc::Rc<std::cell::RefCell<Option<ApiClient>>>,
) -> Option<ApiClient> {
    let needs_init = handle.borrow().is_none();
    if needs_init {
        let client = ApiClient::from_config().await?;
        *handle.borrow_mut() = Some(client);
    }
    handle.borrow().clone()
}

// ==================== Error / Constants ====================

#[derive(Debug)]
pub struct ApiError(pub String);

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ApiError {}

/// Maximum entities to display on the map at once.
const MAX_ENTITIES: usize = 500;
/// Entities fetched per API page.
const PAGE_SIZE: u32 = 100;

/// Result of fetching entities for a viewport.
pub struct EntityFetchResult {
    pub entities: Vec<EntitySummary>,
    /// True if there were more entities available than `MAX_ENTITIES`.
    pub truncated: bool,
}
