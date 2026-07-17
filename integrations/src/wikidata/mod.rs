//! Wikidata and Wikimedia Commons API client.
//!
//! Provides typed access to the Wikidata Action API, SPARQL endpoint, and
//! Wikimedia Commons. Generic over [`HttpClient`] so tests can use
//! [`CachingClient`](crate::http::CachingClient) or `MockHttpClient` without
//! hitting the network.
//!
//! # Example
//!
//! ```ignore
//! use chronoscope_integrations::wikidata::WikidataClient;
//! use chronoscope_integrations::ReqwestClient;
//!
//! let http = ReqwestClient::new()?;
//! let client = WikidataClient::new(http);
//! let entities = client
//!     .get_entities_at_revisions(&[("Q243", 1554857333)])
//!     .await?;
//! ```

pub mod commons;
pub mod entity;
pub mod filter;
mod sparql;
pub mod stream;

pub use commons::url_for_filename;
pub use entity::{
    Claim, CommonsFilename, CoordinateValue, DataValue, EntityRefValue, Label, LanguageCode,
    MonolingualTextValue, PageId, PropertyId, QuantityAmount, QuantityUnit, QuantityValue, Rank,
    RevisionId, SiteId, Sitelink, Snak, TimeValue, WikidataEntity, WikidataEntityContent,
    WikidataEntityType, WikidataId, WikidataPrecision, WikidataTimestamp,
};

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{HeaderMap, RETRY_AFTER};
use serde_json::Value;
use tokio::time::Instant;
use url::Url;

use crate::http::{HttpClient, HttpError, HttpRequest, HttpResponse};

/// Maximum revisions per MediaWiki API batch request.
const BATCH_SIZE: usize = 50;

/// Pause between consecutive API requests. Wikimedia's rate limiter allows
/// only a small burst from anonymous clients; spacing requests out keeps us
/// under it most of the time, with the 429 retry loop as backstop.
const DEFAULT_MIN_REQUEST_INTERVAL: Duration = Duration::from_millis(200);

/// Total tries per request, counting the first.
const MAX_ATTEMPTS: u32 = 6;

/// Ceiling on any single rate-limit wait, whatever Retry-After claims.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(120);

/// First wait of the doubling backoff used when a 429 arrives with a
/// missing or unparseable Retry-After header.
const FALLBACK_BACKOFF_BASE: Duration = Duration::from_secs(5);

/// Characters of response body quoted in errors.
const BODY_EXCERPT_CHARS: usize = 200;

// =============================================================================
// API Timestamp
// =============================================================================

/// A validated MediaWiki API timestamp in ISO 8601 format.
///
/// MediaWiki API endpoints like `rvstart` expect timestamps in the format
/// `"2022-01-03T00:00:00Z"` (standard ISO 8601 with UTC timezone). This is
/// distinct from [`WikidataTimestamp`] which uses Wikidata's variant format
/// with `+`/`-` prefix and variable-width years.
///
/// # Examples
///
/// ```
/// use chronoscope_integrations::wikidata::ApiTimestamp;
///
/// let ts = ApiTimestamp::try_from("2022-01-03T00:00:00Z".to_string());
/// assert!(ts.is_ok());
///
/// let bad = ApiTimestamp::try_from("not-a-timestamp".to_string());
/// assert!(bad.is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApiTimestamp(String);

impl ApiTimestamp {
    /// Get the timestamp as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ApiTimestamp {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        // Reject Wikidata-style timestamps with +/- prefix
        if s.starts_with('+') || s.starts_with('-') {
            return Err(format!(
                "invalid API timestamp '{s}': must not start with '+' or '-'"
            ));
        }
        chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M:%SZ")
            .map_err(|e| format!("invalid API timestamp '{s}': {e}"))?;
        Ok(Self(s))
    }
}

impl std::str::FromStr for ApiTimestamp {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_string())
    }
}

impl std::fmt::Display for ApiTimestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// =============================================================================
// Error type
// =============================================================================

/// Error type for Wikidata API operations.
#[derive(Debug, thiserror::Error)]
pub enum WikidataError {
    /// HTTP transport error.
    #[error("HTTP error: {0}")]
    Http(#[from] HttpError),

    /// JSON parse error.
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    /// API returned a non-success HTTP status.
    #[error("HTTP {status} from {url}: {body_excerpt}")]
    Status {
        status: u16,
        url: String,
        body_excerpt: String,
    },

    /// API returned a success status with a body that failed to parse as JSON.
    #[error("invalid JSON from {url}: {source} (body: {body_excerpt})")]
    JsonBody {
        url: String,
        source: serde_json::Error,
        body_excerpt: String,
    },

    /// Wikidata API returned an error or unexpected response.
    #[error("API error: {message}")]
    Api { message: String },

    /// URL parse error.
    #[error("URL parse error: {0}")]
    Url(#[from] url::ParseError),

    /// Response body exceeds maximum allowed size.
    #[error("response too large: {0} bytes")]
    ResponseTooLarge(usize),
}

/// First [`BODY_EXCERPT_CHARS`] characters of a response body,
/// whitespace-collapsed so the error stays on one line. Lossy on invalid
/// UTF-8.
fn body_excerpt(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let excerpt: String = chars.by_ref().take(BODY_EXCERPT_CHARS).collect();
    if excerpt.is_empty() {
        "(empty body)".to_string()
    } else if chars.next().is_some() {
        format!("{excerpt}…")
    } else {
        excerpt
    }
}

/// Parse the Retry-After header as a number of seconds.
fn retry_after_duration(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?;
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// Parse a response body as JSON, quoting the body in the error on failure.
fn parse_json_body(url: &Url, body: &[u8]) -> Result<Value, WikidataError> {
    serde_json::from_slice(body).map_err(|source| WikidataError::JsonBody {
        url: url.to_string(),
        source,
        body_excerpt: body_excerpt(body),
    })
}

// =============================================================================
// Client
// =============================================================================

/// Client for the Wikidata Action API, SPARQL endpoint, and Wikimedia Commons.
///
/// Generic over `H: HttpClient` to enable testing with cached or mock HTTP.
///
/// All requests go through a shared pacing/retry layer: consecutive requests
/// are spaced at least `min_request_interval` apart, and 429 responses are
/// retried after the server's Retry-After delay (bounded attempts, capped
/// waits). This policy belongs to the Wikimedia endpoints specifically, so it
/// lives here and the [`HttpClient`] underneath stays generic.
pub struct WikidataClient<H: HttpClient> {
    http: H,
    min_request_interval: Duration,
    last_request: tokio::sync::Mutex<Option<Instant>>,
}

impl<H: HttpClient> WikidataClient<H> {
    /// Create a new client wrapping the given HTTP implementation.
    pub fn new(http: H) -> Self {
        Self::with_min_request_interval(http, DEFAULT_MIN_REQUEST_INTERVAL)
    }

    /// Create a client with a custom pacing interval between API requests.
    ///
    /// `Duration::ZERO` disables pacing; useful when replaying recorded
    /// fixtures.
    pub fn with_min_request_interval(http: H, min_request_interval: Duration) -> Self {
        Self {
            http,
            min_request_interval,
            last_request: tokio::sync::Mutex::new(None),
        }
    }

    /// Sleep until at least `min_request_interval` has passed since the
    /// previous request. The lock is held across the sleep so concurrent
    /// callers queue up behind it.
    async fn pace(&self) {
        if self.min_request_interval.is_zero() {
            return;
        }
        let mut last = self.last_request.lock().await;
        if let Some(previous) = *last {
            #[expect(
                clippy::disallowed_methods,
                reason = "deliberate inter-request pacing toward the Wikimedia API"
            )]
            tokio::time::sleep_until(previous + self.min_request_interval).await;
        }
        *last = Some(Instant::now());
    }

    /// Execute a request with pacing and rate-limit retries, returning the
    /// response only for a success status.
    ///
    /// On 429 this waits out the server's Retry-After (doubling backoff when
    /// the header is absent or unparseable) and retries, up to
    /// [`MAX_ATTEMPTS`] tries total. Every other non-success status, and 429
    /// once attempts run out, becomes [`WikidataError::Status`] carrying the
    /// status, URL, and a body excerpt.
    async fn execute_checked(&self, request: HttpRequest) -> Result<HttpResponse, WikidataError> {
        let mut attempt: u32 = 1;
        loop {
            self.pace().await;
            let response = self.http.execute(request.clone()).await?;

            if response.status.is_success() {
                return Ok(response);
            }

            if response.status == StatusCode::TOO_MANY_REQUESTS && attempt < MAX_ATTEMPTS {
                let wait = retry_after_duration(&response.headers)
                    .unwrap_or(FALLBACK_BACKOFF_BASE * 2u32.pow(attempt - 1))
                    .min(MAX_RETRY_WAIT);
                // Visible progress: these fetches run inside Nix builds,
                // where a silent wait looks like a hang.
                eprintln!(
                    "wikidata: HTTP 429 from {}; waiting {}s before retry \
                     (attempt {attempt} of {MAX_ATTEMPTS})",
                    request.url,
                    wait.as_secs(),
                );
                #[expect(
                    clippy::disallowed_methods,
                    reason = "deliberate wait honoring the server's rate-limit Retry-After"
                )]
                tokio::time::sleep(wait).await;
                attempt += 1;
                continue;
            }

            return Err(WikidataError::Status {
                status: response.status.as_u16(),
                url: request.url.to_string(),
                body_excerpt: body_excerpt(&response.body),
            });
        }
    }

    /// GET a URL and parse the response body as JSON, with pacing, status
    /// checking, and rate-limit retries.
    async fn get_json(&self, url: Url) -> Result<Value, WikidataError> {
        let response = self.execute_checked(HttpRequest::get(url.clone())).await?;
        parse_json_body(&url, &response.body)
    }

    // =========================================================================
    // Entity fetching
    // =========================================================================

    /// Fetch entities at specific revisions, returning a map from entity ID to entity.
    ///
    /// Each pair is `(entity_id, revision_id)`. Revision IDs are batched
    /// (up to 50 per request). Each returned entity's ID is validated against
    /// the expected ID from the input — if a revision resolves to a different
    /// entity, this returns an error.
    ///
    /// Uses `action=query&revids=<rev1>|<rev2>&prop=revisions&rvprop=content`
    /// since `wbgetentities` does not support a revision parameter.
    ///
    /// # Errors
    /// Returns an error if any fetch fails, entity data is malformed, or an
    /// entity ID does not match the revision content.
    pub async fn get_entities_at_revisions(
        &self,
        revisions: &[(&str, RevisionId)],
    ) -> Result<HashMap<WikidataId, WikidataEntity>, WikidataError> {
        // Build a map from revision ID -> expected entity ID for validation
        let expected: HashMap<RevisionId, &str> =
            revisions.iter().map(|&(id, rev)| (rev, id)).collect();

        let mut all_entities = HashMap::with_capacity(revisions.len());

        for chunk in revisions.chunks(BATCH_SIZE) {
            let rev_ids: Vec<String> = chunk.iter().map(|(_, rev)| rev.0.to_string()).collect();
            let joined = rev_ids.join("|");
            let url = format!(
                "https://www.wikidata.org/w/api.php?\
                 action=query&revids={joined}&prop=revisions&rvprop=ids%7Ccontent&format=json"
            );

            let json = self.get_json(Url::parse(&url)?).await?;

            Self::extract_revision_entities(&json, &expected, &mut all_entities)?;
        }

        Ok(all_entities)
    }

    /// Resolve revision IDs for entities at a specific timestamp, returning
    /// a map from entity ID to revision ID.
    ///
    /// Uses the MediaWiki API to find the latest revision of each entity
    /// that existed at or before the given timestamp. Use this to bootstrap
    /// a manifest with explicit revision IDs.
    ///
    /// Queries are made one entity at a time because the `rvstart` parameter
    /// cannot be used with multiple titles in the MediaWiki API.
    pub async fn resolve_revisions(
        &self,
        ids: &[&str],
        timestamp: &ApiTimestamp,
    ) -> Result<HashMap<WikidataId, RevisionId>, WikidataError> {
        let mut results = HashMap::with_capacity(ids.len());

        for id in ids {
            let url = format!(
                "https://www.wikidata.org/w/api.php?\
                 action=query&titles={id}&prop=revisions\
                 &rvprop=ids&rvstart={timestamp}&rvdir=older&rvlimit=1&format=json"
            );

            let json = self.get_json(Url::parse(&url)?).await?;

            let pages = json
                .pointer("/query/pages")
                .and_then(|p| p.as_object())
                .ok_or_else(|| WikidataError::Api {
                    message: format!("no pages in revision response for '{id}'"),
                })?;

            let (_page_id, page_obj) = pages.iter().next().ok_or_else(|| WikidataError::Api {
                message: format!("empty pages object for '{id}'"),
            })?;

            let rev_id = page_obj
                .pointer("/revisions/0/revid")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| WikidataError::Api {
                    message: format!("no revision found for '{id}' at timestamp {timestamp}"),
                })?;

            let wikidata_id =
                WikidataId::try_from(id.to_string()).map_err(|e| WikidataError::Api {
                    message: format!("invalid entity ID from API: {e}"),
                })?;
            results.insert(wikidata_id, RevisionId(rev_id));
        }

        Ok(results)
    }

    /// Resolve and fetch entities at a specific timestamp in one operation.
    ///
    /// Combines [`resolve_revisions`](Self::resolve_revisions) and
    /// [`get_entities_at_revisions`](Self::get_entities_at_revisions). For each
    /// entity with a P935 (Commons gallery) claim, also resolves the gallery
    /// revision and fetches gallery media filenames.
    ///
    /// Returns `(entities, gallery_media)` where `gallery_media` maps gallery
    /// name to its media filenames at the given timestamp.
    ///
    /// # Errors
    /// Returns an error if revision resolution or entity fetching fails.
    pub async fn get_entities_at_timestamp(
        &self,
        ids: &[&str],
        timestamp: &ApiTimestamp,
    ) -> Result<
        (
            HashMap<WikidataId, WikidataEntity>,
            HashMap<String, Vec<CommonsFilename>>,
        ),
        WikidataError,
    > {
        // Step 1: Resolve revision IDs
        let revisions = self.resolve_revisions(ids, timestamp).await?;

        // Step 2: Fetch entities at those revisions
        let revision_pairs: Vec<(&str, RevisionId)> = revisions
            .iter()
            .map(|(id, rev)| (id.as_str(), *rev))
            .collect();
        let entities = self.get_entities_at_revisions(&revision_pairs).await?;

        // Step 3: Discover and resolve P935 (Commons gallery) claims
        let gallery_revisions = self
            .resolve_gallery_revisions_from_entities(&entities, timestamp)
            .await?;

        let mut gallery_media = HashMap::new();
        for (gallery_name, resolution) in &gallery_revisions {
            match resolution {
                Some((_page_id, rev_id)) => {
                    let media = self.fetch_gallery_media_at_revision(*rev_id).await?;
                    gallery_media.insert(gallery_name.clone(), media);
                }
                None => {
                    gallery_media.insert(gallery_name.clone(), Vec::new());
                }
            }
        }

        Ok((entities, gallery_media))
    }

    /// Resolve P935 (Commons gallery) page revisions from a set of entities.
    ///
    /// Iterates all entities for P935 claims, resolves each unique gallery
    /// page at the given timestamp, and returns a map from gallery name to
    /// its resolution result: `Some((page_id, revision_id))` if the page
    /// exists, `None` if it does not.
    ///
    /// # Errors
    /// Returns an error if any gallery revision resolution fails.
    pub async fn resolve_gallery_revisions_from_entities(
        &self,
        entities: &HashMap<WikidataId, WikidataEntity>,
        timestamp: &ApiTimestamp,
    ) -> Result<HashMap<String, Option<(PageId, RevisionId)>>, WikidataError> {
        let mut results = HashMap::new();
        for entity in entities.values() {
            if let Some(p935_claims) = entity.claims.get("P935") {
                for claim in p935_claims {
                    if let Some(gallery_name) = claim.mainsnak.string_value() {
                        if results.contains_key(gallery_name) {
                            continue;
                        }
                        let resolution = self
                            .resolve_gallery_revision(gallery_name, timestamp)
                            .await?;
                        results.insert(gallery_name.to_string(), resolution);
                    }
                }
            }
        }
        Ok(results)
    }

    /// Check a MediaWiki API response for an error object.
    fn check_api_error(json: &Value) -> Result<(), WikidataError> {
        if let Some(error) = json.get("error") {
            let info = error
                .get("info")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            return Err(WikidataError::Api {
                message: info.to_string(),
            });
        }
        Ok(())
    }

    /// Extract entities from an `action=query&revids=...&rvprop=ids|content` response.
    ///
    /// The entity JSON is stored as a string inside `revisions[0]["*"]` rather
    /// than directly in the response (unlike `wbgetentities`). Each entity's ID
    /// is validated against `expected` (revision ID -> expected entity ID).
    fn extract_revision_entities(
        json: &Value,
        expected: &HashMap<RevisionId, &str>,
        out: &mut HashMap<WikidataId, WikidataEntity>,
    ) -> Result<(), WikidataError> {
        Self::check_api_error(json)?;

        let pages = json
            .pointer("/query/pages")
            .and_then(|p| p.as_object())
            .ok_or_else(|| WikidataError::Api {
                message: "no pages in revisions+content response".to_string(),
            })?;

        for (_page_id, page_obj) in pages {
            if page_obj.get("missing").is_some() {
                continue;
            }

            let rev_id = page_obj
                .pointer("/revisions/0/revid")
                .and_then(|v| v.as_u64())
                .map(RevisionId)
                .ok_or_else(|| WikidataError::Api {
                    message: "no revision ID in response".to_string(),
                })?;

            let content_str = page_obj
                .pointer("/revisions/0/*")
                .and_then(|v| v.as_str())
                .ok_or_else(|| WikidataError::Api {
                    message: "no revision content in response".to_string(),
                })?;

            let content: WikidataEntityContent = serde_json::from_str(content_str)?;

            // Validate entity ID matches what we expected for this revision
            if let Some(&expected_id) = expected.get(&rev_id)
                && content.id != expected_id
            {
                return Err(WikidataError::Api {
                    message: format!(
                        "revision {rev_id} resolved to entity '{}', \
                         expected '{expected_id}'",
                        content.id
                    ),
                });
            }

            let entity = content.with_revision(rev_id);
            out.insert(entity.id.clone(), entity);
        }

        Ok(())
    }

    // =========================================================================
    // SPARQL
    // =========================================================================

    /// Fetch all transitive subclasses of a Wikidata type.
    ///
    /// # Errors
    /// Returns an error if the SPARQL query fails.
    pub async fn fetch_subclasses(
        &self,
        root_type: &WikidataId,
    ) -> Result<HashSet<WikidataId>, WikidataError> {
        sparql::fetch_subclasses(self, root_type).await
    }

    // =========================================================================
    // Commons
    // =========================================================================

    /// Fetch media from a Wikimedia Commons gallery at a specific revision.
    ///
    /// Returns filenames of media files (images, video, PDFs) in the gallery.
    /// Takes a `revision_id` directly — use
    /// [`resolve_gallery_revision`](Self::resolve_gallery_revision) to obtain
    /// this from a gallery title and timestamp.
    ///
    /// # Errors
    /// Returns an error if the API request fails.
    pub async fn fetch_gallery_media_at_revision(
        &self,
        rev_id: RevisionId,
    ) -> Result<Vec<CommonsFilename>, WikidataError> {
        commons::fetch_gallery_media_at_revision(self, rev_id).await
    }

    /// Resolve a Commons gallery page's revision at a specific timestamp.
    ///
    /// Returns `(page_id, revision_id)` for the gallery page as it existed
    /// at or before the given timestamp. Use this to bootstrap a manifest
    /// with explicit revision IDs.
    ///
    /// Returns `None` if the gallery page does not exist.
    ///
    /// # Errors
    /// Returns an error if the API request fails.
    pub async fn resolve_gallery_revision(
        &self,
        gallery: &str,
        timestamp: &ApiTimestamp,
    ) -> Result<Option<(PageId, RevisionId)>, WikidataError> {
        commons::resolve_gallery_revision(self, gallery, timestamp).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;

    use super::*;
    use crate::http::{HeaderValue, MockHttpClient};

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn test_commons_url_for_filename() {
        let url = url_for_filename(&CommonsFilename("Test.jpg".to_string()));
        assert!(url.as_str().contains("upload.wikimedia.org"));
        assert!(url.as_str().contains("Test.jpg"));
    }

    // =========================================================================
    // ApiTimestamp tests
    // =========================================================================

    #[test]
    fn test_api_timestamp_valid() -> TestResult {
        let ts = ApiTimestamp::try_from("2022-01-03T00:00:00Z".to_string())?;
        assert_eq!(ts.as_str(), "2022-01-03T00:00:00Z");
        assert_eq!(ts.to_string(), "2022-01-03T00:00:00Z");
        Ok(())
    }

    #[test]
    fn test_api_timestamp_boundary_values() -> TestResult {
        // Minimum valid date
        let ts = ApiTimestamp::try_from("0001-01-01T00:00:00Z".to_string())?;
        assert_eq!(ts.as_str(), "0001-01-01T00:00:00Z");

        // Maximum valid time components
        let ts = ApiTimestamp::try_from("9999-12-31T23:59:59Z".to_string())?;
        assert_eq!(ts.as_str(), "9999-12-31T23:59:59Z");

        Ok(())
    }

    #[test]
    fn test_api_timestamp_rejects_invalid() {
        // Wrong length
        assert!(ApiTimestamp::try_from("2022-01-03".to_string()).is_err());
        // Missing Z suffix
        assert!(ApiTimestamp::try_from("2022-01-03T00:00:00X".to_string()).is_err());
        // Wrong separators
        assert!(ApiTimestamp::try_from("2022/01/03T00:00:00Z".to_string()).is_err());
        // Non-digit where digit expected
        assert!(ApiTimestamp::try_from("20X2-01-03T00:00:00Z".to_string()).is_err());
        // Month out of range
        assert!(ApiTimestamp::try_from("2022-13-03T00:00:00Z".to_string()).is_err());
        assert!(ApiTimestamp::try_from("2022-00-03T00:00:00Z".to_string()).is_err());
        // Day out of range
        assert!(ApiTimestamp::try_from("2022-01-32T00:00:00Z".to_string()).is_err());
        assert!(ApiTimestamp::try_from("2022-01-00T00:00:00Z".to_string()).is_err());
        // Hour out of range
        assert!(ApiTimestamp::try_from("2022-01-03T25:00:00Z".to_string()).is_err());
        // Impossible date: Feb 31
        assert!(ApiTimestamp::try_from("2022-02-31T00:00:00Z".to_string()).is_err());
        // Wikidata-style timestamp (not API format)
        assert!(ApiTimestamp::try_from("+2022-01-03T00:00:00Z".to_string()).is_err());
    }

    // =========================================================================
    // Rate-limit, status-check, and pacing tests (scripted MockHttpClient).
    //
    // Paused tokio time: sleeps auto-advance the clock, so retry waits and
    // pacing are observable without real delays.
    // =========================================================================

    fn api_url() -> Result<Url, url::ParseError> {
        Url::parse("https://www.wikidata.org/w/api.php")
    }

    fn response(
        url: &Url,
        status: StatusCode,
        headers: HeaderMap,
        body: &str,
    ) -> Result<HttpResponse, HttpError> {
        Ok(HttpResponse {
            status,
            headers,
            body: Bytes::copy_from_slice(body.as_bytes()),
            final_url: url.clone(),
        })
    }

    fn retry_after_headers(seconds: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static(seconds));
        headers
    }

    fn revisions_json(revid: u64) -> String {
        serde_json::json!({
            "query": { "pages": { "1": { "revisions": [{ "revid": revid }] } } }
        })
        .to_string()
    }

    #[tokio::test(start_paused = true)]
    async fn retries_429_after_server_delay() -> TestResult {
        let url = api_url()?;
        let mock = Arc::new(MockHttpClient::with_responses(vec![
            response(
                &url,
                StatusCode::TOO_MANY_REQUESTS,
                retry_after_headers("51"),
                "You are making too many requests to the API.",
            ),
            response(&url, StatusCode::OK, HeaderMap::new(), &revisions_json(456)),
        ]));
        let client = WikidataClient::with_min_request_interval(Arc::clone(&mock), Duration::ZERO);
        let ts = ApiTimestamp::try_from("2022-01-03T00:00:00Z".to_string())?;

        let start = Instant::now();
        let revisions = client.resolve_revisions(&["Q243"], &ts).await?;

        assert_eq!(revisions.get("Q243").copied(), Some(RevisionId(456)));
        assert_eq!(mock.request_count(), 2, "429 should be retried once");
        assert!(
            start.elapsed() >= Duration::from_secs(51),
            "retry should wait out the Retry-After delay"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn missing_retry_after_falls_back_to_backoff() -> TestResult {
        let url = api_url()?;
        let mock = Arc::new(MockHttpClient::with_responses(vec![
            response(
                &url,
                StatusCode::TOO_MANY_REQUESTS,
                HeaderMap::new(),
                "rate limited, no header",
            ),
            response(&url, StatusCode::OK, HeaderMap::new(), &revisions_json(456)),
        ]));
        let client = WikidataClient::with_min_request_interval(Arc::clone(&mock), Duration::ZERO);
        let ts = ApiTimestamp::try_from("2022-01-03T00:00:00Z".to_string())?;

        let start = Instant::now();
        let revisions = client.resolve_revisions(&["Q243"], &ts).await?;

        assert_eq!(revisions.len(), 1);
        assert_eq!(mock.request_count(), 2);
        assert!(
            start.elapsed() >= FALLBACK_BACKOFF_BASE,
            "header-less 429 should wait the first backoff step"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_after_bounded_429_attempts() -> TestResult {
        let url = api_url()?;
        let scripted: Vec<_> = (0..MAX_ATTEMPTS)
            .map(|_| {
                response(
                    &url,
                    StatusCode::TOO_MANY_REQUESTS,
                    retry_after_headers("1"),
                    "still rate limited",
                )
            })
            .collect();
        let mock = Arc::new(MockHttpClient::with_responses(scripted));
        let client = WikidataClient::with_min_request_interval(Arc::clone(&mock), Duration::ZERO);
        let ts = ApiTimestamp::try_from("2022-01-03T00:00:00Z".to_string())?;

        let err = client
            .resolve_revisions(&["Q243"], &ts)
            .await
            .err()
            .ok_or("expected rate-limit exhaustion to error")?;

        let message = err.to_string();
        assert!(
            message.contains("429"),
            "error should name the status: {message}"
        );
        assert!(
            message.contains("still rate limited"),
            "error should quote the body: {message}"
        );
        assert_eq!(mock.request_count(), MAX_ATTEMPTS as usize);
        Ok(())
    }

    #[tokio::test]
    async fn non_success_error_carries_status_and_body_excerpt() -> TestResult {
        let url = api_url()?;
        let body = format!("access denied by abuse filter {}", "x".repeat(400));
        let mock = Arc::new(MockHttpClient::with_response(response(
            &url,
            StatusCode::FORBIDDEN,
            HeaderMap::new(),
            &body,
        )));
        let client = WikidataClient::with_min_request_interval(Arc::clone(&mock), Duration::ZERO);
        let ts = ApiTimestamp::try_from("2022-01-03T00:00:00Z".to_string())?;

        let err = client
            .resolve_revisions(&["Q243"], &ts)
            .await
            .err()
            .ok_or("expected 403 to error")?;

        let message = err.to_string();
        assert!(
            message.contains("403"),
            "error should name the status: {message}"
        );
        assert!(
            message.contains("access denied by abuse filter"),
            "error should quote the body: {message}"
        );
        assert!(
            !message.contains(&"x".repeat(BODY_EXCERPT_CHARS + 1)),
            "body excerpt should be truncated: {message}"
        );
        assert_eq!(
            mock.request_count(),
            1,
            "statuses besides 429 fail immediately"
        );
        Ok(())
    }

    #[tokio::test]
    async fn json_parse_failure_quotes_body() -> TestResult {
        let url = api_url()?;
        let mock = Arc::new(MockHttpClient::with_response(response(
            &url,
            StatusCode::OK,
            HeaderMap::new(),
            "<html>Wikimedia Error</html>",
        )));
        let client = WikidataClient::with_min_request_interval(mock, Duration::ZERO);
        let ts = ApiTimestamp::try_from("2022-01-03T00:00:00Z".to_string())?;

        let err = client
            .resolve_revisions(&["Q243"], &ts)
            .await
            .err()
            .ok_or("expected HTML body to fail JSON parsing")?;

        let message = err.to_string();
        assert!(
            message.contains("invalid JSON"),
            "error should name the failure: {message}"
        );
        assert!(
            message.contains("Wikimedia Error"),
            "error should quote the body: {message}"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn paces_consecutive_requests() -> TestResult {
        let url = api_url()?;
        let mock = Arc::new(MockHttpClient::with_responses(vec![
            response(&url, StatusCode::OK, HeaderMap::new(), &revisions_json(1)),
            response(&url, StatusCode::OK, HeaderMap::new(), &revisions_json(2)),
        ]));
        let interval = Duration::from_millis(200);
        let client = WikidataClient::with_min_request_interval(Arc::clone(&mock), interval);
        let ts = ApiTimestamp::try_from("2022-01-03T00:00:00Z".to_string())?;

        let start = Instant::now();
        let revisions = client.resolve_revisions(&["Q1", "Q2"], &ts).await?;

        assert_eq!(revisions.len(), 2);
        assert_eq!(mock.request_count(), 2);
        assert!(
            start.elapsed() >= interval,
            "second request should wait out the pacing interval"
        );
        Ok(())
    }

    // =========================================================================
    // VCR fixture tests — recorded from real Wikidata/Commons API responses.
    //
    // Record fixtures: cargo test -p chronoscope-integrations --features record-fixtures
    // Replay fixtures: cargo test -p chronoscope-integrations
    // =========================================================================

    fn wikidata_fixtures_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/wikidata")
    }

    fn wikidata_vcr_client() -> Result<WikidataClient<crate::CachingClient>, crate::HttpError> {
        let mode = if cfg!(feature = "record-fixtures") {
            crate::CacheMode::Online
        } else {
            crate::CacheMode::Offline
        };
        let http = crate::CachingClient::new(wikidata_fixtures_dir(), mode)?;
        // Replay needs no pacing; recording leans on the 429 retry loop.
        Ok(WikidataClient::with_min_request_interval(
            http,
            Duration::ZERO,
        ))
    }

    #[tokio::test]
    async fn vcr_get_entities_at_revision() -> TestResult {
        let client = wikidata_vcr_client()?;
        let ts = ApiTimestamp::try_from("2022-01-03T00:00:00Z".to_string())?;

        // Resolve Eiffel Tower's revision at 2022-01-03, then fetch it
        let revisions = client.resolve_revisions(&["Q243"], &ts).await?;
        assert_eq!(revisions.len(), 1);
        let rev_id = revisions
            .get("Q243")
            .copied()
            .ok_or("Q243 not in results")?;
        assert!(rev_id.0 > 0, "revision ID should be positive");

        // Fetch at that revision
        let entities = client
            .get_entities_at_revisions(&[("Q243", rev_id)])
            .await?;
        assert_eq!(entities.len(), 1);
        let entity = entities.get("Q243").ok_or("Q243 not in results")?;
        assert_eq!(entity.id, "Q243");

        Ok(())
    }

    #[tokio::test]
    async fn vcr_resolve_revisions() -> TestResult {
        let client = wikidata_vcr_client()?;
        let ts = ApiTimestamp::try_from("2022-01-03T00:00:00Z".to_string())?;

        let revisions = client.resolve_revisions(&["Q243", "Q2981"], &ts).await?;

        assert_eq!(revisions.len(), 2);
        assert!(revisions.contains_key("Q243"));
        assert!(revisions.contains_key("Q2981"));

        // All revision IDs should be positive
        for rev_id in revisions.values() {
            assert!(rev_id.0 > 0);
        }

        Ok(())
    }

    #[tokio::test]
    async fn vcr_gallery_at_revision() -> TestResult {
        let client = wikidata_vcr_client()?;
        let ts = ApiTimestamp::try_from("2022-01-03T00:00:00Z".to_string())?;

        // Resolve Ayasofya gallery revision at a timestamp
        let result = client.resolve_gallery_revision("Ayasofya", &ts).await?;

        let (_page_id, rev_id) = result.ok_or("gallery page should exist")?;
        assert!(rev_id.0 > 0);

        // Fetch gallery at that revision
        let media = client.fetch_gallery_media_at_revision(rev_id).await?;

        assert!(!media.is_empty(), "historical gallery should have media");

        Ok(())
    }

    #[tokio::test]
    async fn vcr_gallery_missing_page() -> TestResult {
        let client = wikidata_vcr_client()?;
        let ts = ApiTimestamp::try_from("2022-01-03T00:00:00Z".to_string())?;

        let result = client
            .resolve_gallery_revision("NonexistentGalleryPage12345", &ts)
            .await?;

        assert!(result.is_none(), "nonexistent gallery should return None");

        Ok(())
    }
}
