//! Backend-agnostic conformance suite for [`FactStore`] implementations.
//!
//! Store/view/submit semantics — submit round-trips, declaration resolution,
//! content-address dedup, retraction visibility, walks and pagination,
//! equivalence classes, transaction atomicity and poisoning — are contracts
//! every backend owes, so their tests are written once, generic over the
//! store, and stamped per backend:
//!
//! - [`cases`] holds the generic test bodies — each an `async fn` taking a
//!   fresh store and returning [`TestResult`]. Assertions thread the ids a
//!   submit's [`SubmitResult`](crate::submit::SubmitResult) resolutions
//!   minted; nothing assumes a backend's id shape or minting order beyond the
//!   `Ord` the id scheme carries.
//! - [`fixtures`] holds the producer-form fact/bundle builders and commit
//!   helpers the cases (and backend-specific tests) share.
//! - [`fact_store_conformance!`](crate::fact_store_conformance) stamps the
//!   suite as `#[tokio::test]` functions in the instantiating crate, one per
//!   case, individually named and reportable.
//!
//! Instantiating a backend takes three things: a store-builder expression (a
//! future resolving to `Result<S, E>`, evaluated fresh per test), an
//! [`UnmintedIds`] impl for the store, and `tokio` (`macros` + `rt`)
//! available where the macro expands.
//!
//! ```ignore
//! use chronoscope_core::store::conformance::UnmintedIds;
//!
//! impl UnmintedIds for MyFactStore { /* ids outside the mintable space */ }
//!
//! chronoscope_core::fact_store_conformance!(async {
//!     Ok::<_, MyError>(MyFactStore::fresh().await?)
//! });
//! ```
//!
//! Other crates reach this module through core's `test-support` feature (a
//! dev-dependency on `chronoscope-core` with that feature enabled); core's
//! own tests reach it through `cfg(test)`.
//!
//! [`FactStore`]: crate::store::FactStore

pub mod cases;
pub mod fixtures;

use crate::store::{EntityIdOf, EventIdOf, FactStore, ImageIdOf};

/// Boxed error the suite's cases and fixtures propagate with `?`.
pub type TestError = Box<dyn std::error::Error>;

/// The `Result` every conformance case returns.
pub type TestResult = Result<(), TestError>;

/// Ids the store under test never mints — fixtures for the `Decl::Existing`
/// unknown-id rejections. Implemented alongside each backend's suite
/// instantiation, since only the backend knows its id space.
pub trait UnmintedIds: FactStore {
    /// An entity id outside the store's mintable space.
    fn unminted_entity() -> EntityIdOf<Self>;
    /// An event id outside the store's mintable space.
    fn unminted_event() -> EventIdOf<Self>;
    /// An image id outside the store's mintable space.
    fn unminted_image() -> ImageIdOf<Self>;
}

/// Stamp the fact-store conformance suite against one backend.
///
/// `$build_store` is an expression evaluating to a future of `Result<S, E>`
/// (`E: std::error::Error + 'static`) — a fresh empty store per test,
/// re-evaluated for each. The store must implement
/// [`UnmintedIds`](crate::store::conformance::UnmintedIds), and `tokio`
/// (`macros` + `rt`) must be available where the macro expands.
///
/// Each case becomes its own `#[tokio::test]`, so a backend's failures
/// report under the case's name. The ignored cases pin contracts for reads
/// the backends still stub (`walk_events`, event classes); they flip green
/// once the read is implemented.
#[macro_export]
macro_rules! fact_store_conformance {
    ($build_store:expr) => {
        $crate::fact_store_conformance! { @cases $build_store =>
            roundtrip_small_commit_through_fact_lookup,
            existing_decl_passes_through_to_supplied_id,
            local_decls_mint_distinct_newly_minted_ids,
            walk_entity_classes_group_submitted_facts_into_one_class,
            class_walk_pages_distinct_entities_as_contiguous_runs,
            all_facts_about_entity_returns_facts_mentioning_it,
            #[ignore = "walk_events is stubbed to an empty page; this pins the walk-returns-submitted-facts contract and flips green once walk_events reads the fact bag"]
            walk_events_returns_submitted_event_facts,
            all_facts_about_event_returns_facts_mentioning_it,
            walk_image_classes_group_submitted_facts_into_one_class,
            all_facts_about_image_returns_facts_mentioning_it,
            entity_class_contains_both_same_entity_members,
            entity_representative_is_canonical_across_same_entity_members,
            #[ignore = "event_class is stubbed to the singleton {member}; this pins that a SameEvent-linked pair shares a two-member class and flips green once the union-find read is implemented"]
            event_class_contains_both_same_event_members,
            #[ignore = "event_representative is stubbed to return member itself; this pins that both SameEvent members share one canonical representative and flips green once representative selection is implemented"]
            event_representative_is_canonical_across_same_event_members,
            image_class_contains_both_same_artifact_members,
            image_representative_is_canonical_across_same_artifact_members,
            fact_referencing_out_of_range_entity_idx_returns_error,
            fact_referencing_out_of_range_event_idx_returns_error,
            fact_referencing_out_of_range_image_idx_returns_error,
            entity_idx_at_decl_count_is_first_rejected,
            entity_idx_at_decl_count_minus_one_is_accepted,
            unreferenced_entity_decl_rejected_as_unused,
            decl_existing_unknown_entity_id_rejected,
            decl_existing_unknown_event_id_rejected,
            decl_existing_unknown_image_id_rejected,
            same_entity_resolving_to_one_id_rejected_at_substitution,
            two_entity_decls_resolving_to_one_id_rejected,
            retract_commit_targeting_unrecorded_commit_rejected,
            retract_commit_targeting_recorded_commit_succeeds,
            two_commits_share_one_with_tx_brand,
            err_from_with_tx_closure_rolls_back_submitted_commit,
            retract_commit_of_earlier_commit_in_same_tx_lands,
            swallowed_submit_rejection_poisons_the_transaction,
            record_commit_marks_only_its_own_facts_committed,
            overlapping_recorded_commits_fail_the_transaction_at_apply,
            retract_fact_targeting_same_commit_fact_rejected,
            retract_fact_targeting_prior_fact_accepted,
            supersede_fact_targeting_same_commit_fact_rejected,
            supersede_fact_with_equal_target_and_replacement_rejected,
            retract_fact_hides_target_only_after_its_commit,
            read_snapshot_placement_is_committed_or_absent,
            read_snapshot_placement_never_inflight_past_watermark,
            retract_commit_hides_every_fact_of_target,
            supersede_fact_hides_target_and_keeps_replacement,
            retraction_of_retraction_restores_visibility,
            supersede_fact_with_unminted_replacement_rejected,
            retracted_by_reports_lowest_still_effective_retractor,
            multi_rule_violations_accumulate_in_one_batch,
            resolvability_gate_batches_and_skips_rules,
            all_facts_about_image_excludes_retracted,
            all_facts_about_image_respects_snapshot,
            construction_location_accepted,
            disjoint_conjunction_location_rejected,
            pending_conjunction_location_accepted,
            consistent_conjunction_location_accepted,
            over_complex_location_rejected_at_cap_accepted,
            event_with_has_event_and_consistent_payloads_stored,
            event_without_has_event_rejected,
            event_referenced_only_as_gap_endpoint_without_has_event_rejected,
            event_two_has_event_kinds_rejected,
            event_two_has_event_entities_rejected,
            event_payload_contradicts_declared_kind_rejected,
            event_date_contradicts_declared_category_rejected,
            cross_source_kind_conflict_via_same_event_stored,
            event_kind_rule_sees_same_commit_retraction,
            event_retype_without_retracting_stale_payload_rejected,
            observation_depiction_retracted_in_same_commit_rejected,
            name_window_inverted_rejected,
            name_window_equal_accepted,
            name_window_open_bound_accepted,
            composite_self_parent_rejected,
            composite_distinct_accepted,
            composite_multiple_parents_across_commits_rejected,
            composite_self_loop_does_not_trip_multiple_parents,
            composite_chain_across_commits_rejected,
            captured_date_submits_cleanly,
            depiction_with_medium_submits_cleanly,
            former_role_conflict_combination_now_submits,
            disagreeing_media_submit_cleanly,
            observation_without_depiction_rejected,
            observation_with_same_commit_depiction_accepted,
            observation_with_prior_commit_depiction_accepted,
            observation_cited_external_accepted,
            single_interval_bookend_accepted,
            disjunctive_bookend_date_rejected,
            empty_bookend_date_rejected,
            disjunctive_judgment_citation_date_rejected,
            disjunctive_meta_citation_date_rejected,
            walk_entity_classes_in_bbox_surfaces_located_and_moved_in_entities,
            class_walk_next_class_cursor_skips_to_the_next_representative,
        }
    };
    (@cases $build_store:expr => $($(#[$attr:meta])* $case:ident,)+) => {
        $(
            $(#[$attr])*
            #[tokio::test]
            async fn $case() -> $crate::store::conformance::TestResult {
                let store = $build_store.await?;
                $crate::store::conformance::cases::$case(store).await
            }
        )+
    };
}
