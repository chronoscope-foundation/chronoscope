//! Wikimedia Commons integration.
//!
//! URL generation and gallery fetching for Commons images.

use anyhow::Result;
use md5::{Digest, Md5};
use reqwest_middleware::ClientWithMiddleware;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use url::Url;
use urlencoding::encode as urlencode;

use crate::wikidata::build_client;

/// Timeout for Commons API requests (30 seconds).
const COMMONS_TIMEOUT: Duration = Duration::from_secs(30);

/// Default concurrency limit for API requests.
const DEFAULT_CONCURRENCY: usize = 20;

/// Maximum response size for API requests (1 MB).
const MAX_RESPONSE_SIZE: usize = 1024 * 1024;

// =============================================================================
// RATE-LIMITED CLIENT
// =============================================================================

/// HTTP client with built-in concurrency limiting.
#[derive(Clone)]
pub struct RateLimitedClient {
    inner: ClientWithMiddleware,
    semaphore: Arc<Semaphore>,
}

impl RateLimitedClient {
    /// Create a new rate-limited client with the given concurrency limit.
    ///
    /// # Errors
    /// Returns an error if the HTTP client cannot be initialized.
    pub fn new(concurrency: usize) -> Result<Self> {
        Ok(Self {
            inner: build_client(COMMONS_TIMEOUT)?,
            semaphore: Arc::new(Semaphore::new(concurrency)),
        })
    }

    /// Create a new rate-limited client with default concurrency.
    ///
    /// # Errors
    /// Returns an error if the HTTP client cannot be initialized.
    pub fn with_defaults() -> Result<Self> {
        Self::new(DEFAULT_CONCURRENCY)
    }

    /// Fetch images from a single Commons gallery.
    ///
    /// Returns the list of image filenames in the gallery, filtered to common image types.
    ///
    /// # Errors
    /// Returns an error if the API request fails, the response is too large, or cannot be parsed.
    pub async fn fetch_gallery(&self, gallery_name: &str) -> Result<Vec<String>> {
        let _permit = self.semaphore.acquire().await?;

        let url = format!(
            "https://commons.wikimedia.org/w/api.php?action=parse&page={}&prop=images&format=json",
            urlencode(&gallery_name.replace(' ', "_"))
        );

        let response = self.inner.get(&url).send().await?;

        // Check content-length header if available (early rejection)
        if let Some(content_length) = response.content_length()
            && content_length > MAX_RESPONSE_SIZE as u64
        {
            anyhow::bail!("Response too large: {content_length} bytes (max {MAX_RESPONSE_SIZE})");
        }

        // Stream the response with size limit using take() to avoid buffering oversized responses
        use futures::TryStreamExt;
        use tokio::io::AsyncReadExt;
        use tokio_util::io::StreamReader;

        let content_length = response.content_length();
        let stream = response.bytes_stream().map_err(std::io::Error::other);
        let reader = StreamReader::new(stream);
        let mut limited_reader = reader.take(MAX_RESPONSE_SIZE as u64);

        let mut body = Vec::with_capacity(
            content_length
                .map(|len| (len as usize).min(MAX_RESPONSE_SIZE))
                .unwrap_or(0),
        );
        limited_reader.read_to_end(&mut body).await?;

        let body = String::from_utf8(body)?;
        let json: Value = serde_json::from_str(&body)?;

        let images = json
            .get("parse")
            .and_then(|p| p.get("images"))
            .and_then(|i| i.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|img| img.as_str())
                    .filter(|s| {
                        let lower = s.to_lowercase();
                        // Note: this list intentionally differs from workers/url_fetcher/content.rs
                        // ImageFormat. Commons galleries include vector (svg) and archival (tif/tiff)
                        // formats that the URL fetcher's image pipeline doesn't process.
                        matches!(
                            lower.rsplit('.').next(),
                            Some(
                                "jpg"
                                    | "jpeg"
                                    | "png"
                                    | "gif"
                                    | "svg"
                                    | "tif"
                                    | "tiff"
                                    | "webp"
                                    | "avif",
                            )
                        )
                    })
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();

        Ok(images)
    }
}

/// Generate Wikimedia Commons URL from filename.
///
/// Commons URLs use an MD5 hash of the filename to determine the path.
///
/// # Errors
/// Returns an error if the generated URL cannot be parsed.
pub fn url_for_filename(filename: &str) -> Result<Url, url::ParseError> {
    let filename_underscored = filename.replace(' ', "_");
    let mut hasher = Md5::new();
    hasher.update(filename_underscored.as_bytes());
    let hash = hasher.finalize();
    let hash_hex = format!("{:x}", hash);

    let url_str = format!(
        "https://upload.wikimedia.org/wikipedia/commons/{}/{}/{}",
        &hash_hex[0..1],
        &hash_hex[0..2],
        urlencode(&filename_underscored)
    );

    Url::parse(&url_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn test_url_spaces_become_underscores() -> TestResult {
        let url = url_for_filename("My Photo.jpg")?;
        // Spaces become underscores in the path (URL-encoded)
        let path = url.path();
        assert!(path.contains("My_Photo.jpg") || path.contains("My%20Photo.jpg"));
        // Should not contain raw space in the full URL
        assert!(!url.as_str().contains(' '));
        Ok(())
    }

    #[test]
    fn test_url_md5_path_structure() -> TestResult {
        // The URL should have /a/ab/ structure from MD5 hash
        let url = url_for_filename("Test.jpg")?;
        let segments: Vec<_> = url
            .path_segments()
            .ok_or("URL should have path segments")?
            .collect();
        // wikipedia / commons / <first char> / <first two chars> / filename
        let commons_idx = segments
            .iter()
            .position(|&p| p == "commons")
            .ok_or("should have 'commons' segment")?;
        assert_eq!(segments[commons_idx + 1].len(), 1); // single char
        assert_eq!(segments[commons_idx + 2].len(), 2); // two chars
        assert!(segments[commons_idx + 2].starts_with(segments[commons_idx + 1]));
        Ok(())
    }

    // =============================================================================
    // url_for_filename Property Tests
    // =============================================================================

    proptest! {
        #[test]
        fn prop_url_always_valid_format(filename in "[a-zA-Z0-9_]+\\.(jpg|png|gif)") {
            let url = url_for_filename(&filename)
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            prop_assert_eq!(url.host_str(), Some("upload.wikimedia.org"));
            prop_assert!(url.path().starts_with("/wikipedia/commons/"));
        }

        #[test]
        fn prop_url_is_deterministic(filename in "[a-zA-Z0-9_ ]+\\.[a-z]{3}") {
            let url1 = url_for_filename(&filename)
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            let url2 = url_for_filename(&filename)
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            prop_assert_eq!(url1, url2);
        }

        #[test]
        fn prop_url_no_spaces(filename in "[a-zA-Z0-9 ]+\\.jpg") {
            let url = url_for_filename(&filename)
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            prop_assert!(!url.as_str().contains(' '), "URL should not contain raw spaces");
            prop_assert!(!url.as_str().contains("%20"), "URL should not contain urlencoded spaces (spaces become underscores)");
        }

        #[test]
        fn prop_url_path_hash_consistency(filename in "[a-zA-Z0-9_]+\\.jpg") {
            let url = url_for_filename(&filename)
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            let segments: Vec<_> = url.path_segments()
                .ok_or_else(|| TestCaseError::fail("no path segments".to_string()))?
                .collect();
            let commons_idx = segments.iter().position(|&p| p == "commons")
                .ok_or_else(|| TestCaseError::fail("no 'commons' segment".to_string()))?;
            let first_char = segments[commons_idx + 1];
            let two_chars = segments[commons_idx + 2];
            // The two-char path should start with the single char
            prop_assert!(two_chars.starts_with(first_char),
                "Path structure should be /X/XY/ but got /{}/{}/", first_char, two_chars);
        }
    }
}
