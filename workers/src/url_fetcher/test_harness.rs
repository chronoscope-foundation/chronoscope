//! Shared test harness for URL fetcher integration tests.
//!
//! Provides VCR-style HTTP caching for reproducible tests.

use std::sync::Arc;

use chrono::Utc;
use chronoscope_db::media_store::InMemoryMediaStore;
use chronoscope_db::{Database, Email, Media, Page, ResearchUrlStatus, ResolvedContent, UserId};
use url::Url;

use crate::http::{CacheMode, CachingClient, HttpClient};
use crate::url_fetcher::{FetchError, UrlFetcherWorker};
use crate::worker::{ItemResult, Worker};

/// Result type for tests.
pub type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Error for test harness failures.
#[derive(Debug)]
pub struct TestError(pub String);

impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for TestError {}

/// Check if a `FetchError` indicates a cache miss (fixture not recorded).
fn is_cache_miss(error: &FetchError) -> bool {
    // Cache misses come through as Http errors from the CachingClient
    matches!(error, FetchError::Http(msg) if msg.to_lowercase().contains("cache miss"))
}

fn fixtures_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn vcr_mode() -> CacheMode {
    if cfg!(feature = "record-fixtures") {
        CacheMode::Online
    } else {
        CacheMode::Offline
    }
}

/// Test harness for URL fetcher integration tests.
pub struct TestHarness {
    db: Arc<Database>,
    media_store: Arc<InMemoryMediaStore>,
    user_id: UserId,
}

impl TestHarness {
    /// Create a new test harness with an in-memory database.
    pub async fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let db = Arc::new(Database::new("sqlite::memory:").await?);
        let user_id = UserId::generate();
        db.create_user(&user_id, "testuser", &Email::new("test@example.com"))
            .await?;
        Ok(Self {
            db,
            media_store: Arc::new(InMemoryMediaStore::new()),
            user_id,
        })
    }

    /// Process a URL and expect a Page result.
    pub async fn fetch_page(
        &self,
        url: &str,
    ) -> Result<FetchedPage, Box<dyn std::error::Error + Send + Sync>> {
        match self.fetch(url).await? {
            FetchResult::Page(page) => Ok(page),
            FetchResult::Media(_) => Err(Box::new(TestError("expected Page, got Media".into()))),
            FetchResult::Error(e) => Err(Box::new(e)),
        }
    }

    /// Process a URL and expect a Media result.
    pub async fn fetch_media(
        &self,
        url: &str,
    ) -> Result<FetchedMedia, Box<dyn std::error::Error + Send + Sync>> {
        match self.fetch(url).await? {
            FetchResult::Media(media) => Ok(media),
            FetchResult::Page(_) => Err(Box::new(TestError("expected Media, got Page".into()))),
            FetchResult::Error(e) => Err(Box::new(e)),
        }
    }

    /// Process a URL and expect an error.
    pub async fn fetch_error(
        &self,
        url: &str,
    ) -> Result<FetchError, Box<dyn std::error::Error + Send + Sync>> {
        match self.fetch(url).await? {
            FetchResult::Error(e) => Ok(e),
            FetchResult::Page(_) => Err(Box::new(TestError("expected error, got Page".into()))),
            FetchResult::Media(_) => Err(Box::new(TestError("expected error, got Media".into()))),
        }
    }

    /// Process a URL through the full `UrlFetcherWorker` pipeline.
    async fn fetch(
        &self,
        url: &str,
    ) -> Result<FetchResult, Box<dyn std::error::Error + Send + Sync>> {
        self.db.submit_url(&self.user_id, url).await?;

        let stale_cutoff = Utc::now().naive_utc() - chrono::Duration::hours(1);
        let claimed = self.db.claim_urls("test-worker", 1, stale_cutoff).await?;
        let research_url = claimed
            .into_iter()
            .next()
            .ok_or_else(|| TestError("should have claimed URL".into()))?;
        let url_id = research_url.id.clone();

        let http_client: Arc<dyn HttpClient> =
            Arc::new(CachingClient::new(fixtures_dir(), vcr_mode())?);
        let worker =
            UrlFetcherWorker::with_defaults(self.db.clone(), http_client, self.media_store.clone());

        let results = worker.process_batch(vec![research_url]).await;
        let (_research_url, item_result) = results
            .into_iter()
            .next()
            .ok_or_else(|| TestError("should have result".into()))?;

        let discovered_urls = match item_result {
            ItemResult::Success { discovered } => discovered,
            ItemResult::RetriableFailure { ref error }
            | ItemResult::PermanentFailure { ref error }
                if is_cache_miss(error) =>
            {
                return Err(Box::new(TestError(format!(
                    "fixture missing for: {url}\n\
                     Record with: cargo test -p chronoscope-workers --features record-fixtures"
                ))));
            }
            ItemResult::RetriableFailure { error } | ItemResult::PermanentFailure { error } => {
                return Ok(FetchResult::Error(error));
            }
        };

        let dossier = self.db.get_research_dossier(&url_id).await?;
        let dossier =
            dossier.ok_or_else(|| TestError(format!("dossier should exist for URL: {url}")))?;

        // Invariant: if ItemResult::Success was returned, the URL must be marked Complete.
        // If this fails, it's a bug in the worker or harness, not a test failure.
        assert_eq!(dossier.research_url.status, ResearchUrlStatus::Complete);

        let resolved = dossier
            .resolved
            .ok_or_else(|| TestError("should have resolved content".into()))?;

        match resolved {
            ResolvedContent::Page(page) => Ok(FetchResult::Page(FetchedPage {
                page,
                discovered_urls,
            })),
            ResolvedContent::Media(media) => Ok(FetchResult::Media(FetchedMedia {
                media,
                media_store: self.media_store.clone(),
            })),
        }
    }
}

/// Internal result type for fetch operations.
enum FetchResult {
    Page(FetchedPage),
    Media(FetchedMedia),
    Error(FetchError),
}

/// Result of successfully fetching a page.
pub struct FetchedPage {
    pub page: Page,
    pub discovered_urls: Vec<Url>,
}

/// Result of successfully fetching media.
pub struct FetchedMedia {
    pub media: Media,
    pub media_store: Arc<InMemoryMediaStore>,
}
