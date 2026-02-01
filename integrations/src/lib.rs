//! Domain-specific integrations for external platforms.
//!
//! This crate provides specialized fetchers for specific domains (Reddit, Instagram, etc.)
//! that know how to extract structured content from their platforms. Integrations are pure
//! data transformations - they have no database access.
//!
//! # Architecture
//!
//! - [`IntegrationMeta`]: Metadata shared by all integrations (name, domains, URL normalization)
//! - [`SingleFetcher`]: Fetches one URL at a time (e.g., Reddit JSON API)
//! - [`BatchFetcher`]: Fetches multiple URLs in a single batch (e.g., Instagram via Apify)
//! - [`IntegrationRegistry`]: Routes URLs to the appropriate integration
//!
//! The workers crate handles all persistence - integrations just return [`FetchedContent`].

pub mod content;
pub mod http;
pub mod instagram;
pub mod reddit;
mod registry;

pub use content::FetchedContent;
pub use http::{
    CacheMode, CachingClient, FetchError, HttpClient, HttpError, HttpRequest, HttpResponse,
    ReqwestClient, ReqwestConfig,
};
#[cfg(feature = "testing")]
pub use http::{MockHttpClient, MockHttpError};
pub use instagram::{ApifyConfig, InstagramIntegration};
pub use reddit::RedditIntegration;
pub use registry::{IntegrationRegistry, RegistrationError};

use std::fmt;
use std::sync::Arc;

use url::Url;

/// Create an integration registry with all supported integrations.
///
/// If `instagram_config` is provided, Instagram fetching will use those credentials.
/// Otherwise, Instagram URLs will still be detected and normalized, but actual
/// fetching will fail without real credentials.
///
/// # Errors
///
/// Returns `RegistrationError` if integration domains conflict (should never
/// happen with default integrations, but the error is propagated for safety).
pub fn create_registry(
    instagram_config: Option<ApifyConfig>,
) -> Result<IntegrationRegistry, RegistrationError> {
    let mut registry = IntegrationRegistry::new();

    // Register domain-specific integrations
    registry.register(Integration::Single(Arc::new(RedditIntegration::new())))?;
    registry.register(Integration::Batch(Arc::new(InstagramIntegration::new(
        instagram_config,
    ))))?;

    Ok(registry)
}

/// Typed integration names - avoids stringly-typed APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntegrationName {
    Reddit,
    Instagram,
}

impl IntegrationName {
    /// Get the string representation for database storage.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Reddit => "reddit",
            Self::Instagram => "instagram",
        }
    }
}

impl fmt::Display for IntegrationName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Metadata shared by all integrations.
pub trait IntegrationMeta: Send + Sync {
    /// Human-readable name (e.g., "reddit", "instagram").
    fn name(&self) -> IntegrationName;

    /// Domains this integration handles (e.g., `["reddit.com", "redd.it"]`).
    fn domains(&self) -> &'static [&'static str];

    /// Normalize a URL (strip tracking params, canonicalize).
    ///
    /// The default implementation returns the URL unchanged. Integrations can override
    /// to apply domain-specific normalization (e.g., stripping Reddit share params).
    /// Generic normalization (removing common tracking params like utm_*) is applied
    /// by the caller after this method.
    fn normalize_url(&self, url: &Url) -> Url {
        url.clone()
    }
}

/// Single-URL fetcher (e.g., Reddit).
#[async_trait::async_trait]
pub trait SingleFetcher: IntegrationMeta {
    /// Fetch a single URL and return structured content.
    async fn fetch(&self, http: &dyn HttpClient, url: &Url) -> Result<FetchedContent, FetchError>;
}

/// Batch-URL fetcher (e.g., Instagram via Apify).
#[async_trait::async_trait]
pub trait BatchFetcher: IntegrationMeta {
    /// Fetch multiple URLs in a single batch operation.
    ///
    /// Returns results paired with their input URLs. Some URLs may succeed
    /// while others fail - each result is independent.
    async fn fetch_batch(
        &self,
        http: &dyn HttpClient,
        urls: &[Url],
    ) -> Vec<(Url, Result<FetchedContent, FetchError>)>;
}

/// Enum wrapping either fetcher type.
#[derive(Clone)]
pub enum Integration {
    Single(Arc<dyn SingleFetcher>),
    Batch(Arc<dyn BatchFetcher>),
}

impl Integration {
    /// Get the integration name.
    #[must_use]
    pub fn name(&self) -> IntegrationName {
        match self {
            Self::Single(f) => f.name(),
            Self::Batch(f) => f.name(),
        }
    }

    /// Get the domains this integration handles.
    #[must_use]
    pub fn domains(&self) -> &'static [&'static str] {
        match self {
            Self::Single(f) => f.domains(),
            Self::Batch(f) => f.domains(),
        }
    }

    /// Normalize a URL using integration-specific rules.
    #[must_use]
    pub fn normalize_url(&self, url: &Url) -> Url {
        match self {
            Self::Single(f) => f.normalize_url(url),
            Self::Batch(f) => f.normalize_url(url),
        }
    }
}
