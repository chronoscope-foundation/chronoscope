//! Generic URL fetcher for HTML, images, and video.
//!
//! Handles URLs that don't have domain-specific fetchers by detecting content
//! type and dispatching to the appropriate processor.
//!
//! # TODO: Streaming for large files (optimization)
//!
//! Currently loads entire response into memory before processing. The `HttpClient`
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
use chronoscope_integrations::HttpRequest;
use tracing::{instrument, warn};
use url::Url;

use super::content::{ContentType, detect_content_type};
use super::fetcher::{FetchContext, FetchError, FetchResult};

/// Generic fetcher that handles HTML, images, and video.
///
/// Routes content to specialized processors based on detected content type.
/// Used for URLs that don't have a specialized integration (Reddit, Instagram, etc.).
#[derive(Default)]
pub struct GenericFetcher;

impl GenericFetcher {
    /// Create a new generic fetcher.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Process a URL and return the result.
    ///
    /// Detects content type and dispatches to the appropriate processor
    /// (HTML, image, or video).
    #[instrument(skip(self, ctx, research_url), fields(url = %research_url.url))]
    pub async fn process(
        &self,
        ctx: &FetchContext,
        research_url: &ResearchUrl,
    ) -> Result<FetchResult, FetchError> {
        let url = Url::parse(&research_url.url)
            .map_err(|e| FetchError::ContentProcessing(format!("invalid URL: {e}")))?;

        // Fetch the content (HttpClient handles size limits internally via streaming)
        let request = HttpRequest::get(url.clone());
        let response = ctx
            .http
            .execute(request)
            .await
            .map_err(FetchError::from_http_error)?;

        // Check response status
        FetchError::from_status(response.status)?;

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
    use crate::url_fetcher::test_harness::{TestHarness, TestResult};
    use chrono::Datelike;
    use chronoscope_db::MediaType;
    use chronoscope_db::media_store::MediaStore;

    // ==================== Integration Tests ====================
    //
    // Run with: cargo test -p chronoscope-workers --features record-fixtures
    // to record new fixtures.

    /// Image with EXIF GPS and timestamp data
    #[tokio::test]
    async fn test_process_image_with_exif() -> TestResult {
        let url =
            "https://raw.githubusercontent.com/ianare/exif-samples/master/jpg/gps/DSCN0010.jpg";

        let fetched = TestHarness::new().await?.fetch_media(url).await?;

        // Image dimensions
        assert_eq!(fetched.media.data.media_type, MediaType::Image);
        assert_eq!(fetched.media.data.width, 640);
        assert_eq!(fetched.media.data.height, 480);

        // EXIF: captured 2008-10-22 in Tuscany, Italy
        let captured = fetched
            .media
            .data
            .captured
            .ok_or("should have capture time")?;
        assert_eq!(captured.earliest().year(), 2008);

        let location = fetched
            .media
            .data
            .location
            .ok_or("should have GPS location")?;
        if let chronoscope_core::UncertainLocation::Coordinates { lat, .. } = &location {
            assert!((*lat - 43.467).abs() < 0.01, "lat: {lat}",);
        } else {
            return Err("expected Coordinates location".into());
        }

        // Stored in media store
        assert!(
            fetched
                .media_store
                .exists(&fetched.media.data.storage_key)
                .await?
        );
        Ok(())
    }

    /// HTML page with article extraction and image discovery
    #[tokio::test]
    async fn test_process_html_extracts_content_and_images() -> TestResult {
        let url = "https://en.wikipedia.org/w/index.php?title=William_K._Vanderbilt_House&oldid=1316532748";

        let fetched = TestHarness::new().await?.fetch_page(url).await?;

        // Title
        assert_eq!(
            fetched.page.data.title.as_deref(),
            Some("William K. Vanderbilt House")
        );

        // Content includes article text
        let content = fetched
            .page
            .data
            .content
            .as_deref()
            .ok_or("should have content")?;
        assert!(content.contains("The mansion was built for"));
        assert!(content.contains("from 1878 to 1882"));
        assert!(content.contains("William Kissam Vanderbilt"));

        // Readability strips boilerplate
        assert!(!content.contains("About Wikipedia"));
        assert!(!content.contains("Wikipedia® is a registered trademark"));

        // Historical images discovered
        let urls: Vec<_> = fetched.discovered_urls.iter().map(|u| u.as_str()).collect();
        assert!(
            urls.iter().any(|u| u.contains("WKVanderbiltHouse")),
            "urls: {urls:?}"
        );
        assert!(
            urls.iter().any(|u| u.contains("Alva_Vanderbilt")),
            "urls: {urls:?}"
        );
        assert!(
            urls.iter().any(|u| u.contains("Petit_Chateau")),
            "urls: {urls:?}"
        );
        Ok(())
    }

    /// MP4 video with metadata extraction
    #[tokio::test]
    async fn test_process_video_extracts_metadata() -> TestResult {
        let url = "https://raw.githubusercontent.com/mozilla/mp4parse-rust/refs/heads/master/mp4parse/tests/minimal.mp4";

        let fetched = TestHarness::new().await?.fetch_media(url).await?;

        // Video dimensions
        assert_eq!(fetched.media.data.media_type, MediaType::Video);
        assert_eq!(fetched.media.data.width, 320);
        assert_eq!(fetched.media.data.height, 240);

        // Duration (~0.04s for this minimal test file)
        let duration = fetched
            .media
            .data
            .duration_seconds
            .ok_or("should have duration")?;
        assert!((0.03..0.05).contains(&duration), "duration: {duration}");

        // Stored in media store
        assert!(
            fetched
                .media_store
                .exists(&fetched.media.data.storage_key)
                .await?
        );
        Ok(())
    }

    /// 404 returns `NotFound` error
    #[tokio::test]
    async fn test_process_404_returns_not_found() -> TestResult {
        let url = "https://example.com/nonexistent-page-12345";

        let error = TestHarness::new().await?.fetch_error(url).await?;

        assert!(
            matches!(
                error,
                FetchError::Integration(chronoscope_integrations::FetchError::NotFound)
            ),
            "error: {error:?}"
        );
        Ok(())
    }
}
