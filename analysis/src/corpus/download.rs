//! Image downloading for corpus test suites.
//!
//! Downloads images from direct URLs and Reddit galleries, caching them on disk
//! to avoid re-downloading on subsequent runs. Reddit gallery entries sharing
//! the same post URL are grouped so each post is fetched only once.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tracing::{info, warn};
use url::Url;

use chronoscope_integrations::{HttpClient, HttpRequest, RedditIntegration, SingleFetcher};

use super::CorpusError;
use super::manifest::{CorpusManifest, ImageEntry};

/// Recognized image file extensions for corpus images.
const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "tif", "tiff"];

/// Downloads corpus images to disk.
///
/// Groups Reddit gallery entries by post URL so each Reddit post is fetched
/// at most once. Tracks rate-limit backoff per host across requests.
pub struct ImageDownloader {
    http: Arc<dyn HttpClient>,
    image_dir: PathBuf,
    /// Per-host backoff delay carried forward from previous 429 responses.
    backoff: Mutex<HashMap<String, Duration>>,
}

impl ImageDownloader {
    /// Create a new downloader that saves images to `image_dir`.
    pub fn new(http: Arc<dyn HttpClient>, image_dir: PathBuf) -> Self {
        Self {
            http,
            image_dir,
            backoff: Mutex::new(HashMap::new()),
        }
    }

    /// Download all images from a manifest, returning an ID-to-path map.
    ///
    /// Groups Reddit entries by URL to fetch each post once, then downloads
    /// individual media items. Direct entries are downloaded individually.
    pub async fn download_all(
        &self,
        manifest: &CorpusManifest,
    ) -> Result<BTreeMap<String, PathBuf>, CorpusError> {
        tokio::fs::create_dir_all(&self.image_dir)
            .await
            .map_err(CorpusError::Io)?;

        // Separate direct and Reddit entries, grouping Reddit by post URL.
        let mut reddit_groups: BTreeMap<&str, Vec<(&str, usize)>> = BTreeMap::new();
        let mut direct_entries: Vec<(&str, &str)> = Vec::new();

        for (id, entry) in &manifest.images {
            let (url, reddit_index) = match entry {
                ImageEntry::Single {
                    url, reddit_index, ..
                }
                | ImageEntry::Composite {
                    url, reddit_index, ..
                } => (url.as_str(), *reddit_index),
            };
            if let Some(index) = reddit_index {
                reddit_groups
                    .entry(url)
                    .or_default()
                    .push((id.as_str(), index));
            } else {
                direct_entries.push((id.as_str(), url));
            }
        }

        let mut all = BTreeMap::new();

        // Download direct images.
        for (id, url) in &direct_entries {
            let (img_id, path) = self.download_direct(id, url).await?;
            all.insert(img_id, path);
        }

        // Download Reddit gallery images (one fetch per unique post URL).
        for (reddit_url, entries) in &reddit_groups {
            let imgs = self.download_reddit_group(reddit_url, entries).await?;
            all.extend(imgs);
        }

        Ok(all)
    }

    /// Download a single direct image, returning `(id, path)` cached or freshly downloaded.
    async fn download_direct(
        &self,
        id: &str,
        entry_url: &str,
    ) -> Result<(String, PathBuf), CorpusError> {
        let url = Url::parse(entry_url)
            .map_err(|e| CorpusError::Download(format!("invalid URL {entry_url}: {e}")))?;

        let ext = image_extension(&url);
        let filename = format!("{id}.{ext}");
        let path = self.image_dir.join(filename);

        // Skip if already downloaded.
        if path.exists() {
            info!(%id, "image already on disk, skipping download");
            return Ok((id.to_string(), path));
        }

        info!(%id, url = %entry_url, "downloading image");
        let bytes = self.fetch_bytes(&url).await?;

        tokio::fs::write(&path, &bytes)
            .await
            .map_err(CorpusError::Io)?;

        Ok((id.to_string(), path))
    }

    /// Download images from a single Reddit gallery post.
    ///
    /// `entries` contains `(manifest_id, reddit_media_index)` pairs that all
    /// reference the same Reddit post URL. The post is fetched once and each
    /// entry is matched to its media item by index.
    async fn download_reddit_group(
        &self,
        reddit_url: &str,
        entries: &[(&str, usize)],
    ) -> Result<BTreeMap<String, PathBuf>, CorpusError> {
        // Check if all entries are already cached.
        let mut result = BTreeMap::new();
        let mut missing = Vec::new();
        for &(id, index) in entries {
            if let Some(path) = self.find_cached_image(id) {
                result.insert(id.to_string(), path);
            } else {
                missing.push((id, index));
            }
        }

        if missing.is_empty() {
            info!(url = %reddit_url, count = result.len(), "Reddit images already on disk");
            return Ok(result);
        }

        // Fetch the Reddit post to discover media URLs.
        let url = Url::parse(reddit_url)
            .map_err(|e| CorpusError::Download(format!("invalid URL {reddit_url}: {e}")))?;

        let reddit = RedditIntegration::new();
        let content = reddit.fetch(self.http.as_ref(), &url).await.map_err(|e| {
            CorpusError::Download(format!("Reddit fetch failed for {reddit_url}: {e}"))
        })?;

        if content.media.is_empty() {
            return Err(CorpusError::Download(format!(
                "no media URLs found in Reddit post: {reddit_url}"
            )));
        }

        // Download missing images.
        for (id, index) in &missing {
            let media_url = content.media.get(*index).ok_or_else(|| {
                CorpusError::Download(format!(
                    "{id}: reddit_index {index} out of range (post has {} media items)",
                    content.media.len()
                ))
            })?;

            let ext = image_extension(media_url);
            let path = self.image_dir.join(format!("{id}.{ext}"));

            info!(%id, index, url = %media_url, "downloading Reddit image");
            let bytes = self.fetch_bytes(media_url).await?;

            tokio::fs::write(&path, &bytes)
                .await
                .map_err(CorpusError::Io)?;

            result.insert(id.to_string(), path);
        }

        Ok(result)
    }

    /// Find a cached image file for the given entry ID.
    ///
    /// Checks for `{id}.{ext}` in the image directory for each known extension.
    fn find_cached_image(&self, id: &str) -> Option<PathBuf> {
        for ext in IMAGE_EXTENSIONS {
            let path = self.image_dir.join(format!("{id}.{ext}"));
            if path.exists() {
                return Some(path);
            }
        }
        None
    }

    async fn fetch_bytes(&self, url: &Url) -> Result<Vec<u8>, CorpusError> {
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

        let mut delay = Duration::from_secs(1);
        let max_retries = 5;
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
                let wait = response
                    .headers
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(Duration::from_secs)
                    .unwrap_or(delay);

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

/// Extract image file extension from a URL, defaulting to `"jpg"`.
///
/// Only recognizes known image extensions; anything else falls back to `"jpg"`.
fn image_extension(url: &Url) -> &str {
    let filename = url.path().rsplit('/').next().unwrap_or("");
    filename
        .rsplit('.')
        .next()
        .filter(|ext| IMAGE_EXTENSIONS.contains(ext))
        .unwrap_or("jpg")
}
