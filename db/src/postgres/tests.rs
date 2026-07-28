//! Postgres fact-store tests: the backend-agnostic conformance suite stamped
//! against [`PostgresFactStore`] over the ephemeral-cluster harness, plus a
//! `PostGIS` smoke check.
//!
//! The ignore list is exactly the `PostGIS` `InViewport` reads, deferred to
//! their own unit. Retraction and tiled clustering both land — clustering is a
//! Morton-range scan on the `quadkey` facet and never touches `PostGIS` — so
//! their cases run GREEN. Anything the spatial stubs break fails *loudly* (a red
//! assertion, never a false green), so a missed ignore surfaces on the first run.

use super::harness::{fresh_pg_store, fresh_pg_store_at_default_isolation};
use super::{PostgresFactStore, PostgresFactStoreError};
use crate::common::ids::{SqlEntityId, SqlEventId, SqlImageId};
use chronoscope_core::grammar::ids::FactId;
use chronoscope_core::store::FactStore;
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

chronoscope_core::fact_store_conformance!(
    fresh_pg_store(),
    ignore(
        // --- `PostGIS` spatial reads (InViewport streams) ---
        walk_entity_classes_in_viewport_surfaces_located_and_moved_in_entities:
            "PostGIS spatial reads deferred to a later unit",
        walk_entity_classes_in_viewport_surfaces_cap_overlapping_viewport_edge:
            "PostGIS spatial reads deferred to a later unit",
        walk_entity_classes_in_viewport_surfaces_conjunction_with_unresolved_member:
            "PostGIS spatial reads deferred to a later unit",
        walk_image_classes_in_viewport_surfaces_captured_locations:
            "PostGIS spatial reads deferred to a later unit",
    )
);

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

/// The write path issues `SET TRANSACTION ISOLATION LEVEL READ COMMITTED` after
/// `BEGIN` because the counters-row `FOR UPDATE` serializer needs RC. Run it
/// against a database whose *session default* is `repeatable read`: the write tx
/// must still observe `read committed`, proving it pins its own isolation rather
/// than riding whatever the session default happens to be.
#[tokio::test]
async fn write_tx_pins_read_committed_despite_a_stricter_session_default() -> TestResult {
    use super::AsConn;

    let (store, _cx) = fresh_pg_store_at_default_isolation("repeatable read").await?;
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

    let (store, _cx) = fresh_pg_store_at_default_isolation("repeatable read").await?;
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
