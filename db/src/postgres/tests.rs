//! Postgres fact-store tests: the backend-agnostic conformance suite stamped
//! against [`PostgresFactStore`] over the ephemeral-cluster harness, plus what
//! is genuinely backend-local: `PostGIS` availability, the isolation levels the
//! submit serializer and the read views rest on, and the axis order of the
//! stored geometry.
//!
//! Nothing is ignored here beyond the suite-wide `SameEvent` cases the macro
//! bakes in.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use sqlx::ConnectOptions;
use tokio::sync::mpsc;

use super::harness::{
    IsolationLevel, fresh_pg_store, fresh_pg_store_at_default_isolation,
    fresh_unmigrated_database_url,
};
use super::{
    AccessToken, AccessTokenSource, PostgresAuth, PostgresFactStore, PostgresFactStoreError,
    TokenFetchError,
};
use crate::common::ids::{SqlEntityId, SqlEventId, SqlImageId};
use chronoscope_core::grammar::ids::FactId;
use chronoscope_core::store::FactStore;
use chronoscope_core::store::conformance::fixtures::{commit_ok, construction_at, local_bundle};
use chronoscope_core::store::conformance::{RefusalKinds, TestResult, UnmintedIds};

/// Counters mint dense from zero, so `i64::MAX` is never assigned.
impl UnmintedIds for PostgresFactStore {
    fn unminted_entity() -> SqlEntityId {
        SqlEntityId(i64::MAX)
    }
    fn unminted_event() -> SqlEventId {
        SqlEventId(i64::MAX)
    }
    fn unminted_image() -> SqlImageId {
        SqlImageId(i64::MAX)
    }
}

impl RefusalKinds for PostgresFactStore {
    fn is_cluster_tile_cap_refusal(error: &PostgresFactStoreError) -> bool {
        matches!(error, PostgresFactStoreError::ClusterTiles(_))
    }
}

chronoscope_core::fact_store_conformance!(fresh_pg_store());

/// The serving constructor must refuse a database with no fact schema and leave
/// it untouched. A fact-store location is one mistyped environment variable away
/// from another database — the app one, also a Postgres URL — and the cost of
/// getting this wrong is a whole fact schema plus `PostGIS` created inside it.
#[tokio::test]
async fn connect_refuses_an_unmigrated_database_and_creates_nothing_in_it() -> TestResult {
    use sqlx::Connection;

    let url = fresh_unmigrated_database_url().await?;
    let refusal = match PostgresFactStore::connect(&url, PostgresAuth::ConnectionString).await {
        Err(crate::DbError::Config(message)) => message,
        Err(other) => return Err(format!("expected a schema refusal, got: {other}").into()),
        Ok(store) => {
            store.close().await;
            return Err("connect must refuse a database carrying no fact schema".into());
        }
    };
    assert!(
        refusal.contains("no schema"),
        "the refusal must say the schema is absent, got: {refusal}"
    );

    let mut conn = sqlx::PgConnection::connect(&url).await?;
    let (created,): (bool,) = sqlx::query_as(
        "SELECT to_regclass('facts') IS NOT NULL \
          OR EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'postgis')",
    )
    .fetch_one(&mut conn)
    .await?;
    conn.close().await?;
    assert!(
        !created,
        "a refused connect must leave no fact tables and no PostGIS extension behind"
    );
    Ok(())
}

/// The pair closes: what the loader's migrating constructor prepares is exactly
/// what the serving one accepts, so the split costs a deployment nothing beyond
/// running the loader first.
#[tokio::test]
async fn connect_accepts_a_database_the_migrating_constructor_prepared() -> TestResult {
    let url = fresh_unmigrated_database_url().await?;
    let loader =
        PostgresFactStore::connect_and_migrate(&url, PostgresAuth::ConnectionString).await?;
    loader.close().await;
    let server = PostgresFactStore::connect(&url, PostgresAuth::ConnectionString).await?;
    server.close().await;
    Ok(())
}

/// `PostGIS` loaded in a fresh store's database (via the template's
/// `CREATE EXTENSION postgis`), reachable over the cluster's unix socket.
#[tokio::test]
async fn postgis_extension_loads_in_a_fresh_store() -> TestResult {
    let (store, _cx) = fresh_pg_store().await?;
    let (version,): (String,) = sqlx::query_as("SELECT postgis_lib_version()")
        .fetch_one(store.pool())
        .await?;
    assert!(
        !version.trim().is_empty(),
        "postgis_lib_version() returned empty"
    );
    Ok(())
}

/// `ST_MakeEnvelope` takes `(xmin, ymin, xmax, ymax)`, so a stored region holds
/// longitude on x and latitude on y. Transposing *both* the insert and the read
/// is invisible to conformance: `&&` is then the same interval test with the
/// axes renamed, every case stays green, and the geometry column quietly holds
/// latitude in x until something geodesic touches it. Pinning the stored corner
/// against the submitted coordinates is what catches that.
#[tokio::test]
async fn stored_regions_carry_longitude_on_the_x_axis() -> TestResult {
    let (lat, lon) = (40.5, -73.25);
    let (store, _cx) = fresh_pg_store().await?;
    commit_ok(
        &store,
        local_bundle(1, 0, 0, 0, vec![construction_at(0, lat, lon)?])?,
    )
    .await?;

    // Assert the one-row expectation rather than assume it: `fetch_one` returns
    // the first of however many arrive, so a second located fact would quietly
    // move this assertion onto an arbitrary row.
    let rows: Vec<(f64, f64)> =
        sqlx::query_as("SELECT ST_XMin(region), ST_YMin(region) FROM facts_spatial")
            .fetch_all(store.pool())
            .await?;
    let [(xmin, ymin)] = rows.as_slice() else {
        return Err(format!(
            "one located fact must write one spatial row, got {}",
            rows.len()
        )
        .into());
    };
    // A point's covering rect is padded only by the cap's millimeter rim
    // tolerance, well inside this margin.
    let tolerance = 1e-6;
    assert!(
        (xmin - lon).abs() < tolerance && (ymin - lat).abs() < tolerance,
        "stored region corner ({xmin}, {ymin}) must be (lon, lat) = ({lon}, {lat})"
    );
    Ok(())
}

/// The write path issues `SET TRANSACTION ISOLATION LEVEL READ COMMITTED` after
/// `BEGIN` because the counters-row `FOR UPDATE` serializer needs RC. Run it
/// against a database whose *session default* is `repeatable read`: the write tx
/// must still observe `read committed`, proving it pins its own isolation rather
/// than riding whatever the session default happens to be.
#[tokio::test]
async fn write_tx_pins_read_committed_despite_a_stricter_session_default() -> TestResult {
    use super::AsConn;

    let (store, _cx) = fresh_pg_store_at_default_isolation(IsolationLevel::RepeatableRead).await?;
    let observed = store
        .with_tx(|_s, tx| {
            Box::pin(async move {
                let (iso,): (String,) = sqlx::query_as("SHOW transaction_isolation")
                    .fetch_one(tx.conn.conn())
                    .await
                    .map_err(|e| e.to_string())?;
                Ok::<String, String>(iso)
            })
        })
        .await??;
    assert_eq!(
        observed, "read committed",
        "with_tx must pin READ COMMITTED for the FOR UPDATE serializer, \
         regardless of the database's default_transaction_isolation"
    );
    Ok(())
}

/// A view's consistency is the `fact_id < N` bound, so its transaction pins RC
/// as well — a long paginated walk stays immune to serialization failures on a
/// database defaulted to something stricter. Both constructors are checked: they
/// open their own transaction, so each has to pin its own isolation.
#[tokio::test]
async fn read_views_pin_read_committed_despite_a_stricter_database_default() -> TestResult {
    use super::AsConn;

    let (store, _cx) = fresh_pg_store_at_default_isolation(IsolationLevel::RepeatableRead).await?;
    for (label, mut view) in [
        ("now", store.now().await?),
        ("no_later_than", store.no_later_than(FactId::new(0)).await?),
    ] {
        let (observed,): (String,) = sqlx::query_as("SHOW transaction_isolation")
            .fetch_one(view.conn.conn())
            .await?;
        assert_eq!(
            observed, "read committed",
            "{label} must pin READ COMMITTED, regardless of the database's \
             default_transaction_isolation"
        );
    }
    Ok(())
}

/// A token issuer under the test's control. The cluster trusts the peer on its
/// socket, so what it hands over is checked by reading it back off the pool
/// rather than by a handshake.
#[derive(Debug)]
struct CountedTokens {
    fetches: AtomicU32,
    /// Dropped with the source, so a case can tell when its last holder let go.
    _held: mpsc::UnboundedSender<()>,
}

/// A token source and the signal its last holder dropping closes.
fn counted_tokens() -> (Arc<CountedTokens>, mpsc::UnboundedReceiver<()>) {
    let (held, released) = mpsc::unbounded_channel();
    let tokens = Arc::new(CountedTokens {
        fetches: AtomicU32::new(0),
        _held: held,
    });
    (tokens, released)
}

/// The value the source below issues. Alphanumeric, so it survives a round trip
/// through a URL's percent encoding unchanged.
const TEST_TOKEN: &str = "issuedtokenvalue";

#[async_trait::async_trait]
impl AccessTokenSource for CountedTokens {
    async fn fetch(&self) -> Result<AccessToken, TokenFetchError> {
        self.fetches.fetch_add(1, Ordering::Relaxed);
        Ok(AccessToken::new(TEST_TOKEN, Duration::from_secs(3600))?)
    }
}

/// The seam a deployment arrives through, which no other case reaches: naming
/// [`PostgresAuth::IamTokens`] has to produce a pool whose handshakes carry a
/// token this store fetched, and leave a refresher holding that pool so the next
/// token lands before this one expires.
///
/// Everything downstream is asserted on a virtual clock against a lazily-built
/// pool, so without this a store that ignored the token source, or that built the
/// pool and started nothing, would pass the whole suite and then fail an hour
/// into a real deployment.
#[tokio::test]
async fn an_iam_authenticated_store_builds_its_pool_from_a_fetched_token() -> TestResult {
    let url = fresh_unmigrated_database_url().await?;
    let (source, _released) = counted_tokens();
    let store =
        PostgresFactStore::connect_and_migrate(&url, PostgresAuth::IamTokens(source.clone()))
            .await?;

    assert_eq!(
        source.fetches.load(Ordering::Relaxed),
        1,
        "the store must build its pool from a token it asked the source for"
    );
    assert_eq!(
        store.pool().connect_options().to_url_lossy().password(),
        Some(TEST_TOKEN),
        "the pool's next handshake must carry the fetched token as its password"
    );
    assert!(
        store.credentials().reporter_alive(),
        "the store must leave a refresher keeping its token fresh"
    );
    assert!(
        store.credentials().reported().is_none(),
        "a store that just connected has lost nothing"
    );

    store.close().await;
    Ok(())
}

/// A refused preparation must close the pool it opened. Under IAM the refresher
/// holds a pool handle of its own, so dropping the pool closes nothing: the
/// session stays open on the server and a task keeps fetching tokens for a store
/// that never existed, one of each per boot attempt. Closing ends both, which
/// shows up here as the refresher letting go of the token source.
#[tokio::test]
async fn a_refused_connect_closes_the_pool_it_opened() -> TestResult {
    let url = fresh_unmigrated_database_url().await?;
    let (source, mut released) = counted_tokens();
    match PostgresFactStore::connect(&url, PostgresAuth::IamTokens(source.clone())).await {
        Err(crate::DbError::Config(_)) => {}
        Err(other) => return Err(format!("expected a schema refusal, got: {other}").into()),
        Ok(store) => {
            store.close().await;
            return Err("connect must refuse a database carrying no fact schema".into());
        }
    }

    // The refresher is the only other holder, so this resolves the moment its
    // task ends. The bound is generous real time: a refresher the close reached
    // never approaches it, and one it did not would otherwise hang the suite.
    drop(source);
    match tokio::time::timeout(Duration::from_secs(30), released.recv()).await {
        Ok(None) => Ok(()),
        Ok(Some(())) => Err("nothing sends on this channel".into()),
        Err(_) => Err(
            "the refused connect left its refresher running, so the pool it opened \
             was dropped instead of closed and its session is still on the server"
                .into(),
        ),
    }
}
