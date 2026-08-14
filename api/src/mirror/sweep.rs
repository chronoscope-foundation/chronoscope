//! Build mirror-queue messages from the fact store's images.
//!
//! [`mirror_requests_for_image`] is the pure per-image policy — which of an
//! image's URLs to mirror, and the fetch instructions to send with each —
//! testable without a store or a queue. [`stream_mirror_requests`] is the walk
//! that feeds it every image, yielding messages lazily so a sweep of a large
//! store never materializes the whole set: the shape a continuous sweep over the
//! production DB needs. The batched network POST lives in [`sweep_mirror`], the
//! sweep the `/mirror/sweep` endpoint drives.
//!
//! No cross-image dedup here: a URL two images cite yields a message twice, and
//! the consumer's existence check collapses the re-send. Any real deduplication
//! lands elsewhere.
//!
//! First pass is Commons-only, so the fetch policy is a constant rather than a
//! per-source registry; a registry is a later phase when a second source lands.

use chronoscope_integrations::{DisplayableKey, MirrorRequest, displayable_content_types};
use url::Url;

/// Identifies the fetcher to upstream hosts. Wikimedia's User-Agent policy asks
/// automated clients to name themselves with a contact URL; this is the string
/// the dev-time resolver (`dev/src/image_resolve.rs`) already presents, so the
/// two fetch paths speak to Commons under one identity.
const COMMONS_USER_AGENT: &str =
    "Chronoscope/0.1 (https://github.com/copumpkin/chronoscope; fact-store image resolver)";

/// A safety cap on the fetched response: a larger object dead-letters, bounding
/// what one message can write to paid storage. Archival sizing is a later phase.
const MAX_MIRROR_BYTES: u64 = 104_857_600;

/// The mirror messages to send for one image's source URLs.
///
/// Classifies each URL in one pass. [`DisplayableKey::for_url`] yields the key
/// for a URL a browser can render, `None` for one that mirrors but does not
/// render (svg, pdf, a TIFF master), and an error for one that cannot be keyed
/// at all (a cited Commons thumbnail, a bad scheme). The error names a data
/// problem, so it is logged rather than dropped silently; the other two outcomes
/// are expected and dropped. Each survivor is stamped with the Commons fetch
/// policy, its accept list drawn from the same source as the display gate.
#[must_use]
pub fn mirror_requests_for_image(urls: &[Url]) -> Vec<MirrorRequest> {
    urls.iter()
        .filter_map(|url| match DisplayableKey::for_url(url) {
            Ok(Some(key)) => Some(MirrorRequest::new(
                key.key().as_str().to_string(),
                url.clone(),
                COMMONS_USER_AGENT.to_string(),
                displayable_content_types().map(|s| s.to_string()).collect(),
                MAX_MIRROR_BYTES,
            )),
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(url = %url, %error, "skipping a cited URL that cannot be mirror-keyed");
                None
            }
        })
        .collect()
}

pub use dispatch::{QueueTarget, SweepReport, sweep_mirror};
pub use walk::stream_mirror_requests;

// The walk's `FactStore`-facing imports and stream helpers live behind the
// `mirror` feature so the default `ingest` build (which pins the wikidata
// facts-DB pipeline) never compiles them.
mod walk {
    use std::num::NonZeroUsize;

    use async_stream::try_stream;
    use chronoscope_core::projection::{member_lineage, project_image};
    use chronoscope_core::store::schema::ImageStream;
    use chronoscope_core::store::{FactStore, ImageIdOf, ImageView};
    use chronoscope_core::typed;
    use chronoscope_integrations::MirrorRequest;
    use futures_util::Stream;
    use url::Url;

    use super::mirror_requests_for_image;

    /// Page size for the image-class walk. One page covers the curated snapshot;
    /// larger stores page through.
    const WALK_PAGE: NonZeroUsize = match NonZeroUsize::new(256) {
        Some(n) => n,
        None => NonZeroUsize::MIN,
    };

    /// Yield the mirror messages for every image in `store`, lazily.
    ///
    /// Pages through the image classes; for each `SameArtifact` representative
    /// it projects the image and yields a message per displayable source URL, so
    /// only one page and one image are held at a time and a sweep of a large
    /// store stays bounded. Store-generic, so it walks a Postgres store the same
    /// way it walks the baked SQLite one. Consecutive-row dedup collapses a
    /// class's repeats within the walk; there is none across images.
    pub fn stream_mirror_requests<S: FactStore>(
        store: &S,
    ) -> impl Stream<Item = Result<MirrorRequest, S::Error>> + '_ {
        try_stream! {
            let mut view = store.now().await?;
            let mut after = None;
            let mut last_rep: Option<ImageIdOf<S>> = None;
            loop {
                let page = view
                    .walk_image_classes(&ImageStream::All, after, WALK_PAGE)
                    .await?;
                for row in &page.rows {
                    if last_rep.as_ref() == Some(&row.representative) {
                        continue;
                    }
                    last_rep = Some(row.representative.clone());
                    if let Some((class, projected)) =
                        project_image::<S, _, _>(&mut view, row.representative.clone(), member_lineage)
                            .await?
                    {
                        let image = typed::Image::parse(&projected, &class);
                        let urls: Vec<Url> = image
                            .urls
                            .iter()
                            .map(|attributed| attributed.value.clone())
                            .collect();
                        for request in mirror_requests_for_image(&urls) {
                            yield request;
                        }
                    }
                }
                match page.next_class {
                    None => break,
                    Some(next) => after = Some(next),
                }
            }
        }
    }
}

// The sweep's HTTP client and its Cloudflare envelope types ride the same
// `mirror` feature as the walk, so nothing here reaches the default build.
mod dispatch {
    use std::time::Duration;

    use chronoscope_core::store::FactStore;
    use chronoscope_integrations::MirrorRequest;
    use futures_util::{StreamExt, TryStreamExt};
    use serde::{Deserialize, Serialize};

    use super::stream_mirror_requests;

    /// Messages per batch push. The Cloudflare Queues batch endpoint caps a call
    /// at 100 messages or 256 KB, whichever comes first; a mirror message is a
    /// few hundred bytes, so 100 stays far under the size cap.
    const BATCH_MAX: usize = 100;

    /// Batch POSTs in flight at once. The walk outruns one serial round-trip, so
    /// sends overlap; bounded so a large store cannot open an unbounded number of
    /// connections.
    const SEND_CONCURRENCY: usize = 8;

    /// Failure reasons kept for the report, capped so a large store's failures
    /// cannot grow the sample without bound.
    const FAILURE_SAMPLE_MAX: usize = 50;

    /// How long one batch POST may take before it is treated as failed. reqwest
    /// has no default timeout, so without this a stalled connection hangs the
    /// sweep on a single batch.
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

    /// A Cloudflare Queues target: the account and queue a sweep enqueues to, and
    /// the API token authorizing the writes. Built from configuration the caller
    /// holds; the token is a secret and is never logged.
    pub struct QueueTarget {
        pub account_id: String,
        pub queue_id: String,
        pub token: String,
    }

    /// What a sweep did: messages enqueued, messages failed, a capped sample of
    /// failure reasons, and whether it stopped short of the whole store.
    #[derive(Debug, Default)]
    pub struct SweepReport {
        pub sent: usize,
        pub failed: usize,
        pub failure_sample: Vec<String>,
        /// The sweep did not cover the whole store: a store read failed partway.
        /// The counts are of what completed before it stopped; a re-run is
        /// idempotent and resumes.
        pub incomplete: bool,
    }

    /// A sweep that could not start.
    #[derive(Debug, thiserror::Error)]
    pub enum SweepError {
        /// The HTTP client could not be built, so no sweep ran. Send failures and
        /// a walk that stops partway are recorded in the [`SweepReport`], not
        /// raised, so this is the one thing that leaves no report.
        #[error("building the HTTP client: {0}")]
        Client(String),
    }

    /// The result of one batched send, or a walk error surfaced as a chunk. A
    /// relay from the concurrent send futures to the drain loop, which records
    /// each — walk errors included — rather than short-circuiting, so an error
    /// never discards the counts already gathered or cancels sends in flight.
    enum BatchOutcome {
        Sent {
            count: usize,
            outcome: Result<(), String>,
        },
        /// The walk errored mid-chunk. `partial` is the send of the images it had
        /// already buffered, so they are not skipped; `message` names the error.
        WalkFailed {
            partial: Option<(usize, Result<(), String>)>,
            message: String,
        },
    }

    /// One Cloudflare Queues REST message.
    #[derive(Serialize)]
    struct QueueMessage<'a> {
        body: &'a MirrorRequest,
        content_type: &'static str,
    }

    /// A batch of messages, the shape the batch push endpoint takes.
    #[derive(Serialize)]
    struct QueueBatch<'a> {
        messages: Vec<QueueMessage<'a>>,
    }

    /// The Cloudflare v4 response envelope. An enqueue can fail with HTTP 200 and
    /// `success: false`, so a send is judged by this field, not the status alone.
    #[derive(Deserialize)]
    struct CfEnvelope {
        success: bool,
        #[serde(default)]
        errors: Vec<CfError>,
    }

    /// One entry of the envelope's `errors`, kept so a rejection names why.
    #[derive(Deserialize)]
    struct CfError {
        code: i64,
        message: String,
    }

    /// Sweep every image in `store` and enqueue a mirror message per displayable
    /// URL, batched and sent with bounded concurrency.
    ///
    /// Store-generic: the caller passes whichever backend it holds, so the API
    /// server sweeps its SQLite artifact in tests and its Postgres store in
    /// production through this same code. No cross-image dedup — a URL two images
    /// cite enqueues twice and the consumer's existence check collapses the
    /// re-send.
    ///
    /// Always returns a [`SweepReport`] once the client is built: send failures
    /// and a walk that stops it partway are recorded in the report (with
    /// `incomplete` set), not raised, so the counts already gathered survive and
    /// sends in flight are drained rather than cancelled. The whole walk is swept;
    /// a systematically broken endpoint issues a doomed POST per batch (bounded
    /// by the store, cheap over the first-pass corpus) with the failure in the
    /// report. Bounding effort on a large sweep belongs with the cursor in the
    /// continuous phase, where out-of-order draining makes a mid-sweep abort
    /// tractable; a cumulative count here cannot tell systemic from scattered.
    /// Memory is bounded by the capped failure sample.
    ///
    /// # Errors
    /// [`SweepError::Client`] if the HTTP client cannot be built — the one failure
    /// that leaves no sweep to report.
    pub async fn sweep_mirror<S: FactStore>(
        store: &S,
        target: &QueueTarget,
    ) -> Result<SweepReport, SweepError> {
        let endpoint = format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/queues/{}/messages/batch",
            target.account_id, target.queue_id
        );
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| SweepError::Client(error.to_string()))?;

        // Batch the streamed messages, then run the batch POSTs with bounded
        // concurrency. try_chunks surfaces a walk error as the chunk's error.
        // The client, endpoint, and token are shared by reference across the
        // in-flight sends, borrowed for the length of the sweep.
        let client = &client;
        let endpoint = endpoint.as_str();
        let token = target.token.as_str();
        let sends = stream_mirror_requests(store)
            .try_chunks(BATCH_MAX)
            .map(move |chunk| async move {
                match chunk {
                    Ok(batch) => BatchOutcome::Sent {
                        count: batch.len(),
                        outcome: send_batch(client, endpoint, token, &batch).await,
                    },
                    Err(error) => {
                        // The walk errored mid-chunk. Send the images it had
                        // already buffered so they are not skipped this run.
                        let partial = if error.0.is_empty() {
                            None
                        } else {
                            Some((
                                error.0.len(),
                                send_batch(client, endpoint, token, &error.0).await,
                            ))
                        };
                        BatchOutcome::WalkFailed {
                            partial,
                            message: format!("{:?}", error.1),
                        }
                    }
                }
            })
            .buffer_unordered(SEND_CONCURRENCY);
        futures_util::pin_mut!(sends);

        let mut report = SweepReport::default();
        while let Some(outcome) = sends.next().await {
            match outcome {
                BatchOutcome::Sent { count, outcome } => record(&mut report, count, outcome),
                BatchOutcome::WalkFailed { partial, message } => {
                    if let Some((count, outcome)) = partial {
                        record(&mut report, count, outcome);
                    }
                    // A store read failed partway. Mark the sweep incomplete, and
                    // keep draining the sends already in flight; the walk yields
                    // no more chunks after its error.
                    if report.failure_sample.len() < FAILURE_SAMPLE_MAX {
                        report.failure_sample.push(format!("walk error: {message}"));
                    }
                    report.incomplete = true;
                }
            }
        }
        Ok(report)
    }

    /// Fold one batch's send outcome into the running report, capping the sample.
    fn record(report: &mut SweepReport, count: usize, outcome: Result<(), String>) {
        match outcome {
            Ok(()) => report.sent += count,
            Err(reason) => {
                report.failed += count;
                if report.failure_sample.len() < FAILURE_SAMPLE_MAX {
                    report.failure_sample.push(reason);
                }
            }
        }
    }

    /// Send one batch and report whether every message in it was enqueued. Reads
    /// the Cloudflare envelope's `success`, not just the HTTP status: a rejected
    /// batch can answer 200 with `success: false`.
    async fn send_batch(
        client: &reqwest::Client,
        endpoint: &str,
        token: &str,
        batch: &[MirrorRequest],
    ) -> Result<(), String> {
        let queue = QueueBatch {
            messages: batch
                .iter()
                .map(|request| QueueMessage {
                    body: request,
                    content_type: "json",
                })
                .collect(),
        };
        let response = client
            .post(endpoint)
            .bearer_auth(token)
            .json(&queue)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status();
        let envelope: CfEnvelope = response
            .json()
            .await
            .map_err(|error| format!("status {status}: unreadable response: {error}"))?;
        if status.is_success() && envelope.success {
            Ok(())
        } else if envelope.errors.is_empty() {
            Err(format!("status {status}: success=false with no errors"))
        } else {
            let detail = envelope
                .errors
                .iter()
                .map(|error| format!("[{}] {}", error.code, error.message))
                .collect::<Vec<_>>()
                .join("; ");
            Err(format!("status {status}: {detail}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chronoscope_integrations::MirrorKey;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const COMMONS_MASTER: &str = "https://upload.wikimedia.org/wikipedia/commons/a/ab/Foo.jpg";

    fn urls(specs: &[&str]) -> Result<Vec<Url>, url::ParseError> {
        specs.iter().map(|s| Url::parse(s)).collect()
    }

    #[test]
    fn a_displayable_url_becomes_a_commons_policy_request() -> TestResult {
        let url = Url::parse(COMMONS_MASTER)?;
        let requests = mirror_requests_for_image(std::slice::from_ref(&url));

        let request = requests.first().ok_or("the jpg should mirror")?;
        assert_eq!(requests.len(), 1);
        assert_eq!(request.key, MirrorKey::for_url(&url)?.as_str());
        assert_eq!(request.url, url);
        assert_eq!(
            request.accept,
            ["image/jpeg", "image/png", "image/gif", "image/webp"]
        );
        assert_eq!(request.user_agent, COMMONS_USER_AGENT);
        assert_eq!(request.max_bytes, 104_857_600);
        Ok(())
    }

    #[test]
    fn a_commons_master_keys_under_the_commons_source() -> TestResult {
        let requests = mirror_requests_for_image(&urls(&[COMMONS_MASTER])?);
        let request = requests.first().ok_or("the master should mirror")?;
        assert!(
            request.key.starts_with("commons/"),
            "a Commons master keys under its source prefix, got {}",
            request.key
        );
        Ok(())
    }

    #[test]
    fn svg_pdf_and_thumbnail_urls_are_dropped() -> TestResult {
        // A displayable jpg alongside three the gate refuses: an svg and a pdf
        // no browser renders through the edge, and a Commons thumbnail that
        // names no master file to key.
        let specs = urls(&[
            COMMONS_MASTER,
            "https://example.com/plan.svg",
            "https://example.com/report.pdf",
            "https://upload.wikimedia.org/wikipedia/commons/thumb/a/ab/Foo.jpg/64px-Foo.jpg",
        ])?;
        let requests = mirror_requests_for_image(&specs);
        assert_eq!(requests.len(), 1, "only the jpg survives the display gate");
        assert_eq!(
            requests
                .first()
                .ok_or("the jpg should mirror")?
                .url
                .as_str(),
            COMMONS_MASTER
        );
        Ok(())
    }

    #[test]
    fn each_displayable_url_yields_its_own_request() -> TestResult {
        let specs = urls(&["https://example.com/a.jpg", "https://example.com/b.png"])?;
        let requests = mirror_requests_for_image(&specs);
        assert_eq!(requests.len(), 2);
        assert_ne!(
            requests[0].key, requests[1].key,
            "distinct source URLs key distinctly"
        );
        Ok(())
    }

    #[test]
    fn no_displayable_urls_sends_nothing() -> TestResult {
        let requests = mirror_requests_for_image(&urls(&["https://example.com/plan.svg"])?);
        assert!(requests.is_empty());
        Ok(())
    }
}
