//! Fetcher trait and registry for URL processing.
//!
//! The `Fetcher` trait defines how to process URLs for specific domains.
//! The `FetcherRegistry` routes URLs to the appropriate fetcher based on domain.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chronoscope_db::media_store::{MediaStore, MediaStoreError};
use chronoscope_db::{Database, MediaId, PageId, ResearchUrl};
use reqwest::StatusCode;
use url::Url;

use crate::http::{HttpClient, HttpError};

/// Configuration for URL fetching.
#[derive(Debug, Clone)]
pub struct FetcherConfig {
    /// Maximum concurrent fetches per worker.
    pub max_concurrent: usize,
    /// Delay between fetches to avoid rate limiting.
    pub per_item_delay: Duration,
    /// Timeout for individual HTTP requests.
    pub fetch_timeout: Duration,
}

impl Default for FetcherConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 4,
            per_item_delay: Duration::from_millis(100),
            fetch_timeout: Duration::from_secs(30),
        }
    }
}

/// Context passed to fetchers during processing.
pub struct FetchContext {
    /// Database for storing pages/media and marking URLs resolved.
    pub db: Arc<Database>,
    /// HTTP client for making requests.
    pub http: Arc<dyn HttpClient>,
    /// Media storage for images/videos.
    pub media_store: Arc<dyn MediaStore>,
    /// Fetcher configuration.
    pub config: FetcherConfig,
}

/// Result of successfully fetching a URL.
#[derive(Debug)]
pub struct FetchResult {
    /// What the URL resolved to.
    pub outcome: FetchOutcome,
    /// Additional URLs discovered during processing (e.g., images in HTML).
    pub discovered_urls: Vec<Url>,
}

/// What a URL resolved to.
#[derive(Debug)]
pub enum FetchOutcome {
    /// Resolved to a page (HTML content).
    Page { page_id: PageId },
    /// Resolved to media (image/video).
    Media { media_id: MediaId },
}

/// Errors that can occur during fetching.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// HTTP request failed (may be retriable).
    #[error("HTTP error: {0}")]
    Http(String),

    /// Rate limited - should retry after delay.
    #[error("rate limited")]
    RateLimited,

    /// Server error (5xx) - retriable.
    #[error("server error: {}", status.as_u16())]
    ServerError { status: StatusCode },

    /// Content not found (404) - permanent.
    #[error("not found")]
    NotFound,

    /// Access forbidden (403) - permanent.
    #[error("forbidden")]
    Forbidden,

    /// Unsupported content type - permanent.
    #[error("unsupported content type: {0}")]
    UnsupportedContentType(String),

    /// Failed to parse content - permanent.
    #[error("parse error: {0}")]
    ParseError(String),

    /// Database error - retriable.
    #[error("database error: {0}")]
    Database(#[from] chronoscope_db::DbError),

    /// Media storage error - retriable.
    #[error("media store error: {0}")]
    MediaStore(#[from] MediaStoreError),

    /// VCR cache miss - permanent (only occurs in test mode).
    #[error("cache miss: {0}")]
    CacheMiss(String),
}

impl FetchError {
    /// Convert an HTTP error to a `FetchError`.
    ///
    /// This handles `CacheMiss` specially as a permanent error (for VCR testing),
    /// while other HTTP errors remain retriable.
    #[must_use]
    pub fn from_http_error(e: HttpError) -> Self {
        match e {
            HttpError::CacheMiss { url } => Self::CacheMiss(url),
            other => Self::Http(other.to_string()),
        }
    }

    /// Convert an HTTP status code to a `FetchError`, returning `Ok(())` for success codes.
    ///
    /// This provides consistent error handling across all fetchers.
    pub fn from_status(status: StatusCode) -> Result<(), Self> {
        if status.is_success() {
            return Ok(());
        }
        Err(match status.as_u16() {
            404 => Self::NotFound,
            403 => Self::Forbidden,
            429 => Self::RateLimited,
            500..=599 => Self::ServerError { status },
            code => Self::Http(format!("unexpected status: {code}")),
        })
    }

    /// Whether this error should be retried.
    #[must_use]
    pub fn is_retriable(&self) -> bool {
        matches!(
            self,
            Self::Http(_)
                | Self::RateLimited
                | Self::ServerError { .. }
                | Self::Database(_)
                | Self::MediaStore(_)
        )
    }
}

/// Trait for domain-specific URL fetchers.
///
/// Fetchers process URLs and return structured results. The generic fetcher
/// handles most URLs, while domain-specific fetchers (Reddit, Instagram, etc.)
/// can provide better extraction for their platforms.
#[async_trait::async_trait]
pub trait Fetcher: Send + Sync {
    /// Domains this fetcher handles (e.g., `["reddit.com", "redd.it"]`).
    ///
    /// Return an empty slice for the generic/fallback fetcher.
    fn domains(&self) -> &'static [&'static str];

    /// Process a URL and return the result.
    ///
    /// On success, the fetcher is responsible for:
    /// 1. Creating the Page or Media in the database
    /// 2. Storing media content in the media store
    /// 3. Marking the URL as resolved via `db.mark_url_resolved_to_*`
    ///
    /// The returned `FetchResult` includes any discovered URLs to enqueue.
    async fn process(
        &self,
        ctx: &FetchContext,
        url: &ResearchUrl,
    ) -> Result<FetchResult, FetchError>;
}

/// Registry that routes URLs to appropriate fetchers.
pub struct FetcherRegistry {
    /// Domain-specific fetchers.
    by_domain: HashMap<&'static str, Arc<dyn Fetcher>>,
    /// Fallback fetcher for unrecognized domains.
    generic: Arc<dyn Fetcher>,
}

impl FetcherRegistry {
    /// Create a new registry with a generic fallback fetcher.
    #[must_use]
    pub fn new(generic: Arc<dyn Fetcher>) -> Self {
        Self {
            by_domain: HashMap::new(),
            generic,
        }
    }

    /// Register a domain-specific fetcher.
    ///
    /// The fetcher will be used for all domains returned by `fetcher.domains()`.
    pub fn register(&mut self, fetcher: Arc<dyn Fetcher>) {
        for domain in fetcher.domains() {
            self.by_domain.insert(domain, fetcher.clone());
        }
    }

    /// Get the fetcher for a URL.
    ///
    /// Returns the domain-specific fetcher if one is registered,
    /// otherwise returns the generic fetcher.
    #[must_use]
    pub fn get(&self, url: &Url) -> Arc<dyn Fetcher> {
        url.host_str()
            .and_then(|host| {
                // Try exact match first
                if let Some(fetcher) = self.by_domain.get(host) {
                    return Some(fetcher.clone());
                }
                // Try without www. prefix
                let host = host.strip_prefix("www.").unwrap_or(host);
                self.by_domain.get(host).cloned()
            })
            .unwrap_or_else(|| self.generic.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    // ==================== FetchError::is_retriable Tests ====================

    #[test]
    fn test_retriable_errors() {
        // HTTP errors are retriable (network issues, etc.)
        assert!(FetchError::Http("connection reset".into()).is_retriable());

        // Rate limiting should be retried after delay
        assert!(FetchError::RateLimited.is_retriable());

        // Server errors (5xx) are retriable
        assert!(
            FetchError::ServerError {
                status: StatusCode::INTERNAL_SERVER_ERROR
            }
            .is_retriable()
        );
        assert!(
            FetchError::ServerError {
                status: StatusCode::BAD_GATEWAY
            }
            .is_retriable()
        );
        assert!(
            FetchError::ServerError {
                status: StatusCode::SERVICE_UNAVAILABLE
            }
            .is_retriable()
        );

        // Database errors are retriable (transient failures)
        assert!(FetchError::Database(chronoscope_db::DbError::UserNotFound).is_retriable());

        // Media store errors are retriable (storage issues)
        assert!(
            FetchError::MediaStore(MediaStoreError::from(std::io::Error::other("s3 timeout")))
                .is_retriable()
        );
    }

    #[test]
    fn test_permanent_errors() {
        // 404 Not Found - resource doesn't exist
        assert!(!FetchError::NotFound.is_retriable());

        // 403 Forbidden - access denied
        assert!(!FetchError::Forbidden.is_retriable());

        // Unsupported content type - won't change on retry
        assert!(!FetchError::UnsupportedContentType("application/pdf".into()).is_retriable());

        // Parse errors - content is malformed
        assert!(!FetchError::ParseError("invalid json".into()).is_retriable());
    }

    // ==================== FetcherRegistry Tests ====================

    /// Minimal fetcher for testing domain routing.
    struct TestFetcher {
        domains: &'static [&'static str],
    }

    impl TestFetcher {
        fn new(domains: &'static [&'static str]) -> Self {
            Self { domains }
        }
    }

    #[async_trait::async_trait]
    impl Fetcher for TestFetcher {
        fn domains(&self) -> &'static [&'static str] {
            self.domains
        }

        async fn process(
            &self,
            _ctx: &FetchContext,
            _url: &ResearchUrl,
        ) -> Result<FetchResult, FetchError> {
            unimplemented!("test fetcher")
        }
    }

    /// Helper to get fetcher name for assertions.
    fn fetcher_name(fetcher: &Arc<dyn Fetcher>) -> &'static str {
        // Downcast to TestFetcher to get name
        // This is a bit hacky but works for tests
        fetcher.domains().first().copied().unwrap_or("generic")
    }

    #[test]
    fn test_registry_exact_domain_match() -> TestResult {
        let generic: Arc<dyn Fetcher> = Arc::new(TestFetcher::new(&[]));
        let reddit: Arc<dyn Fetcher> = Arc::new(TestFetcher::new(&["reddit.com", "redd.it"]));

        let mut registry = FetcherRegistry::new(generic);
        registry.register(reddit);

        let url = Url::parse("https://reddit.com/r/rust")?;
        let fetcher = registry.get(&url);
        assert_eq!(fetcher_name(&fetcher), "reddit.com");

        let url = Url::parse("https://redd.it/abc123")?;
        let fetcher = registry.get(&url);
        assert_eq!(fetcher_name(&fetcher), "reddit.com");
        Ok(())
    }

    #[test]
    fn test_registry_strips_www_prefix() -> TestResult {
        let generic: Arc<dyn Fetcher> = Arc::new(TestFetcher::new(&[]));
        let example: Arc<dyn Fetcher> = Arc::new(TestFetcher::new(&["example.com"]));

        let mut registry = FetcherRegistry::new(generic);
        registry.register(example);

        // Should match with www. prefix
        let url = Url::parse("https://www.example.com/page")?;
        let fetcher = registry.get(&url);
        assert_eq!(fetcher_name(&fetcher), "example.com");

        // Should also match without www.
        let url = Url::parse("https://example.com/page")?;
        let fetcher = registry.get(&url);
        assert_eq!(fetcher_name(&fetcher), "example.com");
        Ok(())
    }

    #[test]
    fn test_registry_falls_back_to_generic() -> TestResult {
        let generic: Arc<dyn Fetcher> = Arc::new(TestFetcher::new(&[]));
        let reddit: Arc<dyn Fetcher> = Arc::new(TestFetcher::new(&["reddit.com"]));

        let mut registry = FetcherRegistry::new(generic);
        registry.register(reddit);

        // Unknown domain should fall back to generic
        let url = Url::parse("https://unknown-site.com/page")?;
        let fetcher = registry.get(&url);
        assert_eq!(fetcher_name(&fetcher), "generic");
        Ok(())
    }

    #[test]
    fn test_registry_handles_url_without_host() -> TestResult {
        let generic: Arc<dyn Fetcher> = Arc::new(TestFetcher::new(&[]));
        let registry = FetcherRegistry::new(generic);

        // file:// URL has no host
        let url = Url::parse("file:///path/to/file")?;
        let fetcher = registry.get(&url);
        assert_eq!(fetcher_name(&fetcher), "generic");
        Ok(())
    }
}
