//! Image downloading with per-host rate limiting.
//!
//! Used by the `corpus-fetch` binary for Nix FOD downloads and hash generation.
//! Supports retry with `Retry-After` headers and exponential backoff.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tracing::{info, warn};
use url::Url;

use chronoscope_integrations::{HttpClient, HttpRequest, RedditIntegration, SingleFetcher};

use super::CorpusError;

/// Check if a URL is a Reddit post (not a direct image link like i.redd.it).
pub fn is_reddit_post(url: &Url) -> bool {
    let host = url.host_str().unwrap_or("unknown");
    (host == "reddit.com" || host.ends_with(".reddit.com")) && url.path().contains("/comments/")
}

/// Downloads images with per-host rate-limit tracking.
///
/// Tracks `Retry-After` headers and exponential backoff per host, carrying
/// delays forward across requests so subsequent URLs to the same host
/// pre-sleep before sending.
pub struct ImageDownloader {
    http: Arc<dyn HttpClient>,
    /// Per-host backoff delay carried forward from previous 429 responses.
    backoff: Mutex<HashMap<String, Duration>>,
}

impl ImageDownloader {
    /// Create a new downloader.
    pub fn new(http: Arc<dyn HttpClient>) -> Self {
        Self {
            http,
            backoff: Mutex::new(HashMap::new()),
        }
    }

    /// Download a URL to a directory in FOD format (numbered files).
    ///
    /// Reddit posts are resolved via `RedditIntegration` and each media item
    /// is saved as `0`, `1`, `2`, ... Direct URLs are saved as `0`.
    ///
    /// No file extensions — decoders read by magic bytes, and the
    /// deterministic naming keeps `corpus-hashes.json` simple.
    pub async fn download_fod(&self, url: &Url, output_dir: &Path) -> Result<(), CorpusError> {
        std::fs::create_dir_all(output_dir).map_err(CorpusError::Io)?;

        if is_reddit_post(url) {
            let reddit = RedditIntegration::new();
            let content = reddit.fetch(self.http.as_ref(), url).await.map_err(|e| {
                CorpusError::Download(format!("Reddit fetch failed for {url}: {e}"))
            })?;

            if content.media.is_empty() {
                return Err(CorpusError::Download(format!(
                    "no media found in Reddit post: {url}"
                )));
            }

            for (i, media_url) in content.media.iter().enumerate() {
                info!(url = %media_url, index = i, "downloading Reddit media item");
                let bytes = self.fetch_bytes(media_url).await?;
                std::fs::write(output_dir.join(i.to_string()), &bytes).map_err(CorpusError::Io)?;
            }
        } else {
            info!(%url, "downloading image");
            let bytes = self.fetch_bytes(url).await?;
            std::fs::write(output_dir.join("0"), &bytes).map_err(CorpusError::Io)?;
        }

        Ok(())
    }

    /// Fetch bytes from a URL with retry and per-host backoff.
    ///
    /// Respects `Retry-After` headers, uses exponential backoff, and carries
    /// the last successful wait duration forward so future requests to the
    /// same host pre-sleep.
    pub async fn fetch_bytes(&self, url: &Url) -> Result<Vec<u8>, CorpusError> {
        let host = url.host_str().unwrap_or("unknown").to_string();

        // Pre-sleep if a previous request to this host was rate-limited.
        {
            let backoff = self.backoff.lock().await;
            if let Some(&d) = backoff.get(&host)
                && !d.is_zero()
            {
                info!(%url, ?d, "pre-sleeping from previous rate limit");
                drop(backoff);
                #[allow(clippy::disallowed_methods)]
                tokio::time::sleep(d).await;
            }
        }

        let mut delay = Duration::from_secs(2);
        let max_retries = 10;
        let mut last_wait = Duration::ZERO;

        for attempt in 0..=max_retries {
            let request = HttpRequest::get(url.clone());
            let response = self
                .http
                .execute(request)
                .await
                .map_err(|e| CorpusError::Download(format!("HTTP error: {e}")))?;

            if response.is_success() {
                if !last_wait.is_zero() {
                    self.backoff.lock().await.insert(host, last_wait);
                }
                return Ok(response.body.to_vec());
            }

            if response.status.as_u16() == 429 && attempt < max_retries {
                // Use the greater of Retry-After header and our exponential backoff.
                // Servers like Wikimedia send Retry-After: 1 which is too short
                // when many parallel fetchers compete.
                let server_wait = response
                    .headers
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(Duration::from_secs);
                let wait = server_wait.map_or(delay, |s| s.max(delay));

                warn!(%url, attempt, ?wait, "rate limited, backing off");
                last_wait = wait;
                #[allow(clippy::disallowed_methods)]
                tokio::time::sleep(wait).await;
                delay *= 2;
                continue;
            }

            return Err(CorpusError::Download(format!(
                "HTTP {} for {url}",
                response.status
            )));
        }

        Err(CorpusError::Download(format!(
            "rate limited after {max_retries} retries for {url}"
        )))
    }
}
