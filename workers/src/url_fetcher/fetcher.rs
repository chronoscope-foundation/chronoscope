//! Worker-specific types for URL fetching.
//!
//! Integrations handle the actual fetching and return structured content.
//! This module provides the types needed to adapt that content for persistence
//! and coordinate with the worker framework.

use std::sync::Arc;
use std::time::Duration;

use chronoscope_db::Database;
use chronoscope_db::media_store::{MediaStore, MediaStoreError};
use chronoscope_integrations::HttpClient;
use url::Url;

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

/// Context passed during URL processing.
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

/// What a URL resolved to (after persistence).
#[derive(Debug)]
pub enum FetchOutcome {
    /// Resolved to a page (HTML content).
    Page { page_id: chronoscope_db::PageId },
    /// Resolved to media (image/video).
    Media { media_id: chronoscope_db::MediaId },
}

/// Result of successfully fetching and persisting a URL.
#[derive(Debug)]
pub struct FetchResult {
    /// What the URL resolved to.
    pub outcome: FetchOutcome,
    /// Additional URLs discovered during processing (e.g., images in HTML).
    pub discovered_urls: Vec<Url>,
}

/// Errors that can occur during fetching.
///
/// Wraps [`chronoscope_integrations::FetchError`] for HTTP-level errors and adds
/// worker-specific error variants for content processing, database, and media storage.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// Error from integration layer (HTTP, rate limiting, API parse errors, etc.)
    #[error(transparent)]
    Integration(#[from] chronoscope_integrations::FetchError),

    /// Failed to process content (HTML extraction, image decoding, video parsing) - permanent.
    #[error("content processing error: {0}")]
    ContentProcessing(String),

    /// Unsupported content type - permanent.
    #[error("unsupported content type: {0}")]
    UnsupportedContentType(String),

    /// Database error - retriable.
    #[error("database error: {0}")]
    Database(#[from] chronoscope_db::DbError),

    /// Media storage error - retriable.
    #[error("media store error: {0}")]
    MediaStore(#[from] MediaStoreError),

    /// Batch integration not yet implemented - permanent.
    #[error("batch integration not implemented: {0}")]
    BatchNotImplemented(String),
}

impl FetchError {
    /// Convert an HTTP error to a [`FetchError`].
    ///
    /// Delegates to [`chronoscope_integrations::FetchError::from_http_error`].
    #[must_use]
    pub fn from_http_error(e: chronoscope_integrations::HttpError) -> Self {
        Self::Integration(chronoscope_integrations::FetchError::from_http_error(e))
    }

    /// Convert an HTTP status code to a [`FetchError`], returning `Ok(())` for success codes.
    ///
    /// Delegates to [`chronoscope_integrations::FetchError::from_status`].
    pub fn from_status(status: reqwest::StatusCode) -> Result<(), Self> {
        chronoscope_integrations::FetchError::from_status(status).map_err(Self::Integration)
    }

    /// Whether this error should be retried.
    #[must_use]
    pub fn is_retriable(&self) -> bool {
        match self {
            Self::Integration(e) => e.is_retriable(),
            Self::Database(_) | Self::MediaStore(_) => true,
            Self::ContentProcessing(_)
            | Self::UnsupportedContentType(_)
            | Self::BatchNotImplemented(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chronoscope_integrations::FetchError as IntegrationError;

    #[test]
    fn test_retriable_errors() {
        // Service errors are retriable (integration-level issues)
        assert!(
            FetchError::Integration(IntegrationError::service("connection reset")).is_retriable()
        );

        // Rate limiting should be retried after delay
        assert!(FetchError::Integration(IntegrationError::RateLimited).is_retriable());

        // Server errors (5xx) are retriable
        assert!(
            FetchError::Integration(IntegrationError::ServerError { status: 500 }).is_retriable()
        );
        assert!(
            FetchError::Integration(IntegrationError::ServerError { status: 502 }).is_retriable()
        );
        assert!(
            FetchError::Integration(IntegrationError::ServerError { status: 503 }).is_retriable()
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
        assert!(!FetchError::Integration(IntegrationError::NotFound).is_retriable());

        // 403 Forbidden - access denied
        assert!(!FetchError::Integration(IntegrationError::Forbidden).is_retriable());

        // Unsupported content type - won't change on retry
        assert!(!FetchError::UnsupportedContentType("application/pdf".into()).is_retriable());

        // Content processing errors - HTML extraction, image decode, etc.
        assert!(!FetchError::ContentProcessing("failed to decode image".into()).is_retriable());

        // API parse errors - malformed JSON from integrations
        assert!(
            !FetchError::Integration(IntegrationError::ParseError("invalid json".into()))
                .is_retriable()
        );

        // Batch not implemented - programming error, not transient
        assert!(!FetchError::BatchNotImplemented("instagram".into()).is_retriable());
    }
}
