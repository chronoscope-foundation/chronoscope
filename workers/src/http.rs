//! HTTP client abstraction for workers.
//!
//! This module provides an `HttpClient` trait that abstracts HTTP operations,
//! with implementations for production use and VCR-style caching for tests.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use reqwest::header::HeaderMap;
use reqwest::{Method, StatusCode};
use url::Url;

/// Error type for HTTP operations.
#[derive(Debug, Clone)]
pub enum HttpError {
    /// Network-level error (timeout, connection refused, etc.)
    Network(String),
    /// Request building error (invalid URL, headers, etc.)
    Request(String),
    /// Cache miss in offline mode
    CacheMiss { url: String },
    /// I/O error during cache operations
    CacheIo(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(msg) => write!(f, "network error: {msg}"),
            Self::Request(msg) => write!(f, "request error: {msg}"),
            Self::CacheMiss { url } => write!(f, "cache miss for {url} in offline mode"),
            Self::CacheIo(msg) => write!(f, "cache I/O error: {msg}"),
        }
    }
}

impl std::error::Error for HttpError {}

/// HTTP request to execute.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: Method,
    pub url: Url,
    pub headers: HeaderMap,
    pub body: Option<Bytes>,
    /// Request timeout (overrides client default if set)
    pub timeout: Option<Duration>,
}

impl HttpRequest {
    /// Create a simple GET request.
    #[must_use]
    pub fn get(url: Url) -> Self {
        Self {
            method: Method::GET,
            url,
            headers: HeaderMap::new(),
            body: None,
            timeout: None,
        }
    }

    /// Add a header to the request.
    #[must_use]
    pub fn header(
        mut self,
        name: impl Into<reqwest::header::HeaderName>,
        value: impl Into<reqwest::header::HeaderValue>,
    ) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    /// Set the request timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

/// HTTP response from a request.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
    /// Final URL after following redirects
    pub final_url: Url,
}

impl HttpResponse {
    /// Check if the response status indicates success (2xx).
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }
}

/// Trait for HTTP clients, allowing different implementations for production,
/// caching (VCR-style), and testing.
#[async_trait::async_trait]
pub trait HttpClient: Send + Sync {
    /// Execute an HTTP request and return the response.
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HttpError>;
}

// ==================== ReqwestClient (Production) ====================

/// Production HTTP client using reqwest.
pub struct ReqwestClient {
    client: reqwest::Client,
}

impl ReqwestClient {
    /// Create a new reqwest-based HTTP client.
    ///
    /// # Errors
    /// Returns an error if the client cannot be built (unlikely).
    pub fn new() -> Result<Self, HttpError> {
        Self::with_config(&ReqwestConfig::default())
    }

    /// Create a client with custom configuration.
    ///
    /// # Errors
    /// Returns an error if the client cannot be built.
    pub fn with_config(config: &ReqwestConfig) -> Result<Self, HttpError> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .connect_timeout(config.connect_timeout)
            .user_agent(&config.user_agent)
            .redirect(reqwest::redirect::Policy::limited(config.max_redirects))
            .build()
            .map_err(|e| HttpError::Request(e.to_string()))?;

        Ok(Self { client })
    }
}

/// Configuration for `ReqwestClient`.
pub struct ReqwestConfig {
    pub timeout: Duration,
    pub connect_timeout: Duration,
    pub user_agent: String,
    pub max_redirects: usize,
}

impl Default for ReqwestConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            user_agent: "Chronoscope/0.1".to_string(),
            max_redirects: 10,
        }
    }
}

#[async_trait::async_trait]
impl HttpClient for ReqwestClient {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let mut builder = self
            .client
            .request(request.method, request.url.as_str())
            .headers(request.headers);

        if let Some(body) = request.body {
            builder = builder.body(body);
        }

        if let Some(timeout) = request.timeout {
            builder = builder.timeout(timeout);
        }

        let response = builder.send().await.map_err(|e| {
            if e.is_timeout() {
                HttpError::Network(format!("request timed out: {e}"))
            } else if e.is_connect() {
                HttpError::Network(format!("connection failed: {e}"))
            } else {
                HttpError::Network(e.to_string())
            }
        })?;

        // Extract fields before .bytes() which consumes the response
        let final_url = response.url().clone();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .bytes()
            .await
            .map_err(|e| HttpError::Network(format!("failed to read body: {e}")))?;

        Ok(HttpResponse {
            status,
            headers,
            body,
            final_url,
        })
    }
}

// ==================== CachingClient (VCR-style) ====================

/// Cache mode for VCR-style HTTP caching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMode {
    /// Online: fetch from network, cache responses
    Online,
    /// Offline: only read from cache, error on miss
    Offline,
    /// Passthrough: always fetch from network, never cache
    Passthrough,
}

/// VCR-style caching HTTP client for integration tests.
///
/// In online mode, fetches from network and caches responses to disk.
/// In offline mode, reads from cache only (errors on cache miss).
/// Cache files should be gitignored during development but can be committed
/// for reproducible CI tests.
pub struct CachingClient {
    inner: ReqwestClient,
    cache_dir: PathBuf,
    mode: CacheMode,
}

impl CachingClient {
    /// Create a new caching client.
    ///
    /// # Errors
    /// Returns an error if the inner client cannot be created.
    pub fn new(cache_dir: PathBuf, mode: CacheMode) -> Result<Self, HttpError> {
        Ok(Self {
            inner: ReqwestClient::new()?,
            cache_dir,
            mode,
        })
    }

    /// Generate a cache key from a request.
    fn cache_key(request: &HttpRequest) -> String {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        hasher.update(request.method.as_str().as_bytes());
        hasher.update(b"|");
        hasher.update(request.url.as_str().as_bytes());
        let hash = hasher.finalize();
        hex::encode(&hash[..16]) // Use first 16 bytes (32 hex chars)
    }

    /// Get the cache file path for a request.
    fn cache_path(&self, request: &HttpRequest) -> PathBuf {
        let key = Self::cache_key(request);
        self.cache_dir.join(format!("{key}.json"))
    }

    /// Read a cached response from disk.
    async fn read_cache(&self, request: &HttpRequest) -> Result<Option<CachedResponse>, HttpError> {
        let path = self.cache_path(request);
        match tokio::fs::read(&path).await {
            Ok(data) => {
                let cached: CachedResponse = serde_json::from_slice(&data)
                    .map_err(|e| HttpError::CacheIo(format!("invalid cache file: {e}")))?;
                Ok(Some(cached))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(HttpError::CacheIo(e.to_string())),
        }
    }

    /// Write a response to the cache.
    async fn write_cache(
        &self,
        request: &HttpRequest,
        response: &HttpResponse,
    ) -> Result<(), HttpError> {
        // Ensure cache directory exists
        tokio::fs::create_dir_all(&self.cache_dir)
            .await
            .map_err(|e| HttpError::CacheIo(e.to_string()))?;

        let cached = CachedResponse::from_response(request, response);
        let data = serde_json::to_vec_pretty(&cached)
            .map_err(|e| HttpError::CacheIo(format!("failed to serialize: {e}")))?;

        let path = self.cache_path(request);
        tokio::fs::write(&path, data)
            .await
            .map_err(|e| HttpError::CacheIo(e.to_string()))?;

        Ok(())
    }
}

/// Serializable cached response.
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedResponse {
    url: String,
    method: String,
    status: u16,
    headers: HashMap<String, String>,
    #[serde(with = "base64_bytes")]
    body: Vec<u8>,
    final_url: String,
}

impl CachedResponse {
    fn from_response(request: &HttpRequest, response: &HttpResponse) -> Self {
        // HeaderValue::to_str() fails for non-visible-ASCII bytes. Such headers are
        // technically malformed per HTTP spec, and rare in practice. For VCR-style
        // test fixtures, silently dropping them is acceptable.
        let headers = response
            .headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();

        Self {
            url: request.url.to_string(),
            method: request.method.to_string(),
            status: response.status.as_u16(),
            headers,
            body: response.body.to_vec(),
            final_url: response.final_url.to_string(),
        }
    }
}

impl TryFrom<CachedResponse> for HttpResponse {
    type Error = HttpError;

    fn try_from(cached: CachedResponse) -> Result<Self, Self::Error> {
        let status = StatusCode::from_u16(cached.status)
            .map_err(|_| HttpError::CacheIo(format!("invalid status code: {}", cached.status)))?;

        let mut headers = HeaderMap::new();
        for (k, v) in cached.headers {
            if let (Ok(name), Ok(value)) = (
                k.parse::<reqwest::header::HeaderName>(),
                v.parse::<reqwest::header::HeaderValue>(),
            ) {
                headers.insert(name, value);
            }
        }

        let final_url = Url::parse(&cached.final_url)
            .map_err(|e| HttpError::CacheIo(format!("invalid cached URL: {e}")))?;

        Ok(HttpResponse {
            status,
            headers,
            body: Bytes::from(cached.body),
            final_url,
        })
    }
}

/// Serde helper for base64-encoding bytes.
mod base64_bytes {
    use base64::prelude::*;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&BASE64_STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(deserializer)?;
        BASE64_STANDARD.decode(&s).map_err(serde::de::Error::custom)
    }
}

#[async_trait::async_trait]
impl HttpClient for CachingClient {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        match self.mode {
            CacheMode::Passthrough => self.inner.execute(request).await,

            CacheMode::Offline => {
                // Only read from cache
                match self.read_cache(&request).await? {
                    Some(cached) => cached.try_into(),
                    None => Err(HttpError::CacheMiss {
                        url: request.url.to_string(),
                    }),
                }
            }

            CacheMode::Online => {
                // Check cache first
                if let Some(cached) = self.read_cache(&request).await? {
                    return cached.try_into();
                }

                // Fetch from network
                let response = self.inner.execute(request.clone()).await?;

                // Cache the response
                self.write_cache(&request, &response).await?;

                Ok(response)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_key_deterministic() {
        let url = Url::parse("https://example.com/page?a=1").expect("valid url");
        let request = HttpRequest::get(url);

        let key1 = CachingClient::cache_key(&request);
        let key2 = CachingClient::cache_key(&request);

        assert_eq!(key1, key2);
        assert_eq!(key1.len(), 32); // 16 bytes = 32 hex chars
    }

    #[test]
    fn test_cache_key_differs_for_different_urls() {
        let url1 = Url::parse("https://example.com/page1").expect("valid url");
        let url2 = Url::parse("https://example.com/page2").expect("valid url");

        let key1 = CachingClient::cache_key(&HttpRequest::get(url1));
        let key2 = CachingClient::cache_key(&HttpRequest::get(url2));

        assert_ne!(key1, key2);
    }

    // ==================== VCR-style integration tests ====================
    //
    // These tests use cached HTTP fixtures by default (offline mode).
    // If fixtures are missing, tests print instructions and return early.
    //
    // To record fixtures: cargo test -p chronoscope-workers --features record-fixtures
    //
    // example.com is owned by IANA (RFC 2606) - stable and safe for testing.

    /// Get the fixtures directory path.
    fn fixtures_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
    }

    /// Determine VCR mode from feature flag.
    fn vcr_mode() -> CacheMode {
        if cfg!(feature = "record-fixtures") {
            CacheMode::Online
        } else {
            CacheMode::Offline
        }
    }

    /// Create a VCR client for tests.
    fn vcr_client() -> Result<CachingClient, HttpError> {
        CachingClient::new(fixtures_dir(), vcr_mode())
    }

    /// Execute a request, returning None if fixture is missing (with helpful message).
    async fn vcr_fetch(client: &CachingClient, request: HttpRequest) -> Option<HttpResponse> {
        match client.execute(request.clone()).await {
            Ok(response) => Some(response),
            Err(HttpError::CacheMiss { url }) => {
                eprintln!(
                    "\n  ⚠ Fixture missing for: {url}\n  \
                     Record with: cargo test -p chronoscope-workers --features record-fixtures\n"
                );
                None
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    #[tokio::test]
    async fn test_example_com_returns_expected_content() {
        let client = vcr_client().expect("create client");
        let url = Url::parse("https://example.com/").expect("valid url");

        let Some(response) = vcr_fetch(&client, HttpRequest::get(url)).await else {
            return; // Fixture missing - message already printed
        };

        assert_eq!(response.status, StatusCode::OK);

        let body = String::from_utf8_lossy(&response.body);
        assert!(
            body.contains("Example Domain"),
            "example.com should contain 'Example Domain'"
        );
        assert!(
            body.contains("This domain is for use in"),
            "example.com should contain usage description"
        );
    }

    #[tokio::test]
    async fn test_example_com_404_response() {
        let client = vcr_client().expect("create client");
        let url = Url::parse("https://example.com/nonexistent-page-12345").expect("valid url");

        let Some(response) = vcr_fetch(&client, HttpRequest::get(url)).await else {
            return;
        };

        assert_eq!(response.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_redirect_is_followed() {
        let client = vcr_client().expect("create client");
        // httpbin.org provides reliable redirect testing
        let url = Url::parse("https://httpbin.org/redirect-to?url=https%3A%2F%2Fexample.com%2F")
            .expect("valid url");

        let Some(response) = vcr_fetch(&client, HttpRequest::get(url)).await else {
            return;
        };

        assert_eq!(response.status, StatusCode::OK);
        assert!(
            response.final_url.host_str() == Some("example.com"),
            "should have followed redirect to example.com, got: {}",
            response.final_url
        );
    }

    // ==================== CachingClient unit tests (use temp dirs) ====================

    #[tokio::test]
    async fn test_caching_client_round_trip() {
        let cache_dir = tempfile::tempdir().expect("create temp dir");

        // We test the cache read path by pre-writing a cache file
        let url = Url::parse("https://test.example/cached").expect("valid url");
        let request = HttpRequest::get(url.clone());
        let cache_key = CachingClient::cache_key(&request);
        let cache_path = cache_dir.path().join(format!("{cache_key}.json"));

        // Write a fake cached response
        let cached = serde_json::json!({
            "url": "https://test.example/cached",
            "method": "GET",
            "status": 200,
            "headers": {},
            "body": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"cached content"),
            "final_url": "https://test.example/cached"
        });
        std::fs::write(&cache_path, serde_json::to_string_pretty(&cached).unwrap())
            .expect("write cache");

        // Offline client reads from cache
        let offline = CachingClient::new(cache_dir.path().to_path_buf(), CacheMode::Offline)
            .expect("create client");

        let response = offline.execute(request).await.expect("read from cache");
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body.as_ref(), b"cached content");
    }

    #[tokio::test]
    async fn test_caching_client_offline_cache_miss() {
        let cache_dir = tempfile::tempdir().expect("create temp dir");
        let client = CachingClient::new(cache_dir.path().to_path_buf(), CacheMode::Offline)
            .expect("create client");

        let url = Url::parse("https://example.com/not-cached").expect("valid url");

        let result = client.execute(HttpRequest::get(url)).await;

        assert!(
            matches!(result, Err(HttpError::CacheMiss { .. })),
            "offline mode should return CacheMiss for uncached URL"
        );
    }
}
