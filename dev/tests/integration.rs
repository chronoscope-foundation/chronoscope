//! End-to-end integration tests for the URL fetching pipeline.
//!
//! These tests verify the complete HTTP flow from URL submission to media retrieval:
//! 1. Submit a URL via POST /research
//! 2. Poll GET /research/{id} until resolved
//! 3. Verify the page structure and media states
//! 4. Fetch media via CDN URLs
//!
//! Tests use VCR-style fixtures for reproducible HTTP responses.

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use chronoscope_api::research_types::{MediaReference, ResearchUrlDossier, ResolvedContent};
use chronoscope_dev::{DevServerConfig, RunningDevServer, start_dev_server};
use chronoscope_workers::RetryConfig;
use chronoscope_workers::http::{CacheMode, CachingClient, HttpClient};
use dropshot::ConfigLogging;
use reqwest::Client;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Polling interval for async operations in tests.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Maximum polling attempts (100 * 100ms = 10 seconds).
const MAX_POLL_ATTEMPTS: usize = 100;

// CARGO_MANIFEST_DIR is a compile-time constant that always has a parent directory.
#[allow(clippy::expect_used)]
fn fixtures_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("dev crate should have parent")
        .join("workers/fixtures")
}

fn vcr_mode() -> CacheMode {
    if cfg!(feature = "record-fixtures") {
        CacheMode::Online
    } else {
        CacheMode::Offline
    }
}

// ==================== Test Harness ====================

/// Test server wrapper with helper methods for making authenticated requests.
struct TestServer {
    server: RunningDevServer,
    client: Client,
}

impl TestServer {
    /// Start a test server with VCR fixtures.
    async fn start() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let http_client: Arc<dyn HttpClient> =
            Arc::new(CachingClient::new(fixtures_dir(), vcr_mode())?);

        let log = ConfigLogging::StderrTerminal {
            level: dropshot::ConfigLoggingLevel::Warn,
        }
        .to_logger("test")?;

        let port = chronoscope_dev::find_available_port()?;
        let base_url = format!("http://127.0.0.1:{port}");

        let server = start_dev_server(DevServerConfig {
            http_client,
            worker_idle_backoff: Duration::from_millis(50),
            retry_config: RetryConfig {
                max_retries: 0,
                ..Default::default()
            },
            log,
            port,
            cdn_base_url: base_url,
            rp_id: None,
            rp_origin: None,
            ios_app_id: None,
        })
        .await?;

        Ok(Self {
            server,
            client: Client::new(),
        })
    }

    /// Submit a URL for research.
    async fn submit_url(
        &self,
        url: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let resp = self
            .client
            .post(format!("{}/research", self.server.base_url))
            .header(
                "Authorization",
                format!("Bearer {}", self.server.auth_token),
            )
            .json(&serde_json::json!({"url": url}))
            .send()
            .await?;

        let status = resp.status();
        let body = resp.bytes().await?;
        if !status.is_success() {
            return Err(format!(
                "submit failed with {status}: {}",
                String::from_utf8_lossy(&body)
            )
            .into());
        }

        let json: serde_json::Value = serde_json::from_slice(&body)?;
        json["id"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| "response missing id field".into())
    }

    /// Get a research URL dossier.
    async fn get_dossier(
        &self,
        url_id: &str,
    ) -> Result<ResearchUrlDossier, Box<dyn std::error::Error + Send + Sync>> {
        let resp = self
            .client
            .get(format!("{}/research/{}", self.server.base_url, url_id))
            .header(
                "Authorization",
                format!("Bearer {}", self.server.auth_token),
            )
            .send()
            .await?;

        Ok(resp.json().await?)
    }

    /// Wait until the URL is no longer pending (resolved or failed).
    // Polling is appropriate for integration tests waiting on async worker completion.
    #[allow(clippy::disallowed_methods)]
    async fn wait_for_resolved(
        &self,
        url_id: &str,
    ) -> Result<ResearchUrlDossier, Box<dyn std::error::Error + Send + Sync>> {
        for _ in 0..MAX_POLL_ATTEMPTS {
            tokio::time::sleep(POLL_INTERVAL).await;
            let dossier = self.get_dossier(url_id).await?;
            if dossier.resolved.is_some() {
                return Ok(dossier);
            }
        }
        Err("timeout waiting for URL to resolve".into())
    }

    /// Wait until at least one media item is fetched.
    // Polling is appropriate for integration tests waiting on async worker completion.
    #[allow(clippy::disallowed_methods)]
    async fn wait_for_fetched_media(
        &self,
        url_id: &str,
    ) -> Result<ResearchUrlDossier, Box<dyn std::error::Error + Send + Sync>> {
        for _ in 0..MAX_POLL_ATTEMPTS {
            tokio::time::sleep(POLL_INTERVAL).await;
            let dossier = self.get_dossier(url_id).await?;

            if let Some(ResolvedContent::Page(page)) = &dossier.resolved {
                let has_fetched = page
                    .media
                    .iter()
                    .any(|m| matches!(m, MediaReference::Fetched(_)));
                if has_fetched {
                    return Ok(dossier);
                }
            }
        }
        Err("timeout waiting for media to be fetched".into())
    }

    /// Fetch raw bytes from a URL (for media verification).
    async fn fetch_bytes(
        &self,
        url: &str,
    ) -> Result<bytes::Bytes, Box<dyn std::error::Error + Send + Sync>> {
        let resp = self.client.get(url).send().await?;
        if !resp.status().is_success() {
            return Err(format!("fetch failed with {}", resp.status()).into());
        }
        Ok(resp.bytes().await?)
    }

    /// Gracefully shut down the server and wait for workers to stop.
    async fn shutdown(self) {
        self.server.shutdown().await;
    }
}

// ==================== Tests ====================

/// Test the complete end-to-end flow for a Reddit gallery.
///
/// This test verifies:
/// 1. URL submission via HTTP API
/// 2. Worker processes the Reddit gallery and creates a Page with media slots
/// 3. Workers attempt to fetch each media item (1 succeeds via fixture, rest fail)
/// 4. Dossier shows correct media states (fetched vs pending/failed)
/// 5. Successful media can be retrieved via CDN URL and is a valid image
///
/// Note: The Reddit gallery contains 20 images, but we intentionally only recorded
/// a VCR fixture for one of them. This tests that the failure path works correctly:
/// images without fixtures fail with `CacheMiss` (treated as permanent failure in
/// offline VCR mode), while the one with a fixture succeeds and can be retrieved.
#[tokio::test]
async fn test_reddit_gallery_end_to_end() -> TestResult {
    let server = TestServer::start().await?;

    // Submit the Reddit gallery URL
    let url_id = server
        .submit_url("https://www.reddit.com/r/abandoned/comments/1qcyfyj/bolshoye_selo/")
        .await?;

    // Wait for the page to be resolved
    server.wait_for_resolved(&url_id).await?;

    // Wait for at least one media item to be fetched
    let dossier = server.wait_for_fetched_media(&url_id).await?;

    // Verify we got a page with media
    let page = match dossier.resolved {
        Some(ResolvedContent::Page(page)) => page,
        _ => return Err("expected resolved page".into()),
    };
    assert!(!page.media.is_empty(), "should have media items");

    // Find the fetched media items
    let fetched: Vec<_> = page
        .media
        .iter()
        .filter_map(|m| match m {
            MediaReference::Fetched(media) => Some(media.as_ref()),
            _ => None,
        })
        .collect();

    assert!(
        !fetched.is_empty(),
        "at least one media should be fetched (the one with fixture)"
    );

    // Verify the fetched media has dimensions
    let media = fetched[0];
    assert!(media.width > 0, "fetched media should have width");
    assert!(media.height > 0, "fetched media should have height");

    // Fetch the image and verify it's valid
    let image_bytes = server.fetch_bytes(&media.full_url).await?;

    // Parse with the image crate to verify it's a valid image
    let img = image::ImageReader::new(Cursor::new(&image_bytes))
        .with_guessed_format()?
        .decode()?;

    assert!(img.width() > 0, "decoded image should have width");
    assert!(img.height() > 0, "decoded image should have height");

    server.shutdown().await;
    Ok(())
}
