//! The dev-side mirror pipeline: warm fact-store images into our own media store
//! for the dev CDN.
//!
//! This is the dev equivalent of the production mirror. The reused production
//! walk ([`stream_mirror_requests`]) is the producer, an in-process channel is
//! the queue, and [`consume`] is the Rust twin of the Cloudflare Worker in
//! `integrations/mirror-consumer.js`. Only the transport differs from prod: an
//! in-process channel in place of Cloudflare Queues.
//!
//! The consumer is decoupled and non-blocking: [`spawn_media_consumer`] runs it as
//! an independent background task and [`dispatch_media_warm`] only pushes onto the
//! queue and returns, so the sweep never waits on a fetch — exactly like prod's
//! enqueue. `start_dev_server` dispatches at startup and serves immediately, images
//! streaming in as they warm; browser tests await `RunningDevServer::await_media_warmed`
//! for determinism. When the continuous mirror phase lands, a continuous producer
//! replaces the one-shot startup sweep and feeds the same queue and consumer,
//! exactly as prod's continuous sweep will feed Cloudflare Queues.
//!
//! Two modes share one store layout:
//! - [`ImageResolveMode::Fetch`] downloads each source URL over the SSRF-guarded
//!   [`HttpClient`] and stores the raw bytes under its dev media key. It fetches
//!   politely: a paced start plus in-job retry keeps it under
//!   `upload.wikimedia.org`'s burst rate limit.
//! - [`ImageResolveMode::Placeholder`] stores one small deterministic JPEG per
//!   image, hitting no network. Browser tests use this: every image gets a
//!   same-origin, offline URL the map can draw to a canvas.
//!
//! Warming is best-effort. A single image that fails to fetch or store is logged
//! and skipped; it never fails server startup.

use std::sync::Arc;
use std::time::Duration;

use chronoscope_api::mirror::stream_mirror_requests;
use chronoscope_core::store::FactStore;
use chronoscope_db::media_store::MediaStore;
use chronoscope_integrations::MirrorRequest;
use chronoscope_workers::url_fetcher::{ContentType, detect_content_type};
use chronoscope_workers::{HttpClient, HttpRequest, HttpResponse};
use futures_util::StreamExt;
use reqwest::header::{CONTENT_TYPE, HeaderValue, RETRY_AFTER, USER_AGENT};
use tokio::sync::{mpsc, watch};
use url::Url;

/// Attempts per image before giving up (initial try plus retries).
const MAX_FETCH_ATTEMPTS: u32 = 4;

/// Base delay for the per-attempt linear backoff when a response gives no
/// `Retry-After`.
const BASE_BACKOFF: Duration = Duration::from_millis(500);

/// Longest we'll wait between attempts. `upload.wikimedia.org` throttles bursts
/// and can answer a 429 with `Retry-After: 600`; honoring that would stall the
/// serial consumer for ten minutes, so a wait past this cap becomes a drop.
const MAX_BACKOFF: Duration = Duration::from_secs(20);

/// Minimum gap between fetch starts. `upload.wikimedia.org` 429s a burst of
/// requests; spacing fetches keeps the consumer comfortably under its rate limit
/// so it rarely trips the block (in-job retry is only the safety net). The
/// consumer is serial, so one gap per message paces the whole sweep.
const FETCH_SPACING: Duration = Duration::from_millis(300);

/// How to resolve fact-store images into media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageResolveMode {
    /// Download each source URL and store the raw bytes.
    Fetch,
    /// Store one deterministic placeholder JPEG per image — no network. Browser
    /// tests select this so every image has a same-origin, offline URL the map
    /// can draw to its canvas thumbnail.
    Placeholder,
}

/// Spawn the decoupled media consumer: a background task that drains the queue,
/// fetching/storing each request through [`consume`], for as long as senders
/// live. It is never joined by the sweep, so dispatching a request never blocks
/// on a fetch — the dev equivalent of the production queue consumer running
/// independently of the enqueue.
///
/// Returns the queue's sending half (the dispatch handle) and a [`watch`] carrying
/// the count of requests processed so far, which [`warm_and_drain`] and
/// `RunningDevServer::await_media_warmed` use to tell when a one-shot sweep has
/// fully drained.
pub(crate) fn spawn_media_consumer(
    media_store: Arc<dyn MediaStore>,
    http_client: Arc<dyn HttpClient>,
    mode: ImageResolveMode,
) -> (mpsc::UnboundedSender<MirrorRequest>, watch::Receiver<usize>) {
    let (queue_tx, mut queue_rx) = mpsc::unbounded_channel::<MirrorRequest>();
    let (progress_tx, progress_rx) = watch::channel(0usize);
    tokio::spawn(async move {
        let mut warmed = 0usize;
        let mut skipped = 0usize;
        let mut dropped = 0usize;
        let mut processed = 0usize;
        while let Some(request) = queue_rx.recv().await {
            match consume(&request, &media_store, &http_client, mode).await {
                Disposition::Stored => warmed += 1,
                Disposition::Skipped => skipped += 1,
                Disposition::Dropped => dropped += 1,
            }
            processed += 1;
            // A closed receiver just means nothing is waiting on the drain
            // signal (a live dev server never is); keep draining regardless.
            let _ = progress_tx.send(processed);
        }
        tracing::info!(
            warmed,
            skipped,
            dropped,
            ?mode,
            "fact-store media warm complete"
        );
    });
    (queue_tx, progress_rx)
}

/// Dispatch a one-shot sweep onto the consumer's queue: walk every fact-store
/// image and hand off a [`MirrorRequest`] per displayable URL, returning how many
/// were dispatched. Non-blocking on fetches — an unbounded send never awaits, so
/// this pushes every request and returns, exactly like the production sweep
/// enqueuing to Cloudflare Queues. A walk error ends the sweep early; the requests
/// already dispatched still warm.
pub(crate) async fn dispatch_media_warm<S>(
    store: &S,
    queue: &mpsc::UnboundedSender<MirrorRequest>,
) -> usize
where
    S: FactStore,
{
    let stream = stream_mirror_requests(store);
    tokio::pin!(stream);
    let mut dispatched = 0usize;
    while let Some(item) = stream.next().await {
        match item {
            Ok(request) => {
                if queue.send(request).is_err() {
                    // The consumer is gone; nothing more can be warmed.
                    break;
                }
                dispatched += 1;
            }
            Err(error) => {
                tracing::warn!(error = ?error, "fact-store media warm: walk stopped early");
                break;
            }
        }
    }
    dispatched
}

/// Run a one-shot media warm and block until it fully drains: spawn the consumer,
/// dispatch every image, and return once every dispatched request has been
/// consumed, reporting how many were dispatched.
///
/// This is the blocking convenience for callers that need the media present before
/// proceeding — the tests, and any future blocking CLI warm.
/// [`start_dev_server`](crate::start_dev_server) does NOT use it: it dispatches and
/// lets the consumer drain in the background so startup never waits on fetches, and
/// exposes `RunningDevServer::await_media_warmed` for the browser tests that do want
/// to wait.
pub async fn warm_and_drain<S>(
    store: &S,
    media_store: &Arc<dyn MediaStore>,
    http_client: &Arc<dyn HttpClient>,
    mode: ImageResolveMode,
) -> usize
where
    S: FactStore,
{
    let (queue, mut progress) =
        spawn_media_consumer(media_store.clone(), http_client.clone(), mode);
    let dispatched = dispatch_media_warm(store, &queue).await;
    // Close the queue so the consumer finishes once it has drained the one-shot
    // sweep.
    drop(queue);
    // Wait until every dispatched request has been processed, or the consumer's
    // sender drops (it finished, or panicked) — either way nothing more is coming.
    while *progress.borrow_and_update() < dispatched {
        if progress.changed().await.is_err() {
            break;
        }
    }
    dispatched
}

/// What one message did. Mirrors the JS consumer's terminal actions: `Stored`
/// and `Skipped` are its `ack` after a store or an already-present key, `Dropped`
/// is its log-and-`ack` on a permanent failure. The JS `retry` has no variant
/// here because dev retries in-job (a sleep-and-loop inside [`fetch_with_retry`])
/// instead of re-injecting the message onto the queue.
enum Disposition {
    Stored,
    Skipped,
    Dropped,
}

/// Fetch (or fake) one image and store it under its dev media key.
///
/// Conceptual twin of `integrations/mirror-consumer.js`, the production queue
/// consumer. Both take a `MirrorRequest` and land its bytes at the key; they stay
/// two trivial implementations rather than one shared one because that one runs
/// in a Cloudflare Worker (JS) and this runs in-process (Rust). While both stay
/// this small the duplication is cheaper than a cross-language shared core (WASM
/// or otherwise); the day either grows real policy, that is the signal to dedupe.
/// A change to one is a prompt to check the other.
///
/// Deliberate divergences, because dev defends none of the boundaries prod does:
/// dev retries synchronously in-job instead of re-injecting on the queue, stores
/// under the flat `media/{hash}` dev key instead of the R2 mirror key, omits the
/// manual-redirect and RP-scope guards (no untrusted upstream reaches a
/// scriptable-document host here), and enforces neither `request.accept` (it
/// stores any sniffed image type) nor `request.max_bytes` (no length cap; a large
/// original loads whole into the in-memory store).
async fn consume(
    request: &MirrorRequest,
    media_store: &Arc<dyn MediaStore>,
    http_client: &Arc<dyn HttpClient>,
    mode: ImageResolveMode,
) -> Disposition {
    let store_key = chronoscope_api::cdn::local_media_key_for_mirror_key(&request.key);

    // Already warmed: the key is present, so we are done without fetching — the
    // skip that makes re-running the whole sweep cheap.
    match media_store.get(&store_key).await {
        Ok(Some(_)) => return Disposition::Skipped,
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(key = %store_key, error = ?error, "warm: media read failed");
            return Disposition::Dropped;
        }
    }

    match mode {
        ImageResolveMode::Placeholder => store_placeholder(media_store, &store_key).await,
        ImageResolveMode::Fetch => {
            fetch_and_store(request, media_store, http_client, &store_key).await
        }
    }
}

/// Store one deterministic JPEG under `store_key`, hitting no network.
async fn store_placeholder(media_store: &Arc<dyn MediaStore>, store_key: &str) -> Disposition {
    let bytes = match crate::placeholder_jpeg() {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, "warm: placeholder encode failed");
            return Disposition::Dropped;
        }
    };
    match media_store.put(store_key, bytes, "image/jpeg").await {
        Ok(()) => Disposition::Stored,
        Err(error) => {
            tracing::warn!(key = %store_key, error = ?error, "warm: placeholder store failed");
            Disposition::Dropped
        }
    }
}

/// Fetch `request.url` and store the raw bytes under `store_key`.
async fn fetch_and_store(
    request: &MirrorRequest,
    media_store: &Arc<dyn MediaStore>,
    http_client: &Arc<dyn HttpClient>,
    store_key: &str,
) -> Disposition {
    // Space fetch starts so a bulk warm stays under Wikimedia's burst rate limit
    // rather than tripping it and leaning on retry recovery.
    #[expect(
        clippy::disallowed_methods,
        reason = "pace fetch starts under Wikimedia's burst rate limit; the serial consumer spaces the whole sweep at one gap per image"
    )]
    tokio::time::sleep(FETCH_SPACING).await;

    let response = match fetch_with_retry(http_client, &request.url, &request.user_agent).await {
        Ok(response) => response,
        Err(reason) => {
            tracing::warn!(url = %request.url, %reason, "warm: gave up fetching");
            return Disposition::Dropped;
        }
    };

    // The extension was a claim; the true type is the response's. Sniff it and
    // store the raw bytes only when they are an image — the same second gate the
    // JS consumer applies. Neither side decodes.
    let format = {
        let header = response
            .headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok());
        match detect_content_type(header, &response.body) {
            ContentType::Image(format) => format,
            other => {
                tracing::warn!(url = %request.url, ?other, "warm: response is not an image");
                return Disposition::Dropped;
            }
        }
    };

    match media_store
        .put(store_key, response.body, format.mime_type())
        .await
    {
        Ok(()) => Disposition::Stored,
        Err(error) => {
            tracing::warn!(key = %store_key, error = ?error, "warm: store write failed");
            Disposition::Dropped
        }
    }
}

/// One fetch attempt's classification, mirroring the JS consumer's decisions:
/// `Done` is its `ack` on a good response, `Retry` its `message.retry` on
/// transient throttling, `Drop` its log-and-`ack` on a permanent status. Dev acts
/// on `Retry` in-job (sleep and loop) rather than re-injecting.
enum Attempt {
    Done(HttpResponse),
    Retry(Duration),
    Drop(String),
}

/// Classify a response, given the attempt count for backoff sizing.
fn classify(response: HttpResponse, attempt: u32) -> Attempt {
    if response.is_success() {
        return Attempt::Done(response);
    }
    let retryable = response.status.as_u16() == 429 || response.status.is_server_error();
    if !retryable {
        return Attempt::Drop(format!("status {}", response.status));
    }
    let wait = retry_after(&response).unwrap_or(BASE_BACKOFF * attempt);
    if wait > MAX_BACKOFF {
        return Attempt::Drop(format!(
            "status {}; asked to wait {}s, over the {}s cap",
            response.status,
            wait.as_secs(),
            MAX_BACKOFF.as_secs()
        ));
    }
    Attempt::Retry(wait)
}

/// Fetch `url`, retrying transient throttling in-job. `upload.wikimedia.org`
/// answers a burst of requests with `429 Too Many Requests`, so a lone request
/// loses most images. On a 429 or 5xx we wait the server's `Retry-After` (else
/// a per-attempt linear backoff) and try again, up to [`MAX_FETCH_ATTEMPTS`]; a permanent
/// status, an over-cap wait, or the attempt cap returns an error the caller logs
/// and drops.
async fn fetch_with_retry(
    http_client: &Arc<dyn HttpClient>,
    url: &Url,
    user_agent: &str,
) -> Result<HttpResponse, String> {
    let user_agent = HeaderValue::from_str(user_agent)
        .map_err(|error| format!("invalid user agent `{user_agent}`: {error}"))?;
    let mut attempt = 0u32;
    loop {
        let request = HttpRequest::get(url.clone()).header(USER_AGENT, user_agent.clone());
        let response = http_client
            .execute(request)
            .await
            .map_err(|error| error.to_string())?;
        attempt += 1;
        match classify(response, attempt) {
            Attempt::Done(response) => return Ok(response),
            Attempt::Drop(reason) => return Err(reason),
            Attempt::Retry(_) if attempt >= MAX_FETCH_ATTEMPTS => {
                return Err(format!(
                    "still throttled after {MAX_FETCH_ATTEMPTS} attempts"
                ));
            }
            Attempt::Retry(wait) => {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "polite in-job backoff between retries against a rate-limiting upstream; bounded by MAX_FETCH_ATTEMPTS and MAX_BACKOFF"
                )]
                tokio::time::sleep(wait).await;
            }
        }
    }
}

/// The `Retry-After` delay a response asks for, when expressed as an integer
/// number of seconds (the form `upload.wikimedia.org` sends). The HTTP-date form
/// is ignored — backoff covers it.
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
    use chronoscope_db::media_store::InMemoryMediaStore;
    use chronoscope_integrations::{DisplayableKey, MockHttpClient};
    use reqwest::StatusCode;
    use reqwest::header::HeaderMap;

    type BoxError = Box<dyn std::error::Error + Send + Sync>;
    type TestResult = Result<(), BoxError>;

    const TEST_USER_AGENT: &str = "Chronoscope-test/0.1 (https://chronoscope.io)";

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

    async fn fetch(mock: &Arc<MockHttpClient>) -> Result<HttpResponse, String> {
        let client: Arc<dyn HttpClient> = mock.clone();
        let url = Url::parse("https://upload.wikimedia.org/x.jpg").map_err(|e| e.to_string())?;
        fetch_with_retry(&client, &url, TEST_USER_AGENT).await
    }

    // `start_paused` makes `tokio::time::sleep` auto-advance, so the backoff and
    // pacing waits complete instantly in test time — deterministic, no real delay.

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
    async fn drops_immediately_when_retry_after_exceeds_cap() -> TestResult {
        // A 600s Retry-After (what upload.wikimedia.org hands out under a burst
        // block) is over the cap, so we give up without a second request rather
        // than stall the serial consumer.
        let mock = mock(vec![response(429, Some("600"))?]);
        assert!(fetch(&mock).await.is_err(), "an over-cap wait is a drop");
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

    /// A `MirrorRequest` for `url`, as the walk would emit it.
    fn request_for(url: &str) -> Result<MirrorRequest, BoxError> {
        let parsed = Url::parse(url)?;
        let key = DisplayableKey::for_url(&parsed)?.ok_or("the url is displayable")?;
        Ok(MirrorRequest::new(
            key.key().as_str().to_string(),
            parsed,
            TEST_USER_AGENT.to_string(),
            vec!["image/jpeg".to_string()],
            104_857_600,
        ))
    }

    fn jpeg_response(body: &Bytes) -> Result<HttpResponse, BoxError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, "image/jpeg".parse()?);
        Ok(HttpResponse {
            status: StatusCode::from_u16(200)?,
            headers,
            body: body.clone(),
            final_url: Url::parse("https://upload.wikimedia.org/x.jpg")?,
        })
    }

    #[tokio::test]
    async fn head_skip_leaves_a_present_key_untouched_and_makes_no_request() -> TestResult {
        let media_store: Arc<dyn MediaStore> = Arc::new(InMemoryMediaStore::new());
        let request = request_for("https://upload.wikimedia.org/wikipedia/commons/a/ab/Foo.jpg")?;
        let store_key = chronoscope_api::cdn::local_media_key_for_mirror_key(&request.key);
        media_store
            .put(
                &store_key,
                Bytes::from_static(b"already here"),
                "image/jpeg",
            )
            .await?;

        let mock = mock(vec![]);
        let client: Arc<dyn HttpClient> = mock.clone();
        let disposition = consume(&request, &media_store, &client, ImageResolveMode::Fetch).await;

        assert!(matches!(disposition, Disposition::Skipped));
        assert_eq!(mock.request_count(), 0, "a present key is not re-fetched");
        Ok(())
    }

    #[tokio::test]
    async fn placeholder_mode_stores_without_a_request() -> TestResult {
        let media_store: Arc<dyn MediaStore> = Arc::new(InMemoryMediaStore::new());
        let request = request_for("https://upload.wikimedia.org/wikipedia/commons/a/ab/Foo.jpg")?;
        let mock = mock(vec![]);
        let client: Arc<dyn HttpClient> = mock.clone();

        let disposition = consume(
            &request,
            &media_store,
            &client,
            ImageResolveMode::Placeholder,
        )
        .await;

        assert!(matches!(disposition, Disposition::Stored));
        let store_key = chronoscope_api::cdn::local_media_key_for_mirror_key(&request.key);
        assert!(media_store.get(&store_key).await?.is_some());
        assert_eq!(mock.request_count(), 0, "placeholder hits no network");
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn a_non_image_response_is_dropped_and_stored_nowhere() -> TestResult {
        let media_store: Arc<dyn MediaStore> = Arc::new(InMemoryMediaStore::new());
        let request = request_for("https://upload.wikimedia.org/wikipedia/commons/a/ab/Foo.jpg")?;

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, "text/html".parse()?);
        let html = HttpResponse {
            status: StatusCode::from_u16(200)?,
            headers,
            body: Bytes::from_static(b"<!doctype html><title>not an image</title>"),
            final_url: request.url.clone(),
        };
        let client: Arc<dyn HttpClient> = mock(vec![html]);

        let disposition = consume(&request, &media_store, &client, ImageResolveMode::Fetch).await;

        assert!(matches!(disposition, Disposition::Dropped));
        let store_key = chronoscope_api::cdn::local_media_key_for_mirror_key(&request.key);
        assert!(
            media_store.get(&store_key).await?.is_none(),
            "a non-image response stores nothing"
        );
        Ok(())
    }

    /// A store of two images, each with a distinct source URL — enough for the
    /// producer to stream two messages and the consumer to make two paced fetches.
    async fn two_image_store() -> Result<MemoryFactStore, BoxError> {
        use chronoscope_core::grammar::assertions::FactualAssertion;
        use chronoscope_core::grammar::citations::{Excerpt, ExternalSource, FactualCitation};
        use chronoscope_core::grammar::ids::UserId;
        use chronoscope_core::grammar::image;
        use chronoscope_core::store::memory::MemoryIds;
        use chronoscope_core::submit::{Commit, CommitAuthor, Decl, ImageIdx, SubmitFact};

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
            author: CommitAuthor::User(UserId::new("test")?),
            recorded_at: chrono::Utc::now(),
            entities: Vec::new(),
            events: Vec::new(),
            images: vec![Decl::Local, Decl::Local],
            facts: [
                source(
                    0,
                    "https://upload.wikimedia.org/wikipedia/commons/a/aa/One.jpg",
                )?,
                source(
                    1,
                    "https://upload.wikimedia.org/wikipedia/commons/b/bb/Two.jpg",
                )?,
            ]
            .into_iter()
            .collect(),
        };

        let store = MemoryFactStore::new();
        chronoscope_core::submit::commit_facts(&store, commit)
            .await
            .map_err(|e| format!("{e:?}"))?;
        Ok(store)
    }

    /// A commit declaring `n` distinct displayable Commons images by source URL —
    /// the seed both the standalone-store and the server-context repros warm.
    fn image_commit(
        n: usize,
    ) -> Result<chronoscope_core::submit::Commit<chronoscope_api::state::ServerIds>, BoxError> {
        use chronoscope_core::grammar::assertions::FactualAssertion;
        use chronoscope_core::grammar::citations::{Excerpt, ExternalSource, FactualCitation};
        use chronoscope_core::grammar::ids::UserId;
        use chronoscope_core::grammar::image;
        use chronoscope_core::submit::{Commit, CommitAuthor, Decl, ImageIdx, SubmitFact};

        let source = |idx: usize| -> Result<SubmitFact, BoxError> {
            let url = Url::parse(&format!(
                "https://upload.wikimedia.org/wikipedia/commons/a/aa/Img{idx}.jpg"
            ))?;
            // The seed count only matters if each URL is displayable; guard it.
            DisplayableKey::for_url(&url)?.ok_or("seed URL must be displayable")?;
            let citation = FactualCitation::new(
                ExternalSource::Url {
                    url: url.clone(),
                    published: None,
                },
                vec![Excerpt::new("src")?],
            )?;
            Ok(SubmitFact::Factual {
                assertion: FactualAssertion::Image {
                    fact: image::Fact::Source {
                        image: ImageIdx(idx),
                        url,
                    },
                },
                citation,
            })
        };

        Ok(Commit {
            author: CommitAuthor::User(UserId::new("test")?),
            recorded_at: chrono::Utc::now(),
            entities: Vec::new(),
            events: Vec::new(),
            images: (0..n).map(|_| Decl::Local).collect(),
            facts: (0..n)
                .map(source)
                .collect::<Result<std::collections::BTreeSet<_>, _>>()?,
        })
    }

    /// Startup dispatches the warm without blocking, and the background drain
    /// completes, inside the full server/worker context.
    ///
    /// Mirrors the browser test's server path without a browser: real
    /// `start_dev_server` (its URL-fetcher workers and axum server share the one
    /// current-thread runtime with the warm), a standalone `Writable` fact store
    /// seeded via `seed_commits`, Placeholder warming. `start_dev_server` must
    /// return promptly (the warm no longer blocks it); `await_media_warmed` then
    /// drains, and the timeouts turn a startup or drain deadlock into a clean
    /// failure rather than a hung suite.
    #[tokio::test]
    async fn start_dev_server_completes_with_a_warming_load() -> TestResult {
        use chronoscope_workers::{ReqwestClient, RetryConfig};
        use dropshot::{ConfigLogging, ConfigLoggingLevel};

        let dir = tempfile::tempdir()?;
        let facts_overlay = dir.path().join("facts.db").display().to_string();
        let port = crate::find_available_port()?;
        let log = ConfigLogging::StderrTerminal {
            level: ConfigLoggingLevel::Warn,
        }
        .to_logger("warm-repro")?;
        let http_client: Arc<dyn HttpClient> = Arc::new(ReqwestClient::new()?);

        let config = crate::DevServerConfig {
            database_url: Some("sqlite::memory:".into()),
            facts: crate::FactsDbSource::Writable(facts_overlay),
            seed_commits: vec![image_commit(70)?],
            http_client,
            worker_idle_backoff: std::time::Duration::from_secs(60),
            retry_config: RetryConfig::default(),
            log,
            port,
            cdn_base_url: format!("http://127.0.0.1:{port}/api"),
            image_resolve: ImageResolveMode::Placeholder,
            rp_id: None,
            rp_origin: None,
            ios_app_id: None,
            apify_config: None,
            dns_resolver: chronoscope_api::state::permissive_dns_resolver(),
        };

        let server = tokio::time::timeout(
            std::time::Duration::from_secs(45),
            crate::start_dev_server(config),
        )
        .await
        .map_err(|_| "start_dev_server hung")??;
        // Startup no longer blocks on the warm; the drain runs in the background.
        // Await it here (as the browser harness does) — this is where a warm
        // deadlock would now surface.
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            server.await_media_warmed(),
        )
        .await
        .map_err(|_| "media warm drain deadlocked")?;
        server.shutdown().await;
        Ok(())
    }

    /// Build a durable, mountable base file seeded with `n` images: a standalone
    /// store, committed, then stamped (which `TRUNCATE`-checkpoints the WAL into
    /// the file, so an `immutable=1` mount does not read back empty) and closed.
    /// Returns the base path and the `TempDir` holding it, which must outlive use.
    async fn written_base(n: usize) -> Result<(std::path::PathBuf, tempfile::TempDir), BoxError> {
        use chronoscope_db::{FactStoreLocations, SqliteFactStore};

        let dir = tempfile::tempdir()?;
        let base_path = dir.path().join("base.db");
        let base = SqliteFactStore::open(FactStoreLocations::standalone_at(&base_path)?)
            .await
            .map_err(|e| format!("{e:?}"))?;
        chronoscope_core::submit::commit_facts(&base, image_commit(n)?)
            .await
            .map_err(|e| format!("{e:?}"))?;
        base.stamp_codec_version()
            .await
            .map_err(|e| format!("{e:?}"))?;
        base.close().await;
        Ok((base_path, dir))
    }

    /// The browser test's actual warm path: `start_dev_server` (URL-fetcher
    /// workers + axum share the one current-thread runtime with the warm) over a
    /// **mounted** store (`mode=ro&immutable=1` base beneath a fresh overlay) —
    /// the store shape that wedged the old blocking warm. Startup must return
    /// promptly and the background drain must complete.
    #[tokio::test]
    async fn start_dev_server_completes_with_a_mounted_warming_load() -> TestResult {
        use chronoscope_workers::{ReqwestClient, RetryConfig};
        use dropshot::{ConfigLogging, ConfigLoggingLevel};

        let (base_path, dir) = written_base(70).await?;
        let overlay = dir.path().join("overlay.db").display().to_string();
        let port = crate::find_available_port()?;
        let log = ConfigLogging::StderrTerminal {
            level: ConfigLoggingLevel::Warn,
        }
        .to_logger("warm-repro")?;
        let http_client: Arc<dyn HttpClient> = Arc::new(ReqwestClient::new()?);

        let config = crate::DevServerConfig {
            database_url: Some("sqlite::memory:".into()),
            facts: crate::FactsDbSource::Mounted {
                base: base_path.display().to_string(),
                overlay,
            },
            seed_commits: Vec::new(),
            http_client,
            worker_idle_backoff: std::time::Duration::from_secs(60),
            retry_config: RetryConfig::default(),
            log,
            port,
            cdn_base_url: format!("http://127.0.0.1:{port}/api"),
            image_resolve: ImageResolveMode::Placeholder,
            rp_id: None,
            rp_origin: None,
            ios_app_id: None,
            apify_config: None,
            dns_resolver: chronoscope_api::state::permissive_dns_resolver(),
        };

        let server = tokio::time::timeout(
            std::time::Duration::from_secs(45),
            crate::start_dev_server(config),
        )
        .await
        .map_err(|_| "start_dev_server hung on a mounted store")??;
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            server.await_media_warmed(),
        )
        .await
        .map_err(|_| "media warm drain deadlocked on a mounted store")?;
        server.shutdown().await;
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn fetch_mode_warms_a_key_per_image_and_paces_starts() -> TestResult {
        let store = two_image_store().await?;
        let media_store: Arc<dyn MediaStore> = Arc::new(InMemoryMediaStore::new());
        // A real (tiny) JPEG so the sniff succeeds and both images warm; the
        // warm's only waits are then the pacing gaps.
        let jpeg = crate::placeholder_jpeg()?;
        let client: Arc<dyn HttpClient> = mock(vec![jpeg_response(&jpeg)?, jpeg_response(&jpeg)?]);

        let start = tokio::time::Instant::now();
        let dispatched =
            warm_and_drain(&store, &media_store, &client, ImageResolveMode::Fetch).await;
        let elapsed = start.elapsed();

        assert_eq!(
            dispatched, 2,
            "both images are dispatched and warm one key from their JPEG responses"
        );
        // The bytes land under the dev media key `LocalCdn` builds its URL from
        // (`media/{hash}`), served by the existing `/media/{key}` route.
        for url in [
            "https://upload.wikimedia.org/wikipedia/commons/a/aa/One.jpg",
            "https://upload.wikimedia.org/wikipedia/commons/b/bb/Two.jpg",
        ] {
            let key =
                DisplayableKey::for_url(&Url::parse(url)?)?.ok_or("the url is displayable")?;
            let store_key = chronoscope_api::cdn::local_media_key(&key);
            assert!(
                media_store.get(&store_key).await?.is_some(),
                "the fetched bytes are stored under {store_key}"
            );
        }
        assert!(
            elapsed >= FETCH_SPACING,
            "paced fetch starts advance virtual time by at least one gap; elapsed {elapsed:?}"
        );
        Ok(())
    }
}
