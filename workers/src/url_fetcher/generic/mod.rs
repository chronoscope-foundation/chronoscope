//! Generic URL fetcher for HTML, images, and video.
//!
//! Handles URLs that don't have domain-specific fetchers by detecting content
//! type and dispatching to the appropriate processor.
//!
//! # TODO: Streaming for large files (optimization)
//!
//! Currently loads entire response into memory before processing. The HttpClient
//! enforces a size limit while streaming, so this is safe but inefficient for large
//! files. For better memory efficiency with video, we could stream directly to
//! [`MediaStore`]:
//!
//! 1. Add `HttpClient::execute_streaming()` returning a `bytes_stream()`
//! 2. Buffer first ~1MB for content type detection (magic bytes need ~12 bytes)
//! 3. For MP4/MOV: parse atom headers from buffer to find `moov`
//!    - If `moov` is present and fits in buffer → extract metadata
//!    - If `mdat` comes first (non-fast-start) → skip metadata for now
//! 4. Prepend buffered bytes to stream, pipe rest directly to `MediaStore::put_stream()`

pub mod html;
pub mod image;
pub mod video;

use chronoscope_db::ResearchUrl;
use tracing::{instrument, warn};
use url::Url;

use super::content::{ContentType, detect_content_type};
use super::fetcher::{FetchContext, FetchError, FetchResult, Fetcher};
use crate::http::HttpRequest;

/// Generic fetcher that handles HTML, images, and video.
///
/// Routes content to specialized processors based on detected content type.
pub struct GenericFetcher;

impl GenericFetcher {
    /// Create a new generic fetcher.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for GenericFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Fetcher for GenericFetcher {
    fn domains(&self) -> &'static [&'static str] {
        // Empty = fallback fetcher for all unrecognized domains
        &[]
    }

    #[instrument(skip(self, ctx, research_url), fields(url = %research_url.url))]
    async fn process(
        &self,
        ctx: &FetchContext,
        research_url: &ResearchUrl,
    ) -> Result<FetchResult, FetchError> {
        let url = Url::parse(&research_url.url)
            .map_err(|e| FetchError::ParseError(format!("invalid URL: {e}")))?;

        // Fetch the content (HttpClient handles size limits internally via streaming)
        let request = HttpRequest::get(url.clone());
        let response = ctx
            .http
            .execute(request)
            .await
            .map_err(|e| FetchError::Http(e.to_string()))?;

        // Check response status
        match response.status.as_u16() {
            200..=299 => {} // OK
            404 => return Err(FetchError::NotFound),
            403 => return Err(FetchError::Forbidden),
            429 => return Err(FetchError::RateLimited),
            status @ 500..=599 => return Err(FetchError::ServerError { status }),
            status => {
                return Err(FetchError::Http(format!("unexpected status: {status}")));
            }
        }

        // Detect content type from headers and magic bytes
        let content_type_header = response
            .headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok());
        let content_type = detect_content_type(content_type_header, &response.body);

        // Dispatch to appropriate processor
        match content_type {
            ContentType::Html => {
                html::process(ctx, research_url, &response.final_url, &response.body).await
            }
            ContentType::Image(format) => {
                image::process(
                    ctx,
                    research_url,
                    &response.final_url,
                    &response.body,
                    format,
                )
                .await
            }
            ContentType::Video(format) => {
                video::process(
                    ctx,
                    research_url,
                    &response.final_url,
                    &response.body,
                    format,
                )
                .await
            }
            ContentType::Unknown(mime) => {
                warn!(mime = %mime, "unsupported content type");
                Err(FetchError::UnsupportedContentType(mime))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{CacheMode, CachingClient, HttpClient, HttpError};
    use crate::url_fetcher::UrlFetcherWorker;
    use crate::worker::{ItemResult, Worker};
    use chrono::{Datelike, Utc};
    use chronoscope_db::media_store::{InMemoryMediaStore, MediaStore};
    use chronoscope_db::{Database, Email, MediaType, ResearchUrlStatus, ResolvedContent, UserId};
    use std::sync::Arc;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

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

    fn vcr_client() -> Result<CachingClient, HttpError> {
        CachingClient::new(fixtures_dir(), vcr_mode())
    }

    // ==================== Test Harness ====================

    /// Error for test harness failures.
    #[derive(Debug)]
    struct TestError(String);

    impl std::fmt::Display for TestError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    impl std::error::Error for TestError {}

    struct TestHarness {
        db: Arc<Database>,
        media_store: Arc<InMemoryMediaStore>,
    }

    impl TestHarness {
        async fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
            let db = Arc::new(Database::new("sqlite::memory:").await?);
            let user_id = UserId::generate();
            db.create_user(&user_id, "testuser", &Email::new("test@example.com"))
                .await?;
            Ok(Self {
                db,
                media_store: Arc::new(InMemoryMediaStore::new()),
            })
        }

        /// Process a URL through the full `UrlFetcherWorker` pipeline.
        /// Returns None if fixture is missing (prints instructions).
        async fn process(
            &self,
            url: &str,
        ) -> Result<Option<ProcessResult>, Box<dyn std::error::Error + Send + Sync>> {
            // Submit and claim the URL (ignore error if user already exists from prior test)
            let user_id = UserId::generate();
            self.db
                .create_user(&user_id, "testuser2", &Email::new("test2@example.com"))
                .await
                .ok();
            self.db.submit_url(&user_id, url).await?;

            let stale_cutoff = Utc::now().naive_utc() - chrono::Duration::hours(1);
            let claimed = self.db.claim_urls("test-worker", 1, stale_cutoff).await?;
            let research_url = claimed
                .into_iter()
                .next()
                .ok_or_else(|| TestError("should have claimed URL".into()))?;
            let url_id = research_url.id.clone();

            // Create worker with VCR client - this tests the full entry point
            let http_client: Arc<dyn HttpClient> = Arc::new(vcr_client()?);
            let worker = UrlFetcherWorker::with_defaults(
                self.db.clone(),
                http_client,
                self.media_store.clone(),
            );

            // Process through the Worker interface
            let results = worker.process_batch(vec![research_url]).await;
            let (_research_url, item_result) = results
                .into_iter()
                .next()
                .ok_or_else(|| TestError("should have result".into()))?;

            // Handle the result
            let discovered_urls = match item_result {
                ItemResult::Success { discovered } => discovered,
                ItemResult::RetriableFailure { error } | ItemResult::PermanentFailure { error }
                    if error.contains("CacheMiss") =>
                {
                    eprintln!(
                        "\n  ⚠ Fixture missing for: {url}\n  \
                         Record with: cargo test -p chronoscope-workers --features record-fixtures\n"
                    );
                    return Ok(None);
                }
                ItemResult::RetriableFailure { error } => {
                    return Ok(Some(ProcessResult::Error(FetchError::Http(error))));
                }
                ItemResult::PermanentFailure { error } => {
                    // Try to reconstruct a more specific error type
                    if error.contains("not found") || error.contains("NotFound") {
                        return Ok(Some(ProcessResult::Error(FetchError::NotFound)));
                    }
                    return Ok(Some(ProcessResult::Error(FetchError::ParseError(error))));
                }
            };

            // Fetch the resolved content from DB
            let dossier = self.db.get_research_dossier(&url_id).await?;
            let dossier =
                dossier.ok_or_else(|| TestError(format!("dossier should exist for URL: {url}")))?;

            assert_eq!(dossier.research_url.status, ResearchUrlStatus::Complete);

            let resolved = dossier
                .resolved
                .ok_or_else(|| TestError("should have resolved content".into()))?;

            Ok(Some(ProcessResult::Success {
                resolved,
                discovered_urls,
                media_store: self.media_store.clone(),
            }))
        }
    }

    enum ProcessResult {
        Success {
            resolved: ResolvedContent,
            discovered_urls: Vec<Url>,
            media_store: Arc<InMemoryMediaStore>,
        },
        Error(FetchError),
    }

    // ==================== Integration Tests ====================
    //
    // Run with: cargo test -p chronoscope-workers --features record-fixtures
    // to record new fixtures.

    #[tokio::test]
    async fn test_process_image_with_exif() -> TestResult {
        let harness = TestHarness::new().await?;
        let Some(result) = harness
            .process(
                "https://raw.githubusercontent.com/ianare/exif-samples/master/jpg/gps/DSCN0010.jpg",
            )
            .await?
        else {
            return Ok(());
        };

        let ProcessResult::Success {
            resolved: ResolvedContent::Media(media),
            media_store,
            ..
        } = result
        else {
            return Err(TestError("expected Media result".into()).into());
        };

        assert_eq!(media.data.media_type, MediaType::Image);
        assert_eq!(media.data.width, 640);
        assert_eq!(media.data.height, 480);

        // EXIF: captured 2008-10-22 in Tuscany, Italy
        let captured = media
            .data
            .captured_at
            .ok_or_else(|| TestError("should have capture time".into()))?;
        assert_eq!(captured.and_utc().year(), 2008);
        let location = media
            .data
            .location
            .ok_or_else(|| TestError("should have GPS location".into()))?;
        assert!((location.latitude - 43.467).abs() < 0.01);

        assert!(media_store.exists(&media.data.storage_key).await?);
        Ok(())
    }

    #[tokio::test]
    async fn test_process_html_extracts_content_and_images() -> TestResult {
        let harness = TestHarness::new().await?;
        let Some(result) = harness
            .process("https://en.wikipedia.org/w/index.php?title=William_K._Vanderbilt_House&oldid=1316532748")
            .await?
        else {
            return Ok(());
        };

        let ProcessResult::Success {
            resolved: ResolvedContent::Page(page),
            discovered_urls: discovered,
            ..
        } = result
        else {
            return Err(TestError("expected Page result".into()).into());
        };

        // Title extraction
        assert_eq!(
            page.data.title.as_deref(),
            Some("William K. Vanderbilt House")
        );

        // Content should include specific article text
        let content = page
            .data
            .content
            .as_ref()
            .ok_or_else(|| TestError("page should have content".into()))?;
        assert!(
            content.contains("The mansion was built for"),
            "expected content to contain 'The mansion was built for'"
        );
        assert!(
            content.contains("from 1878 to 1882"),
            "expected content to contain 'from 1878 to 1882'"
        );
        assert!(
            content.contains("William Kissam Vanderbilt"),
            "expected content to mention William Kissam Vanderbilt"
        );

        // Readability should strip Wikipedia boilerplate (sidebar, footer)
        assert!(
            !content.contains("About Wikipedia"),
            "content should not include sidebar 'About Wikipedia' link"
        );
        assert!(
            !content.contains("Contact Wikipedia"),
            "content should not include sidebar 'Contact Wikipedia' link"
        );
        assert!(
            !content.contains("Wikipedia® is a registered trademark"),
            "content should not include footer trademark notice"
        );

        // The 3 interesting historical images should be discovered
        let image_strs: Vec<_> = discovered.iter().map(|u| u.as_str()).collect();
        let expected_images = [
            "https://upload.wikimedia.org/wikipedia/commons/thumb/0/03/WKVanderbiltHouse_Cropped_version.jpeg/330px-WKVanderbiltHouse_Cropped_version.jpeg",
            "https://upload.wikimedia.org/wikipedia/commons/thumb/f/f6/Alva_Vanderbilt_1883_Costume_Ball.jpg/250px-Alva_Vanderbilt_1883_Costume_Ball.jpg",
            "https://upload.wikimedia.org/wikipedia/commons/thumb/5/51/Petit_Chateau_salon.jpg/250px-Petit_Chateau_salon.jpg",
        ];
        for expected in &expected_images {
            assert!(
                image_strs.contains(expected),
                "expected to find image {expected}, got: {image_strs:?}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_process_video_extracts_metadata() -> TestResult {
        let harness = TestHarness::new().await?;
        let Some(result) = harness
            .process("https://raw.githubusercontent.com/mozilla/mp4parse-rust/refs/heads/master/mp4parse/tests/minimal.mp4")
            .await?
        else {
            return Ok(());
        };

        let ProcessResult::Success {
            resolved: ResolvedContent::Media(media),
            media_store,
            ..
        } = result
        else {
            return Err(TestError("expected Media result".into()).into());
        };

        assert_eq!(media.data.media_type, MediaType::Video);
        assert_eq!(media.data.width, 320);
        assert_eq!(media.data.height, 240);

        // Duration is very short (~0.04s) for this minimal test file
        let duration = media
            .data
            .duration_seconds
            .ok_or_else(|| TestError("should have duration".into()))?;
        assert!(
            (0.03..0.05).contains(&duration),
            "expected duration ~0.04s, got {duration}"
        );

        assert!(media_store.exists(&media.data.storage_key).await?);
        Ok(())
    }

    #[tokio::test]
    async fn test_process_404_returns_not_found() -> TestResult {
        let harness = TestHarness::new().await?;
        let Some(result) = harness
            .process("https://example.com/nonexistent-page-12345")
            .await?
        else {
            return Ok(());
        };

        let ProcessResult::Error(error) = result else {
            return Err(TestError("expected error result".into()).into());
        };
        assert!(matches!(error, FetchError::NotFound));
        Ok(())
    }
}
