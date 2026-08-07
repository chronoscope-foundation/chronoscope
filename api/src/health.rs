//! Readiness probe.
//!
//! A platform that probes by TCP connect calls the server healthy the moment
//! Dropshot binds, which is well before either store is known to answer. This
//! endpoint makes that verdict depend on the two pools every request needs: the
//! app database and the mounted fact store.

use std::sync::Arc;
use std::time::Duration;

use chronoscope_core::store::{FactStore, FactView};
use dropshot::{HttpError, HttpResponseUpdatedNoContent, RequestContext, endpoint};

use crate::state::AppState;

/// How long the probe waits on the two pools before answering anyway.
///
/// Both pools cap their connections and a fact-store read view holds one for
/// its lifetime, so a loaded instance can legitimately have nothing free.
/// sqlx's own acquire timeout is thirty seconds, long enough that a platform
/// probe times out first and recycles an instance whose only fault was traffic,
/// shifting that traffic onto its neighbors.
const PROBE_BUDGET: Duration = Duration::from_secs(2);

/// Report a probe failure as 503, the status a load balancer reads as "route
/// elsewhere". The cause goes to the log; clients get the bare status.
fn unavailable(e: impl std::fmt::Debug) -> HttpError {
    HttpError::for_unavail(None, format!("{e:?}"))
}

/// Take a connection out of each store and read through the second one.
///
/// Checking a connection out is the liveness signal: sqlx tests one before
/// handing it over, so a closed pool or a database that stopped answering
/// surfaces here instead of in the next request handler. Reading the frontier
/// takes a transaction spanning the frozen base and the writable overlay, which
/// is what makes the fact-store half of the probe real.
///
/// `pub(crate)` so the suite can read the refusal itself: the endpoint answers
/// with a bare status, and the reason a probe failed is only visible here.
pub(crate) async fn observe(state: &AppState) -> Result<(), HttpError> {
    // A pool whose credential is gone still answers from the connections it has
    // open, so the two checks below would pass while the instance is minutes
    // from serving nothing. The process shuts itself down on that report; this
    // is what tells a probe arriving during the drain to route elsewhere.
    if let Some(loss) = state.credentials.reported() {
        return Err(unavailable(loss));
    }
    state.db.ping().await.map_err(unavailable)?;

    let mut view = state.facts.now().await.map_err(unavailable)?;
    view.snapshot().await.map_err(unavailable)?;
    Ok(())
}

/// Readiness of the API server's two stores.
///
/// The status code is the whole contract: a platform acts on it and has no way
/// to interpret a body, so success versus 503 is everything a caller can act
/// on, and the response carries nothing else. Dropshot spells a body-less
/// success as 204. The store checks still earn their round trip, catching
/// breakage that arrives after startup and that a TCP probe never sees.
///
/// Exhausting the budget answers the same success as a clean probe: the
/// instance whose pool ran dry is the one carrying the most traffic, and
/// recycling it moves that traffic onto its neighbors until they go the same
/// way.
///
/// Unauthenticated, because a health probe runs before any credential exists.
#[endpoint {
    method = GET,
    path = "/health",
}]
pub async fn health(
    ctx: RequestContext<Arc<AppState>>,
) -> Result<HttpResponseUpdatedNoContent, HttpError> {
    match tokio::time::timeout(PROBE_BUDGET, observe(ctx.context())).await {
        Ok(Err(broken)) => Err(broken),
        Ok(Ok(())) | Err(_) => Ok(HttpResponseUpdatedNoContent()),
    }
}
