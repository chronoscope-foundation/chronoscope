//! Instagram-specific integration via Apify.
//!
//! Fetches Instagram posts using Apify's Instagram Scraper actor. This is a batch
//! fetcher because Apify charges per run, so batching multiple URLs into a single
//! run is more cost-effective.
//!
//! # Architecture
//!
//! The Apify workflow is:
//! 1. POST to `/v2/acts/{actor_id}/runs` with a list of URLs
//! 2. Poll GET `/v2/actor-runs/{run_id}` until status is `SUCCEEDED` or `FAILED`
//! 3. GET `/v2/datasets/{dataset_id}/items` to retrieve the scraped data
//!
//! # Testing
//!
//! Run with VCR recording: `APIFY_API_TOKEN=xxx cargo test -p chronoscope-integrations --features record-fixtures`

use std::time::Duration;

use chrono::{TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument, warn};
use url::Url;

use crate::content::FetchedContent;
use crate::http::{HttpClient, HttpRequest};
use crate::{BatchFetcher, FetchError, IntegrationMeta, IntegrationName};

/// Actor ID for the Apify Instagram scraper.
const INSTAGRAM_ACTOR_ID: &str = "apify/instagram-scraper";

/// Configuration for the Apify API.
#[derive(Debug, Clone)]
pub struct ApifyConfig {
    /// Apify API token.
    pub api_token: String,

    /// How often to poll for run completion.
    pub poll_interval: Duration,

    /// Maximum time to wait for a run to complete.
    pub max_wait: Duration,
}

impl ApifyConfig {
    /// Create a new Apify configuration with the given API token.
    #[must_use]
    pub fn new(api_token: String) -> Self {
        Self {
            api_token,
            poll_interval: Duration::from_secs(5),
            max_wait: Duration::from_mins(5),
        }
    }
}

/// Instagram-specific integration via Apify.
pub struct InstagramIntegration {
    config: Option<ApifyConfig>,
}

impl InstagramIntegration {
    /// Create a new Instagram integration with the given Apify configuration.
    ///
    /// If `config` is `None`, the integration will still normalize URLs but
    /// will not be able to fetch content (all fetches will return `NotFound`).
    #[must_use]
    pub fn new(config: Option<ApifyConfig>) -> Self {
        Self { config }
    }
}

impl IntegrationMeta for InstagramIntegration {
    fn name(&self) -> IntegrationName {
        IntegrationName::Instagram
    }

    fn domains(&self) -> &'static [&'static str] {
        &["instagram.com", "instagr.am"]
    }

    fn normalize_url(&self, url: &Url) -> Url {
        let mut url = url.clone();

        // Normalize www subdomain
        if let Some(host) = url.host_str()
            && host == "www.instagram.com"
        {
            let _ = url.set_host(Some("instagram.com"));
        }

        // Instagram URLs don't need query strings; they only contain tracking parameters
        url.set_query(None);

        url
    }
}

#[async_trait::async_trait]
impl BatchFetcher for InstagramIntegration {
    #[instrument(skip(self, http), fields(url_count = urls.len()))]
    async fn fetch_batch(
        &self,
        http: &dyn HttpClient,
        urls: &[Url],
    ) -> Vec<(Url, Result<FetchedContent, FetchError>)> {
        if urls.is_empty() {
            return Vec::new();
        }

        let Some(config) = &self.config else {
            // No config means we can't fetch - return NotFound for all URLs
            return urls
                .iter()
                .map(|url| (url.clone(), Err(FetchError::NotFound)))
                .collect();
        };

        match Self::run_apify_pipeline(http, config, urls).await {
            Ok(posts) => Self::match_results_to_urls(urls, &posts),
            Err(e) => {
                let error_msg = e.to_string();
                warn!(error = %error_msg, "Apify pipeline failed");
                urls.iter()
                    .map(|url| (url.clone(), Err(FetchError::service(error_msg.clone()))))
                    .collect()
            }
        }
    }
}

impl InstagramIntegration {
    /// Run the full Apify pipeline: start run, poll until complete, fetch results.
    async fn run_apify_pipeline(
        http: &dyn HttpClient,
        config: &ApifyConfig,
        urls: &[Url],
    ) -> Result<Vec<ApifyPost>, FetchError> {
        let run_id = Self::start_run(http, config, urls).await?;
        debug!(run_id = %run_id, "Started Apify run");

        let dataset_id = Self::poll_until_complete(http, config, &run_id).await?;
        debug!(run_id = %run_id, dataset_id = %dataset_id, "Apify run completed");

        let posts = Self::fetch_results(http, config, &dataset_id).await?;
        debug!(post_count = posts.len(), "Fetched Instagram posts");

        Ok(posts)
    }

    /// Create the Bearer authorization header for Apify API requests.
    fn auth_header(config: &ApifyConfig) -> Result<reqwest::header::HeaderValue, FetchError> {
        let auth_value = format!("Bearer {}", config.api_token);
        reqwest::header::HeaderValue::from_str(&auth_value)
            .map_err(|e| FetchError::ParseError(format!("invalid auth header: {e}")))
    }

    /// Start an Apify actor run with the given URLs.
    async fn start_run(
        http: &dyn HttpClient,
        config: &ApifyConfig,
        urls: &[Url],
    ) -> Result<String, FetchError> {
        let encoded_actor_id = urlencoding::encode(INSTAGRAM_ACTOR_ID);
        let api_url = format!("https://api.apify.com/v2/acts/{encoded_actor_id}/runs");

        let input = ApifyInput {
            direct_urls: urls.iter().map(ToString::to_string).collect(),
            results_limit: 1, // We only want the specific posts, not feeds
        };

        let body = serde_json::to_vec(&input)
            .map_err(|e| FetchError::ParseError(format!("failed to serialize input: {e}")))?;

        let request = HttpRequest::post(
            Url::parse(&api_url)
                .map_err(|e| FetchError::ParseError(format!("invalid API URL: {e}")))?,
        )
        .header(reqwest::header::AUTHORIZATION, Self::auth_header(config)?)
        .json_body(body)
        .timeout(Duration::from_secs(30));

        let response = http
            .execute(request)
            .await
            .map_err(FetchError::from_http_error)?;

        FetchError::from_status(response.status)?;

        let run_response: ApifyRunResponse = serde_json::from_slice(&response.body)
            .map_err(|e| FetchError::ParseError(format!("invalid run response: {e}")))?;

        Ok(run_response.data.id)
    }

    /// Poll until the Apify run completes, returning the dataset ID.
    async fn poll_until_complete(
        http: &dyn HttpClient,
        config: &ApifyConfig,
        run_id: &str,
    ) -> Result<String, FetchError> {
        let api_url = format!("https://api.apify.com/v2/actor-runs/{run_id}");
        let url = Url::parse(&api_url)
            .map_err(|e| FetchError::ParseError(format!("invalid API URL: {e}")))?;

        let auth_header = Self::auth_header(config)?;
        let start = std::time::Instant::now();

        loop {
            if start.elapsed() > config.max_wait {
                return Err(FetchError::service(format!(
                    "Apify run timed out after {:?}",
                    config.max_wait
                )));
            }

            let request = HttpRequest::get(url.clone())
                .header(reqwest::header::AUTHORIZATION, auth_header.clone())
                .timeout(Duration::from_secs(30));

            let response = http
                .execute(request)
                .await
                .map_err(FetchError::from_http_error)?;

            FetchError::from_status(response.status)?;

            let run_status: ApifyRunStatusResponse = serde_json::from_slice(&response.body)
                .map_err(|e| FetchError::ParseError(format!("invalid status response: {e}")))?;

            match run_status.data.status.as_str() {
                "SUCCEEDED" => {
                    return Ok(run_status.data.default_dataset_id);
                }
                "FAILED" | "ABORTED" | "TIMED-OUT" => {
                    return Err(FetchError::service(format!(
                        "Apify run failed with status: {}",
                        run_status.data.status
                    )));
                }
                _ => {
                    // Still running, wait and poll again
                    #[allow(clippy::disallowed_methods)]
                    tokio::time::sleep(config.poll_interval).await;
                }
            }
        }
    }

    /// Fetch results from an Apify dataset.
    async fn fetch_results(
        http: &dyn HttpClient,
        config: &ApifyConfig,
        dataset_id: &str,
    ) -> Result<Vec<ApifyPost>, FetchError> {
        let api_url = format!("https://api.apify.com/v2/datasets/{dataset_id}/items");

        let url = Url::parse(&api_url)
            .map_err(|e| FetchError::ParseError(format!("invalid API URL: {e}")))?;

        let request = HttpRequest::get(url)
            .header(reqwest::header::AUTHORIZATION, Self::auth_header(config)?)
            .timeout(Duration::from_secs(60));

        let response = http
            .execute(request)
            .await
            .map_err(FetchError::from_http_error)?;

        FetchError::from_status(response.status)?;

        let posts: Vec<ApifyPost> = serde_json::from_slice(&response.body)
            .map_err(|e| FetchError::ParseError(format!("invalid dataset response: {e}")))?;

        Ok(posts)
    }

    /// Match Apify results back to input URLs.
    ///
    /// Apify returns posts in arbitrary order, so we need to match them back
    /// to the input URLs. Posts that couldn't be fetched will have no match.
    fn match_results_to_urls(
        urls: &[Url],
        posts: &[ApifyPost],
    ) -> Vec<(Url, Result<FetchedContent, FetchError>)> {
        let mut results: Vec<(Url, Result<FetchedContent, FetchError>)> =
            Vec::with_capacity(urls.len());

        for url in urls {
            // Try to find a matching post by URL
            let matching_post = posts.iter().find(|post| {
                // Normalize both URLs for comparison
                if let Ok(post_url) = Url::parse(&post.url) {
                    Self::urls_match(url, &post_url)
                } else {
                    warn!(post_url = %post.url, "Failed to parse post URL");
                    false
                }
            });

            let result = if let Some(post) = matching_post {
                Ok(Self::post_to_content(post))
            } else {
                warn!(
                    input_url = %url,
                    post_count = posts.len(),
                    "No matching post found for input URL"
                );
                Err(FetchError::NotFound)
            };

            results.push((url.clone(), result));
        }

        results
    }

    /// Check if two Instagram URLs refer to the same post.
    fn urls_match(url1: &Url, url2: &Url) -> bool {
        // Extract the post shortcode from the URL path
        // Instagram URLs look like: /p/{shortcode}/ or /reel/{shortcode}/
        let shortcode1 = Self::extract_shortcode(url1);
        let shortcode2 = Self::extract_shortcode(url2);

        match (shortcode1, shortcode2) {
            (Some(s1), Some(s2)) => s1 == s2,
            _ => false,
        }
    }

    /// Extract the shortcode from an Instagram URL.
    fn extract_shortcode(url: &Url) -> Option<String> {
        let path = url.path();
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        // Look for /p/{shortcode} or /reel/{shortcode} or /tv/{shortcode}
        if segments.len() >= 2 {
            match segments[0] {
                "p" | "reel" | "tv" => Some(segments[1].to_string()),
                _ => None,
            }
        } else {
            None
        }
    }

    /// Convert an Apify post to `FetchedContent`.
    fn post_to_content(post: &ApifyPost) -> FetchedContent {
        // Parse timestamp
        let published_at = post.timestamp.as_ref().and_then(|ts| {
            // Apify returns ISO 8601 timestamps
            chrono::DateTime::parse_from_rfc3339(ts)
                .ok()
                .map(|dt| dt.naive_utc())
                .or_else(|| {
                    // Try parsing as Unix timestamp (some scrapers return this)
                    ts.parse::<i64>()
                        .ok()
                        .and_then(|secs| Utc.timestamp_opt(secs, 0).single())
                        .map(|dt| dt.naive_utc())
                })
        });

        // Collect media URLs, deduplicating to avoid constraint violations
        let mut media = Vec::new();
        let mut seen_urls = std::collections::HashSet::new();

        let mut add_url = |url_str: &str| {
            if let Ok(url) = Url::parse(url_str)
                && seen_urls.insert(url.to_string())
            {
                media.push(url);
            }
        };

        // Add display URL (main image/video thumbnail)
        if let Some(ref display_url) = post.display_url {
            add_url(display_url);
        }

        // Add video URL if present
        if let Some(ref video_url) = post.video_url {
            add_url(video_url);
        }

        // Add carousel images if present
        if let Some(ref images) = post.images {
            for img in images {
                add_url(img);
            }
        }

        // Build content from caption and comments
        let content = {
            let caption_iter = post.caption.iter().cloned();
            let comments_iter = post.latest_comments.iter().flatten().map(|comment| {
                let username = comment.owner_username.as_deref().unwrap_or("anonymous");
                format!("@{username}: {}", comment.text)
            });

            let parts: Vec<String> = caption_iter.chain(comments_iter).collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n\n"))
            }
        };

        FetchedContent {
            title: None, // Instagram posts don't have titles
            author: post.owner_username.clone(),
            published_at,
            content,
            media,
            discovered_urls: Vec::new(),
        }
    }
}

// ==================== Apify API Types ====================

/// Input for the Apify Instagram scraper.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApifyInput {
    /// Direct URLs to scrape.
    direct_urls: Vec<String>,
    /// Limit results per URL (1 = just the post, no feed scrolling).
    results_limit: u32,
}

/// Response from starting an Apify run.
#[derive(Debug, Deserialize)]
struct ApifyRunResponse {
    data: ApifyRunData,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApifyRunData {
    id: String,
}

/// Response from polling an Apify run status.
#[derive(Debug, Deserialize)]
struct ApifyRunStatusResponse {
    data: ApifyRunStatusData,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApifyRunStatusData {
    status: String,
    default_dataset_id: String,
}

/// An Instagram post from Apify's dataset.
///
/// Most fields are Optional because the Apify API doesn't guarantee their presence
/// (edge cases like deleted users, private accounts, API changes). We handle None
/// values defensively downstream.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApifyPost {
    /// URL of the post.
    url: String,
    /// Post caption/text.
    caption: Option<String>,
    /// Owner's username.
    owner_username: Option<String>,
    /// Timestamp (ISO 8601 or Unix).
    timestamp: Option<String>,
    /// Main display URL (image or video thumbnail).
    display_url: Option<String>,
    /// Video URL if this is a video post.
    video_url: Option<String>,
    /// Carousel/album images.
    images: Option<Vec<String>>,
    /// Latest comments on the post.
    latest_comments: Option<Vec<ApifyComment>>,
}

/// A comment on an Instagram post.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApifyComment {
    /// Comment text.
    text: String,
    /// Username of the commenter.
    owner_username: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    // ==================== Unit Tests ====================

    #[test]
    fn test_normalize_url_removes_tracking() -> TestResult {
        let integration = InstagramIntegration::new(None);

        let url = Url::parse("https://www.instagram.com/p/ABC123/?igsh=xyz&utm_source=share")?;
        let normalized = integration.normalize_url(&url);

        assert_eq!(normalized.as_str(), "https://instagram.com/p/ABC123/");
        Ok(())
    }

    #[test]
    fn test_extract_shortcode_post() -> TestResult {
        let url = Url::parse("https://instagram.com/p/ABC123/")?;
        assert_eq!(
            InstagramIntegration::extract_shortcode(&url),
            Some("ABC123".to_string())
        );
        Ok(())
    }

    #[test]
    fn test_extract_shortcode_reel() -> TestResult {
        let url = Url::parse("https://instagram.com/reel/XYZ789/")?;
        assert_eq!(
            InstagramIntegration::extract_shortcode(&url),
            Some("XYZ789".to_string())
        );
        Ok(())
    }

    #[test]
    fn test_extract_shortcode_tv() -> TestResult {
        let url = Url::parse("https://instagram.com/tv/IGTV123/")?;
        assert_eq!(
            InstagramIntegration::extract_shortcode(&url),
            Some("IGTV123".to_string())
        );
        Ok(())
    }

    #[test]
    fn test_extract_shortcode_profile_returns_none() -> TestResult {
        let url = Url::parse("https://instagram.com/username/")?;
        assert_eq!(InstagramIntegration::extract_shortcode(&url), None);
        Ok(())
    }

    #[test]
    fn test_urls_match_same_shortcode() -> TestResult {
        let url1 = Url::parse("https://instagram.com/p/ABC123/")?;
        let url2 = Url::parse("https://www.instagram.com/p/ABC123/?igsh=xyz")?;

        assert!(InstagramIntegration::urls_match(&url1, &url2));
        Ok(())
    }

    #[test]
    fn test_urls_match_different_shortcode() -> TestResult {
        let url1 = Url::parse("https://instagram.com/p/ABC123/")?;
        let url2 = Url::parse("https://instagram.com/p/XYZ789/")?;

        assert!(!InstagramIntegration::urls_match(&url1, &url2));
        Ok(())
    }

    #[test]
    fn test_post_to_content() {
        let post = ApifyPost {
            url: "https://instagram.com/p/ABC123/".to_string(),
            caption: Some("Test caption".to_string()),
            owner_username: Some("testuser".to_string()),
            timestamp: Some("2024-01-15T12:00:00Z".to_string()),
            display_url: Some("https://example.com/image.jpg".to_string()),
            video_url: None,
            images: None,
            latest_comments: None,
        };

        let content = InstagramIntegration::post_to_content(&post);

        assert_eq!(content.title, None);
        assert_eq!(content.author, Some("testuser".to_string()));
        assert_eq!(content.content, Some("Test caption".to_string()));
        assert_eq!(content.media.len(), 1);
        assert!(content.published_at.is_some());
    }

    #[test]
    fn test_post_to_content_with_carousel() {
        let post = ApifyPost {
            url: "https://instagram.com/p/ABC123/".to_string(),
            caption: Some("Carousel post".to_string()),
            owner_username: Some("testuser".to_string()),
            timestamp: None,
            display_url: Some("https://example.com/thumb.jpg".to_string()),
            video_url: None,
            images: Some(vec![
                "https://example.com/img1.jpg".to_string(),
                "https://example.com/img2.jpg".to_string(),
            ]),
            latest_comments: None,
        };

        let content = InstagramIntegration::post_to_content(&post);

        // display_url + 2 carousel images
        assert_eq!(content.media.len(), 3);
    }

    #[test]
    fn test_post_to_content_with_video() {
        let post = ApifyPost {
            url: "https://instagram.com/reel/ABC123/".to_string(),
            caption: Some("Video post".to_string()),
            owner_username: Some("testuser".to_string()),
            timestamp: None,
            display_url: Some("https://example.com/thumb.jpg".to_string()),
            video_url: Some("https://example.com/video.mp4".to_string()),
            images: None,
            latest_comments: None,
        };

        let content = InstagramIntegration::post_to_content(&post);

        // display_url (thumbnail) + video_url
        assert_eq!(content.media.len(), 2);
    }

    #[test]
    fn test_match_results_partial_failure() -> TestResult {
        // Input: 3 URLs, but only 2 have matching posts
        let urls = vec![
            Url::parse("https://instagram.com/p/ABC123/")?,
            Url::parse("https://instagram.com/p/MISSING/")?, // No matching post
            Url::parse("https://instagram.com/p/XYZ789/")?,
        ];

        // Posts only contain ABC123 and XYZ789
        let posts = vec![
            ApifyPost {
                url: "https://instagram.com/p/ABC123/".to_string(),
                caption: Some("First post".to_string()),
                owner_username: None,
                timestamp: None,
                display_url: None,
                video_url: None,
                images: None,
                latest_comments: None,
            },
            ApifyPost {
                url: "https://instagram.com/p/XYZ789/".to_string(),
                caption: Some("Third post".to_string()),
                owner_username: None,
                timestamp: None,
                display_url: None,
                video_url: None,
                images: None,
                latest_comments: None,
            },
        ];

        let results = InstagramIntegration::match_results_to_urls(&urls, &posts);

        assert_eq!(results.len(), 3);

        // First URL should succeed
        assert!(results[0].1.is_ok());
        assert_eq!(
            results[0]
                .1
                .as_ref()
                .ok()
                .and_then(|c| c.content.as_deref()),
            Some("First post")
        );

        // Second URL should fail with NotFound
        assert!(matches!(results[1].1, Err(FetchError::NotFound)));

        // Third URL should succeed
        assert!(results[2].1.is_ok());
        assert_eq!(
            results[2]
                .1
                .as_ref()
                .ok()
                .and_then(|c| c.content.as_deref()),
            Some("Third post")
        );

        Ok(())
    }
}
