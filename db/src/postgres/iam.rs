//! Cloud SQL IAM database authentication: a short-lived token in the password
//! field, kept fresh for as long as the pool is open.
//!
//! Cloud Run's built-in Cloud SQL connection is a unix socket. It carries
//! transport, not login, so the handshake still needs a credential. IAM database
//! authentication makes that credential an OAuth 2.0 access token scoped to
//! [`SQL_LOGIN_SCOPE`], with the username being the runtime service account's
//! email minus its `.gserviceaccount.com` suffix (the connection URL carries
//! that; this module supplies only the password). Nothing is stored, so nothing
//! rotates by hand.
//!
//! Tokens are not per-connection. One authenticates a handshake, and the session
//! it opened outlives the token's expiry, so a single token serves every
//! connection the pool opens while it is valid. Staying alive is therefore a
//! matter of swapping a fresh token into the pool's connect options before the
//! current one runs out ([`keep_fresh`]); connections already open are
//! untouched, and connections opened after the swap pick the new one up.
//!
//! ## An unrenewable credential ends the process
//!
//! The pool reaps and reopens on sqlx's defaults (idle at ten minutes, retired
//! at thirty), and it reopens from its *current* connect options, so a pool
//! holding an expired token drains to nothing inside half an hour and then fails
//! every request at the handshake. A refresh that cannot succeed is therefore
//! fatal on a delay, which is the worst shape a failure can take: the instance
//! keeps passing a TCP probe while it stops being able to serve.
//!
//! So the loop retries only while the token in hand still covers new
//! connections, and reports [`CredentialsLost`] on its [`CredentialWatch`] once
//! it no longer does. The process that owns the pool watches for that report and
//! shuts down on it, letting the runtime replace it with an instance that can
//! authenticate.
//!
//! Tokens themselves come from an [`AccessTokenSource`], which is a trait so the
//! schedule above it can be tested with no metadata server to reach.

use std::sync::Arc;
use std::time::Duration;

use sqlx::pool::CloseEvent;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::DbError;
use crate::credentials::{CredentialWatch, CredentialsLost};

#[cfg(test)]
mod tests;

// ============================================================================
// The token
// ============================================================================

/// Why a source could not produce a token, left open as a box: each source fails
/// in its own vocabulary — HTTP for the metadata server, something else for a
/// test double — and the refresh loop only logs it.
pub type TokenFetchError = Box<dyn std::error::Error + Send + Sync>;

/// A stated remaining life under this is a malformed issuance, not a schedule.
///
/// A refresh spends 80% of what is left and recovers from a failure on a backoff
/// starting at [`RETRY_BACKOFF_INITIAL`], so a shorter answer would refetch
/// faster than a single failure can be retried. This floor is the only thing
/// standing between the loop and that hot spin: the give-up bound trips on
/// failed fetches, and a stream of successful-but-tiny tokens is all successes.
///
/// Ten seconds sits far under anything the schedule has been seen to ask for,
/// which is what a floor of this kind wants. The metadata server hands its
/// cached token back with less life each time and replaces it while minutes
/// still remain, but that is observed behavior and not a promise, and a token
/// refused here counts as a failed fetch. A floor set close to an issuer's
/// habits would turn a change in them into a process exit.
const MIN_TOKEN_LIFETIME: Duration = Duration::from_secs(10);

/// A short-lived database credential and how long its issuer said it has left.
///
/// [`AccessToken::new`] is the only way in, and it refuses the two shapes the
/// refresh loop could not act on, so the schedule below carries no guards.
pub struct AccessToken {
    value: String,
    expires_in: Duration,
}

/// A token an issuer handed back that cannot be used as a database password.
#[derive(Debug, thiserror::Error)]
pub enum MalformedToken {
    #[error("the token issuer returned an empty access token")]
    EmptyValue,
    #[error(
        "the token issuer stated {stated:?} of remaining life, under the {MIN_TOKEN_LIFETIME:?} \
         floor a refresh schedule can work with"
    )]
    LifetimeTooShort { stated: Duration },
}

impl AccessToken {
    /// Wrap a token that is good from now for `expires_in`.
    ///
    /// # Errors
    /// Returns [`MalformedToken`] if the value is empty or the stated remaining
    /// life is too short to schedule a refresh against.
    pub fn new(value: impl Into<String>, expires_in: Duration) -> Result<Self, MalformedToken> {
        let value = value.into();
        if value.is_empty() {
            return Err(MalformedToken::EmptyValue);
        }
        if expires_in < MIN_TOKEN_LIFETIME {
            return Err(MalformedToken::LifetimeTooShort { stated: expires_in });
        }
        Ok(Self { value, expires_in })
    }

    /// The password a handshake sends.
    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    /// How long the issuer said this token still has, which is what the refresh
    /// schedule and the give-up bound are both derived from.
    pub(crate) fn expires_in(&self) -> Duration {
        self.expires_in
    }
}

/// Hand-written so the credential cannot reach a log or an error context through
/// a `{:?}` on anything holding one.
impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessToken")
            .field("value", &"<redacted>")
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// The seam that puts the refresh schedule under test: its timing, its backoff,
/// its give-up bound and its stop on a pool close are the parts that have to be
/// right, and no build sandbox can reach a metadata server. A deployment passes
/// [`MetadataServerTokens`]; a test passes an issuer it scripts.
#[async_trait::async_trait]
pub trait AccessTokenSource: std::fmt::Debug + Send + Sync {
    /// Issue a token good from now for the remaining life it states.
    ///
    /// # Errors
    /// Returns [`TokenFetchError`] if the issuer could not be reached or answered
    /// with something unusable. The refresh loop treats every failure as
    /// retryable, up to the expiry of the token it already holds.
    async fn fetch(&self) -> Result<AccessToken, TokenFetchError>;
}

// ============================================================================
// The opt-in
// ============================================================================

/// How a fact-store pool authenticates, stated by the caller.
///
/// Never inferred from the absence of a password: local development, the test
/// harness's trust-authenticated socket and a production IAM connection all
/// connect without one, so absence carries no signal and a rule reading it would
/// fire on all three.
#[derive(Debug, Clone)]
pub enum PostgresAuth {
    /// Whatever the connection URL and libpq's environment already carry —
    /// `PGPASSWORD`, a `.pgpass` entry, or a socket that trusts the peer.
    ConnectionString,
    /// Cloud SQL IAM database authentication against the given token source.
    IamTokens(Arc<dyn AccessTokenSource>),
}

impl PostgresAuth {
    /// IAM authentication against `source`.
    pub fn iam_tokens(source: impl AccessTokenSource + 'static) -> Self {
        Self::IamTokens(Arc::new(source))
    }
}

// ============================================================================
// The pool and its refresh task
// ============================================================================

/// First wait after a failed refresh. Short, because the token in hand is still
/// valid at that point and a transient metadata-server failure resolves in
/// seconds.
const RETRY_BACKOFF_INITIAL: Duration = Duration::from_secs(1);

/// Ceiling on the retry wait. An hour-long token refreshed at 80% leaves twelve
/// minutes of validity, so a minute between attempts still gives a dozen tries
/// before the credential is lost, while an outage that outlives the token stops
/// hammering the issuer.
const RETRY_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// How long the boot spends trying for the token the pool is built with.
///
/// The metadata server answers 503 while an instance is still coming up, which
/// is why Google's own clients retry it, and a container runtime allows minutes
/// for a process to bind its port. The budget is bounded so a service account
/// that simply cannot hold [`SQL_LOGIN_SCOPE`] fails the boot with the issuer's
/// reason inside the startup probe's patience.
const FIRST_TOKEN_BUDGET: Duration = Duration::from_secs(30);

/// Build a pool whose handshakes carry an IAM token, and keep that token fresh
/// for as long as the pool is open.
///
/// # Errors
/// Returns [`DbError::Sqlx`] if the URL does not parse or the first connection
/// fails, and [`DbError::DatabaseToken`] if no token can be had inside
/// [`FIRST_TOKEN_BUDGET`].
pub(super) async fn pool_with_refreshed_tokens(
    options: PgPoolOptions,
    url: &str,
    source: Arc<dyn AccessTokenSource>,
) -> crate::DbResult<(PgPool, CredentialWatch)> {
    let base: PgConnectOptions = url.parse()?;
    let token = first_token(source.as_ref()).await?;
    let schedule = RefreshSchedule::new(token.expires_in());
    let pool = options
        .connect_with(TokenOptions::new(&base, &token).into_options())
        .await?;
    let credentials = start_refreshing(&pool, base, source, schedule);
    Ok((pool, credentials))
}

/// The token the pool starts with, retried inside a boot budget.
///
/// A single 503 from a metadata server that is itself still starting is the
/// common failure here, and it clears in seconds.
async fn first_token(source: &dyn AccessTokenSource) -> crate::DbResult<AccessToken> {
    let give_up_at = Instant::now() + FIRST_TOKEN_BUDGET;
    let mut backoff = RETRY_BACKOFF_INITIAL;
    let mut attempts: u32 = 1;
    loop {
        let failure = match source.fetch().await {
            Ok(token) => return Ok(token),
            Err(failure) => failure,
        };
        let budget_left = give_up_at.saturating_duration_since(Instant::now());
        if budget_left.is_zero() {
            return Err(DbError::DatabaseToken(failure));
        }
        let wait = backoff.min(budget_left);
        tracing::warn!(
            attempts,
            retry_in_secs = wait.as_secs_f64(),
            budget_left_secs = budget_left.as_secs_f64(),
            error = %failure,
            "could not fetch the fact store's first IAM database access token; the pool has no \
             credential until one arrives",
        );
        #[expect(
            clippy::disallowed_methods,
            reason = "backoff between boot attempts at a metadata server that answers 503 while \
                      it is coming up; bounded by FIRST_TOKEN_BUDGET, and no pool exists yet \
                      whose close event this could wait on instead"
        )]
        tokio::time::sleep(wait).await;
        backoff = (backoff * 2).min(RETRY_BACKOFF_MAX);
        attempts += 1;
    }
}

/// Start the refresh over `pool` and hand back the watch it reports a lost
/// credential on.
///
/// The task is detached and the watch is the whole handle, because the one thing
/// a join handle would add is a way to abandon a live pool's refresh. The task
/// carries a pool handle of its own and ends on the close event, so
/// `PostgresFactStore::close` ends both, and that call is how an
/// IAM-authenticated store is finished with.
fn start_refreshing(
    pool: &PgPool,
    base: PgConnectOptions,
    source: Arc<dyn AccessTokenSource>,
    schedule: RefreshSchedule,
) -> CredentialWatch {
    let (report, watch) = CredentialWatch::reporting();
    tokio::spawn(keep_fresh(pool.clone(), base, source, schedule, report));
    watch
}

/// The pool's connect options with a live token in the password field, redacted
/// in `Debug`.
///
/// Wrapped because [`PgConnectOptions`] derives `Debug` over an unredacted
/// password field: this is the value a token travels in once it leaves
/// [`AccessToken`], and sqlx is the only thing that should ever hold it.
struct TokenOptions(PgConnectOptions);

impl TokenOptions {
    /// `base` with `token` in the password field. Host or socket, IAM username,
    /// database and TLS mode come from the configured URL and stay as they were,
    /// and `base` is the configured options rather than the previous token's, so
    /// nothing compounds across refreshes.
    fn new(base: &PgConnectOptions, token: &AccessToken) -> Self {
        Self(base.clone().password(token.value()))
    }

    fn into_options(self) -> PgConnectOptions {
        self.0
    }
}

impl std::fmt::Debug for TokenOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("TokenOptions").field(&"<redacted>").finish()
    }
}

/// The two deadlines the token in hand sets.
#[derive(Debug, Clone, Copy)]
struct RefreshSchedule {
    /// When the next token is fetched, four fifths through the life this one
    /// stated. Taken from the issuer's own number so the margin scales with
    /// whatever it says; the last fifth is the window a failing refresh has to
    /// recover in, which is what bounds the retry backoff.
    refresh_at: Instant,
    /// When this token stops covering new connections, which is the bound the
    /// retry loop gives up at.
    expires_at: Instant,
}

impl RefreshSchedule {
    /// The clock is read once the token is in hand, so the time a fetch itself
    /// took counts against the token.
    fn new(stated_life: Duration) -> Self {
        let issued = Instant::now();
        Self {
            // Fifths before the multiply, so no stated life can overflow it.
            refresh_at: issued + stated_life / 5 * 4,
            expires_at: issued + stated_life,
        }
    }
}

/// Swap a fresh token into `pool` ahead of each expiry, until the pool closes or
/// the credential is lost.
async fn keep_fresh(
    pool: PgPool,
    base: PgConnectOptions,
    source: Arc<dyn AccessTokenSource>,
    first: RefreshSchedule,
    report: watch::Sender<Option<CredentialsLost>>,
) {
    // One event for the whole task: it fuses once fired, so every wait below
    // returns immediately after a close.
    let mut closed = pool.close_event();
    let mut schedule = first;
    loop {
        if let Waited::PoolClosed = wait_until(schedule.refresh_at, &mut closed).await {
            break;
        }
        match next_token(&mut closed, source.as_ref(), schedule.expires_at).await {
            RefreshOutcome::Issued(token) => {
                schedule = RefreshSchedule::new(token.expires_in());
                pool.set_connect_options(TokenOptions::new(&base, &token).into_options());
                let next_refresh = schedule
                    .refresh_at
                    .saturating_duration_since(Instant::now());
                tracing::info!(
                    expires_in_secs = token.expires_in().as_secs(),
                    next_refresh_secs = next_refresh.as_secs(),
                    "refreshed the fact store's IAM database access token",
                );
            }
            RefreshOutcome::PoolClosed => break,
            RefreshOutcome::Expired(loss) => {
                tracing::error!(
                    reason = %loss,
                    "the fact store's database credential can no longer be renewed; the pool \
                     opens no further connections",
                );
                // A watcher that has already gone is a process on its way down
                // for its own reasons, which is the outcome this asks for.
                let _ = report.send(Some(loss));
                return;
            }
        }
    }
    tracing::debug!("the fact-store pool closed; its IAM token refresh is stopping");
}

/// How a due refresh ended.
enum RefreshOutcome {
    Issued(AccessToken),
    /// The pool closed under the refresh, so there is nothing left to keep fresh.
    PoolClosed,
    /// The token in hand ran out first.
    Expired(CredentialsLost),
}

/// Fetch until a token comes back, giving up when the one in hand stops covering
/// new connections.
///
/// The wait before each attempt is clamped to what the held token has left, so
/// the last attempt lands exactly at its expiry: a fetch needs nothing from the
/// database, and one that succeeds there is still a full recovery.
async fn next_token(
    closed: &mut CloseEvent,
    source: &dyn AccessTokenSource,
    expires_at: Instant,
) -> RefreshOutcome {
    let started = Instant::now();
    let mut backoff = RETRY_BACKOFF_INITIAL;
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        let failure = match closed.do_until(source.fetch()).await {
            Err(_) => return RefreshOutcome::PoolClosed,
            Ok(Ok(token)) => return RefreshOutcome::Issued(token),
            Ok(Err(failure)) => failure,
        };
        let failed_at = Instant::now();
        let token_left = expires_at.saturating_duration_since(failed_at);
        if token_left.is_zero() {
            return RefreshOutcome::Expired(CredentialsLost {
                attempts,
                retried_for: started.elapsed(),
                last_error: failure.to_string(),
            });
        }
        let wait = backoff.min(token_left);
        tracing::warn!(
            attempts,
            retry_in_secs = wait.as_secs_f64(),
            token_left_secs = token_left.as_secs_f64(),
            error = %failure,
            "could not fetch the fact store's IAM database access token; the process exits if \
             none arrives before the one in hand expires",
        );
        if let Waited::PoolClosed = wait_until(failed_at + wait, closed).await {
            return RefreshOutcome::PoolClosed;
        }
        backoff = (backoff * 2).min(RETRY_BACKOFF_MAX);
    }
}

/// What ended a wait.
enum Waited {
    /// The deadline arrived with the pool still open, which is the only case
    /// with work left to do.
    DeadlineReached,
    /// The pool closed, so whatever the wait was for no longer matters.
    PoolClosed,
}

/// Wait until `deadline`, cut short if the pool closes first.
///
/// Waiting on the close event under a timeout, in place of a plain sleep, is what
/// makes every wait in this task cancellable by the thing that ends it.
async fn wait_until(deadline: Instant, closed: &mut CloseEvent) -> Waited {
    match tokio::time::timeout_at(deadline, &mut *closed).await {
        Err(_elapsed) => Waited::DeadlineReached,
        Ok(()) => Waited::PoolClosed,
    }
}

// ============================================================================
// The deployment's token source
// ============================================================================

/// The scope Cloud SQL requires of a token used as a database password.
const SQL_LOGIN_SCOPE: &str = "https://www.googleapis.com/auth/sqlservice.login";

/// The instance metadata server's token endpoint for the attached service
/// account. Plain HTTP to a link-local service only the instance itself can
/// reach, which is why this crate's HTTP client carries no TLS.
const METADATA_TOKEN_ENDPOINT: &str =
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token";

/// The header the metadata server requires, which a browser following a link
/// cannot set.
const METADATA_FLAVOR_HEADER: &str = "Metadata-Flavor";
const METADATA_FLAVOR_VALUE: &str = "Google";

/// How long to wait on the metadata server. It answers from the local
/// hypervisor, so a slow answer is a stuck one and the retry loop wants its turn
/// back.
const METADATA_TIMEOUT: Duration = Duration::from_secs(10);

/// How much of a refused answer's body to keep. Enough for the metadata server's
/// sentence about the scope or the service account, capped because an error body
/// off a proxy or a captive portal is a whole HTML page.
const MAX_ERROR_BODY_CHARS: usize = 512;

/// Tokens for the service account the instance runs as, from the GCE / Cloud Run
/// metadata server. The deployment's [`AccessTokenSource`].
#[derive(Debug)]
pub struct MetadataServerTokens {
    client: reqwest::Client,
}

/// Building the client failed, which is the only way constructing a
/// [`MetadataServerTokens`] can. A struct rather than a one-variant enum, and
/// separate from [`MetadataTokenError`], so neither operation's callers face
/// the other's failures.
#[derive(Debug, thiserror::Error)]
#[error("building the HTTP client for the instance metadata server: {source}")]
pub struct MetadataClientError {
    #[source]
    source: reqwest::Error,
}

/// Why a token request came back without a usable token. Every variant is
/// reachable from a single `fetch`.
#[derive(Debug, thiserror::Error)]
pub enum MetadataTokenError {
    #[error(
        "requesting a Cloud SQL login token from the instance metadata server at \
         {METADATA_TOKEN_ENDPOINT}: {source}"
    )]
    Request {
        #[source]
        source: reqwest::Error,
    },
    #[error(
        "the instance metadata server answered {status} for a Cloud SQL login token: {body}. The \
         runtime service account must exist and be able to hold the scope {SQL_LOGIN_SCOPE}"
    )]
    Status {
        status: reqwest::StatusCode,
        body: String,
    },
    /// Carries serde's error, not reqwest's: the exchange succeeded and the
    /// bytes arrived, so what failed is this crate reading them, and serde is
    /// what names the field that did not fit.
    #[error("the instance metadata server's token document did not parse: {source}")]
    Document {
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Malformed(#[from] MalformedToken),
}

/// The metadata server's token document. It carries a `token_type` as well,
/// always `Bearer`, which means nothing to a Postgres handshake.
#[derive(serde::Deserialize)]
struct TokenDocument {
    access_token: String,
    /// How much life the token has *left*. The metadata server caches the
    /// service account's token and hands the same one back with less of it each
    /// time, minting a fresh hour-long token once the cached one is within five
    /// minutes of expiring.
    #[serde(deserialize_with = "seconds")]
    expires_in: Duration,
}

/// Seconds on the wire become a `Duration` where the document is read, so
/// nothing past this boundary carries a bare number it could misread as
/// milliseconds.
fn seconds<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    serde::Deserialize::deserialize(deserializer).map(Duration::from_secs)
}

impl MetadataServerTokens {
    /// Build the source.
    ///
    /// # Errors
    /// Returns [`MetadataClientError`] if the HTTP client cannot be
    /// built.
    pub fn new() -> Result<Self, MetadataClientError> {
        let client = reqwest::Client::builder()
            .timeout(METADATA_TIMEOUT)
            // reqwest reads `HTTP_PROXY` and friends by default. The metadata
            // server sits on a link-local address that a forward proxy has no
            // route to, and the request carries a credential, so this client
            // asks the environment for nothing.
            .no_proxy()
            .build()
            .map_err(|source| MetadataClientError { source })?;
        Ok(Self { client })
    }

    /// The structured form of a fetch, which [`AccessTokenSource::fetch`] boxes.
    async fn request_token(&self) -> Result<AccessToken, MetadataTokenError> {
        let response = self
            .client
            .get(METADATA_TOKEN_ENDPOINT)
            .header(METADATA_FLAVOR_HEADER, METADATA_FLAVOR_VALUE)
            .query(&[("scopes", SQL_LOGIN_SCOPE)])
            .send()
            .await
            .map_err(|source| MetadataTokenError::Request { source })?;
        let status = response.status();
        if !status.is_success() {
            // The body is where the metadata server names what it refused: a
            // service account that cannot hold the scope reads as a sentence
            // there and as a bare 403 without it.
            let body = response.text().await.map_or_else(
                |source| format!("<the body did not read: {source}>"),
                |body| body.chars().take(MAX_ERROR_BODY_CHARS).collect(),
            );
            return Err(MetadataTokenError::Status { status, body });
        }
        // Read the bytes, then parse them separately, so a server we could not
        // talk to and a document we could not read stay different failures.
        // `Response::json` would fold both into reqwest's error type.
        let body = response
            .text()
            .await
            .map_err(|source| MetadataTokenError::Request { source })?;
        let document: TokenDocument = serde_json::from_str(&body)
            .map_err(|source| MetadataTokenError::Document { source })?;
        Ok(AccessToken::new(
            document.access_token,
            document.expires_in,
        )?)
    }
}

#[async_trait::async_trait]
impl AccessTokenSource for MetadataServerTokens {
    async fn fetch(&self) -> Result<AccessToken, TokenFetchError> {
        Ok(self.request_token().await?)
    }
}
