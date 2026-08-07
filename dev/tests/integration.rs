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
use chronoscope_api::state::permissive_dns_resolver;
use chronoscope_api_client::Client;
use chronoscope_api_client::client::AuthClient;
use chronoscope_db::{MediaType, ResearchUrlStatus};
use chronoscope_dev::{
    DevServerConfig, FactsDbSource, ImageResolveMode, RunningDevServer, start_dev_server,
};
use chronoscope_workers::RetryConfig;
use chronoscope_workers::{ApifyConfig, CacheMode, CachingClient, HttpClient};
use dropshot::ConfigLogging;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Initialize tracing for integration tests. Respects `RUST_LOG` env var
/// (e.g. `RUST_LOG=debug`). Safe to call from multiple tests — only the
/// first call installs the subscriber.
fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_test_writer()
        .try_init();
}

/// "No worker progress at all" safety net. Tests drive their waits on
/// `Database::worker_progress`, so a worker making forward progress
/// never hits this; only a genuinely stuck pipeline does.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

fn http_fixtures_dir() -> Result<std::path::PathBuf, &'static str> {
    // HTTP fixtures are in the integrations crate
    Ok(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("dev crate should have parent")?
        .join("integrations/fixtures"))
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
///
/// Workers are automatically signaled to stop when this is dropped (via
/// `RunningDevServer`'s `Drop` impl). For clean shutdown in passing tests,
/// call `shutdown().await` explicitly to wait for workers to finish.
struct TestServer {
    server: RunningDevServer,
    auth: AuthClient,
    /// The tempdir holding the server's database file, kept alive for the
    /// harness's duration.
    _db_dir: tempfile::TempDir,
}

impl TestServer {
    /// Start a test server with VCR fixtures.
    async fn start() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::start_with_config(None).await
    }

    /// Start a test server with VCR fixtures and optional Apify config.
    async fn start_with_config(
        apify_config: Option<ApifyConfig>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        init_tracing();

        let http_client: Arc<dyn HttpClient> =
            Arc::new(CachingClient::new(http_fixtures_dir()?, vcr_mode())?);

        let log = ConfigLogging::StderrTerminal {
            level: dropshot::ConfigLoggingLevel::Warn,
        }
        .to_logger("test")?;

        let port = chronoscope_dev::find_available_port()?;
        let base_url = format!("http://127.0.0.1:{port}");

        // Fresh file-backed app + facts DBs per harness — the fact store's
        // views hold read transactions on the (empty) facts overlay, which an
        // in-memory overlay would serialize at table locks. These tests
        // exercise the URL-fetch pipeline, not fact data, so the facts overlay
        // is a fresh writable scratch file, created+migrated and left empty.
        let db_dir = tempfile::tempdir()?;
        let database_url = format!("sqlite:{}", db_dir.path().join("test.db").display());
        let facts_database = format!("sqlite:{}", db_dir.path().join("facts.db").display());

        let server = start_dev_server(DevServerConfig {
            database_url: Some(database_url),
            facts: FactsDbSource::Writable(facts_database),
            seed_commits: Vec::new(),
            http_client,
            worker_idle_backoff: Duration::from_millis(50),
            retry_config: RetryConfig {
                max_retries: 0,
                ..Default::default()
            },
            log,
            port,
            cdn_base_url: base_url.clone(),
            // These tests exercise the URL-fetch pipeline, not fact-store images
            // (the fact store starts empty here, so no images are resolved).
            image_resolve: ImageResolveMode::Placeholder,
            rp_id: None,
            rp_origin: None,
            ios_app_id: None,
            apify_config,
            dns_resolver: permissive_dns_resolver(),
        })
        .await?;

        // Disabling the idle connection pool avoids reusing a keep-alive
        // connection the local server has already closed (the IncompleteMessage race).
        let reqwest_client = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()?;
        let client = Client::with_reqwest(reqwest_client, base_url);
        let auth = AuthClient::new(client, server.auth_token.clone());

        Ok(Self {
            server,
            auth,
            _db_dir: db_dir,
        })
    }

    /// Submit a URL for research.
    async fn submit_url(
        &self,
        url: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let resp = self
            .auth
            .reqwest_client()
            .post(format!("{}/research", self.auth.base_url()))
            .bearer_auth(self.auth.token())
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
            .auth
            .reqwest_client()
            .get(format!("{}/research/{}", self.auth.base_url(), url_id))
            .bearer_auth(self.auth.token())
            .send()
            .await?;

        Ok(resp.json().await?)
    }

    /// Wait until a condition on the page's media is satisfied.
    async fn wait_for_media_condition(
        &self,
        url_id: &str,
        condition: impl Fn(&[MediaReference]) -> bool,
        timeout: Duration,
    ) -> Result<ResearchUrlDossier, Box<dyn std::error::Error + Send + Sync>> {
        await_on_worker_progress(self.server.db(), timeout, || async {
            let dossier = self.get_dossier(url_id).await?;
            if let Some(ResolvedContent::Page(page)) = &dossier.resolved
                && condition(&page.media)
            {
                Ok(Some(dossier))
            } else {
                Ok(None)
            }
        })
        .await
    }

    /// Fetch raw bytes from a URL (for media verification).
    async fn fetch_bytes(
        &self,
        url: &str,
    ) -> Result<bytes::Bytes, Box<dyn std::error::Error + Send + Sync>> {
        let resp = self.auth.reqwest_client().get(url).send().await?;
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

// ==================== Helpers ====================

/// Extract fetched media from a slice of `MediaReferences`.
fn fetched_media(media: &[MediaReference]) -> Vec<&chronoscope_api::research_types::MediaDossier> {
    media
        .iter()
        .filter_map(|m| match m {
            MediaReference::Fetched(media) => Some(media.as_ref()),
            MediaReference::Pending { .. } => None,
        })
        .collect()
}

/// Drive a poll loop on `Database::worker_progress` rather than wallclock
/// ticks. `check` runs on entry and after every progress event; return
/// `Ok(Some(t))` to break with `t`, `Ok(None)` to keep waiting, or `Err`
/// to bail. `timeout` is a true "no progress at all" safety net — a worker
/// making forward progress will never hit it.
async fn await_on_worker_progress<F, Fut, T>(
    db: &chronoscope_db::Database,
    timeout: Duration,
    mut check: F,
) -> Result<T, Box<dyn std::error::Error + Send + Sync>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Option<T>, Box<dyn std::error::Error + Send + Sync>>>,
{
    let mut progress = db.worker_progress();
    let deadline = tokio::time::Instant::now() + timeout;
    // Intentionally never call `borrow_and_update` / `mark_unchanged` on
    // `progress` — we want `changed()` to fire once per `send_modify` from
    // subscribe time onward, not just on value transitions.
    loop {
        if let Some(value) = check().await? {
            return Ok(value);
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "the no-progress timeout deadline this helper exists to enforce"
        )]
        let deadline_sleep = tokio::time::sleep_until(deadline);
        tokio::select! {
            res = progress.changed() => {
                res.map_err(|e| format!("worker_progress sender dropped: {e}"))?;
            }
            () = deadline_sleep => {
                return Err(format!(
                    "timeout after {timeout:?} (no worker progress)"
                ).into());
            }
        }
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
///
/// # Recording fixtures
///
/// To record/update fixtures:
/// ```bash
/// cargo test -p chronoscope-dev --features record-fixtures -- test_reddit_gallery
/// ```
#[tokio::test]
async fn test_reddit_gallery_end_to_end() -> TestResult {
    let server = TestServer::start().await?;

    // Submit the Reddit gallery URL.
    // Reddit is a good test source because URLs are nearly permanent (content is hard
    // to delete), posts are append-only, and this gallery is relevant to our domain
    // (historical/abandoned places).
    let url_id = server
        .submit_url("https://www.reddit.com/r/abandoned/comments/1qcyfyj/bolshoye_selo/")
        .await?;

    // Wait for the expected media to be fetched (1 image has a fixture, rest have pinned 404s)
    let dossier = server
        .wait_for_media_condition(
            &url_id,
            |media| {
                media
                    .iter()
                    .any(|m| matches!(m, MediaReference::Fetched(_)))
            },
            DEFAULT_TIMEOUT,
        )
        .await?;

    // Verify we got a page with media
    let Some(ResolvedContent::Page(page)) = dossier.resolved else {
        return Err("expected resolved page".into());
    };
    assert!(!page.media.is_empty(), "should have media items");

    // Find the fetched media items
    let fetched = fetched_media(&page.media);

    // Only one fixture is kept (first gallery image); others have pinned 404 failures
    assert_eq!(
        fetched.len(),
        1,
        "exactly one media should be fetched (others have pinned 404 fixtures)"
    );

    // Verify metadata dimensions (from fixture: 3504x2336)
    let media = fetched[0];
    assert_eq!(media.width, 3504, "media width should be 3504");
    assert_eq!(media.height, 2336, "media height should be 2336");

    // Fetch the full image and verify it's valid
    let image_bytes = server.fetch_bytes(&media.full_url).await?;

    // Parse with the image crate to verify it's a valid image
    let img = image::ImageReader::new(Cursor::new(&image_bytes))
        .with_guessed_format()?
        .decode()?;

    // Verify decoded dimensions match metadata
    assert_eq!(
        img.width(),
        media.width,
        "decoded width should match metadata"
    );
    assert_eq!(
        img.height(),
        media.height,
        "decoded height should match metadata"
    );

    // Fetch the thumbnail and verify it's valid
    let thumb_bytes = server.fetch_bytes(&media.thumbnail_url).await?;

    let thumb = image::ImageReader::new(Cursor::new(&thumb_bytes))
        .with_guessed_format()?
        .decode()?;

    // Thumbnail should be at most 512px on longest edge
    assert!(
        thumb.width() <= 512 && thumb.height() <= 512,
        "thumbnail should be at most 512px, got {}x{}",
        thumb.width(),
        thumb.height()
    );

    // Thumbnail should be smaller than or equal to original
    assert!(
        thumb.width() <= img.width() && thumb.height() <= img.height(),
        "thumbnail should not be larger than original"
    );

    server.shutdown().await;
    Ok(())
}

/// Test that URLs with no recorded fixtures fail gracefully.
///
/// This verifies the system handles complete fetch failures without panicking,
/// demonstrating the "graceful degradation" principle from the project vision.
#[tokio::test]
async fn test_url_with_no_fixtures_fails_gracefully() -> TestResult {
    let server = TestServer::start().await?;

    // Submit a URL that has no VCR fixtures recorded at all.
    // In offline VCR mode, this will fail to fetch the page itself.
    let url_id = server
        .submit_url("https://example.com/no-fixture-exists")
        .await?;

    // Failure ticks worker_progress via Queue::mark_failed, so we use the
    // same event-driven wait as success-path tests.
    await_on_worker_progress(server.server.db(), DEFAULT_TIMEOUT, || async {
        let dossier = server.get_dossier(&url_id).await?;
        if dossier.resolved.is_some() {
            return Err("expected fetch failure, but URL resolved successfully".into());
        }
        Ok((dossier.status == ResearchUrlStatus::Failed).then_some(()))
    })
    .await?;
    server.shutdown().await;
    Ok(())
}

/// Test that invalid URLs are rejected at submission time.
#[tokio::test]
async fn test_invalid_url_rejected() -> TestResult {
    let server = TestServer::start().await?;

    // Try to submit an invalid URL
    let result = server.submit_url("not-a-valid-url").await;

    // Should fail at submission time, not during processing
    assert!(result.is_err(), "invalid URL should be rejected");

    server.shutdown().await;
    Ok(())
}

/// Helper to get Apify config for Instagram tests.
///
/// In recording mode (with `record-fixtures` feature), uses the real API token
/// from the environment. In playback mode (default), uses a dummy token since
/// the token isn't part of the cache key (it's in the Authorization header).
fn apify_config_for_test() -> Option<ApifyConfig> {
    if cfg!(feature = "record-fixtures") {
        // Recording mode: need real token from environment
        // The test_with attribute ensures this env var exists
        std::env::var("APIFY_API_TOKEN").ok().map(ApifyConfig::new)
    } else {
        // Playback mode: use obvious dummy token
        // Cache keys don't include the Authorization header, so this works
        Some(ApifyConfig::new("EXAMPLE_KEY".to_string()))
    }
}

/// Test the complete end-to-end flow for an Instagram post via Apify.
///
/// This test verifies:
/// 1. URL submission via HTTP API
/// 2. Worker processes the Instagram post via Apify batch fetcher
/// 3. Post content is extracted correctly
///
/// # Running
///
/// In playback mode (default), this test uses cached VCR fixtures:
/// ```bash
/// cargo test -p chronoscope-dev -- test_instagram
/// ```
///
/// To record/update fixtures, set `APIFY_API_TOKEN` and enable the feature:
/// ```bash
/// APIFY_API_TOKEN=xxx cargo test -p chronoscope-dev --features record-fixtures -- test_instagram
/// ```
///
/// Without the token in recording mode, the test is skipped (not failed).
#[cfg_attr(feature = "record-fixtures", test_with::env(APIFY_API_TOKEN))]
#[tokio::test]
async fn test_instagram_post_end_to_end() -> TestResult {
    let apify_config =
        apify_config_for_test().ok_or("apify config should be available in playback mode")?;

    let server = TestServer::start_with_config(Some(apify_config)).await?;

    // Submit the Instagram post URL
    let url_id = server
        .submit_url("https://www.instagram.com/p/DP1Y0KYDCHh")
        .await?;

    // Wait for the expected media to be fetched (1 image has a fixture, rest have pinned 404s)
    let dossier = server
        .wait_for_media_condition(
            &url_id,
            |media| {
                media
                    .iter()
                    .any(|m| matches!(m, MediaReference::Fetched(_)))
            },
            DEFAULT_TIMEOUT,
        )
        .await?;

    // Verify we got a page
    let Some(ResolvedContent::Page(page)) = &dossier.resolved else {
        return Err("expected resolved page".into());
    };

    // Check for expected content from the post caption
    // Note: Using "elegant Italian villa" to avoid apostrophe encoding issues
    let content = page.content.as_deref().unwrap_or("");
    assert!(
        content.contains("elegant Italian villa"),
        "post caption should contain expected text, got: {content}"
    );

    // Check that comments are included
    assert!(
        content.contains("@oganiangallery: Amazing shots"),
        "content should include comments, got: {content}"
    );

    // Only one fixture is kept (smallest carousel image); others have pinned 404 failures
    let fetched = fetched_media(&page.media);

    assert_eq!(
        fetched.len(),
        1,
        "exactly one carousel image should be fetched (others have pinned 404 fixtures)"
    );

    // Verify the one fetched image
    let media = fetched[0];
    assert_eq!(media.media_type, MediaType::Image, "should be an image");
    assert_eq!(media.width, 1080, "image width should be 1080");
    assert_eq!(media.height, 1440, "image height should be 1440");

    // Fetch the image from CDN and verify it's valid
    let image_bytes = server.fetch_bytes(&media.full_url).await?;
    let img = image::ImageReader::new(Cursor::new(&image_bytes))
        .with_guessed_format()?
        .decode()?;

    // Verify decoded dimensions match the metadata
    assert_eq!(img.width(), media.width, "decoded width should match");
    assert_eq!(img.height(), media.height, "decoded height should match");

    server.shutdown().await;
    Ok(())
}

/// Test the complete end-to-end flow for an Instagram reel (video content) via Apify.
///
/// This test verifies:
/// 1. URL submission via HTTP API
/// 2. Worker processes the Instagram reel via Apify batch fetcher
/// 3. Reel content is extracted correctly (display URL thumbnail + video URL)
///
/// # Reel structure
///
/// Instagram reels have:
/// - `display_url`: A static thumbnail image (the video's cover frame)
/// - `video_url`: The actual video content
///
/// Both are fetched and stored. The thumbnail can be used for previews while the
/// full video is available for playback.
///
/// # Running
///
/// In playback mode (default), this test uses cached VCR fixtures:
/// ```bash
/// cargo test -p chronoscope-dev -- test_instagram_reel
/// ```
///
/// To record/update fixtures, set `APIFY_API_TOKEN` and enable the feature:
/// ```bash
/// APIFY_API_TOKEN=xxx cargo test -p chronoscope-dev --features record-fixtures -- test_instagram_reel
/// ```
///
/// Without the token in recording mode, the test is skipped (not failed).
#[cfg_attr(feature = "record-fixtures", test_with::env(APIFY_API_TOKEN))]
#[tokio::test]
async fn test_instagram_reel_end_to_end() -> TestResult {
    let apify_config =
        apify_config_for_test().ok_or("apify config should be available in playback mode")?;

    let server = TestServer::start_with_config(Some(apify_config)).await?;

    // Submit the Instagram reel URL
    let url_id = server
        .submit_url("https://www.instagram.com/reel/DTnX730ETHG/")
        .await?;

    // Wait for both media items to be fetched (thumbnail + video)
    // The video is ~22MB so this may take a moment
    let dossier = server
        .wait_for_media_condition(
            &url_id,
            |media| {
                media
                    .iter()
                    .filter(|m| matches!(m, MediaReference::Fetched(_)))
                    .count()
                    >= 2
            },
            DEFAULT_TIMEOUT,
        )
        .await?;

    // Verify we got a page
    let Some(ResolvedContent::Page(page)) = &dossier.resolved else {
        return Err("expected resolved page".into());
    };

    // Reels should have 2 media items: thumbnail (display_url) + video (video_url)
    let fetched = fetched_media(&page.media);

    assert_eq!(
        fetched.len(),
        2,
        "both thumbnail and video should be fetched"
    );

    // Find the image (thumbnail) and video by media type
    let thumbnail = fetched
        .iter()
        .find(|m| m.media_type == MediaType::Image)
        .ok_or("expected thumbnail image")?;
    let video = fetched
        .iter()
        .find(|m| m.media_type == MediaType::Video)
        .ok_or("expected video")?;

    // Verify thumbnail dimensions (from fixture)
    assert_eq!(thumbnail.width, 640, "thumbnail width should be 640");
    assert_eq!(thumbnail.height, 1136, "thumbnail height should be 1136");

    // Verify video metadata (from fixture)
    assert_eq!(video.width, 720, "video width should be 720");
    assert_eq!(video.height, 1280, "video height should be 1280");
    let duration = video.duration_seconds.ok_or("video should have duration")?;
    // Duration is ~56.6 seconds; check within a small tolerance
    assert!(
        (56.0..57.0).contains(&duration),
        "video duration should be ~56.6s, got {duration}"
    );

    // Fetch the thumbnail from CDN and verify it's a valid image
    let image_bytes = server.fetch_bytes(&thumbnail.full_url).await?;
    let img = image::ImageReader::new(Cursor::new(&image_bytes))
        .with_guessed_format()?
        .decode()?;

    // Verify decoded dimensions match the metadata
    assert_eq!(
        img.width(),
        thumbnail.width,
        "decoded thumbnail width should match metadata"
    );
    assert_eq!(
        img.height(),
        thumbnail.height,
        "decoded thumbnail height should match metadata"
    );

    server.shutdown().await;
    Ok(())
}
