//! Backend-agnostic conformance suite for [`FactStore`] implementations.
//!
//! Store/view/submit semantics — submit round-trips, declaration resolution,
//! content-address dedup, retraction visibility, walks and pagination,
//! equivalence classes, transaction atomicity and rejected-submit
//! containment — are contracts every backend owes, so their tests are
//! written once, generic over the store, and stamped per backend:
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
//! Instantiating a backend takes four things: a store-builder expression (a
//! future resolving to `Result<(S, Cx), E>`, evaluated fresh per test — `Cx`
//! is backend context held alive for the test's duration, `()` when the
//! store needs none), an [`UnmintedIds`] and a [`RefusalKinds`] impl for the
//! store, and `tokio` (`macros` + `rt`) available where the macro expands.
//!
//! ```ignore
//! use chronoscope_core::store::conformance::{RefusalKinds, UnmintedIds};
//!
//! impl UnmintedIds for MyFactStore { /* ids outside the mintable space */ }
//! impl RefusalKinds for MyFactStore { /* which error means which refusal */ }
//!
//! chronoscope_core::fact_store_conformance!(async {
//!     Ok::<_, MyError>((MyFactStore::fresh().await?, ()))
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

/// Recognition of the refusals a case asserts *by identity*. `S::Error` is
/// backend-shaped, so a generic case can only tell "it failed"; a mis-ordered
/// bind or a malformed statement would satisfy that just as well as the refusal
/// under test. Each backend answers for its own error type here, and the case
/// pins the failure it means.
pub trait RefusalKinds: FactStore {
    /// Whether `error` is the clustering read's refusal of a viewport spanning
    /// more tiles than one read may enumerate — the level being too fine for the
    /// span, as opposed to any other backend failure.
    fn is_cluster_tile_cap_refusal(error: &Self::Error) -> bool;
}

/// Stamp the fact-store conformance suite against one backend.
///
/// `$build_store` is an expression evaluating to a future of
/// `Result<(S, Cx), E>` (`E: std::error::Error + 'static`) — a fresh empty
/// store per test, re-evaluated for each. `Cx` is backend context the test
/// holds alive alongside the store (a file-backed store's tempdir); pass
/// `()` when the store needs none. The store must implement
/// [`UnmintedIds`](crate::store::conformance::UnmintedIds) and
/// [`RefusalKinds`](crate::store::conformance::RefusalKinds), and `tokio`
/// (`macros` + `rt`) must be available where the macro expands.
///
/// Each case becomes its own `#[tokio::test]`, so a backend's failures
/// report under the case's name. The suite-wide ignored cases pin contracts
/// for reads every backend still stubs (event classes); they flip green once
/// the read is implemented.
///
/// The optional `ignore(...)` form marks additional cases `#[ignore]` for
/// this backend only, each with a reason naming the capability the backend
/// doesn't offer yet:
///
/// ```ignore
/// chronoscope_core::fact_store_conformance!(
///     async { MyFactStore::fresh().await },
///     ignore(
///         walk_entity_classes_group_submitted_facts_into_one_class:
///             "class-stream walks are not implemented",
///     )
/// );
/// ```
///
/// Ignored cases still compile and stay countable in the runner's ignored
/// tally, so a backend's gaps are visible rather than silently absent. A
/// name that isn't a real case fails to compile.
#[macro_export]
macro_rules! fact_store_conformance {
    ($build_store:expr $(,)?) => {
        $crate::fact_store_conformance!($build_store, ignore());
    };
    ($build_store:expr, ignore($($icase:ident: $ireason:literal),* $(,)?) $(,)?) => {
        $crate::fact_store_conformance! { @expand ($) $build_store, [$(($icase, $ireason))*] }
    };
    (@expand ($d:tt) $build_store:expr, [$(($icase:ident, $ireason:literal))*]) => {
        // Existence check: a renamed or removed case can't linger silently in
        // an ignore list.
        $( use $crate::store::conformance::cases::$icase as _; )*

        // One arm per backend-ignored case name, matched ahead of the
        // catch-all: listing a case ident routes its stamp through the
        // `#[ignore]` arm, so name matching happens at macro-expansion time.
        macro_rules! __fact_store_conformance_case {
            $(
                ($d(#[$d gattr:meta])* $icase) => {
                    $d(#[$d gattr])*
                    #[ignore = $ireason]
                    #[tokio::test]
                    async fn $icase() -> $crate::store::conformance::TestResult {
                        let (store, _cx) = $build_store.await?;
                        $crate::store::conformance::cases::$icase(store).await
                    }
                };
            )*
            ($d(#[$d attr:meta])* $d case:ident) => {
                $d(#[$d attr])*
                #[tokio::test]
                async fn $d case() -> $crate::store::conformance::TestResult {
                    let (store, _cx) = $build_store.await?;
                    $crate::store::conformance::cases::$d case(store).await
                }
            };
        }

        $crate::fact_store_conformance! { @cases =>
            roundtrip_small_commit_through_fact_lookup,
            existing_decl_passes_through_to_supplied_id,
            local_decls_mint_distinct_newly_minted_ids,
            name_twin_with_distinct_references_mints_fresh,
            name_match_without_references_joins_existing_class,
            external_reference_match_joins_existing_entity_class,
            source_url_match_joins_existing_image_class,
            retracted_identity_edge_splits_classes_and_earlier_snapshots_stay_merged,
            re_merging_a_split_pair_submits_cleanly_and_restores_the_class,
            walk_entity_classes_group_submitted_facts_into_one_class,
            class_walk_pages_distinct_entities_as_contiguous_runs,
            all_facts_about_entity_returns_facts_mentioning_it,
            all_facts_about_event_returns_facts_mentioning_it,
            walk_image_classes_group_submitted_facts_into_one_class,
            walk_entity_depictions_pages_the_images_depicting_an_entity,
            walk_entity_depictions_pages_across_the_next_class_cursor,
            all_facts_about_image_returns_facts_mentioning_it,
            entity_class_contains_both_same_entity_members,
            entity_representative_is_canonical_across_same_entity_members,
            #[ignore = "submit rejects SameEvent while event_class is stubbed to the singleton {member}; this pins that a SameEvent-linked pair shares a two-member class and flips green once reads resolve event equivalence and the submit rejection is lifted"]
            event_class_contains_both_same_event_members,
            #[ignore = "submit rejects SameEvent while event_representative is stubbed to return member itself; this pins that both SameEvent members share one canonical representative and flips green once reads resolve event equivalence and the submit rejection is lifted"]
            event_representative_is_canonical_across_same_event_members,
            image_class_contains_both_same_artifact_members,
            image_representative_is_canonical_across_same_artifact_members,
            image_representatives_batch_matches_per_id_resolution,
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
            swallowed_submit_rejection_leaves_nothing_durable,
            record_commit_marks_only_its_own_facts_committed,
            overlapping_recorded_commits_never_commit,
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
            cross_source_kind_conflict_stores_as_separate_events,
            existing_event_accepts_new_facts_without_restating_has_event,
            same_event_identity_fact_rejected,
            event_retype_across_same_commit_retraction_rejected,
            event_rehome_across_retraction_rejected,
            event_retype_across_retraction_rejected,
            event_reassert_identical_has_event_after_retraction_accepted,
            event_fresh_id_sets_its_own_pin,
            event_readopt_retracted_id_under_new_owner_rejected,
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
            walk_entity_classes_in_viewport_surfaces_located_and_moved_in_entities,
            walk_entity_classes_in_viewport_surfaces_cap_overlapping_viewport_edge,
            walk_entity_classes_in_viewport_surfaces_conjunction_with_unresolved_member,
            walk_image_classes_in_viewport_surfaces_captured_locations,
            class_walk_next_class_cursor_skips_to_the_next_representative,
            cluster_tile_cells_match_the_same_tile_inside_a_viewport,
            cluster_tile_cells_keep_high_sub_tiles_under_a_dense_low_corner,
            cluster_entities_in_viewport_buckets_by_tile,
            cluster_entities_in_viewport_respects_snapshot,
            cluster_entities_in_viewport_groups_colocated_entities,
            cluster_cluster_becomes_singleton_when_a_member_is_retracted,
            cluster_colocated_becomes_singleton_when_a_member_is_retracted,
            cluster_entities_in_viewport_places_a_moved_entity_under_its_owner,
            cluster_entities_in_viewport_refuses_a_level_too_fine_for_the_span,
            cluster_entities_in_viewport_excludes_image_capture_locations,
        }
    };
    (@cases => $($(#[$attr:meta])* $case:ident,)+) => {
        $( __fact_store_conformance_case! { $(#[$attr])* $case } )+
    };
}
