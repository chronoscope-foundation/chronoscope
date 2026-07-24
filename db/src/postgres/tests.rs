//! Postgres fact-store tests: the backend-agnostic conformance suite stamped
//! against [`PostgresFactStore`] over the ephemeral-cluster harness, plus a
//! `PostGIS` smoke check.
//!
//! The ignore list is exactly the spatial-touching cases — the `PostGIS`
//! `InViewport` reads and tiled clustering, deferred to their own unit.
//! Retraction now lands (the recursive `RETRACTOR_CLOSURE` fixpoint +
//! `record_retraction`), so every retraction case runs GREEN. Anything the
//! spatial stubs break fails *loudly* (a red assertion, never a false green), so
//! a missed ignore surfaces on the first run.

use super::PostgresFactStore;
use super::harness::{fresh_pg_store, fresh_pg_store_at_default_isolation};
use crate::common::ids::{SqlEntityId, SqlEventId, SqlImageId};
use chronoscope_core::store::FactStore;
use chronoscope_core::store::conformance::{TestResult, UnmintedIds};

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
        // --- tiled clustering (spatial read; the clustering read, not
        //     retraction, is what these still wait on) ---
        cluster_tile_cells_match_the_same_tile_inside_a_viewport:
            "tiled clustering (spatial read) deferred to the pg spatial unit",
        cluster_tile_cells_keep_high_sub_tiles_under_a_dense_low_corner:
            "tiled clustering (spatial read) deferred to the pg spatial unit",
        cluster_entities_in_viewport_buckets_by_tile:
            "tiled clustering (spatial read) deferred to the pg spatial unit",
        cluster_entities_in_viewport_respects_snapshot:
            "tiled clustering (spatial read) deferred to the pg spatial unit",
        cluster_entities_in_viewport_groups_colocated_entities:
            "tiled clustering (spatial read) deferred to the pg spatial unit",
        cluster_cluster_becomes_singleton_when_a_member_is_retracted:
            "tiled clustering (spatial read) deferred to the pg spatial unit",
        cluster_colocated_becomes_singleton_when_a_member_is_retracted:
            "tiled clustering (spatial read) deferred to the pg spatial unit",
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
