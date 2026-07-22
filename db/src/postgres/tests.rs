//! Postgres fact-store tests: the backend-agnostic conformance suite stamped
//! against [`PostgresFactStore`] over the ephemeral-cluster harness, plus a
//! `PostGIS` smoke check.
//!
//! The ignore list is exactly the retraction- and spatial-touching cases —
//! deferred to their own units (the recursive `RETRACTOR_CLOSURE` fixpoint and
//! the `PostGIS` reads). Everything else runs GREEN. The retraction stub breaks
//! every retraction case *loudly* (a red assertion, never a false green), so a
//! missed ignore surfaces on the first run; the list is the grep of case names
//! for `retract|retraction|retracted|supersede|in_viewport|viewport` plus the
//! one grep-miss (`re_merging_a_split_pair_...`, which `commit_retract`s an edge
//! under a "restores" name).

use super::PostgresFactStore;
use super::harness::fresh_pg_store;
use crate::common::ids::{SqlEntityId, SqlEventId, SqlImageId};
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
        // --- retraction (recursive RETRACTOR_CLOSURE + record_retraction) ---
        retracted_identity_edge_splits_classes_and_earlier_snapshots_stay_merged:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        re_merging_a_split_pair_submits_cleanly_and_restores_the_class:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        retract_commit_targeting_unrecorded_commit_rejected:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        retract_commit_targeting_recorded_commit_succeeds:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        retract_commit_of_earlier_commit_in_same_tx_lands:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        retract_fact_targeting_same_commit_fact_rejected:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        retract_fact_targeting_prior_fact_accepted:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        supersede_fact_targeting_same_commit_fact_rejected:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        supersede_fact_with_equal_target_and_replacement_rejected:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        retract_fact_hides_target_only_after_its_commit:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        retract_commit_hides_every_fact_of_target:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        supersede_fact_hides_target_and_keeps_replacement:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        retraction_of_retraction_restores_visibility:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        supersede_fact_with_unminted_replacement_rejected:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        retracted_by_reports_lowest_still_effective_retractor:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        all_facts_about_image_excludes_retracted:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        event_retype_across_same_commit_retraction_rejected:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        event_rehome_across_retraction_rejected:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        event_retype_across_retraction_rejected:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        event_reassert_identical_has_event_after_retraction_accepted:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        event_readopt_retracted_id_under_new_owner_rejected:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        event_retype_without_retracting_stale_payload_rejected:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        observation_depiction_retracted_in_same_commit_rejected:
            "retraction (recursive RETRACTOR_CLOSURE) deferred to a later unit",
        // --- `PostGIS` spatial reads (InViewport streams) ---
        walk_entity_classes_in_viewport_surfaces_located_and_moved_in_entities:
            "PostGIS spatial reads deferred to a later unit",
        walk_entity_classes_in_viewport_surfaces_cap_overlapping_viewport_edge:
            "PostGIS spatial reads deferred to a later unit",
        walk_entity_classes_in_viewport_surfaces_conjunction_with_unresolved_member:
            "PostGIS spatial reads deferred to a later unit",
        walk_image_classes_in_viewport_surfaces_captured_locations:
            "PostGIS spatial reads deferred to a later unit",
        // --- tiled clustering (spatial read; two cases also retract a member) ---
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
            "tiled clustering + retraction deferred to later units",
        cluster_colocated_becomes_singleton_when_a_member_is_retracted:
            "tiled clustering + retraction deferred to later units",
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
