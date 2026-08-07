//! The refresh schedule, which is the part of IAM authentication that has to be
//! right and the part no deployment would report until a token expired.
//!
//! Every case runs on a paused clock against a lazily-built pool: the loop only
//! reads the close event and writes the pool's connect options, so no server is
//! involved. Nothing here waits on a duration — a test waits for the loop's next
//! fetch, and the paused clock jumps straight to whatever deadline that loop is
//! sitting on, so an hour-scale schedule is assertable to the second and a
//! schedule that never fires fails instead of hanging.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::ConnectOptions;
use sqlx::postgres::{PgConnectOptions, PgPool};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::{
    AccessToken, AccessTokenSource, CredentialWatch, MalformedToken, TokenFetchError, TokenOptions,
    first_token, start_refreshing,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// How much virtual time a case will let pass before calling the loop stuck. Far
/// past any schedule asserted below, so it reads as "never" rather than as a
/// bound anything is tuned against.
const GIVE_UP: Duration = secs(86_400);

/// The shape a deployment configures: a username and a database, no password.
const BASE_URL: &str = "postgres://factsuser@db.internal:5432/facts";

/// What a scripted issuer answers with. Token values are alphanumeric so they
/// survive a round trip through a URL's percent encoding unchanged.
#[derive(Debug, Clone)]
enum Answer {
    Issue { value: String, expires_in: Duration },
    Fail,
}

/// What a scripted failure says, so a case can find it again in a report.
const SCRIPTED_FAILURE: &str = "the scripted issuer is unavailable";

/// How many answers a case will give before it stops answering at all, orders of
/// magnitude above any schedule asserted here.
///
/// A loop that fetched without bound would do it at one instant, because the wait
/// it computes from an expired token is zero. The virtual clock only moves while
/// something is parked on it, so such a loop starves every timeout below and
/// hangs the suite. Refusing to answer past this parks it instead, the clock runs
/// on, and the case fails on its own deadline.
const MAX_FETCHES: usize = 10_000;

impl Answer {
    fn issue(value: &str, expires_in_secs: u64) -> Self {
        Self::Issue {
            value: value.to_owned(),
            expires_in: secs(expires_in_secs),
        }
    }
}

/// A token issuer under the test's control. It answers from a script and
/// announces how far into the test each call arrived, which is what lets a case
/// wait on the loop's progress instead of on a duration it guessed.
#[derive(Debug)]
struct ScriptedTokens {
    /// Answers in order. The last one repeats, so a case scripts only the part
    /// it asserts on and the loop keeps running past it.
    script: Mutex<VecDeque<Answer>>,
    answered: AtomicUsize,
    fetched: mpsc::UnboundedSender<Duration>,
    start: Instant,
}

/// A scripted issuer and the stream of instants it fetches at.
fn scripted(
    script: impl IntoIterator<Item = Answer>,
) -> (Arc<ScriptedTokens>, mpsc::UnboundedReceiver<Duration>) {
    let (fetched, fetches) = mpsc::unbounded_channel();
    let tokens = Arc::new(ScriptedTokens {
        script: Mutex::new(script.into_iter().collect()),
        answered: AtomicUsize::new(0),
        fetched,
        start: Instant::now(),
    });
    (tokens, fetches)
}

#[async_trait::async_trait]
impl AccessTokenSource for ScriptedTokens {
    async fn fetch(&self) -> Result<AccessToken, TokenFetchError> {
        if self.answered.fetch_add(1, Ordering::Relaxed) >= MAX_FETCHES {
            return std::future::pending().await;
        }
        let answer = {
            let mut script = self
                .script
                .lock()
                .map_err(|_| "the token script mutex was poisoned".to_owned())?;
            let answer = script
                .front()
                .cloned()
                .ok_or_else(|| "the token script was empty".to_owned())?;
            if script.len() > 1 {
                script.pop_front();
            }
            answer
        };
        self.fetched.send(self.start.elapsed())?;
        match answer {
            Answer::Issue { value, expires_in } => Ok(AccessToken::new(value, expires_in)?),
            Answer::Fail => Err(SCRIPTED_FAILURE.to_owned().into()),
        }
    }
}

/// How far into the test the loop's next fetch arrived. The clock is paused, so
/// this jumps to whatever deadline the loop is waiting on, and a loop waiting on
/// nothing fails here rather than hanging.
async fn next_fetch_at(
    fetches: &mut mpsc::UnboundedReceiver<Duration>,
) -> Result<Duration, String> {
    match tokio::time::timeout(GIVE_UP, fetches.recv()).await {
        Ok(Some(at)) => Ok(at),
        Ok(None) => Err("the token source was dropped before it fetched".to_owned()),
        Err(_) => Err(format!(
            "no token was fetched within {GIVE_UP:?} of virtual time"
        )),
    }
}

/// A pool that never opens a connection, built from the production pool options
/// so the refresh loop sees the shape a deployment gives it.
fn lazy_pool() -> Result<(PgPool, PgConnectOptions), sqlx::Error> {
    let base: PgConnectOptions = BASE_URL.parse()?;
    let pool = crate::postgres::pool_options().connect_lazy_with(base.clone().password("first"));
    Ok((pool, base))
}

/// Run the refresh over `pool` the way [`super::pool_with_refreshed_tokens`]
/// starts it, from a token stating `expires_in`.
fn spawn_refresh(
    pool: &PgPool,
    base: PgConnectOptions,
    tokens: &Arc<ScriptedTokens>,
    expires_in: Duration,
) -> CredentialWatch {
    let source: Arc<dyn AccessTokenSource> = tokens.clone();
    start_refreshing(pool, base, source, super::RefreshSchedule::new(expires_in))
}

/// Close `pool` and wait out the refresh task, which must end on the close
/// itself. The task holds the reporting end of the watch, so the watch closing
/// is the task ending.
///
/// The clock only moves while something is still waiting on it, so a task that
/// sat out the deadline it was holding before noticing the close shows up here as
/// virtual time passing. Ending eventually is not enough: a token's margin is
/// most of an hour, and a job that finishes its load would keep a task and a
/// pool handle alive for the rest of it.
async fn close_and_join(pool: &PgPool, credentials: CredentialWatch) -> TestResult {
    let closed_at = Instant::now();
    pool.close().await;
    tokio::time::timeout(GIVE_UP, credentials.reporter_ended()).await?;
    let waited = closed_at.elapsed();
    if waited > Duration::ZERO {
        return Err(format!(
            "the refresh task ran {waited:?} of virtual time past the pool's close; it must end \
             on the close event, not on whatever deadline it was holding"
        )
        .into());
    }
    Ok(())
}

/// The password the pool would hand its next handshake.
fn pool_password(pool: &PgPool) -> Option<String> {
    pool.connect_options()
        .to_url_lossy()
        .password()
        .map(str::to_owned)
}

/// Each wait comes from the remaining life of the token in hand, not from the
/// first one and not from an interval chosen here, so a token whose stated life
/// changes moves the next refresh with it. Scheduling off a constant, or off the
/// first token forever, puts every fetch on a fixed cadence instead.
#[tokio::test(start_paused = true)]
async fn each_refresh_is_due_at_eighty_percent_of_the_token_in_hand() -> TestResult {
    let (tokens, mut fetches) =
        scripted([Answer::issue("tokenb", 200), Answer::issue("tokenc", 300)]);
    let (pool, base) = lazy_pool()?;
    let credentials = spawn_refresh(&pool, base, &tokens, secs(100));

    // 80% of 100 lands the first fetch at 80; that token states 200, so the next
    // is 160 later; that one states 300, so the third is 240 after it.
    assert_eq!(next_fetch_at(&mut fetches).await?, secs(80));
    assert_eq!(next_fetch_at(&mut fetches).await?, secs(240));
    assert_eq!(next_fetch_at(&mut fetches).await?, secs(480));

    close_and_join(&pool, credentials).await
}

/// `expires_in` counts *down*: the metadata server caches the service account's
/// token and hands the same one back with less life each time, minting a fresh
/// one only once the cached token is nearly out. Reading that number as a freshly
/// issued lifetime schedules the last refreshes past the expiry they were meant
/// to stay ahead of.
#[tokio::test(start_paused = true)]
async fn the_schedule_follows_an_issuer_counting_down_to_a_fresh_token() -> TestResult {
    let (tokens, mut fetches) = scripted([
        Answer::issue("cached", 720),
        Answer::issue("cached", 144),
        Answer::issue("minted", 3600),
    ]);
    let (pool, base) = lazy_pool()?;
    let credentials = spawn_refresh(&pool, base, &tokens, secs(3600));

    // An hour-long token is due at 2880 and the cache answers with the 720 it has
    // left; 80% of that is due at 3456, where 144 is left; 80% of *that* is due
    // at 3571.2, by which point the cache has minted a fresh hour.
    assert_eq!(next_fetch_at(&mut fetches).await?, secs(2880));
    assert_eq!(next_fetch_at(&mut fetches).await?, secs(3456));
    assert_eq!(
        next_fetch_at(&mut fetches).await?,
        Duration::from_millis(3_571_200)
    );
    assert_eq!(
        next_fetch_at(&mut fetches).await?,
        Duration::from_millis(6_451_200)
    );

    close_and_join(&pool, credentials).await
}

/// The whole point of the swap: connections opened after it hand over the new
/// token, and everything else the deployment configured survives untouched.
#[tokio::test(start_paused = true)]
async fn a_refreshed_token_reaches_the_pool_and_disturbs_nothing_else() -> TestResult {
    let (tokens, mut fetches) = scripted([Answer::issue("tokenb", 3600)]);
    let (pool, base) = lazy_pool()?;
    let credentials = spawn_refresh(&pool, base, &tokens, secs(100));

    // The loop applies what it fetched before it waits again, and a paused clock
    // runs one task at a time, so the swap has happened by the time this returns.
    next_fetch_at(&mut fetches).await?;

    let opened_with = pool.connect_options().to_url_lossy();
    assert_eq!(opened_with.password(), Some("tokenb"));
    assert_eq!(opened_with.username(), "factsuser");
    assert_eq!(opened_with.host_str(), Some("db.internal"));
    assert_eq!(opened_with.path(), "/facts");

    close_and_join(&pool, credentials).await
}

/// A failure while the token in hand is still good is survivable: the loop
/// spaces its retries and applies whatever it eventually gets. Ending the task on
/// the first failure would leave the pool on a token that expires.
#[tokio::test(start_paused = true)]
async fn a_failed_refresh_retries_on_a_growing_backoff_and_recovers() -> TestResult {
    let (tokens, mut fetches) = scripted([
        Answer::Fail,
        Answer::Fail,
        Answer::Fail,
        Answer::issue("tokenb", 600),
    ]);
    let (pool, base) = lazy_pool()?;
    let credentials = spawn_refresh(&pool, base, &tokens, secs(100));

    // Due at 80, then retried 1s, 2s and 4s after each failure.
    assert_eq!(next_fetch_at(&mut fetches).await?, secs(80));
    assert_eq!(next_fetch_at(&mut fetches).await?, secs(81));
    assert_eq!(next_fetch_at(&mut fetches).await?, secs(83));
    assert_eq!(next_fetch_at(&mut fetches).await?, secs(87));
    assert_eq!(pool_password(&pool).as_deref(), Some("tokenb"));

    close_and_join(&pool, credentials).await
}

/// The backoff stops doubling at a minute. Left unbounded it would reach a
/// quarter of an hour inside one token's margin, so an issuer that came back
/// would go unnoticed until long after the credential was gone.
#[tokio::test(start_paused = true)]
async fn the_retry_backoff_stops_growing_at_a_minute() -> TestResult {
    let (tokens, mut fetches) = scripted([Answer::Fail]);
    let (pool, base) = lazy_pool()?;
    let credentials = spawn_refresh(&pool, base, &tokens, secs(3600));

    // Due at 2880, then 1, 2, 4, 8, 16 and 32 seconds apart; the wait that would
    // be 64 is a minute, and so is every one after it.
    for at in [2880, 2881, 2883, 2887, 2895, 2911, 2943, 3003, 3063] {
        assert_eq!(next_fetch_at(&mut fetches).await?, secs(at));
    }

    close_and_join(&pool, credentials).await
}

/// The pool reopens reaped connections from its current options, so a token that
/// expires with no replacement drains the pool to nothing. The loop gives that up
/// as lost at the expiry it can name, and reports it, which is what lets the
/// process exit while it can still finish what it is holding.
#[tokio::test(start_paused = true)]
async fn an_expired_token_with_no_replacement_is_reported_as_lost() -> TestResult {
    let (tokens, mut fetches) = scripted([Answer::Fail]);
    let (pool, base) = lazy_pool()?;
    let credentials = spawn_refresh(&pool, base, &tokens, secs(3600));

    // Nothing is reported while the token in hand still covers new connections.
    assert_eq!(next_fetch_at(&mut fetches).await?, secs(2880));
    assert!(credentials.reported().is_none());

    let loss = tokio::time::timeout(GIVE_UP, credentials.lost()).await?;
    assert_eq!(
        Instant::now().duration_since(tokens.start),
        secs(3600),
        "the credential is lost when the token expires, not before and not after"
    );
    assert_eq!(
        loss.retried_for,
        secs(720),
        "the report must span the whole margin the schedule left"
    );
    assert!(
        loss.attempts > 1,
        "a lost credential must have been retried, got {} attempt(s)",
        loss.attempts
    );
    assert!(
        loss.last_error.contains(SCRIPTED_FAILURE),
        "the report must carry what the issuer last said, got: {}",
        loss.last_error
    );
    assert!(
        credentials.reported().is_some(),
        "a probe asking after the fact must see the same report"
    );

    // Giving up ends the task: retrying an issuer for a token nothing can use is
    // work with no consumer.
    tokio::time::timeout(GIVE_UP, credentials.reporter_ended()).await?;
    pool.close().await;
    Ok(())
}

/// A refresh that recovers inside the margin leaves nothing reported: the report
/// is the process's cue to exit, and firing it on a transient failure would
/// recycle a healthy instance.
#[tokio::test(start_paused = true)]
async fn a_recovered_refresh_reports_nothing() -> TestResult {
    let (tokens, mut fetches) = scripted([Answer::Fail, Answer::issue("tokenb", 3600)]);
    let (pool, base) = lazy_pool()?;
    let credentials = spawn_refresh(&pool, base, &tokens, secs(3600));

    next_fetch_at(&mut fetches).await?;
    next_fetch_at(&mut fetches).await?;
    assert_eq!(pool_password(&pool).as_deref(), Some("tokenb"));
    assert!(credentials.reported().is_none());

    close_and_join(&pool, credentials).await
}

/// The loader is a job that ends, so a refresh task outliving its pool is a
/// leaked task holding a closed pool's handle.
#[tokio::test(start_paused = true)]
async fn the_refresh_task_ends_when_the_pool_closes() -> TestResult {
    let (tokens, mut fetches) = scripted([Answer::issue("tokenb", 600)]);
    let (pool, base) = lazy_pool()?;
    let credentials = spawn_refresh(&pool, base, &tokens, secs(600));

    close_and_join(&pool, credentials).await?;
    assert!(
        fetches.try_recv().is_err(),
        "a pool closed before the first refresh was due must never be refreshed"
    );
    Ok(())
}

/// The retry loop needs the same stop condition as the wait above it: a pool that
/// closes while its token source is down must not leave a task retrying against
/// an issuer nobody is waiting on.
#[tokio::test(start_paused = true)]
async fn the_refresh_task_ends_when_the_pool_closes_between_retries() -> TestResult {
    let (tokens, mut fetches) = scripted([Answer::Fail]);
    let (pool, base) = lazy_pool()?;
    let credentials = spawn_refresh(&pool, base, &tokens, secs(100));

    // Two failures in: the loop is now sitting in the retry backoff.
    next_fetch_at(&mut fetches).await?;
    next_fetch_at(&mut fetches).await?;

    close_and_join(&pool, credentials).await?;
    assert!(
        fetches.try_recv().is_err(),
        "no fetch may be attempted after the pool closed"
    );
    Ok(())
}

/// The metadata server answers 503 while an instance is still coming up, so a
/// boot that gave up on the first refusal would fail cold starts that were about
/// to work.
#[tokio::test(start_paused = true)]
async fn the_first_token_is_retried_while_the_issuer_is_still_coming_up() -> TestResult {
    let (tokens, mut fetches) =
        scripted([Answer::Fail, Answer::Fail, Answer::issue("tokenb", 3600)]);

    let token = tokio::time::timeout(GIVE_UP, first_token(tokens.as_ref())).await??;
    assert_eq!(token.value(), "tokenb");

    // Attempted at once, then 1s and 2s after each refusal.
    assert_eq!(next_fetch_at(&mut fetches).await?, secs(0));
    assert_eq!(next_fetch_at(&mut fetches).await?, secs(1));
    assert_eq!(next_fetch_at(&mut fetches).await?, secs(3));
    Ok(())
}

/// The retry is bounded: a service account that cannot hold the login scope never
/// starts working, and a boot hanging on it is a startup probe timing out with
/// nothing said about why.
#[tokio::test(start_paused = true)]
async fn the_first_token_gives_up_at_its_budget_carrying_the_issuers_reason() -> TestResult {
    let (tokens, _fetches) = scripted([Answer::Fail]);

    // Under a timeout, because a boot that retried forever would hang here
    // instead of failing.
    let failure = match tokio::time::timeout(GIVE_UP, first_token(tokens.as_ref())).await? {
        Err(crate::DbError::DatabaseToken(failure)) => failure,
        Err(other) => return Err(format!("expected a token failure, got: {other}").into()),
        Ok(_) => return Err("a boot whose issuer never answers must fail".into()),
    };
    assert!(
        failure.to_string().contains(SCRIPTED_FAILURE),
        "the boot failure must carry what the issuer said, got: {failure}"
    );
    assert_eq!(
        Instant::now().duration_since(tokens.start),
        super::FIRST_TOKEN_BUDGET,
        "the boot must spend its whole budget, and no more, before it gives up"
    );
    Ok(())
}

/// Refresh spends a fraction of the stated remaining life, so a token claiming a
/// second or two would put the loop in a hot loop against the issuer. That is a
/// malformed issuance, refused where the number arrives instead of guarded around
/// wherever it is used.
#[test]
fn a_lifetime_too_short_to_schedule_against_is_refused() -> TestResult {
    match AccessToken::new("tokenb", secs(5)) {
        Err(MalformedToken::LifetimeTooShort { stated }) => assert_eq!(stated, secs(5)),
        Err(other) => return Err(format!("expected a lifetime refusal, got {other}").into()),
        Ok(_) => return Err("a five-second token lifetime must be refused".into()),
    }
    Ok(())
}

/// An empty password reaches the handshake as a credential that cannot work, so
/// it counts as a failed fetch (which retries) instead of a pool authenticating
/// with nothing.
#[test]
fn an_empty_token_value_is_refused() -> TestResult {
    match AccessToken::new("", secs(3600)) {
        Err(MalformedToken::EmptyValue) => Ok(()),
        Err(other) => Err(format!("expected an empty-value refusal, got {other}").into()),
        Ok(_) => Err("an empty access token must be refused".into()),
    }
}

/// The token is a live database credential, and anything holding one is a
/// candidate for a `{:?}` in a log line or an error context.
#[test]
fn a_token_never_prints_its_value() -> TestResult {
    let token = AccessToken::new("s3cr3tvalue", secs(3600))?;
    let rendered = format!("{token:?}");
    assert!(
        !rendered.contains("s3cr3tvalue"),
        "a token's Debug must not carry the credential: {rendered}"
    );
    Ok(())
}

/// Once a token leaves [`AccessToken`] it travels in `PgConnectOptions`, whose
/// derived `Debug` prints the password field verbatim, and from there in the pool
/// built over it. Both are values this crate hands around and could print, so the
/// redaction claim is only worth as much as they are.
#[tokio::test]
async fn a_token_in_the_connect_options_never_prints_either() -> TestResult {
    let base: PgConnectOptions = BASE_URL.parse()?;
    let token = AccessToken::new("s3cr3tvalue", secs(3600))?;

    let options = TokenOptions::new(&base, &token);
    let rendered = format!("{options:?}");
    assert!(
        !rendered.contains("s3cr3tvalue"),
        "connect options carrying a token must not print it: {rendered}"
    );

    let pool = crate::postgres::pool_options().connect_lazy_with(options.into_options());
    let rendered = format!("{pool:?}");
    assert!(
        !rendered.contains("s3cr3tvalue"),
        "a pool must not print the credential its next handshake sends: {rendered}"
    );
    pool.close().await;
    Ok(())
}
