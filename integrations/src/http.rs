//! HTTP client abstraction with SSRF protection and VCR-style caching.
//!
//! This module provides:
//! - [`HttpClient`] trait: abstraction for HTTP operations
//! - [`ReqwestClient`]: production implementation with SSRF protection
//! - [`CachingClient`]: VCR-style caching for reproducible integration tests
//! - [`FetchError`]: error type for fetch operations

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use reqwest::header::HeaderMap;
use reqwest::{Method, StatusCode};
use url::Url;

// ==================== SSRF Protection ====================
//
// TODO: Factor out shared IP blocking logic into a common crate (e.g., chronoscope-security).
// The functions below duplicate `check_ipv4_blocked` and `check_ipv6_blocked` from
// api/src/url_security.rs. That module also does DNS resolution to validate hostnames,
// which we can't do here (reqwest's redirect callback is sync). Once factored out,
// this module should reuse the shared IP checking functions.

/// Check if an IPv4 address is in a blocked range (private/internal/reserved).
fn is_blocked_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();

    // Loopback: 127.0.0.0/8
    octets[0] == 127
        // Private: 10.0.0.0/8
        || octets[0] == 10
        // Private: 172.16.0.0/12
        || (octets[0] == 172 && (16..=31).contains(&octets[1]))
        // Private: 192.168.0.0/16
        || (octets[0] == 192 && octets[1] == 168)
        // Link-local: 169.254.0.0/16 (includes cloud metadata at 169.254.169.254)
        || (octets[0] == 169 && octets[1] == 254)
        // Shared address space: 100.64.0.0/10 (carrier-grade NAT)
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_unspecified()
}

/// Check if an IPv6 address is in a blocked range.
fn is_blocked_ipv6(ip: &Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return true;
    }

    let segments = ip.segments();
    // Link-local: fe80::/10
    if (segments[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    // Unique local: fc00::/7
    if (segments[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    // IPv4-mapped: check embedded IPv4
    if let Some(ipv4) = ip.to_ipv4_mapped() {
        return is_blocked_ipv4(ipv4);
    }

    false
}

/// Check if a host (from redirect target) is potentially an SSRF target.
///
/// This is a sync check suitable for reqwest's redirect callback.
/// It catches IP address literals and known dangerous hostnames, but cannot
/// resolve hostnames to check their IPs (that happens at URL submission time).
fn is_redirect_blocked(host: &str) -> bool {
    // Check for known dangerous hostnames
    let lower = host.to_ascii_lowercase();
    if lower == "localhost"
        || lower.ends_with(".local")
        || lower.ends_with(".internal")
        || lower == "metadata.google.internal"
        || lower == "metadata.goog"
    {
        return true;
    }

    // Check if it's an IP address literal
    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(ipv4) => is_blocked_ipv4(ipv4),
            IpAddr::V6(ipv6) => is_blocked_ipv6(&ipv6),
        };
    }

    false
}

// ==================== Error Types ====================

/// Error type for HTTP operations.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// Request failed (timeout, connection, build error, etc.)
    #[error("request error: {0}")]
    Reqwest(#[from] reqwest::Error),

    /// Response body exceeds maximum allowed size.
    #[error("response too large: {size} bytes exceeds {max} byte limit")]
    ResponseTooLarge { size: usize, max: usize },

    /// Redirect to a blocked destination (SSRF protection).
    #[error("redirect to blocked destination: {url}")]
    BlockedRedirect { url: String },

    /// Cache miss in offline mode.
    #[error("cache miss for {url} in offline mode")]
    CacheMiss { url: String },

    /// I/O error during cache operations.
    #[error("cache I/O error: {0}")]
    CacheIo(#[from] std::io::Error),

    /// Cache file has invalid JSON format.
    #[error("cache format error: {0}")]
    CacheFormat(#[source] serde_json::Error),

    /// Cache data is invalid (e.g., corrupt status code or URL).
    #[error("invalid cache data: {0}")]
    CacheInvalid(String),
}

/// Errors that can occur during fetching.
///
/// This is the main error type for integration fetch operations. It wraps
/// HTTP-level errors and adds semantic errors like "not found" and "rate limited".
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// HTTP transport error (may be retriable).
    #[error("HTTP error: {0}")]
    Http(#[source] HttpError),

    /// Integration service error (e.g., Apify run failed).
    #[error("service error: {0}")]
    Service(String),

    /// Rate limited - should retry after delay.
    #[error("rate limited")]
    RateLimited,

    /// Server error (5xx) - retriable.
    #[error("server error: {status}")]
    ServerError { status: u16 },

    /// Unexpected HTTP status code - retriable.
    #[error("unexpected status: {status}")]
    UnexpectedStatus { status: u16 },

    /// Content not found (404) - permanent.
    #[error("not found")]
    NotFound,

    /// Access forbidden (403) - permanent.
    #[error("forbidden")]
    Forbidden,

    /// Failed to parse content - permanent.
    #[error("parse error: {0}")]
    ParseError(String),

    /// VCR cache miss - permanent (only occurs in test mode).
    #[error("cache miss: {url}")]
    CacheMiss { url: String },
}

impl FetchError {
    /// Convert an HTTP error to a [`FetchError`].
    ///
    /// Handles `CacheMiss` specially as a permanent error (for VCR testing),
    /// while other HTTP errors remain retriable.
    #[must_use]
    pub fn from_http_error(e: HttpError) -> Self {
        match e {
            HttpError::CacheMiss { url } => Self::CacheMiss { url },
            other => Self::Http(other),
        }
    }

    /// Create a service error for integration-level failures.
    #[must_use]
    pub fn service(message: impl Into<String>) -> Self {
        Self::Service(message.into())
    }

    /// Convert an HTTP status code to a [`FetchError`], returning `Ok(())` for success codes.
    ///
    /// Provides consistent error handling across all integrations.
    ///
    /// # Errors
    ///
    /// Returns an error for non-success status codes:
    /// - [`FetchError::NotFound`] for 404
    /// - [`FetchError::Forbidden`] for 401 and 403
    /// - [`FetchError::RateLimited`] for 429
    /// - [`FetchError::ServerError`] for 5xx codes
    /// - [`FetchError::UnexpectedStatus`] for other non-success codes
    pub fn from_status(status: StatusCode) -> Result<(), Self> {
        if status.is_success() {
            return Ok(());
        }
        Err(match status.as_u16() {
            404 => Self::NotFound,
            401 | 403 => Self::Forbidden,
            429 => Self::RateLimited,
            code @ 500..=599 => Self::ServerError { status: code },
            status => Self::UnexpectedStatus { status },
        })
    }

    /// Whether this error should be retried.
    #[must_use]
    pub fn is_retriable(&self) -> bool {
        matches!(
            self,
            Self::Http(_)
                | Self::Service(_)
                | Self::RateLimited
                | Self::ServerError { .. }
                | Self::UnexpectedStatus { .. }
        )
    }
}

// ==================== Request/Response Types ====================

/// HTTP request to execute.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    /// HTTP method.
    pub method: Method,
    /// Request URL.
    pub url: Url,
    /// Request headers.
    pub headers: HeaderMap,
    /// Request body (optional).
    pub body: Option<Bytes>,
    /// Request timeout (overrides client default if set).
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

    /// Create a POST request.
    #[must_use]
    pub fn post(url: Url) -> Self {
        Self {
            method: Method::POST,
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

    /// Set a JSON body, automatically adding the Content-Type header.
    #[must_use]
    pub fn json_body(mut self, body: impl Into<Bytes>) -> Self {
        self.headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        self.body = Some(body.into());
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
    /// Response status code.
    pub status: StatusCode,
    /// Response headers.
    pub headers: HeaderMap,
    /// Response body.
    pub body: Bytes,
    /// Final URL after following redirects.
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

/// Production HTTP client using reqwest with SSRF protection.
pub struct ReqwestClient {
    client: reqwest::Client,
    max_response_size: usize,
}

impl ReqwestClient {
    /// Create a new reqwest-based HTTP client with default configuration.
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
        // Custom redirect policy that blocks redirects to internal/private destinations
        let max_redirects = config.max_redirects;
        let redirect_policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= max_redirects {
                return attempt.stop();
            }

            // Check if redirect target is blocked (SSRF protection)
            // Note: Can't use let-chains here - need to copy host before moving attempt
            #[allow(clippy::collapsible_if)]
            if let Some(host) = attempt.url().host_str() {
                if is_redirect_blocked(host) {
                    let host = host.to_string();
                    return attempt.error(format!("redirect to blocked host: {host}"));
                }
            }

            attempt.follow()
        });

        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .connect_timeout(config.connect_timeout)
            .user_agent(&config.user_agent)
            .redirect(redirect_policy)
            .build()?;

        Ok(Self {
            client,
            max_response_size: config.max_response_size,
        })
    }
}

/// Configuration for [`ReqwestClient`].
pub struct ReqwestConfig {
    /// Overall request timeout.
    pub timeout: Duration,
    /// TCP connection timeout.
    pub connect_timeout: Duration,
    /// User-Agent header value.
    pub user_agent: String,
    /// Maximum number of redirects to follow.
    pub max_redirects: usize,
    /// Maximum response body size in bytes. Checked via Content-Length header
    /// before downloading, and enforced while streaming.
    pub max_response_size: usize,
}

impl Default for ReqwestConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            // Reddit blocks generic user agents; use a descriptive one
            user_agent: "Chronoscope/0.1 (historical research tool)".to_string(),
            max_redirects: 10,
            // TODO: Split into separate limits for images (conservative, ~10MB) and video
            // (larger, ~100MB+). Currently using a single limit that accommodates video.
            max_response_size: 50 * 1024 * 1024, // 50MB
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

        let response = builder.send().await?;

        // Extract fields before .bytes() which consumes the response
        let final_url = response.url().clone();
        let status = response.status();
        let headers = response.headers().clone();

        // Check Content-Length header for early rejection (avoids wasting bandwidth).
        // Note: Servers can lie, so we also enforce the limit while streaming below.
        if let Some(content_length) = headers.get(reqwest::header::CONTENT_LENGTH)
            && let Ok(size_str) = content_length.to_str()
            && let Ok(size) = size_str.parse::<usize>()
            && size > self.max_response_size
        {
            return Err(HttpError::ResponseTooLarge {
                size,
                max: self.max_response_size,
            });
        }

        // Stream the body with a hard size limit to prevent memory exhaustion.
        // We never buffer more than max_response_size bytes, even if the server lies.
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;

            if body.len() + chunk.len() > self.max_response_size {
                return Err(HttpError::ResponseTooLarge {
                    size: body.len() + chunk.len(),
                    max: self.max_response_size,
                });
            }

            body.extend_from_slice(&chunk);
        }

        let body = Bytes::from(body);

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
    /// Online: fetch from network, cache responses.
    Online,
    /// Offline: only read from cache, error on miss.
    Offline,
    /// Passthrough: always fetch from network, never cache.
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
    ///
    /// Includes method, URL, and body hash to distinguish requests to the same
    /// endpoint with different payloads (e.g., Apify POST requests for different URLs).
    ///
    /// CDN hostnames are normalized to avoid cache misses when the same content
    /// is served from different edge servers (e.g., `scontent-lax7-1.cdninstagram.com`
    /// and `scontent-dfw5-2.cdninstagram.com` both normalize to `cdninstagram.com`).
    #[must_use]
    pub fn cache_key(request: &HttpRequest) -> String {
        use sha2::{Digest, Sha256};

        // Normalize URL for cache key
        let normalized_url = Self::normalize_url_for_cache(&request.url);

        let mut hasher = Sha256::new();
        hasher.update(request.method.as_str().as_bytes());
        hasher.update(b"|");
        hasher.update(normalized_url.as_bytes());
        if let Some(ref body) = request.body {
            hasher.update(b"|");
            hasher.update(body.as_ref());
        }
        let hash = hasher.finalize();
        hex::encode(&hash[..16]) // Use first 16 bytes (32 hex chars)
    }

    /// Normalize a URL for cache key computation.
    ///
    /// Strips CDN-specific variations that change per-request but serve the same content:
    /// - Instagram CDN subdomains (scontent-xxx-N.cdninstagram.com → cdninstagram.com)
    /// - Reddit preview signature parameter (s=...) which changes but serves same image
    fn normalize_url_for_cache(url: &Url) -> String {
        let mut normalized = url.clone();

        if let Some(host) = url.host_str() {
            // Normalize *.cdninstagram.com → cdninstagram.com
            if host.ends_with(".cdninstagram.com") || host == "cdninstagram.com" {
                let _ = normalized.set_host(Some("cdninstagram.com"));
            }

            // Strip signature parameter from Reddit preview URLs
            // The 's' param is a time-sensitive signature, but the image content is the same
            if host == "preview.redd.it" {
                let filtered_pairs: Vec<(String, String)> = normalized
                    .query_pairs()
                    .filter(|(k, _)| k != "s")
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect();

                if filtered_pairs.is_empty() {
                    normalized.set_query(None);
                } else {
                    let query = filtered_pairs
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join("&");
                    normalized.set_query(Some(&query));
                }
            }
        }

        normalized.to_string()
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
                let cached: CachedResponse =
                    serde_json::from_slice(&data).map_err(HttpError::CacheFormat)?;
                Ok(Some(cached))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(HttpError::CacheIo(e)),
        }
    }

    /// Write a response to the cache.
    ///
    /// If the existing cache file is pinned (manually modified), this skips writing
    /// and logs an info message instead.
    async fn write_cache(
        &self,
        request: &HttpRequest,
        response: &HttpResponse,
    ) -> Result<(), HttpError> {
        let path = self.cache_path(request);

        // Check if existing fixture is pinned
        if let Ok(existing_data) = tokio::fs::read(&path).await
            && let Ok(existing) = serde_json::from_slice::<CachedResponse>(&existing_data)
            && let Some(reason) = &existing.pinned
        {
            eprintln!(
                "[VCR] Skipping write-through on pinned fixture: {} (reason: {reason})",
                request.url
            );
            return Ok(());
        }

        // Ensure cache directory exists
        tokio::fs::create_dir_all(&self.cache_dir).await?;

        let cached = CachedResponse::from_response(request, response);
        let data = serde_json::to_vec_pretty(&cached).map_err(HttpError::CacheFormat)?;

        tokio::fs::write(&path, data).await?;

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
    /// If set, this fixture has been manually modified and should not be overwritten.
    /// The value describes why/how it was modified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pinned: Option<String>,
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
            pinned: None,
        }
    }
}

impl TryFrom<CachedResponse> for HttpResponse {
    type Error = HttpError;

    fn try_from(cached: CachedResponse) -> Result<Self, Self::Error> {
        let status = StatusCode::from_u16(cached.status).map_err(|_| {
            HttpError::CacheInvalid(format!("invalid status code: {}", cached.status))
        })?;

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
            .map_err(|e| HttpError::CacheInvalid(format!("invalid cached URL: {e}")))?;

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
                // Recording mode: fetch from network and cache the response.
                //
                // Exception: pinned fixtures are returned directly without network
                // requests. This ensures pinned fixtures behave identically in both
                // online and offline modes.
                if let Some(cached) = self.read_cache(&request).await?
                    && cached.pinned.is_some()
                {
                    return cached.try_into();
                }

                // Write-through caching: we overwrite any existing entry.
                // For polling APIs, this means the cache ends up with the final
                // response (e.g., "SUCCEEDED"), which is what we want for playback.
                let response = self.inner.execute(request.clone()).await?;
                self.write_cache(&request, &response).await?;
                Ok(response)
            }
        }
    }
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    // ==================== FetchError tests ====================

    #[test]
    fn test_fetch_error_is_retriable() {
        assert!(FetchError::service("timeout").is_retriable());
        assert!(FetchError::RateLimited.is_retriable());
        assert!(FetchError::ServerError { status: 500 }.is_retriable());
        assert!(FetchError::UnexpectedStatus { status: 418 }.is_retriable());

        assert!(!FetchError::NotFound.is_retriable());
        assert!(!FetchError::Forbidden.is_retriable());
        assert!(!FetchError::ParseError("bad json".into()).is_retriable());
        assert!(!FetchError::CacheMiss { url: "url".into() }.is_retriable());
    }

    #[test]
    fn test_fetch_error_from_status() {
        assert!(FetchError::from_status(StatusCode::OK).is_ok());
        assert!(FetchError::from_status(StatusCode::CREATED).is_ok());

        assert!(matches!(
            FetchError::from_status(StatusCode::NOT_FOUND),
            Err(FetchError::NotFound)
        ));
        assert!(matches!(
            FetchError::from_status(StatusCode::FORBIDDEN),
            Err(FetchError::Forbidden)
        ));
        assert!(matches!(
            FetchError::from_status(StatusCode::TOO_MANY_REQUESTS),
            Err(FetchError::RateLimited)
        ));
        assert!(matches!(
            FetchError::from_status(StatusCode::INTERNAL_SERVER_ERROR),
            Err(FetchError::ServerError { status: 500 })
        ));
    }

    // ==================== CachingClient tests ====================

    #[test]
    fn test_cache_key_deterministic() -> TestResult {
        let url = Url::parse("https://example.com/page?a=1")?;
        let request = HttpRequest::get(url);

        let key1 = CachingClient::cache_key(&request);
        let key2 = CachingClient::cache_key(&request);

        assert_eq!(key1, key2);
        assert_eq!(key1.len(), 32); // 16 bytes = 32 hex chars
        Ok(())
    }

    #[test]
    fn test_cache_key_differs_for_different_urls() -> TestResult {
        let url1 = Url::parse("https://example.com/page1")?;
        let url2 = Url::parse("https://example.com/page2")?;

        let key1 = CachingClient::cache_key(&HttpRequest::get(url1));
        let key2 = CachingClient::cache_key(&HttpRequest::get(url2));

        assert_ne!(key1, key2);
        Ok(())
    }

    // ==================== VCR-style integration tests ====================
    //
    // These tests verify the CachingClient works correctly with real HTTP fixtures.
    // Run with: cargo test -p chronoscope-integrations --features record-fixtures
    // to record fixtures, then run without the feature to replay.

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

    #[tokio::test]
    async fn test_example_com_returns_expected_content() -> TestResult {
        let client = vcr_client()?;
        let url = Url::parse("https://example.com/")?;

        let response = client.execute(HttpRequest::get(url)).await?;

        assert_eq!(response.status, StatusCode::OK);

        let body = String::from_utf8_lossy(&response.body);
        assert!(
            body.contains("Example Domain"),
            "example.com should contain 'Example Domain'"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_example_com_404_response() -> TestResult {
        let client = vcr_client()?;
        let url = Url::parse("https://example.com/nonexistent-page-12345")?;

        let response = client.execute(HttpRequest::get(url)).await?;

        assert_eq!(response.status, StatusCode::NOT_FOUND);
        Ok(())
    }

    #[tokio::test]
    async fn test_redirect_is_followed() -> TestResult {
        let client = vcr_client()?;
        let url = Url::parse("https://httpbin.org/redirect-to?url=https%3A%2F%2Fexample.com%2F")?;

        let response = client.execute(HttpRequest::get(url)).await?;

        assert_eq!(response.status, StatusCode::OK);
        assert!(
            response.final_url.host_str() == Some("example.com"),
            "should have followed redirect to example.com, got: {}",
            response.final_url
        );
        Ok(())
    }

    // ==================== CachingClient unit tests ====================

    #[tokio::test]
    async fn test_caching_client_round_trip() -> TestResult {
        let cache_dir = tempfile::tempdir()?;

        // We test the cache read path by pre-writing a cache file
        let url = Url::parse("https://test.example/cached")?;
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
        std::fs::write(&cache_path, serde_json::to_string_pretty(&cached)?)?;

        // Offline client reads from cache
        let offline = CachingClient::new(cache_dir.path().to_path_buf(), CacheMode::Offline)?;

        let response = offline.execute(request).await?;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body.as_ref(), b"cached content");
        Ok(())
    }

    #[tokio::test]
    async fn test_caching_client_offline_cache_miss() -> TestResult {
        let cache_dir = tempfile::tempdir()?;
        let client = CachingClient::new(cache_dir.path().to_path_buf(), CacheMode::Offline)?;

        let url = Url::parse("https://example.com/not-cached")?;

        let result = client.execute(HttpRequest::get(url)).await;

        assert!(
            matches!(result, Err(HttpError::CacheMiss { .. })),
            "offline mode should return CacheMiss for uncached URL"
        );
        Ok(())
    }
}
