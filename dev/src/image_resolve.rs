//! Resolve fact-store images into our own media store.
//!
//! **Temporary bridge — delete, don't refine.** This exists only so the
//! fact-store read-path flip didn't have to rework the research-URL fetcher
//! queue: it resolves fact-store image facts directly instead of routing them
//! through that queue. When the ingestion queues are unified this whole module goes
//! away — including its hand-rolled paced/retrying fetch and the two
//! `#[expect(clippy::disallowed_methods)]` sleeps. Don't swap those sleeps for a
//! rate-limit/backoff crate; the worker's queue-level retry replaces them then.
//!
//! Every fact-store image cites an upstream source URL (a Wikimedia Commons
//! file, typically). Rather than pointing browsers at that upstream host, we
//! serve each image — original and thumbnail — from our own `GET /media/{key}`
//! endpoint. This module walks every image in the store, resolves
//! each one into a pair of media-store keys, and returns the map the API's
//! read path consumes (`AppState::image_media`).
//!
//! Two modes share one store layout:
//! - [`ImageResolveMode::Fetch`] downloads the source URL over the SSRF-guarded
//!   [`HttpClient`], content-addresses it, and stores the original plus a
//!   generated JPEG thumbnail — reusing the url-fetcher primitives so the key
//!   scheme matches the research pipeline. It fetches politely: a descriptive
//!   User-Agent and paced request starts keep the resolver under
//!   `upload.wikimedia.org`'s burst rate limit, with retry as the safety net.
//! - [`ImageResolveMode::Placeholder`] stores one small deterministic JPEG at
//!   both keys, hitting no network. Browser tests use this: it gives every
//!   image a same-origin, CORS-serveable URL the map can draw to a canvas.
//!
//! Resolution is best-effort. A single image that fails to fetch, decode, or
//! store is logged and skipped; it never fails server startup.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use chronoscope_api::state::{
    ResolvedImageMedia, placeholder_storage_key, placeholder_thumbnail_key,
};
use chronoscope_core::projection::{member_lineage, project_image};
use chronoscope_core::store::schema::ImageStream;
use chronoscope_core::store::{FactStore, ImageIdOf, ImageView};
use chronoscope_core::typed;
use chronoscope_db::media_store::MediaStore;
use chronoscope_workers::url_fetcher::{ContentType, detect_content_type, store_image};
use chronoscope_workers::{HttpClient, HttpRequest, HttpResponse};
use reqwest::header::{CONTENT_TYPE, HeaderValue, RETRY_AFTER, USER_AGENT};
use url::Url;

/// Attempts per image before giving up (initial try plus retries).
const MAX_FETCH_ATTEMPTS: u32 = 4;

/// Base delay for exponential backoff when a response gives no `Retry-After`.
const BASE_BACKOFF: Duration = Duration::from_millis(500);

/// Longest we'll wait between attempts. `upload.wikimedia.org` throttles bursts
/// and can answer a 429 with `Retry-After: 600`; honoring that would stall
/// startup for ten minutes, so a wait past this cap becomes a skip instead.
const MAX_BACKOFF: Duration = Duration::from_secs(20);

/// Minimum gap between fetch starts. `upload.wikimedia.org` 429s a burst of
/// requests; spacing sequential fetches keeps the resolver comfortably under
/// its rate limit so we never trip the block (the retry in `fetch_with_retry`
/// is only the safety net). ~52 images at this spacing is a one-time,
/// acceptable dev-startup cost.
const FETCH_SPACING: Duration = Duration::from_millis(300);

/// Identifies the resolver to upstream hosts. Wikimedia's User-Agent policy
/// asks automated clients to name themselves and give a contact URL; a
/// descriptive UA plus pacing is the polite combination that keeps us served.
const RESOLVER_USER_AGENT: &str =
    "Chronoscope/0.1 (https://github.com/copumpkin/chronoscope; fact-store image resolver)";

/// How to resolve fact-store images into media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageResolveMode {
    /// Download each source URL and store the real original + thumbnail.
    Fetch,
    /// Store one deterministic placeholder JPEG per image — no network. Browser
    /// tests select this so every image has a same-origin, CORS-serveable URL the
    /// map can draw to its canvas thumbnail offline.
    Placeholder,
}

/// Page size for the image-class walk. One page covers the curated dev
/// snapshot; larger stores page through.
const WALK_PAGE: NonZeroUsize = match NonZeroUsize::new(256) {
    Some(n) => n,
    None => NonZeroUsize::MIN,
};

/// Walk every image in `store`, resolve each into media-store keys per `mode`,
/// and return the `image id -> keys` map for the API read path.
///
/// `http_client` is consulted only in [`ImageResolveMode::Fetch`]. Failures for
/// individual images are logged and skipped, so the returned map may be smaller
/// than the store's image count; the call itself never errors.
pub async fn resolve_fact_store_images<S>(
    store: &S,
    media_store: &Arc<dyn MediaStore>,
    http_client: &Arc<dyn HttpClient>,
    mode: ImageResolveMode,
) -> HashMap<ImageIdOf<S>, ResolvedImageMedia>
where
    S: FactStore,
    ImageIdOf<S>: Copy + std::fmt::Display,
{
    let mut resolved = HashMap::new();

    let mut view = match store.now().await {
        Ok(view) => view,
        Err(e) => {
            tracing::warn!(error = ?e, "fact-store image resolve: snapshot unavailable");
            return resolved;
        }
    };

    let image_ids = match image_representatives(&mut view).await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(error = ?e, "fact-store image resolve: enumeration failed");
            return resolved;
        }
    };

    let enumerated = image_ids.len();
    let mut no_source = 0usize;
    let mut failed = 0usize;
    for image_id in image_ids {
        match resolve_one(&mut view, image_id, media_store, http_client, mode).await {
            Ok(Some(media)) => {
                resolved.insert(image_id, media);
            }
            Ok(None) => no_source += 1,
            Err(e) => {
                failed += 1;
                tracing::warn!(image = %image_id, error = %e, "fact-store image resolve: skipped");
            }
        }
    }

    tracing::info!(
        enumerated,
        resolved = resolved.len(),
        no_source,
        failed,
        ?mode,
        "fact-store image resolution complete"
    );

    resolved
}

/// Every image's `SameArtifact` representative, deduplicated. Paging by
/// `next_class` visits each class once; consecutive-row dedup collapses a
/// class's multiple rows within a page.
async fn image_representatives<S, V>(view: &mut V) -> Result<Vec<ImageIdOf<S>>, S::Error>
where
    S: FactStore,
    ImageIdOf<S>: Copy,
    V: ImageView<S> + Sync,
{
    let mut after = None;
    let mut ids: Vec<ImageIdOf<S>> = Vec::new();
    loop {
        let page = view
            .walk_image_classes(&ImageStream::All, after, WALK_PAGE)
            .await?;
        for row in &page.rows {
            if ids.last() != Some(&row.representative) {
                ids.push(row.representative);
            }
        }
        match page.next_class {
            None => break,
            Some(next) => after = Some(next),
        }
    }
    Ok(ids)
}

/// Resolve one image to its media keys, or `None` when it carries no source URL
/// to serve.
async fn resolve_one<S, V>(
    view: &mut V,
    image_id: ImageIdOf<S>,
    media_store: &Arc<dyn MediaStore>,
    http_client: &Arc<dyn HttpClient>,
    mode: ImageResolveMode,
) -> Result<Option<ResolvedImageMedia>, Box<dyn std::error::Error + Send + Sync>>
where
    S: FactStore,
    ImageIdOf<S>: Copy + std::fmt::Display,
    V: ImageView<S> + Sync,
{
    let Some((class, projected)) = project_image::<S, _, _>(&mut *view, image_id, member_lineage)
        .await
        .map_err(|e| format!("{e:?}"))?
    else {
        return Ok(None);
    };
    let image = typed::Image::parse(&projected, &class);
    let Some(source_url) = image
        .urls
        .first()
        .map(|attributed| attributed.value.clone())
    else {
        return Ok(None);
    };

    let media = match mode {
        ImageResolveMode::Placeholder => {
            let bytes = crate::placeholder_jpeg()?;
            let storage_key = placeholder_storage_key(image_id);
            let thumbnail_key = placeholder_thumbnail_key(image_id);
            media_store
                .put(&storage_key, bytes.clone(), "image/jpeg")
                .await?;
            media_store.put(&thumbnail_key, bytes, "image/jpeg").await?;
            ResolvedImageMedia {
                storage_key,
                thumbnail_key,
            }
        }
        ImageResolveMode::Fetch => {
            // Space fetch starts so a bulk resolve stays under Wikimedia's burst
            // rate limit rather than tripping it and relying on retry recovery.
            #[expect(
                clippy::disallowed_methods,
                reason = "pace fetch starts under Wikimedia's burst rate limit; ~52 images x FETCH_SPACING is a one-time dev-startup cost"
            )]
            tokio::time::sleep(FETCH_SPACING).await;

            let response = fetch_with_retry(http_client, &source_url).await?;
            let body = response.body;

            let content_type_header = response
                .headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok());
            let format = match detect_content_type(content_type_header, &body) {
                ContentType::Image(format) => format,
                other => return Err(format!("{source_url} is not an image ({other:?})").into()),
            };

            let stored = store_image(media_store, &body, format).await?;
            // A best-effort thumbnail may have failed; fall back to the stored
            // original so the marker still shows this image rather than a broken
            // key. The browser downscales the full-resolution original.
            let thumbnail_key = stored
                .thumbnail_key
                .unwrap_or_else(|| stored.storage_key.clone());
            ResolvedImageMedia {
                storage_key: stored.storage_key,
                thumbnail_key,
            }
        }
    };

    Ok(Some(media))
}

/// Fetch `url`, retrying transient throttling. `upload.wikimedia.org` answers a
/// burst of requests with `429 Too Many Requests` (bulk-resolving a snapshot is
/// exactly such a burst), so a lone request loses most images. On a 429 or 5xx
/// we wait the server's `Retry-After` (else exponential backoff) and try again,
/// up to [`MAX_FETCH_ATTEMPTS`]; a wait past [`MAX_BACKOFF`] or a non-retryable
/// status returns an error the caller logs and skips.
async fn fetch_with_retry(
    http_client: &Arc<dyn HttpClient>,
    url: &Url,
) -> Result<HttpResponse, Box<dyn std::error::Error + Send + Sync>> {
    let mut attempt = 0u32;
    loop {
        let request = HttpRequest::get(url.clone())
            .header(USER_AGENT, HeaderValue::from_static(RESOLVER_USER_AGENT));
        let response = http_client.execute(request).await?;
        if response.is_success() {
            return Ok(response);
        }

        attempt += 1;
        let retryable = response.status.as_u16() == 429 || response.status.is_server_error();
        if !retryable || attempt >= MAX_FETCH_ATTEMPTS {
            return Err(format!("fetching {url} returned status {}", response.status).into());
        }

        let wait = retry_after(&response).unwrap_or(BASE_BACKOFF * attempt);
        if wait > MAX_BACKOFF {
            return Err(format!(
                "fetching {url} returned status {}; asked to wait {}s, over the {}s cap",
                response.status,
                wait.as_secs(),
                MAX_BACKOFF.as_secs()
            )
            .into());
        }

        #[expect(
            clippy::disallowed_methods,
            reason = "polite backoff between retries against a rate-limiting upstream; bounded by MAX_FETCH_ATTEMPTS and MAX_BACKOFF"
        )]
        tokio::time::sleep(wait).await;
    }
}

/// The `Retry-After` delay a response asks for, when expressed as an integer
/// number of seconds (the form `upload.wikimedia.org` sends). The HTTP-date
/// form is ignored — backoff covers it.
fn retry_after(response: &HttpResponse) -> Option<Duration> {
    let seconds = response
        .headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    use bytes::Bytes;
    use chronoscope_core::store::memory::MemoryFactStore;
    use chronoscope_integrations::MockHttpClient;
    use reqwest::StatusCode;
    use reqwest::header::HeaderMap;

    type BoxError = Box<dyn std::error::Error + Send + Sync>;
    type TestResult = Result<(), BoxError>;

    fn response(status: u16, retry_after: Option<&str>) -> Result<HttpResponse, BoxError> {
        let mut headers = HeaderMap::new();
        if let Some(seconds) = retry_after {
            headers.insert(RETRY_AFTER, seconds.parse()?);
        }
        Ok(HttpResponse {
            status: StatusCode::from_u16(status)?,
            headers,
            body: Bytes::from_static(b"image-bytes"),
            final_url: Url::parse("https://upload.wikimedia.org/x.jpg")?,
        })
    }

    fn mock(responses: Vec<HttpResponse>) -> Arc<MockHttpClient> {
        Arc::new(MockHttpClient::with_responses(
            responses.into_iter().map(Ok).collect(),
        ))
    }

    async fn fetch(mock: &Arc<MockHttpClient>) -> Result<HttpResponse, BoxError> {
        let client: Arc<dyn HttpClient> = mock.clone();
        let url = Url::parse("https://upload.wikimedia.org/x.jpg")?;
        fetch_with_retry(&client, &url).await
    }

    // `start_paused` makes `tokio::time::sleep` auto-advance, so the backoff
    // waits complete instantly in test time — deterministic, no real delay.

    #[tokio::test(start_paused = true)]
    async fn retries_through_429s_then_succeeds() -> TestResult {
        let mock = mock(vec![
            response(429, Some("1"))?,
            response(429, None)?,
            response(200, None)?,
        ]);
        let resp = fetch(&mock).await?;
        assert_eq!(resp.status, StatusCode::OK);
        assert_eq!(mock.request_count(), 3, "should retry twice before the 200");
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_after_exhausting_attempts() -> TestResult {
        let mock = mock(vec![
            response(429, None)?,
            response(429, None)?,
            response(429, None)?,
            response(429, None)?,
        ]);
        assert!(
            fetch(&mock).await.is_err(),
            "persistent 429 yields an error"
        );
        assert_eq!(
            mock.request_count(),
            MAX_FETCH_ATTEMPTS as usize,
            "stops at the attempt cap"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn skips_immediately_when_retry_after_exceeds_cap() -> TestResult {
        // A 600s Retry-After (what upload.wikimedia.org hands out under a burst
        // block) is over the cap, so we give up without a second request.
        let mock = mock(vec![response(429, Some("600"))?]);
        assert!(fetch(&mock).await.is_err(), "an over-cap wait is a skip");
        assert_eq!(
            mock.request_count(),
            1,
            "no retry once the asked wait exceeds the cap"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn does_not_retry_a_permanent_status() -> TestResult {
        let mock = mock(vec![response(404, None)?]);
        assert!(fetch(&mock).await.is_err(), "404 is not retryable");
        assert_eq!(mock.request_count(), 1, "a 404 is not retried");
        Ok(())
    }

    /// A store of two images, each with a distinct source URL — enough for the
    /// Fetch loop to make two paced network requests.
    async fn two_image_store() -> Result<MemoryFactStore, BoxError> {
        use chronoscope_core::grammar::assertions::FactualAssertion;
        use chronoscope_core::grammar::citations::{Excerpt, ExternalSource, FactualCitation};
        use chronoscope_core::grammar::ids::UserId;
        use chronoscope_core::grammar::image;
        use chronoscope_core::store::memory::MemoryIds;
        use chronoscope_core::submit::{
            Commit, CommitAuthor, Decl, ImageIdx, SubmitFact, commit_facts,
        };

        let source = |idx: usize, url: &str| -> Result<SubmitFact, BoxError> {
            let parsed = Url::parse(url)?;
            let citation = FactualCitation::new(
                ExternalSource::Url {
                    url: parsed.clone(),
                    published: None,
                },
                vec![Excerpt::new("src")?],
            )?;
            Ok(SubmitFact::Factual {
                assertion: FactualAssertion::Image {
                    fact: image::Fact::Source {
                        image: ImageIdx(idx),
                        url: parsed,
                    },
                },
                citation,
            })
        };

        let commit = Commit::<MemoryIds> {
            author: CommitAuthor::User(UserId::new("test")),
            recorded_at: chrono::Utc::now(),
            entities: Vec::new(),
            events: Vec::new(),
            images: vec![Decl::Local, Decl::Local],
            facts: [
                source(0, "https://upload.wikimedia.org/a.jpg")?,
                source(1, "https://upload.wikimedia.org/b.jpg")?,
            ]
            .into_iter()
            .collect(),
        };

        let store = MemoryFactStore::new();
        commit_facts(&store, commit)
            .await
            .map_err(|e| format!("{e:?}"))?;
        Ok(store)
    }

    fn jpeg_response(body: &Bytes) -> Result<HttpResponse, BoxError> {
        Ok(HttpResponse {
            status: StatusCode::from_u16(200)?,
            headers: HeaderMap::new(),
            body: body.clone(),
            final_url: Url::parse("https://upload.wikimedia.org/x.jpg")?,
        })
    }

    #[tokio::test(start_paused = true)]
    async fn fetch_mode_paces_request_starts() -> TestResult {
        use chronoscope_db::media_store::{InMemoryMediaStore, MediaStore};

        let store = two_image_store().await?;
        let media_store: Arc<dyn MediaStore> = Arc::new(InMemoryMediaStore::new());
        // A real (tiny) JPEG so decode + thumbnail succeed and both images
        // resolve; the resolver's only sleeps are then the pacing gaps.
        let jpeg = crate::placeholder_jpeg()?;
        let client: Arc<dyn HttpClient> = mock(vec![jpeg_response(&jpeg)?, jpeg_response(&jpeg)?]);

        let start = tokio::time::Instant::now();
        let resolved =
            resolve_fact_store_images(&store, &media_store, &client, ImageResolveMode::Fetch).await;
        let elapsed = start.elapsed();

        assert_eq!(
            resolved.len(),
            2,
            "both images resolve from the JPEG responses"
        );
        assert!(
            elapsed >= FETCH_SPACING,
            "paced fetch starts advance virtual time by at least one gap; elapsed {elapsed:?}"
        );
        Ok(())
    }
}
