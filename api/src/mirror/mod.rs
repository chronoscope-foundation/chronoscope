//! Trigger a mirror sweep: walk the fact store and enqueue a fetch per image.
//!
//! Gated by a shared secret: the request carries it in the `X-Mirror-Sweep-Token`
//! header, matched against the secret the server holds (from `MIRROR_SWEEP_TOKEN`,
//! generated declaratively and plumbed like the JWT secret). Not a passkey
//! session — one token, curled with — but a real credential the public source
//! does not carry, enough to keep an open endpoint from being swept by whoever
//! finds it. With no token configured it refuses every request.

use std::sync::Arc;

use dropshot::{ClientErrorStatusCode, HttpError, HttpResponseOk, RequestContext, endpoint};
use schemars::JsonSchema;
use serde::Serialize;

use self::sweep::{SweepReport, sweep_mirror};
use crate::state::AppState;

pub(crate) mod sweep;

/// The header the shared sweep secret rides in. The secret itself is read from
/// the environment into the config (`mirror_sweep_token`), not hardcoded here.
pub(crate) const SWEEP_TOKEN_HEADER: &str = "x-mirror-sweep-token";

/// The outcome of a mirror sweep.
#[derive(Serialize, JsonSchema)]
pub struct MirrorSweepResult {
    /// Messages enqueued to the mirror queue.
    pub sent: usize,
    /// Messages that failed to enqueue.
    pub failed: usize,
    /// True if the sweep stopped short of the whole store: a store read failed
    /// partway. The counts are of what completed before it stopped.
    pub incomplete: bool,
    /// A capped sample of failure reasons, for diagnosing a run that failed.
    pub failure_sample: Vec<String>,
}

impl From<SweepReport> for MirrorSweepResult {
    fn from(report: SweepReport) -> Self {
        // Destructured, not `..`: a new SweepReport field must be handled here,
        // not silently dropped from the /mirror/sweep response.
        let SweepReport {
            sent,
            failed,
            failure_sample,
            incomplete,
        } = report;
        Self {
            sent,
            failed,
            incomplete,
            failure_sample,
        }
    }
}

/// Sweep the fact store's images and enqueue a mirror fetch for each displayable
/// URL.
///
/// A full sweep with no cursor: re-running is idempotent, since the consumer
/// skips a key it has already stored. Refuses with 403 when the sweep token
/// is missing or wrong, 503 when the mirror queue is unconfigured, 500 when
/// it enqueued nothing because every send failed or the walk could not be
/// read, and answers 200 with the sent/failed split otherwise (`incomplete`
/// in the body flags a partial-but-nonempty run).
///
/// The sweep runs synchronously in the request, which fits the corpus warm it is
/// for (seconds over the curated snapshot). A production-sized or continuous
/// sweep wants a spawned background task with a cursor so a request timeout
/// cannot sever it — part of the continuous phase, not built here.
#[endpoint {
    method = POST,
    path = "/mirror/sweep",
}]
pub async fn trigger_mirror_sweep(
    ctx: RequestContext<Arc<AppState>>,
) -> Result<HttpResponseOk<MirrorSweepResult>, HttpError> {
    let state = ctx.context();

    // A shared-secret gate: the header must match the configured
    // MIRROR_SWEEP_TOKEN. With none set there is nothing to match, so every
    // request is refused. Checked first, so an unauthorized caller learns
    // nothing else about the endpoint.
    let authorized = state
        .config
        .mirror_sweep_token
        .as_deref()
        .is_some_and(|expected| {
            ctx.request
                .headers()
                .get(SWEEP_TOKEN_HEADER)
                .and_then(|value| value.to_str().ok())
                == Some(expected)
        });
    if !authorized {
        return Err(HttpError::for_client_error(
            None,
            ClientErrorStatusCode::FORBIDDEN,
            "missing or invalid mirror sweep token".to_string(),
        ));
    }

    let target = state.config.mirror_queue.as_ref().ok_or_else(|| {
        HttpError::for_unavail(None, "mirror queue is not configured".to_string())
    })?;

    let report = sweep_mirror(&state.facts, target)
        .await
        .map_err(|error| HttpError::for_internal_error(error.to_string()))?;

    // A sweep that enqueued nothing while something went wrong — every send
    // failed, or the walk could not be read — is a failure, not a 200. Partial
    // success (some sent) stays 200 with the split in the body to read, and
    // `incomplete` there marks a run that stopped short after enqueuing some.
    if report.sent == 0 && (report.failed > 0 || report.incomplete) {
        return Err(HttpError::for_internal_error(format!(
            "mirror sweep enqueued nothing{}{}",
            if report.incomplete {
                " (the walk did not complete)"
            } else {
                ""
            },
            report
                .failure_sample
                .first()
                .map(|reason| format!("; e.g. {reason}"))
                .unwrap_or_default(),
        )));
    }

    Ok(HttpResponseOk(report.into()))
}
