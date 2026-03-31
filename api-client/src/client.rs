//! Typed HTTP client for the Chronoscope API.
//!
//! Uses `reqwest` which works on both native (tokio) and WASM (web-sys fetch) targets.

use crate::entities::{EntityResponse, EntitySummary};
use crate::ids::EntityId;
use crate::pagination::ResultsPage;
use crate::types::Bbox;

/// Typed HTTP client for the Chronoscope API.
///
/// Holds the base URL and a `reqwest::Client`. Cheap to clone.
#[derive(Clone, Debug)]
pub struct ChronoscopeClient {
    client: reqwest::Client,
    base_url: String,
}

/// Result of fetching entities with pagination.
pub struct EntityFetchResult {
    pub entities: Vec<EntitySummary>,
    /// True if there were more entities available than the requested maximum.
    pub truncated: bool,
}

/// Client-side API error.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("API error (HTTP {status}): {message}")]
    Api { status: u16, message: String },
}

impl ChronoscopeClient {
    /// Create a new client pointing at the given API base URL.
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
        }
    }

    /// Create a new client with an existing `reqwest::Client`.
    #[must_use]
    pub fn with_client(client: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            client,
            base_url: base_url.into(),
        }
    }

    /// Send a GET request and parse the JSON response, returning an `ApiError` on
    /// non-success status codes.
    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, ApiError> {
        let resp = self.client.get(url).send().await?;
        if !resp.status().is_success() {
            return Err(ApiError::Api {
                status: resp.status().as_u16(),
                message: resp.text().await.unwrap_or_default(),
            });
        }
        Ok(resp.json().await?)
    }

    /// List entities within a geographic bounding box.
    pub async fn list_entities(&self, bbox: &Bbox) -> Result<ResultsPage<EntitySummary>, ApiError> {
        let url = format!(
            "{}/entities?min_lat={}&max_lat={}&min_lon={}&max_lon={}",
            self.base_url,
            bbox.min_lat(),
            bbox.max_lat(),
            bbox.min_lon(),
            bbox.max_lon(),
        );

        self.get_json(&url).await
    }

    /// Paginate through entities in a bounding box, collecting up to `max_entities`.
    ///
    /// Returns all collected entities and whether there were more available.
    pub async fn list_entities_all(
        &self,
        bbox: &Bbox,
        max_entities: usize,
        page_size: u32,
    ) -> Result<EntityFetchResult, ApiError> {
        let first_url = format!(
            "{}/entities?min_lat={}&max_lat={}&min_lon={}&max_lon={}&limit={page_size}",
            self.base_url,
            bbox.min_lat(),
            bbox.max_lat(),
            bbox.min_lon(),
            bbox.max_lon(),
        );

        let mut all_entities = Vec::with_capacity(max_entities);
        let max_pages = max_entities / page_size as usize + 1;
        let mut url = first_url;

        for _ in 0..max_pages {
            let page: ResultsPage<EntitySummary> = self.get_json(&url).await?;
            all_entities.extend(page.items);

            if all_entities.len() >= max_entities {
                all_entities.truncate(max_entities);
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

            url = format!(
                "{}/entities?page_token={}&limit={page_size}",
                self.base_url, next
            );
        }

        // Exceeded max pages guard
        Ok(EntityFetchResult {
            entities: all_entities,
            truncated: true,
        })
    }

    /// Fetch a single entity's full detail by ID.
    pub async fn get_entity(&self, id: &EntityId) -> Result<EntityResponse, ApiError> {
        let url = format!("{}/entities/{}", self.base_url, id);
        self.get_json(&url).await
    }
}
