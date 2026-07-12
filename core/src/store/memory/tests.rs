//! Memory-backend tests: the conformance suite stamped against
//! [`MemoryFactStore`], plus what is genuinely backend-specific — wire
//! canonicalization checks, overlay/counter internals behind
//! [`MemoryFactStore::lock_inner`], property tests over the dense id space,
//! and the listing/projection consumers.

use super::*;

use crate::geo::GeoPoint;
use crate::grammar::attribute::NameText;
use crate::grammar::citations::Language;
use crate::listing;
use crate::projection::{member_lineage, project_entity};
use crate::store::SubmitCommitError;
use crate::store::conformance::fixtures::{
    PAGE_100, commit_name, commit_result, commit_retract, construction_at,
    construction_location_in, fixed_time, has_event_fact, local_bundle, moved_kind, moved_to,
    name_fact, same_entity_fact, sample_viewport, submit_batch, user_author,
};
use crate::store::conformance::{TestResult, UnmintedIds};
use crate::submit::{Commit as SubmitBundle, Decl, EntityIdx, SubmitError, commit_facts};

type TestBundle = SubmitBundle<MemoryIds>;

/// Counters mint dense from zero, so `u64::MAX` is never assigned.
impl UnmintedIds for MemoryFactStore {
    fn unminted_entity() -> MemoryEntityId {
        MemoryEntityId(u64::MAX)
    }
    fn unminted_event() -> MemoryEventId {
        MemoryEventId(u64::MAX)
    }
    fn unminted_image() -> MemoryImageId {
        MemoryImageId(u64::MAX)
    }
}

crate::fact_store_conformance!(async {
    Ok::<_, std::convert::Infallible>((MemoryFactStore::new(), ()))
});

// --- canonical-form wire boundary ---
//
// Commits are content-addressed over their producer-form bytes, so the wire
// boundary rejects non-canonical values: anything that deserializes
// re-serializes byte-identically.

/// Deserializing a non-NFC name errors, and the error names the canonical
/// form.
#[test]
fn deserializing_non_nfc_name_is_rejected() -> TestResult {
    let result = serde_json::from_str::<NameText>("\"Panthe\\u0301on\"");
    let Err(e) = result else {
        return Err(format!("expected a non-NFC rejection, got {result:?}").into());
    };
    assert!(
        e.to_string().contains("Panth\u{e9}on"),
        "the rejection must name the canonical form; got {e}"
    );
    Ok(())
}

/// Deserializing a non-canonical language tag errors, and the error names
/// the canonical form.
#[test]
fn deserializing_non_canonical_language_tag_is_rejected() -> TestResult {
    let result = serde_json::from_str::<Language>("\"en-us\"");
    let Err(e) = result else {
        return Err(format!("expected a non-canonical rejection, got {result:?}").into());
    };
    assert!(
        e.to_string().contains("en-US"),
        "the rejection must name the canonical form; got {e}"
    );
    Ok(())
}

/// A canonical language tag deserializes and re-serializes byte-identically.
#[test]
fn canonical_language_tag_round_trips_byte_identically() -> TestResult {
    let wire = "\"en-US\"";
    let tag: Language = serde_json::from_str(wire)?;
    assert_eq!(serde_json::to_string(&tag)?, wire);
    Ok(())
}

// --- overlay internals (lock_inner) ---

/// Stage a fact through the [`FactWrite`] primitives and verify it lands.
///
/// Starts with one committed fact; inside a transaction, mints one id of each
/// kind, stages a clone of the seed fact, and records a synthetic commit
/// covering it. The staged fact's id is the slot one past the committed
/// facts, `placement` flips from `InFlight` to `Committed` at
/// `record_commit`, and the apply grows the fact bag and counters by exactly
/// the staged delta.
#[tokio::test]
async fn tx_stage_and_record_lands_at_provisional_fact_id() -> TestResult {
    // Commit one fact so the store has committed state to union over.
    let store = MemoryFactStore::new();
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "seed")?].into_iter().collect(),
    };
    let seed_result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(seed_result.fact_ids.len(), 1);

    // Clone the committed fact's body to re-stage, avoiding a freshly
    // synthesised StoredFact.
    let (
        committed_len_before,
        next_entity_before,
        next_event_before,
        next_image_before,
        seed_stored,
    ) = {
        let guard = store.lock_inner().await;
        let seed = guard
            .facts
            .first()
            .ok_or("seed fact missing from inner")?
            .clone();
        (
            guard.facts.len(),
            guard.next_entity_id,
            guard.next_event_id,
            guard.next_image_id,
            seed,
        )
    };

    // The slot the staged fact takes: one past the committed facts.
    let expected_fact_id = FactId::new(committed_len_before as u64);

    let synthetic_id = CommitId::parse("ab".repeat(32))?;
    let recorded_commit_id = synthetic_id.clone();
    let author = user_author()?;
    store
        .with_tx(|_s, tx| {
            Box::pin(async move {
                tx.mint_entity().await.map_err(|e| format!("{e:?}"))?;
                tx.mint_event().await.map_err(|e| format!("{e:?}"))?;
                tx.mint_image().await.map_err(|e| format!("{e:?}"))?;

                let fid = tx
                    .stage_fact(seed_stored)
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                assert_eq!(fid, expected_fact_id);

                // Staged fact is visible at its slot, in flight until recorded.
                let lookup = tx.fact(fid).await.map_err(|e| format!("{e:?}"))?;
                let FactLookup::Active(_) = lookup else {
                    return Err(format!("staged fact must read Active; got {lookup:?}"));
                };
                let placement = tx.placement(fid).await.map_err(|e| format!("{e:?}"))?;
                assert_eq!(placement, FactPlacement::InFlight);

                let stored = StoredCommit {
                    commit_id: synthetic_id.clone(),
                    author,
                    recorded_at: fixed_time(),
                    fact_ids: vec![fid],
                };
                let result = MemSubmitResult {
                    commit_id: synthetic_id,
                    previously_committed: false,
                    fact_ids: vec![fid],
                    entities: HashMap::new(),
                    events: HashMap::new(),
                    images: HashMap::new(),
                    companion_commit_id: None,
                };
                tx.record_commit(stored, &result)
                    .await
                    .map_err(|e| format!("{e:?}"))?;

                // Recording moves the fact behind the in-flight boundary.
                let placement = tx.placement(fid).await.map_err(|e| format!("{e:?}"))?;
                assert_eq!(placement, FactPlacement::Committed);
                Ok::<_, String>(())
            })
        })
        .await
        .map_err(|e| format!("{e:?}"))??;

    // After apply: fact bag grew by one at the promised slot, counters
    // advanced by the mints, and the fact belongs to the recorded commit.
    {
        let guard = store.lock_inner().await;
        assert_eq!(guard.facts.len(), committed_len_before + 1);
        assert_eq!(guard.next_entity_id, next_entity_before + 1);
        assert_eq!(guard.next_event_id, next_event_before + 1);
        assert_eq!(guard.next_image_id, next_image_before + 1);
        assert_eq!(guard.fact_commits.last(), Some(&recorded_commit_id));
        assert!(guard.commits.contains_key(&recorded_commit_id));
    }

    Ok(())
}

/// A swallowed rejection that minted ids but staged no facts cannot burn
/// counters: the poison stops the transaction from applying, so no phantom
/// id becomes durable or satisfies a later commit's `Decl::Existing`.
#[tokio::test]
async fn swallowed_zero_staged_rejection_cannot_burn_counters() -> TestResult {
    let store = MemoryFactStore::new();
    commit_name(&store, "anchor").await?;
    let next_entity_before = store.lock_inner().await.next_entity_id;

    // Two `Existing` decls of one id make the identity fact a substitution
    // self-loop, so it never stages; the `Local` decl still mints. The batch
    // also carries DuplicateEntityDecl and UnusedDeclaration — rejected
    // either way, with one burned counter and zero staged facts.
    let doomed: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![
            Decl::Existing {
                id: MemoryEntityId(0),
            },
            Decl::Existing {
                id: MemoryEntityId(0),
            },
            Decl::Local,
        ],
        events: Vec::new(),
        images: Vec::new(),
        facts: [same_entity_fact(0, 1)?].into_iter().collect(),
    };
    let outcome = store
        .with_tx(|s, tx| {
            Box::pin(async move {
                match s.submit_commit(tx, doomed).await {
                    Err(_) => Ok::<_, String>(()),
                    Ok(_) => Err("expected the submit to be rejected".to_owned()),
                }
            })
        })
        .await;
    assert!(
        outcome.is_err(),
        "a poisoned transaction must fail at apply, got {outcome:?}"
    );

    // The rejected submit's mint never became durable.
    assert_eq!(store.lock_inner().await.next_entity_id, next_entity_before);

    // The phantom id fails a later commit's Existing check.
    let phantom: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(20),
        entities: vec![Decl::Existing {
            id: MemoryEntityId(next_entity_before),
        }],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "phantom")?].into_iter().collect(),
    };
    let err = match commit_facts(&store, phantom).await {
        Ok(_) => return Err("expected UnknownExistingEntity".into()),
        Err(e) => e,
    };
    let errs = submit_batch(err)?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::UnknownExistingEntity { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// This backend's failed submit scope poisons the transaction: a later submit on
/// the same handle refuses as a backend error naming the poison instead of
/// validating against the rejected staging, and a closure that swallows the
/// rejection and returns `Ok` fails at apply rather than committing the
/// leftovers.
#[tokio::test]
async fn swallowed_rejection_poisons_later_submits_and_the_apply() -> TestResult {
    let store = MemoryFactStore::new();
    // Decl 1 is never referenced: the bundle stages its one fact, then
    // rejects with UnusedDeclaration.
    let doomed: TestBundle = local_bundle(2, 0, 0, 0, vec![name_fact(0, "staged-then-rejected")?])?;
    let healthy: TestBundle =
        local_bundle(1, 0, 0, 10, vec![name_fact(0, "after-the-rejection")?])?;
    let outcome = store
        .with_tx(|s, tx| {
            Box::pin(async move {
                if s.submit_commit(tx, doomed).await.is_ok() {
                    return Err("expected the submit to be rejected".to_owned());
                }
                let Err(SubmitCommitError::Backend(e)) = s.submit_commit(tx, healthy).await else {
                    return Err("expected a backend error from the poisoned tx".to_owned());
                };
                let rendered = e.to_string();
                if !rendered.contains("transaction poisoned")
                    || !rendered.contains("an earlier submit failed")
                {
                    return Err(format!("expected the poison cause named, got {rendered}"));
                }
                Ok::<_, String>(())
            })
        })
        .await;
    assert!(
        outcome.is_err(),
        "a poisoned transaction must fail at apply, got {outcome:?}"
    );
    assert_eq!(store.next_fact_id().await?, FactId::new(0));
    Ok(())
}

// --- property tests ---

mod props {
    //! Property tests for the two unscaffolded algorithms: the backlink
    //! pagination cursor loop (async, over a live store) and the
    //! accumulating index substitution (sync, over the pipeline boundary).

    use std::collections::BTreeSet;
    use std::num::NonZeroUsize;

    use proptest::prelude::*;

    use super::{EntityIdx, MemoryEntityId, MemoryFactStore, MemoryIds, SubmitError, name_fact};
    use crate::grammar::ids::FactId;
    use crate::store::{EntityView, FactStore};
    use crate::submit::pipeline::substitute_facts_accumulating;

    /// A pagination scenario: `n` facts about one entity, a per-fact retraction
    /// mask of length `n`, and a page `limit` in `1..=n+2`.
    #[derive(Debug, Clone)]
    struct PaginationSpec {
        n: usize,
        retracted: Vec<bool>,
        limit: NonZeroUsize,
    }

    fn pagination_spec() -> impl Strategy<Value = PaginationSpec> {
        (1usize..=12)
            .prop_flat_map(|n| {
                let mask = proptest::collection::vec(any::<bool>(), n);
                let limit = 1usize..=(n + 2);
                (Just(n), mask, limit)
            })
            .prop_filter_map("limit is non-zero", |(n, retracted, limit)| {
                NonZeroUsize::new(limit).map(|limit| PaginationSpec {
                    n,
                    retracted,
                    limit,
                })
            })
    }

    /// Commit `n` distinct name facts about one entity, retract the masked
    /// subset in later commits, then drain the entity backlink page-by-page
    /// with `limit`. Returns the drained ids in page order and the active set
    /// (submitted minus retracted). Any store error becomes a `TestCaseError`.
    async fn drive_pagination(
        spec: &PaginationSpec,
    ) -> Result<(Vec<FactId>, BTreeSet<FactId>), TestCaseError> {
        let store = MemoryFactStore::new();

        let mut facts = BTreeSet::new();
        for i in 0..spec.n {
            let fact = name_fact(0, &format!("name-{i}"))
                .map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
            facts.insert(fact);
        }
        let bundle = super::SubmitBundle {
            author: super::user_author().map_err(|e| TestCaseError::fail(format!("{e:?}")))?,
            recorded_at: super::fixed_time(),
            entities: vec![super::Decl::Local],
            events: Vec::new(),
            images: Vec::new(),
            facts,
        };
        let result = super::commit_facts(&store, bundle)
            .await
            .map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
        let entity = result
            .entities
            .get(&EntityIdx(0))
            .ok_or_else(|| TestCaseError::fail("entity 0 missing"))?
            .id;

        // Submitted fact ids, in push (ascending) order.
        let submitted = &result.fact_ids;
        if submitted.len() != spec.n {
            return Err(TestCaseError::fail(format!(
                "expected {} facts, committed {}",
                spec.n,
                submitted.len()
            )));
        }

        // Retract the masked subset, each in its own later commit so the
        // retractor meta-facts hash distinctly.
        let mut active = BTreeSet::new();
        for (i, &fid) in submitted.iter().enumerate() {
            if spec.retracted.get(i).copied().unwrap_or(false) {
                super::commit_retract(&store, fid, (i as i64) + 1)
                    .await
                    .map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
            } else {
                active.insert(fid);
            }
        }

        // Drive the cursor loop. Cap iterations so a cursor bug can't hang.
        let mut view = store
            .now()
            .await
            .map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
        let mut drained = Vec::new();
        let mut cursor: Option<FactId> = None;
        let max_pages = spec.n + 2;
        let mut pages = 0;
        loop {
            if pages > max_pages {
                return Err(TestCaseError::fail(format!(
                    "cursor loop did not terminate after {max_pages} pages: {drained:?}"
                )));
            }
            pages += 1;
            let page = view
                .all_facts_about_entity(&entity, cursor, spec.limit)
                .await
                .map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
            drained.extend(page.items.iter().map(|item| item.fact_id));
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        Ok((drained, active))
    }

    /// A scenario: `decl_count` declared entities and a vector of entity
    /// indices, each `< 2 * decl_count` so roughly half land out of range.
    #[derive(Debug, Clone)]
    struct SubstitutionSpec {
        decl_count: usize,
        indices: Vec<usize>,
    }

    fn substitution_spec() -> impl Strategy<Value = SubstitutionSpec> {
        (1usize..=8)
            .prop_flat_map(|decl_count| {
                // Indices span both in-range (< decl_count) and out-of-range
                // (>= decl_count) so each call exercises both partitions.
                let upper = 2 * decl_count;
                let indices = proptest::collection::vec(0usize..upper, 0..=12);
                (Just(decl_count), indices)
            })
            .prop_map(|(decl_count, indices)| SubstitutionSpec {
                decl_count,
                indices,
            })
    }

    proptest! {
        /// Paging the backlink drains exactly the active set, in strictly
        /// ascending id order. Covers the `next_cursor` resume loop, the
        /// all-retracted empty drain, and the short-page-with-cursor case
        /// (a retracted fact is skipped without consuming a slot).
        #[test]
        fn pagination_drains_exactly_the_active_set(spec in pagination_spec()) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| TestCaseError::fail(format!("runtime: {e}")))?;
            let (drained, active) = rt.block_on(drive_pagination(&spec))?;

            let ascending = drained.windows(2).all(|w| w[0] < w[1]);
            prop_assert!(
                ascending,
                "pages must drain ascending with no dup/skip: {drained:?}"
            );
            let drained_set: BTreeSet<FactId> = drained.into_iter().collect();
            prop_assert_eq!(drained_set, active);
        }

        /// Substitution partitions the fact set by entity-index validity:
        /// every in-range fact lands in `out`, every out-of-range one lands
        /// as a typed `EntityIdxOutOfRange` carrying the input `decl_count`,
        /// and the two partitions cover the input exactly once.
        ///
        /// Order-independence is structural (the input is a `BTreeSet`), so
        /// the pinned property is the count-partition and the typed error,
        /// not ordering.
        #[test]
        fn substitution_partitions_facts_by_index_validity(spec in substitution_spec()) {
            // One distinct fact per index (unique names → distinct BTreeSet
            // elements). Distinct names keep every index its own fact even
            // when indices repeat.
            let mut facts = BTreeSet::new();
            for (slot, &idx) in spec.indices.iter().enumerate() {
                let fact = name_fact(idx, &format!("fact-{slot}"))
                    .map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
                facts.insert(fact);
            }
            let total = facts.len();

            // Resolution map for the in-range indices only.
            let entities: std::collections::HashMap<EntityIdx, MemoryEntityId> = (0..spec.decl_count)
                .map(|i| (EntityIdx(i), MemoryEntityId(i as u64)))
                .collect();
            let events = std::collections::HashMap::new();
            let images = std::collections::HashMap::new();

            let (out, errors) =
                substitute_facts_accumulating::<MemoryIds>(&facts, &entities, &events, &images);

            let expected_errors = spec
                .indices
                .iter()
                .filter(|&&idx| idx >= spec.decl_count)
                .count();
            // Indices collapsed by the unique-name BTreeSet can't drop an
            // out-of-range one: each name is distinct, so `total == indices`.
            prop_assert_eq!(total, spec.indices.len());
            prop_assert_eq!(errors.len(), expected_errors);

            for err in &errors {
                match err {
                    SubmitError::EntityIdxOutOfRange { decl_count, .. } => {
                        prop_assert_eq!(*decl_count, spec.decl_count);
                    }
                    other => {
                        return Err(TestCaseError::fail(format!(
                            "expected EntityIdxOutOfRange, got {other:?}"
                        )));
                    }
                }
            }

            // Every fact classified exactly once: in-range → out, else error.
            prop_assert_eq!(out.len(), total - expected_errors);
            prop_assert_eq!(out.len() + errors.len(), total);
        }
    }
}

// --- entity listing (summaries_in_viewport) ---

/// `extract_point` reads a resolved circle's center and declines a symbolic
/// reference — the placeability test the listing filters on.
#[tokio::test]
async fn extract_point_reads_resolved_circle_and_skips_reference() -> TestResult {
    let store = MemoryFactStore::new();
    let placed = commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![construction_at(0, 40.5, -73.5)?])?,
    )
    .await?;
    let placed_id = placed
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing placed")?
        .id;
    let symbolic = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            10,
            vec![construction_location_in(0, "Springfield")?],
        )?,
    )
    .await?;
    let symbolic_id = symbolic
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing symbolic")?
        .id;

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;

    let (class, projected) =
        project_entity::<MemoryFactStore, _, _>(&mut view, placed_id, member_lineage)
            .await?
            .ok_or("placed entity should project")?;
    let entity = crate::typed::Entity::parse(&projected, &class);
    assert_eq!(
        listing::extract_point(&entity),
        Some(GeoPoint::new(40.5, -73.5)?),
        "a resolved circle yields its center"
    );

    let (class, projected) =
        project_entity::<MemoryFactStore, _, _>(&mut view, symbolic_id, member_lineage)
            .await?
            .ok_or("symbolic entity should project")?;
    let entity = crate::typed::Entity::parse(&projected, &class);
    assert_eq!(
        listing::extract_point(&entity),
        None,
        "a symbolic reference has no resolvable point"
    );
    Ok(())
}

/// A located entity surfaces at its point; a `Reference`-only entity, with no
/// resolvable circle, never enters the viewport.
#[tokio::test]
async fn summaries_in_viewport_surfaces_located_excludes_reference() -> TestResult {
    let store = MemoryFactStore::new();
    let inside = (40.5, -73.5);
    let a = commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![construction_at(0, inside.0, inside.1)?])?,
    )
    .await?;
    let a_id = a.entities.get(&EntityIdx(0)).ok_or("missing a")?.id;
    let b = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            10,
            vec![construction_location_in(0, "Springfield")?],
        )?,
    )
    .await?;
    let b_id = b.entities.get(&EntityIdx(0)).ok_or("missing b")?.id;

    let viewport = sample_viewport()?;
    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let page =
        listing::summaries_in_viewport::<MemoryFactStore, _>(&mut view, &viewport, None, PAGE_100)
            .await
            .map_err(|e| format!("{e:?}"))?;

    let ids: std::collections::BTreeSet<MemoryEntityId> =
        page.summaries.iter().map(|s| s.id).collect();
    assert!(ids.contains(&a_id), "the located entity surfaces: {ids:?}");
    assert!(
        !ids.contains(&b_id),
        "the Reference-only entity is excluded: {ids:?}"
    );
    let a_summary = page
        .summaries
        .iter()
        .find(|s| s.id == a_id)
        .ok_or("A missing from summaries")?;
    assert_eq!(
        a_summary.point,
        GeoPoint::new(inside.0, inside.1)?,
        "A surfaces at its construction point"
    );
    assert_eq!(page.next, None, "one page holds every in-box entity");
    Ok(())
}

/// An entity built outside the box but moved into it lists at its current
/// (moved-to) marker, not its construction site.
#[tokio::test]
async fn summaries_in_viewport_surfaces_moved_in_entity_at_current_marker() -> TestResult {
    let store = MemoryFactStore::new();
    let outside = (10.0, 10.0);
    let inside = (40.5, -73.5);
    let moved = commit_result(
        &store,
        local_bundle(
            1,
            1,
            0,
            0,
            vec![
                construction_at(0, outside.0, outside.1)?,
                has_event_fact(0, 0, moved_kind())?,
                moved_to(0, inside.0, inside.1)?,
            ],
        )?,
    )
    .await?;
    let moved_id = moved.entities.get(&EntityIdx(0)).ok_or("missing moved")?.id;

    let viewport = sample_viewport()?;
    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let page =
        listing::summaries_in_viewport::<MemoryFactStore, _>(&mut view, &viewport, None, PAGE_100)
            .await
            .map_err(|e| format!("{e:?}"))?;

    let summary = page
        .summaries
        .iter()
        .find(|s| s.id == moved_id)
        .ok_or("the moved-in entity must surface")?;
    assert_eq!(
        summary.point,
        GeoPoint::new(inside.0, inside.1)?,
        "the moved-in entity lists at its current marker, not its construction site"
    );
    Ok(())
}

/// An entity built inside the box but moved out of it drops from the listing:
/// the spatial walk still surfaces it (its construction sits in-box), but its
/// current marker is now outside, so a listed pin would fall out of view. The
/// mirror of the built-outside/moved-in case.
#[tokio::test]
async fn summaries_in_viewport_excludes_built_in_moved_out_entity() -> TestResult {
    let store = MemoryFactStore::new();
    let inside = (40.5, -73.5);
    let outside = (10.0, 10.0);
    let moved = commit_result(
        &store,
        local_bundle(
            1,
            1,
            0,
            0,
            vec![
                construction_at(0, inside.0, inside.1)?,
                has_event_fact(0, 0, moved_kind())?,
                moved_to(0, outside.0, outside.1)?,
            ],
        )?,
    )
    .await?;
    let moved_id = moved.entities.get(&EntityIdx(0)).ok_or("missing moved")?.id;

    let viewport = sample_viewport()?;
    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let page =
        listing::summaries_in_viewport::<MemoryFactStore, _>(&mut view, &viewport, None, PAGE_100)
            .await
            .map_err(|e| format!("{e:?}"))?;

    assert!(
        page.summaries.iter().all(|s| s.id != moved_id),
        "an entity moved out of the box is not listed — its current pin sits \
         outside the viewport; got {:?}",
        page.summaries
    );
    Ok(())
}

/// With `limit` below the in-box count, the first page carries a resume cursor;
/// replaying it yields the rest, each entity exactly once with no overlap.
#[tokio::test]
async fn summaries_in_viewport_paginates_each_entity_once() -> TestResult {
    let store = MemoryFactStore::new();
    let points = [(40.2, -73.8), (40.5, -73.5), (40.8, -73.2)];
    for (i, (lat, lon)) in points.into_iter().enumerate() {
        commit_result(
            &store,
            local_bundle(
                1,
                0,
                0,
                (i as i64) * 10,
                vec![construction_at(0, lat, lon)?],
            )?,
        )
        .await?;
    }

    let viewport = sample_viewport()?;
    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let limit = std::num::NonZeroUsize::new(2).ok_or("nonzero limit")?;

    let page1 =
        listing::summaries_in_viewport::<MemoryFactStore, _>(&mut view, &viewport, None, limit)
            .await
            .map_err(|e| format!("{e:?}"))?;
    let cursor = page1
        .next
        .clone()
        .ok_or("limit below the count, so the first page carries a cursor")?;
    let page2 = listing::summaries_in_viewport::<MemoryFactStore, _>(
        &mut view,
        &viewport,
        Some(cursor),
        limit,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;

    let mut all: Vec<MemoryEntityId> = page1.summaries.iter().map(|s| s.id).collect();
    all.extend(page2.summaries.iter().map(|s| s.id));
    let unique: std::collections::BTreeSet<MemoryEntityId> = all.iter().copied().collect();
    assert_eq!(
        all.len(),
        unique.len(),
        "no entity appears on both pages: {all:?}"
    );
    assert_eq!(
        unique.len(),
        3,
        "the two pages together cover every in-box entity: {all:?}"
    );
    Ok(())
}

/// A view pinned before a later commit never sees it: the walk reads the exact
/// snapshot the view was opened on, so a write that lands after is invisible.
#[tokio::test]
async fn summaries_in_viewport_pins_snapshot() -> TestResult {
    let store = MemoryFactStore::new();
    commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![construction_at(0, 40.3, -73.7)?])?,
    )
    .await?;
    let snapshot = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;
    let mut view = store
        .no_later_than(snapshot)
        .await
        .map_err(|e| format!("{e:?}"))?;

    // A second in-box entity lands after the snapshot; the pinned view can't see it.
    commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![construction_at(0, 40.6, -73.4)?])?,
    )
    .await?;

    let viewport = sample_viewport()?;
    let page =
        listing::summaries_in_viewport::<MemoryFactStore, _>(&mut view, &viewport, None, PAGE_100)
            .await
            .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        page.summaries.len(),
        1,
        "the fact committed after the snapshot is invisible"
    );
    Ok(())
}
