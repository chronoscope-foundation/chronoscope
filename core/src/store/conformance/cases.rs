//! The generic conformance cases. Each is an `async fn` over `S: FactStore`
//! taking a fresh empty store;
//! [`fact_store_conformance!`](crate::fact_store_conformance) stamps one
//! `#[tokio::test]` per case in the instantiating crate.
//!
//! Ids are captured from each submit's resolutions and threaded through the
//! later assertions; the walk-order checks lean only on the `Ord` the id
//! scheme carries. Pagination cursors stay opaque — a case resumes on a
//! cursor and asserts where the walk lands, never the cursor's shape.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::date::{DatePrecision, UncertainDate};
use crate::geo::{
    GeoPoint, Meters, QuadLevel, TileId, Viewport, mercator_x_to_lon, mercator_y_to_lat, quadkey,
    split_level, viewport_tiles,
};
use crate::grammar::assertions::{FactualAssertion, JudgmentAssertion};
use crate::grammar::attribute;
use crate::grammar::citations::{ExternalSource, JudgmentSource, Justification};
use crate::grammar::identity;
use crate::grammar::ids::{CommitId, FactId, UserId};
use crate::grammar::text::Text;
use crate::location::{Location, LocationReference, UnresolvedLocation};
use crate::store::schema::{
    CELL_DEPTH, CLUSTER_TILE_CAP, CLUSTER_TILE_N, CellKind, ClassRow, ClusterCell, EntityStream,
    ImageStream, RankKey,
};
use crate::store::{
    EntityIdOf, EntityView, EventView, FactPlacement, FactStore, FactView, FactWrite, ImageIdOf,
    ImageView, SubmitCommitError, SubmitCommitInput,
};
use crate::submit::{
    Commit as SubmitBundle, CommitAuthor, DateRole, Decl, EntityIdx, EventIdx, FactLookup,
    ImageIdx, ResolutionOrigin, StoredCommit, StoredFact, SubjectKind, SubmitError, SubmitResult,
    commit_facts,
};

use super::fixtures::{
    PAGE_100, captured_date_fact, captured_location_at, commit_err, commit_name, commit_ok,
    commit_result, commit_retract, construction_at, construction_circle_at,
    construction_location_fact, construction_started_fact, construction_started_in,
    construction_with_location, damaged_kind, depiction_fact, designated_kind, disjunctive_date,
    drain_entity_classes, drain_entity_depictions, drain_image_classes, event_damage_cause_fact,
    event_description_fact, event_durational_date_fact, event_move_method_fact,
    event_moved_to_location_fact, event_point_date_fact, external_judgment_citation,
    external_reference_fact, fixed_time, gap_from_event_fact, has_event_fact,
    image_observation_citation, image_source_fact, local_bundle, map_medium_fact,
    medium_picture_fact, moved_kind, moved_to, n_circle_location, name_fact, name_window_fact,
    observation_external_published, observation_feature_fact, retract_commit_fact, retract_fact,
    same_entity_fact, same_event_fact, sample_citation, sample_viewport, started_with_date,
    subimage_fact, submit_batch, supersede_fact, user_author, year_date,
};
use super::{RefusalKinds, TestError, TestResult, UnmintedIds};

// --- roundtrip & resolution ---

/// A small commit round-trips: the stored fact carries the resolved entity id
/// and the submitted payload.
pub async fn roundtrip_small_commit_through_fact_lookup<S: FactStore>(store: S) -> TestResult {
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "Pantheon")?, construction_started_fact(0)?]
            .into_iter()
            .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(result.fact_ids.len(), 2);
    assert!(!result.previously_committed);

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let resolved_entity = result
        .entities
        .get(&EntityIdx(0))
        .ok_or("entity resolution missing")?
        .id
        .clone();

    let first_id = *result.fact_ids.first().ok_or("no fact ids")?;
    let lookup = view.fact(first_id).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Active(stored) = lookup else {
        return Err(format!("expected Active, got {lookup:?}").into());
    };
    let StoredFact::Factual(stored_factual) = stored.as_ref() else {
        return Err("expected factual stored fact".into());
    };
    let FactualAssertion::Attribute {
        fact:
            attribute::Fact::Name {
                entity,
                name,
                name_type,
                ..
            },
    } = &stored_factual.assertion
    else {
        return Err("expected Name attribute".into());
    };
    assert_eq!(entity, &resolved_entity);
    assert_eq!(name.as_str(), "Pantheon");
    assert_eq!(*name_type, attribute::NameType::Common);

    Ok(())
}

/// A `Decl::Existing` resolves to exactly the supplied id, with
/// `DeclaredExisting` provenance.
pub async fn existing_decl_passes_through_to_supplied_id<S: FactStore>(store: S) -> TestResult {
    let first: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "first")?].into_iter().collect(),
    };
    let first_result = commit_facts(&store, first)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let minted_entity = first_result
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing")?
        .id
        .clone();

    let second: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![Decl::Existing {
            id: minted_entity.clone(),
        }],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "second")?].into_iter().collect(),
    };
    let second_result = commit_facts(&store, second)
        .await
        .map_err(|e| format!("{e:?}"))?;

    let resolution = second_result.entities.get(&EntityIdx(0)).ok_or("missing")?;
    assert_eq!(resolution.id, minted_entity);
    assert_eq!(resolution.origin, ResolutionOrigin::DeclaredExisting);

    Ok(())
}

/// Three `Decl::Local`s mint three distinct ids, each with `NewlyMinted`
/// provenance.
pub async fn local_decls_mint_distinct_newly_minted_ids<S: FactStore>(store: S) -> TestResult {
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local, Decl::Local, Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "a")?, name_fact(1, "b")?, name_fact(2, "c")?]
            .into_iter()
            .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(result.entities.len(), 3);

    let mut seen_ids = HashSet::new();
    for idx in 0..3 {
        let resolution = result.entities.get(&EntityIdx(idx)).ok_or("missing")?;
        assert_eq!(resolution.origin, ResolutionOrigin::NewlyMinted);
        assert!(
            seen_ids.insert(resolution.id.clone()),
            "minted ids must be distinct; got duplicate {:?}",
            resolution.id
        );
    }

    Ok(())
}

// --- matcher anchor precedence ---

/// Name twins carrying distinct external references stay distinct: a decl
/// with any reference draws its candidates from reference walks alone, so a
/// declared-but-unknown reference mints fresh instead of merging with the
/// name twin. No identity is asserted, so there is no companion commit and
/// the two entities read as distinct classes.
pub async fn name_twin_with_distinct_references_mints_fresh<S: FactStore>(store: S) -> TestResult {
    let first = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            0,
            vec![name_fact(0, "Pantheon")?, external_reference_fact(0, 1)?],
        )?,
    )
    .await?;
    let existing = first
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing decl")?
        .id
        .clone();

    let second = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            10,
            vec![name_fact(0, "Pantheon")?, external_reference_fact(0, 2)?],
        )?,
    )
    .await?;
    let resolution = second.entities.get(&EntityIdx(0)).ok_or("missing decl")?;
    assert_eq!(
        resolution.origin,
        ResolutionOrigin::NewlyMinted,
        "a distinct reference is positive evidence of a new subject"
    );
    assert_eq!(
        second.companion_commit_id, None,
        "no match, so no companion commit"
    );

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let class = view
        .entity_class(&resolution.id)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert!(
        !class.members.contains(&existing),
        "the name twins must read as distinct classes; got {:?}",
        class.members
    );
    Ok(())
}

/// A decl carrying no external references still matches by name: the fresh
/// mint classes with the existing same-named entity through the companion
/// commit's identity judgment.
pub async fn name_match_without_references_joins_existing_class<S: FactStore>(
    store: S,
) -> TestResult {
    let first = commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![name_fact(0, "Pantheon")?])?,
    )
    .await?;
    let existing = first
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing decl")?
        .id
        .clone();

    let second = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![name_fact(0, "Pantheon")?])?,
    )
    .await?;
    let resolution = second.entities.get(&EntityIdx(0)).ok_or("missing decl")?;
    assert_eq!(
        resolution.origin,
        ResolutionOrigin::MatchedExisting {
            matched: existing.clone()
        }
    );
    assert!(
        second.companion_commit_id.is_some(),
        "a match records its identity judgment in a companion commit"
    );

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let class = view
        .entity_class(&resolution.id)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert!(
        class.members.contains(&existing),
        "the fresh mint must class with the matched entity; got {:?}",
        class.members
    );
    Ok(())
}

/// The fact id of an active `SameEntity` judgment linking exactly `x` and
/// `y`, found through the backlink walk on `x`.
async fn same_entity_fact_between<S: FactStore, V: EntityView<S>>(
    view: &mut V,
    x: &EntityIdOf<S>,
    y: &EntityIdOf<S>,
) -> Result<Option<FactId>, super::TestError> {
    let page = view
        .all_facts_about_entity(x, None, PAGE_100)
        .await
        .map_err(|e| format!("{e:?}"))?;
    for item in page.items {
        if let StoredFact::Judgment(judgment) = &item.fact
            && let JudgmentAssertion::Identity {
                fact: identity::Fact::SameEntity { pair },
            } = &judgment.assertion
        {
            let linked: BTreeSet<&EntityIdOf<S>> = [pair.a(), pair.b()].into_iter().collect();
            let want: BTreeSet<&EntityIdOf<S>> = [x, y].into_iter().collect();
            if linked == want {
                return Ok(Some(item.fact_id));
            }
        }
    }
    Ok(None)
}

/// The image analogue of [`same_entity_fact_between`].
async fn same_artifact_fact_between<S: FactStore, V: ImageView<S>>(
    view: &mut V,
    x: &ImageIdOf<S>,
    y: &ImageIdOf<S>,
) -> Result<Option<FactId>, super::TestError> {
    let page = view
        .all_facts_about_image(x, None, PAGE_100)
        .await
        .map_err(|e| format!("{e:?}"))?;
    for item in page.items {
        if let StoredFact::Judgment(judgment) = &item.fact
            && let JudgmentAssertion::Identity {
                fact: identity::Fact::SameArtifact { pair },
            } = &judgment.assertion
        {
            let linked: BTreeSet<&ImageIdOf<S>> = [pair.a(), pair.b()].into_iter().collect();
            let want: BTreeSet<&ImageIdOf<S>> = [x, y].into_iter().collect();
            if linked == want {
                return Ok(Some(item.fact_id));
            }
        }
    }
    Ok(None)
}

/// A later commit's `Local` decl carrying the same external reference
/// resolves `MatchedExisting`: the matcher reaches the first commit's entity
/// through the reference walk, records a `SameEntity` judgment in a
/// machine-authored companion commit, and both ids read as one class.
pub async fn external_reference_match_joins_existing_entity_class<S: FactStore>(
    store: S,
) -> TestResult {
    let first = commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![external_reference_fact(0, 42)?])?,
    )
    .await?;
    let existing = first
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing decl")?
        .id
        .clone();

    let second = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![external_reference_fact(0, 42)?])?,
    )
    .await?;
    let resolution = second.entities.get(&EntityIdx(0)).ok_or("missing decl")?;
    assert_eq!(
        resolution.origin,
        ResolutionOrigin::MatchedExisting {
            matched: existing.clone()
        }
    );
    let fresh = resolution.id.clone();
    assert!(
        second.companion_commit_id.is_some(),
        "a match records its identity judgment in a companion commit"
    );

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let edge = same_entity_fact_between::<S, _>(&mut view, &fresh, &existing).await?;
    assert!(
        edge.is_some(),
        "the companion's SameEntity judgment must link the fresh mint to the match"
    );
    let class = view
        .entity_class(&fresh)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert!(
        class.members.contains(&existing) && class.members.contains(&fresh),
        "the fresh mint must class with the matched entity; got {:?}",
        class.members
    );
    Ok(())
}

/// The image analogue of
/// [`external_reference_match_joins_existing_entity_class`]: a later
/// commit's `Local` image decl carrying the same source URL resolves
/// `MatchedExisting`, records a `SameArtifact` judgment in the companion
/// commit, and both ids read as one class.
pub async fn source_url_match_joins_existing_image_class<S: FactStore>(store: S) -> TestResult {
    let url = "https://example.com/artifact-source.jpg";
    let first = commit_result(
        &store,
        local_bundle(0, 0, 1, 0, vec![image_source_fact(0, url)?])?,
    )
    .await?;
    let existing = first
        .images
        .get(&ImageIdx(0))
        .ok_or("missing decl")?
        .id
        .clone();

    let second = commit_result(
        &store,
        local_bundle(0, 0, 1, 10, vec![image_source_fact(0, url)?])?,
    )
    .await?;
    let resolution = second.images.get(&ImageIdx(0)).ok_or("missing decl")?;
    assert_eq!(
        resolution.origin,
        ResolutionOrigin::MatchedExisting {
            matched: existing.clone()
        }
    );
    let fresh = resolution.id.clone();
    assert!(
        second.companion_commit_id.is_some(),
        "a match records its identity judgment in a companion commit"
    );

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let edge = same_artifact_fact_between::<S, _>(&mut view, &fresh, &existing).await?;
    assert!(
        edge.is_some(),
        "the companion's SameArtifact judgment must link the fresh mint to the match"
    );
    let class = view
        .image_class(&fresh)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert!(
        class.members.contains(&existing) && class.members.contains(&fresh),
        "the fresh mint must class with the matched image; got {:?}",
        class.members
    );
    Ok(())
}

/// Retracting the matcher's `SameEntity` judgment splits the merged classes
/// again — while a view pinned before the retraction still resolves the
/// merged representative, because representative history is snapshot-scoped
/// rather than destructively rewritten.
pub async fn retracted_identity_edge_splits_classes_and_earlier_snapshots_stay_merged<
    S: FactStore,
>(
    store: S,
) -> TestResult {
    let first = commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![external_reference_fact(0, 7)?])?,
    )
    .await?;
    let a = first
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing decl")?
        .id
        .clone();
    let second = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![external_reference_fact(0, 7)?])?,
    )
    .await?;
    let b = second
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing decl")?
        .id
        .clone();

    let merged_at = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;
    let edge_fid = {
        let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
        same_entity_fact_between::<S, _>(&mut view, &b, &a)
            .await?
            .ok_or("expected the companion's SameEntity judgment")?
    };

    commit_retract(&store, edge_fid, 20).await?;

    // The classes stand apart again, each id its own representative.
    let mut now_view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let rep_a = now_view
        .entity_representative(&a)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let rep_b = now_view
        .entity_representative(&b)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(rep_a, a, "a returns to its own representative");
    assert_eq!(rep_b, b, "b returns to its own representative");
    let class_b = now_view
        .entity_class(&b)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert!(
        !class_b.members.contains(&a),
        "the classes must split; got {:?}",
        class_b.members
    );

    // A view pinned before the retraction still sees the merge.
    let mut merged_view = store
        .no_later_than(merged_at)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let then_a = merged_view
        .entity_representative(&a)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let then_b = merged_view
        .entity_representative(&b)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        then_a, then_b,
        "the pre-retraction snapshot still resolves the merged representative"
    );
    let merged = merged_view
        .entity_class(&a)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert!(
        merged.members.contains(&a) && merged.members.contains(&b),
        "the pre-retraction snapshot still reads one class; got {:?}",
        merged.members
    );
    assert_eq!(merged.representative, then_a);
    Ok(())
}

/// A split pair can merge again: a `SameEntity` edge merges two entities,
/// its retraction splits them, and a later commit re-asserts the same pair.
/// The re-merge submits cleanly, reads as one class at now, and a snapshot
/// between the retraction and the re-merge still sees the split.
pub async fn re_merging_a_split_pair_submits_cleanly_and_restores_the_class<S: FactStore>(
    store: S,
) -> TestResult {
    let first = commit_result(
        &store,
        local_bundle(2, 0, 0, 0, vec![same_entity_fact(0, 1)?])?,
    )
    .await?;
    let a = first
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing decl 0")?
        .id
        .clone();
    let b = first
        .entities
        .get(&EntityIdx(1))
        .ok_or("missing decl 1")?
        .id
        .clone();
    let edge_fid = *first.fact_ids.first().ok_or("missing the identity fact")?;

    commit_retract(&store, edge_fid, 10).await?;
    let split_at = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;

    let re_merge: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(20),
        entities: vec![
            Decl::Existing { id: a.clone() },
            Decl::Existing { id: b.clone() },
        ],
        events: Vec::new(),
        images: Vec::new(),
        facts: [same_entity_fact(0, 1)?].into_iter().collect(),
    };
    commit_result(&store, re_merge).await?;

    // Now: one class again, one representative for both members.
    let mut now_view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let class = now_view
        .entity_class(&a)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert!(
        class.members.contains(&a) && class.members.contains(&b),
        "the re-merge must read as one class; got {:?}",
        class.members
    );
    let rep_a = now_view
        .entity_representative(&a)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let rep_b = now_view
        .entity_representative(&b)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(rep_a, rep_b, "both members resolve one representative");

    // Between the retraction and the re-merge: still split.
    let mut split_view = store
        .no_later_than(split_at)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        split_view
            .entity_representative(&b)
            .await
            .map_err(|e| format!("{e:?}"))?,
        b,
        "the between-snapshot still sees b as its own representative"
    );
    let split_class = split_view
        .entity_class(&a)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert!(
        !split_class.members.contains(&b),
        "the between-snapshot still reads split classes; got {:?}",
        split_class.members
    );
    Ok(())
}

// --- walk conformance ---

/// A commit's entity-touching facts walk as rows under a single representative
/// whose fact ids are exactly the submitted set — the class-walk conformance
/// every backend owes.
pub async fn walk_entity_classes_group_submitted_facts_into_one_class<S: FactStore>(
    store: S,
) -> TestResult {
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "Pantheon")?, construction_started_fact(0)?]
            .into_iter()
            .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let submitted: BTreeSet<FactId> = result.fact_ids.iter().copied().collect();
    assert_eq!(submitted.len(), 2, "expected two submitted facts");

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let rows: Vec<ClassRow<EntityIdOf<S>>> =
        drain_entity_classes::<S, _>(&mut view, &EntityStream::All, PAGE_100)
            .await
            .map_err(|e| format!("{e:?}"))?;

    let reps: BTreeSet<EntityIdOf<S>> = rows.iter().map(|r| r.representative.clone()).collect();
    assert_eq!(
        reps.len(),
        1,
        "one entity yields one representative; got {rows:?}"
    );
    let walked: BTreeSet<FactId> = rows.iter().map(|r| r.fact_id).collect();
    assert_eq!(
        walked, submitted,
        "the walk's fact ids are exactly the submitted entity-touching facts"
    );
    Ok(())
}

/// At a one-row page limit two entities' rows still page correctly: the walk
/// orders rows by `(representative, fact_id)`, so each entity's rows form one
/// contiguous run across page boundaries, carrying exactly its submitted facts.
pub async fn class_walk_pages_distinct_entities_as_contiguous_runs<S: FactStore>(
    store: S,
) -> TestResult {
    let first = commit_facts(
        &store,
        local_bundle(
            1,
            0,
            0,
            0,
            vec![name_fact(0, "Alpha")?, construction_started_in(0, 1700)?],
        )?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let second = commit_facts(
        &store,
        local_bundle(
            1,
            0,
            0,
            10,
            vec![name_fact(0, "Beta")?, construction_started_in(0, 1800)?],
        )?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let alpha: BTreeSet<FactId> = first.fact_ids.iter().copied().collect();
    let beta: BTreeSet<FactId> = second.fact_ids.iter().copied().collect();

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    // One row per page forces each class to span page boundaries.
    let one = std::num::NonZeroUsize::MIN;
    let rows: Vec<ClassRow<EntityIdOf<S>>> =
        drain_entity_classes::<S, _>(&mut view, &EntityStream::All, one)
            .await
            .map_err(|e| format!("{e:?}"))?;

    // Compress rows to their representative runs; a contiguous walk visits each
    // representative in exactly one run.
    let mut runs: Vec<EntityIdOf<S>> = Vec::new();
    for row in &rows {
        if runs.last() != Some(&row.representative) {
            runs.push(row.representative.clone());
        }
    }
    let distinct: BTreeSet<EntityIdOf<S>> = runs.iter().cloned().collect();
    assert_eq!(
        runs.len(),
        distinct.len(),
        "each entity's rows form one run; a representative recurs across runs in {rows:?}"
    );

    // Each representative's run carries exactly its submitted facts.
    let mut by_rep: BTreeMap<EntityIdOf<S>, BTreeSet<FactId>> = BTreeMap::new();
    for row in &rows {
        by_rep
            .entry(row.representative.clone())
            .or_default()
            .insert(row.fact_id);
    }
    let got: BTreeSet<BTreeSet<FactId>> = by_rep.into_values().collect();
    let want: BTreeSet<BTreeSet<FactId>> = [alpha, beta].into_iter().collect();
    assert_eq!(
        got, want,
        "each entity's rows carry exactly its submitted facts; got {rows:?}"
    );
    Ok(())
}

/// Every fact mentioning an entity must appear in
/// `all_facts_about_entity(entity, ...)`. Commits two entity-touching facts
/// on one entity and asserts both come back.
pub async fn all_facts_about_entity_returns_facts_mentioning_it<S: FactStore>(
    store: S,
) -> TestResult {
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "Pantheon")?, construction_started_fact(0)?]
            .into_iter()
            .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let submitted: BTreeSet<FactId> = result.fact_ids.iter().copied().collect();
    assert_eq!(submitted.len(), 2, "expected two submitted facts");
    let entity = result
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing entity")?
        .id
        .clone();

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let page = view
        .all_facts_about_entity(&entity, None, PAGE_100)
        .await
        .map_err(|e| format!("{e:?}"))?;

    let returned: BTreeSet<FactId> = page.items.iter().map(|item| item.fact_id).collect();
    assert!(
        submitted.is_subset(&returned),
        "all_facts_about_entity must return every fact mentioning the entity; \
         submitted={submitted:?}, returned={returned:?}"
    );
    Ok(())
}

/// Every fact mentioning an event must appear in
/// `all_facts_about_event(event, ...)`.
pub async fn all_facts_about_event_returns_facts_mentioning_it<S: FactStore>(
    store: S,
) -> TestResult {
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: vec![Decl::Local],
        images: Vec::new(),
        facts: [
            has_event_fact(0, 0, designated_kind())?,
            event_point_date_fact(0)?,
            event_description_fact(0)?,
        ]
        .into_iter()
        .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let submitted: BTreeSet<FactId> = result.fact_ids.iter().copied().collect();
    assert_eq!(submitted.len(), 3, "expected three submitted facts");
    let event = result
        .events
        .get(&EventIdx(0))
        .ok_or("missing event")?
        .id
        .clone();

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let page = view
        .all_facts_about_event(&event, None, PAGE_100)
        .await
        .map_err(|e| format!("{e:?}"))?;

    let returned: BTreeSet<FactId> = page.items.iter().map(|item| item.fact_id).collect();
    assert!(
        submitted.is_subset(&returned),
        "all_facts_about_event must return every fact mentioning the event; \
         submitted={submitted:?}, returned={returned:?}"
    );
    Ok(())
}

/// Image analogue of
/// [`walk_entity_classes_group_submitted_facts_into_one_class`].
pub async fn walk_image_classes_group_submitted_facts_into_one_class<S: FactStore>(
    store: S,
) -> TestResult {
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Local],
        facts: [medium_picture_fact(0)?, captured_date_fact(0)?]
            .into_iter()
            .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let submitted: BTreeSet<FactId> = result.fact_ids.iter().copied().collect();
    assert_eq!(submitted.len(), 2, "expected two submitted facts");

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let rows: Vec<ClassRow<ImageIdOf<S>>> =
        drain_image_classes::<S, _>(&mut view, &ImageStream::All, PAGE_100)
            .await
            .map_err(|e| format!("{e:?}"))?;

    let reps: BTreeSet<ImageIdOf<S>> = rows.iter().map(|r| r.representative.clone()).collect();
    assert_eq!(
        reps.len(),
        1,
        "one image yields one representative; got {rows:?}"
    );
    let walked: BTreeSet<FactId> = rows.iter().map(|r| r.fact_id).collect();
    assert_eq!(
        walked, submitted,
        "the walk's fact ids are exactly the submitted image-touching facts"
    );
    Ok(())
}

/// The images depicting an entity page through `walk_entity_depictions` as raw
/// depiction facts under their image rep: exactly the depicted images surface,
/// each carrying its own depiction fact, and an image depicted only by a
/// different entity stays out.
pub async fn walk_entity_depictions_pages_the_images_depicting_an_entity<S: FactStore>(
    store: S,
) -> TestResult {
    // Entity 0 depicts images 0 and 1. Entity 1 depicts image 2 — a real,
    // depicted image, just not by our entity, so surfacing it would mean the
    // walk ignored entity membership.
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local, Decl::Local],
        events: Vec::new(),
        images: vec![Decl::Local, Decl::Local, Decl::Local],
        facts: [
            depiction_fact(0, 0)?,
            depiction_fact(0, 1)?,
            depiction_fact(1, 2)?,
        ]
        .into_iter()
        .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let entity0 = result
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing entity 0")?
        .id
        .clone();
    let image0 = result
        .images
        .get(&ImageIdx(0))
        .ok_or("missing image 0")?
        .id
        .clone();
    let image1 = result
        .images
        .get(&ImageIdx(1))
        .ok_or("missing image 1")?
        .id
        .clone();
    let image2 = result
        .images
        .get(&ImageIdx(2))
        .ok_or("missing image 2")?
        .id
        .clone();

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let rows = drain_entity_depictions::<S, _>(&mut view, &entity0, PAGE_100)
        .await
        .map_err(|e| format!("{e:?}"))?;

    let reps: BTreeSet<ImageIdOf<S>> = rows.iter().map(|r| r.representative.clone()).collect();
    assert_eq!(
        reps,
        [image0, image1].into_iter().collect::<BTreeSet<_>>(),
        "exactly the two images entity 0 depicts must surface; got {rows:?}"
    );
    assert!(
        !reps.contains(&image2),
        "an image depicted only by another entity must be excluded; got {rows:?}"
    );

    // Each row keeps its own depiction fact — readable as a Depiction naming
    // entity 0 and the row's own image rep.
    for row in &rows {
        let StoredFact::Judgment(judgment) = &row.fact else {
            return Err(format!(
                "depiction row must carry a judgment fact; got {:?}",
                row.fact
            )
            .into());
        };
        let JudgmentAssertion::Depiction { fact } = &judgment.assertion else {
            return Err(format!(
                "depiction row must carry a Depiction; got {:?}",
                judgment.assertion
            )
            .into());
        };
        assert_eq!(
            fact.entity, entity0,
            "the depiction names the queried entity"
        );
        assert_eq!(
            fact.image, row.representative,
            "the depiction's image is the row's representative"
        );
    }
    Ok(())
}

/// Paging the depiction walk one image at a time resumes across the
/// `next_class` cursor. Entity 0 depicts four images; two of them are the same
/// artifact, so they share one image representative carrying both their
/// depiction facts, and images 2 and 3 are singletons — three distinct image
/// reps, one of them multi-fact. At a one-image limit each page covers exactly
/// one rep, the shared rep's page keeps both its facts whole, and every
/// depicted image surfaces once across the walk.
pub async fn walk_entity_depictions_pages_across_the_next_class_cursor<S: FactStore>(
    store: S,
) -> TestResult {
    let same_artifact = identity::Fact::same_artifact(ImageIdx(0), ImageIdx(1))?;
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: vec![Decl::Local, Decl::Local, Decl::Local, Decl::Local],
        facts: [
            crate::submit::SubmitFact::Judgment {
                assertion: JudgmentAssertion::Identity {
                    fact: same_artifact,
                },
                citation: JudgmentSource::PersonalKnowledge {
                    user: UserId::new("alice")?,
                    justification: Justification::new("These two scans are the same artifact.")?,
                },
            },
            depiction_fact(0, 0)?,
            depiction_fact(0, 1)?,
            depiction_fact(0, 2)?,
            depiction_fact(0, 3)?,
        ]
        .into_iter()
        .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let entity0 = result
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing entity 0")?
        .id
        .clone();
    let image0 = result
        .images
        .get(&ImageIdx(0))
        .ok_or("missing image 0")?
        .id
        .clone();
    let image1 = result
        .images
        .get(&ImageIdx(1))
        .ok_or("missing image 1")?
        .id
        .clone();
    let image2 = result
        .images
        .get(&ImageIdx(2))
        .ok_or("missing image 2")?
        .id
        .clone();
    let image3 = result
        .images
        .get(&ImageIdx(3))
        .ok_or("missing image 3")?
        .id
        .clone();

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let rep_pair = view
        .image_representative(&image0)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let rep_pair_via_1 = view
        .image_representative(&image1)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        rep_pair, rep_pair_via_1,
        "the SameArtifact pair collapses to one image representative"
    );
    let rep2 = view
        .image_representative(&image2)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let rep3 = view
        .image_representative(&image3)
        .await
        .map_err(|e| format!("{e:?}"))?;

    // Page one image at a time, threading `next_class` as the next `after`.
    let one = std::num::NonZeroUsize::MIN;
    let mut pages: Vec<Vec<_>> = Vec::new();
    let mut cursor_live: Vec<bool> = Vec::new();
    let mut after = None;
    loop {
        assert!(pages.len() < 8, "the depiction walk must terminate");
        let page = view
            .walk_entity_depictions(&entity0, after, one)
            .await
            .map_err(|e| format!("{e:?}"))?;
        cursor_live.push(page.next_class.is_some());
        pages.push(page.rows);
        match page.next_class {
            Some(next) => after = Some(next),
            None => break,
        }
    }

    // Each page covers exactly one image rep; collect the reps in walk order and
    // read every row's depiction fact to recover its underlying image.
    let mut page_reps: Vec<ImageIdOf<S>> = Vec::new();
    let mut seen_images: Vec<ImageIdOf<S>> = Vec::new();
    let mut pair_page_images: Option<BTreeSet<ImageIdOf<S>>> = None;
    for rows in &pages {
        let reps: BTreeSet<ImageIdOf<S>> = rows.iter().map(|r| r.representative.clone()).collect();
        assert_eq!(
            reps.len(),
            1,
            "each page covers exactly one image rep at a one-image limit; got {rows:?}"
        );
        let rep = reps
            .into_iter()
            .next()
            .ok_or("a page under test has a rep")?;
        let mut page_images: BTreeSet<ImageIdOf<S>> = BTreeSet::new();
        for row in rows {
            let StoredFact::Judgment(judgment) = &row.fact else {
                return Err(format!(
                    "depiction row must carry a judgment fact; got {:?}",
                    row.fact
                )
                .into());
            };
            let JudgmentAssertion::Depiction { fact } = &judgment.assertion else {
                return Err(format!(
                    "depiction row must carry a Depiction; got {:?}",
                    judgment.assertion
                )
                .into());
            };
            assert_eq!(
                fact.entity, entity0,
                "the depiction names the queried entity"
            );
            page_images.insert(fact.image.clone());
            seen_images.push(fact.image.clone());
        }
        if rep == rep_pair {
            pair_page_images = Some(page_images);
        }
        page_reps.push(rep);
    }

    assert_eq!(
        page_reps.len(),
        3,
        "one page per distinct image rep at a one-image limit; got {page_reps:?}"
    );
    let distinct_reps: BTreeSet<ImageIdOf<S>> = page_reps.iter().cloned().collect();
    assert_eq!(
        page_reps.len(),
        distinct_reps.len(),
        "each image rep pages contiguously, never twice; got {page_reps:?}"
    );
    let expected_reps: BTreeSet<ImageIdOf<S>> =
        [rep_pair.clone(), rep2, rep3].into_iter().collect();
    assert_eq!(
        distinct_reps, expected_reps,
        "exactly the three depicted image reps surface"
    );

    // The shared rep's page keeps both its depiction facts, unsplit.
    let pair_images = pair_page_images.ok_or("the same-artifact rep must surface as a page")?;
    assert_eq!(
        pair_images,
        [image0.clone(), image1.clone()]
            .into_iter()
            .collect::<BTreeSet<_>>(),
        "the same-artifact rep's page carries both of its depiction facts"
    );

    // Every depicted image surfaces exactly once across the walk.
    let seen_set: BTreeSet<ImageIdOf<S>> = seen_images.iter().cloned().collect();
    assert_eq!(
        seen_images.len(),
        seen_set.len(),
        "no depiction fact surfaces twice; got {seen_images:?}"
    );
    assert_eq!(
        seen_set,
        [image0, image1, image2, image3]
            .into_iter()
            .collect::<BTreeSet<_>>(),
        "every depicted image surfaces exactly once"
    );

    // `next_class` is live on every page but the last.
    let (last_live, earlier_live) = cursor_live
        .split_last()
        .ok_or("the walk emits at least one page")?;
    assert!(!*last_live, "the final page's next_class is exhausted");
    assert!(
        earlier_live.iter().all(|&live| live),
        "every non-final page carries a live next_class"
    );
    Ok(())
}

/// Every fact mentioning an image must appear in
/// `all_facts_about_image(image, ...)`.
pub async fn all_facts_about_image_returns_facts_mentioning_it<S: FactStore>(
    store: S,
) -> TestResult {
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Local],
        facts: [medium_picture_fact(0)?, captured_date_fact(0)?]
            .into_iter()
            .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let submitted: BTreeSet<FactId> = result.fact_ids.iter().copied().collect();
    assert_eq!(submitted.len(), 2, "expected two submitted facts");
    let image = result
        .images
        .get(&ImageIdx(0))
        .ok_or("missing image")?
        .id
        .clone();

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let page = view
        .all_facts_about_image(&image, None, PAGE_100)
        .await
        .map_err(|e| format!("{e:?}"))?;

    let returned: BTreeSet<FactId> = page.items.iter().map(|item| item.fact_id).collect();
    assert!(
        submitted.is_subset(&returned),
        "all_facts_about_image must return every fact mentioning the image; \
         submitted={submitted:?}, returned={returned:?}"
    );
    Ok(())
}

// --- equivalence-class conformance ---
//
// These exercise the `*_class` / `*_representative` view methods. Each
// commits a `Same*` fact between two freshly-minted ids, so the stored
// fact carries two distinct ids in one class.

/// After a `SameEntity` fact links two ids, `entity_class` of either member
/// must contain both.
pub async fn entity_class_contains_both_same_entity_members<S: FactStore>(store: S) -> TestResult {
    let identity_pair = identity::Fact::same_entity(EntityIdx(0), EntityIdx(1))?;
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local, Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [crate::submit::SubmitFact::Judgment {
            assertion: JudgmentAssertion::Identity {
                fact: identity_pair,
            },
            citation: JudgmentSource::PersonalKnowledge {
                user: UserId::new("alice")?,
                justification: Justification::new("These two refer to the same entity.")?,
            },
        }]
        .into_iter()
        .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let a = result
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing a")?
        .id
        .clone();
    let b = result
        .entities
        .get(&EntityIdx(1))
        .ok_or("missing b")?
        .id
        .clone();
    assert_ne!(a, b, "the two Local decls must mint distinct ids");

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let class = view.entity_class(&a).await.map_err(|e| format!("{e:?}"))?;
    assert!(
        class.members.contains(&a) && class.members.contains(&b),
        "entity_class(a) must contain both equivalence members; got {:?}",
        class.members
    );
    Ok(())
}

/// A `SameEntity`-linked pair must resolve to one representative whichever
/// member is queried, and it must be a class member agreeing with
/// `entity_class(..).representative`.
pub async fn entity_representative_is_canonical_across_same_entity_members<S: FactStore>(
    store: S,
) -> TestResult {
    let identity_pair = identity::Fact::same_entity(EntityIdx(0), EntityIdx(1))?;
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local, Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [crate::submit::SubmitFact::Judgment {
            assertion: JudgmentAssertion::Identity {
                fact: identity_pair,
            },
            citation: JudgmentSource::PersonalKnowledge {
                user: UserId::new("alice")?,
                justification: Justification::new("These two refer to the same entity.")?,
            },
        }]
        .into_iter()
        .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let a = result
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing a")?
        .id
        .clone();
    let b = result
        .entities
        .get(&EntityIdx(1))
        .ok_or("missing b")?
        .id
        .clone();
    assert_ne!(a, b, "the two Local decls must mint distinct ids");

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let rep_a = view
        .entity_representative(&a)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let rep_b = view
        .entity_representative(&b)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        rep_a, rep_b,
        "both members of a SameEntity class must share one representative"
    );
    assert!(
        rep_a == a || rep_a == b,
        "the representative must be a member of the class; got {rep_a:?} for {{{a:?}, {b:?}}}"
    );
    let class = view.entity_class(&a).await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        class.representative, rep_a,
        "entity_class.representative must agree with entity_representative"
    );
    Ok(())
}

/// Event analogue of [`entity_class_contains_both_same_entity_members`].
/// Stamped `#[ignore]`d while submit rejects `SameEvent` and the class read
/// is stubbed; both must lift before this runs green.
pub async fn event_class_contains_both_same_event_members<S: FactStore>(store: S) -> TestResult {
    let identity_pair = identity::Fact::same_event(EventIdx(0), EventIdx(1))?;
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: vec![Decl::Local, Decl::Local],
        images: Vec::new(),
        facts: [crate::submit::SubmitFact::Judgment {
            assertion: JudgmentAssertion::Identity {
                fact: identity_pair,
            },
            citation: JudgmentSource::PersonalKnowledge {
                user: UserId::new("alice")?,
                justification: Justification::new("These two refer to the same event.")?,
            },
        }]
        .into_iter()
        .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let a = result
        .events
        .get(&EventIdx(0))
        .ok_or("missing a")?
        .id
        .clone();
    let b = result
        .events
        .get(&EventIdx(1))
        .ok_or("missing b")?
        .id
        .clone();
    assert_ne!(a, b, "the two Local decls must mint distinct ids");

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let class = view.event_class(&a).await.map_err(|e| format!("{e:?}"))?;
    assert!(
        class.members.contains(&a) && class.members.contains(&b),
        "event_class(a) must contain both equivalence members; got {:?}",
        class.members
    );
    Ok(())
}

/// Event analogue of
/// [`entity_representative_is_canonical_across_same_entity_members`].
/// Stamped `#[ignore]`d while submit rejects `SameEvent` and representative
/// selection is stubbed; both must lift before this runs green.
pub async fn event_representative_is_canonical_across_same_event_members<S: FactStore>(
    store: S,
) -> TestResult {
    let identity_pair = identity::Fact::same_event(EventIdx(0), EventIdx(1))?;
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: vec![Decl::Local, Decl::Local],
        images: Vec::new(),
        facts: [crate::submit::SubmitFact::Judgment {
            assertion: JudgmentAssertion::Identity {
                fact: identity_pair,
            },
            citation: JudgmentSource::PersonalKnowledge {
                user: UserId::new("alice")?,
                justification: Justification::new("These two refer to the same event.")?,
            },
        }]
        .into_iter()
        .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let a = result
        .events
        .get(&EventIdx(0))
        .ok_or("missing a")?
        .id
        .clone();
    let b = result
        .events
        .get(&EventIdx(1))
        .ok_or("missing b")?
        .id
        .clone();
    assert_ne!(a, b, "the two Local decls must mint distinct ids");

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let rep_a = view
        .event_representative(&a)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let rep_b = view
        .event_representative(&b)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        rep_a, rep_b,
        "both members of a SameEvent class must share one representative"
    );
    assert!(
        rep_a == a || rep_a == b,
        "the representative must be a member of the class; got {rep_a:?} for {{{a:?}, {b:?}}}"
    );
    let class = view.event_class(&a).await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        class.representative, rep_a,
        "event_class.representative must agree with event_representative"
    );
    Ok(())
}

/// Image analogue of [`entity_class_contains_both_same_entity_members`].
pub async fn image_class_contains_both_same_artifact_members<S: FactStore>(store: S) -> TestResult {
    let identity_pair = identity::Fact::same_artifact(ImageIdx(0), ImageIdx(1))?;
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Local, Decl::Local],
        facts: [crate::submit::SubmitFact::Judgment {
            assertion: JudgmentAssertion::Identity {
                fact: identity_pair,
            },
            citation: JudgmentSource::PersonalKnowledge {
                user: UserId::new("alice")?,
                justification: Justification::new("These two scans are the same artifact.")?,
            },
        }]
        .into_iter()
        .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let a = result
        .images
        .get(&ImageIdx(0))
        .ok_or("missing a")?
        .id
        .clone();
    let b = result
        .images
        .get(&ImageIdx(1))
        .ok_or("missing b")?
        .id
        .clone();
    assert_ne!(a, b, "the two Local decls must mint distinct ids");

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let class = view.image_class(&a).await.map_err(|e| format!("{e:?}"))?;
    assert!(
        class.members.contains(&a) && class.members.contains(&b),
        "image_class(a) must contain both equivalence members; got {:?}",
        class.members
    );
    Ok(())
}

/// Image analogue of
/// [`entity_representative_is_canonical_across_same_entity_members`].
pub async fn image_representative_is_canonical_across_same_artifact_members<S: FactStore>(
    store: S,
) -> TestResult {
    let identity_pair = identity::Fact::same_artifact(ImageIdx(0), ImageIdx(1))?;
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Local, Decl::Local],
        facts: [crate::submit::SubmitFact::Judgment {
            assertion: JudgmentAssertion::Identity {
                fact: identity_pair,
            },
            citation: JudgmentSource::PersonalKnowledge {
                user: UserId::new("alice")?,
                justification: Justification::new("These two scans are the same artifact.")?,
            },
        }]
        .into_iter()
        .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let a = result
        .images
        .get(&ImageIdx(0))
        .ok_or("missing a")?
        .id
        .clone();
    let b = result
        .images
        .get(&ImageIdx(1))
        .ok_or("missing b")?
        .id
        .clone();
    assert_ne!(a, b, "the two Local decls must mint distinct ids");

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let rep_a = view
        .image_representative(&a)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let rep_b = view
        .image_representative(&b)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        rep_a, rep_b,
        "both members of a SameArtifact class must share one representative"
    );
    assert!(
        rep_a == a || rep_a == b,
        "the representative must be a member of the class; got {rep_a:?} for {{{a:?}, {b:?}}}"
    );
    let class = view.image_class(&a).await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        class.representative, rep_a,
        "image_class.representative must agree with image_representative"
    );
    Ok(())
}

/// The batch [`ImageView::image_representatives`] returns, for every member,
/// exactly what [`ImageView::image_representative`] returns per id. Seeds a
/// `SameArtifact` pair and a lone image, then cross-checks the map against the
/// per-id resolution — so resolving the whole batch at once and resolving one
/// member at a time (a singleton batch) must agree, and the pair collapses to
/// one rep while the lone image maps to itself.
pub async fn image_representatives_batch_matches_per_id_resolution<S: FactStore>(
    store: S,
) -> TestResult {
    let identity_pair = identity::Fact::same_artifact(ImageIdx(0), ImageIdx(1))?;
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Local, Decl::Local, Decl::Local],
        facts: [
            crate::submit::SubmitFact::Judgment {
                assertion: JudgmentAssertion::Identity {
                    fact: identity_pair,
                },
                citation: JudgmentSource::PersonalKnowledge {
                    user: UserId::new("alice")?,
                    justification: Justification::new("These two scans are the same artifact.")?,
                },
            },
            image_source_fact(2, "https://example.org/lone.jpg")?,
        ]
        .into_iter()
        .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let ids: Vec<ImageIdOf<S>> = [ImageIdx(0), ImageIdx(1), ImageIdx(2)]
        .into_iter()
        .map(|idx| {
            result
                .images
                .get(&idx)
                .map(|resolution| resolution.id.clone())
                .ok_or_else(|| format!("missing image {idx:?}"))
        })
        .collect::<Result<_, _>>()?;

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;

    let mut per_id: HashMap<ImageIdOf<S>, ImageIdOf<S>> = HashMap::new();
    for id in &ids {
        let rep = view
            .image_representative(id)
            .await
            .map_err(|e| format!("{e:?}"))?;
        per_id.insert(id.clone(), rep);
    }

    let batch = view
        .image_representatives(&ids)
        .await
        .map_err(|e| format!("{e:?}"))?;

    assert_eq!(
        batch, per_id,
        "batch representatives must match the per-id resolution for every member"
    );
    assert_eq!(
        batch.get(&ids[0]),
        batch.get(&ids[1]),
        "the SameArtifact pair must collapse to one representative in the batch"
    );
    assert_eq!(
        batch.get(&ids[2]),
        Some(&ids[2]),
        "the lone image with no SameArtifact class must map to itself"
    );
    Ok(())
}

// --- index-reference error paths ---

#[derive(Debug, Clone, Copy)]
enum ExpectedKind {
    Entity,
    Event,
    Image,
}

async fn assert_idx_out_of_range<S: FactStore>(
    store: S,
    decl_count: usize,
    bad_idx: usize,
    kind: ExpectedKind,
) -> TestResult {
    let bundle: SubmitCommitInput<S> = match kind {
        ExpectedKind::Entity => SubmitBundle {
            author: user_author()?,
            recorded_at: fixed_time(),
            entities: (0..decl_count).map(|_| Decl::Local).collect(),
            events: Vec::new(),
            images: Vec::new(),
            facts: [name_fact(bad_idx, "out-of-range")?].into_iter().collect(),
        },
        ExpectedKind::Event => SubmitBundle {
            author: user_author()?,
            recorded_at: fixed_time(),
            entities: Vec::new(),
            events: (0..decl_count).map(|_| Decl::Local).collect(),
            images: Vec::new(),
            facts: [crate::submit::SubmitFact::Factual {
                assertion: FactualAssertion::Event {
                    fact: crate::grammar::event::Fact::PointDate {
                        event: EventIdx(bad_idx),
                        bound: UncertainDate::with_precision(
                            chrono::NaiveDate::from_ymd_opt(1700, 1, 1).ok_or("date")?,
                            DatePrecision::Year,
                        )?,
                    },
                },
                citation: sample_citation()?,
            }]
            .into_iter()
            .collect(),
        },
        ExpectedKind::Image => SubmitBundle {
            author: user_author()?,
            recorded_at: fixed_time(),
            entities: Vec::new(),
            events: Vec::new(),
            images: (0..decl_count).map(|_| Decl::Local).collect(),
            facts: [crate::submit::SubmitFact::Factual {
                assertion: FactualAssertion::Image {
                    fact: crate::grammar::image::Fact::Medium {
                        image: ImageIdx(bad_idx),
                        medium: crate::grammar::image::ImageMedium::Picture,
                    },
                },
                citation: sample_citation()?,
            }]
            .into_iter()
            .collect(),
        },
    };

    let err = match commit_facts(&store, bundle).await {
        Ok(_) => return Err("expected SubmitError".into()),
        Err(e) => e,
    };

    let errs = submit_batch(err)?;
    assert_eq!(
        errs.len().get(),
        1,
        "expected exactly one out-of-range error"
    );
    match (kind, errs.first()) {
        (
            ExpectedKind::Entity,
            SubmitError::EntityIdxOutOfRange {
                idx,
                decl_count: got,
            },
        ) => {
            assert_eq!(*idx, bad_idx);
            assert_eq!(*got, decl_count);
        }
        (
            ExpectedKind::Event,
            SubmitError::EventIdxOutOfRange {
                idx,
                decl_count: got,
            },
        ) => {
            assert_eq!(*idx, bad_idx);
            assert_eq!(*got, decl_count);
        }
        (
            ExpectedKind::Image,
            SubmitError::ImageIdxOutOfRange {
                idx,
                decl_count: got,
            },
        ) => {
            assert_eq!(*idx, bad_idx);
            assert_eq!(*got, decl_count);
        }
        (k, other) => {
            return Err(format!("expected {k:?} out-of-range error, got {other:?}").into());
        }
    }
    Ok(())
}

/// An entity index past the declaration list is rejected as out of range.
pub async fn fact_referencing_out_of_range_entity_idx_returns_error<S: FactStore>(
    store: S,
) -> TestResult {
    assert_idx_out_of_range(store, 3, 5, ExpectedKind::Entity).await
}

/// The event analogue of
/// [`fact_referencing_out_of_range_entity_idx_returns_error`].
pub async fn fact_referencing_out_of_range_event_idx_returns_error<S: FactStore>(
    store: S,
) -> TestResult {
    assert_idx_out_of_range(store, 3, 5, ExpectedKind::Event).await
}

/// The image analogue of
/// [`fact_referencing_out_of_range_entity_idx_returns_error`].
pub async fn fact_referencing_out_of_range_image_idx_returns_error<S: FactStore>(
    store: S,
) -> TestResult {
    assert_idx_out_of_range(store, 3, 5, ExpectedKind::Image).await
}

/// The first invalid index — `idx == decl_count` — is the boundary the
/// range check guards. With three entity decls (valid indices 0..=2), index
/// 3 is rejected with `EntityIdxOutOfRange`. Entity kind alone pins the
/// boundary; the three out-of-range tests above share the dispatch.
pub async fn entity_idx_at_decl_count_is_first_rejected<S: FactStore>(store: S) -> TestResult {
    assert_idx_out_of_range(store, 3, 3, ExpectedKind::Entity).await
}

/// The last valid index — `idx == decl_count - 1` — is accepted. With three
/// entity decls, index 2 resolves; all three decls are referenced so the
/// only thing under test is the upper bound, not `UnusedDeclaration`.
pub async fn entity_idx_at_decl_count_minus_one_is_accepted<S: FactStore>(store: S) -> TestResult {
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local, Decl::Local, Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [
            name_fact(0, "a")?,
            name_fact(1, "b")?,
            name_fact(2, "last-valid")?,
        ]
        .into_iter()
        .collect(),
    };
    commit_ok(&store, bundle).await
}

// --- unused-declaration error path ---

/// A declared entity that no fact references is rejected with
/// `UnusedDeclaration` naming its position. The bundle pairs the unused decl
/// (position 0) with a referenced one (position 1) so the only complaint is
/// the unreferenced decl.
pub async fn unreferenced_entity_decl_rejected_as_unused<S: FactStore>(store: S) -> TestResult {
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        // Decl 0 is never referenced; decl 1 is.
        entities: vec![Decl::Local, Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(1, "referenced")?].into_iter().collect(),
    };
    let errs = commit_err(&store, bundle).await?;
    assert!(
        errs.iter().any(|e| matches!(
            e,
            SubmitError::UnusedDeclaration {
                kind: SubjectKind::Entity,
                position: 0,
            }
        )),
        "expected UnusedDeclaration for entity decl 0, got {errs:?}"
    );
    Ok(())
}

// --- unknown-existing-id error paths ---

/// Phantom-id rejection kinds; the shared helper dispatches its assertion
/// on this tag.
#[derive(Debug, Clone, Copy)]
enum UnknownKind {
    Entity,
    Event,
    Image,
}

/// Submit a bundle with one `Decl::Existing` naming an id the store never
/// mints (the [`UnmintedIds`] fixture) and a fact referencing it. Asserts the
/// matching `UnknownExisting*` variant at decl position 0.
async fn assert_unknown_existing<S: UnmintedIds>(store: S, kind: UnknownKind) -> TestResult {
    let bundle: SubmitCommitInput<S> = match kind {
        UnknownKind::Entity => SubmitBundle {
            author: user_author()?,
            recorded_at: fixed_time(),
            entities: vec![Decl::Existing {
                id: S::unminted_entity(),
            }],
            events: Vec::new(),
            images: Vec::new(),
            facts: [name_fact(0, "phantom-name")?].into_iter().collect(),
        },
        UnknownKind::Event => SubmitBundle {
            author: user_author()?,
            recorded_at: fixed_time(),
            entities: Vec::new(),
            events: vec![Decl::Existing {
                id: S::unminted_event(),
            }],
            images: Vec::new(),
            facts: [crate::submit::SubmitFact::Factual {
                assertion: FactualAssertion::Event {
                    fact: crate::grammar::event::Fact::PointDate {
                        event: EventIdx(0),
                        bound: UncertainDate::with_precision(
                            chrono::NaiveDate::from_ymd_opt(1700, 1, 1).ok_or("date")?,
                            DatePrecision::Year,
                        )?,
                    },
                },
                citation: sample_citation()?,
            }]
            .into_iter()
            .collect(),
        },
        UnknownKind::Image => SubmitBundle {
            author: user_author()?,
            recorded_at: fixed_time(),
            entities: Vec::new(),
            events: Vec::new(),
            images: vec![Decl::Existing {
                id: S::unminted_image(),
            }],
            facts: [crate::submit::SubmitFact::Factual {
                assertion: FactualAssertion::Image {
                    fact: crate::grammar::image::Fact::Medium {
                        image: ImageIdx(0),
                        medium: crate::grammar::image::ImageMedium::Picture,
                    },
                },
                citation: sample_citation()?,
            }]
            .into_iter()
            .collect(),
        },
    };

    let err = match commit_facts(&store, bundle).await {
        Ok(_) => return Err("expected SubmitError".into()),
        Err(e) => e,
    };

    let errs = submit_batch(err)?;
    assert_eq!(
        errs.len().get(),
        1,
        "expected exactly one UnknownExisting* error"
    );
    match (kind, errs.first()) {
        (UnknownKind::Entity, SubmitError::UnknownExistingEntity { decl_position }) => {
            assert_eq!(*decl_position, EntityIdx(0));
        }
        (UnknownKind::Event, SubmitError::UnknownExistingEvent { decl_position }) => {
            assert_eq!(*decl_position, EventIdx(0));
        }
        (UnknownKind::Image, SubmitError::UnknownExistingImage { decl_position }) => {
            assert_eq!(*decl_position, ImageIdx(0));
        }
        (k, other) => {
            return Err(format!("expected {k:?} UnknownExisting* error, got {other:?}").into());
        }
    }
    Ok(())
}

/// A `Decl::Existing` naming an entity id the store never minted is rejected.
pub async fn decl_existing_unknown_entity_id_rejected<S: UnmintedIds>(store: S) -> TestResult {
    assert_unknown_existing(store, UnknownKind::Entity).await
}

/// The event analogue of [`decl_existing_unknown_entity_id_rejected`].
pub async fn decl_existing_unknown_event_id_rejected<S: UnmintedIds>(store: S) -> TestResult {
    assert_unknown_existing(store, UnknownKind::Event).await
}

/// The image analogue of [`decl_existing_unknown_entity_id_rejected`].
pub async fn decl_existing_unknown_image_id_rejected<S: UnmintedIds>(store: S) -> TestResult {
    assert_unknown_existing(store, UnknownKind::Image).await
}

// --- substitution-time rejection ---

/// Two `Decl::Existing(same_id)` slots resolving to one persistent entity
/// id, plus an identity fact over the two indices, are rejected. The shared id
/// trips two independent guards that accumulate together: the substitution-time
/// `IdentityEntitySelfEquivalence` (the identity fact collapsed) and the
/// decl-distinctness `DuplicateEntityDecl`, both carrying the shared typed id.
pub async fn same_entity_resolving_to_one_id_rejected_at_substitution<S: FactStore>(
    store: S,
) -> TestResult {
    let mint: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "real-entity")?].into_iter().collect(),
    };
    let minted = commit_facts(&store, mint)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let entity_id = minted
        .entities
        .get(&EntityIdx(0))
        .ok_or("expected mint")?
        .id
        .clone();

    let identity_pair = identity::Fact::same_entity(EntityIdx(0), EntityIdx(1))?;
    let user = UserId::new("alice")?;
    let justification =
        Justification::new("Two existing decls collapse to the same id after resolution.")?;
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![
            Decl::Existing {
                id: entity_id.clone(),
            },
            Decl::Existing {
                id: entity_id.clone(),
            },
        ],
        events: Vec::new(),
        images: Vec::new(),
        facts: [crate::submit::SubmitFact::Judgment {
            assertion: JudgmentAssertion::Identity {
                fact: identity_pair,
            },
            citation: JudgmentSource::PersonalKnowledge {
                user,
                justification,
            },
        }]
        .into_iter()
        .collect(),
    };

    let err = match commit_facts(&store, bundle).await {
        Ok(_) => return Err("expected IdentityEntitySelfEquivalence rejection".into()),
        Err(e) => e,
    };

    let errs = submit_batch(err)?;
    assert!(
        errs.iter().any(
            |e| matches!(e, SubmitError::IdentityEntitySelfEquivalence { id } if *id == entity_id)
        ),
        "expected IdentityEntitySelfEquivalence on {entity_id:?}, got {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::DuplicateEntityDecl { id } if *id == entity_id)),
        "expected DuplicateEntityDecl on {entity_id:?}, got {errs:?}"
    );
    Ok(())
}

/// Two `Decl::Existing { id: X }` slots on one minted entity, each referenced by
/// a distinct fact, resolve to the same persistent id and are rejected with
/// `DuplicateEntityDecl` carrying that id. Both decls are referenced, so the
/// rejection is the distinctness guard, not `UnusedDeclaration`.
pub async fn two_entity_decls_resolving_to_one_id_rejected<S: FactStore>(store: S) -> TestResult {
    let mint: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "real-entity")?].into_iter().collect(),
    };
    let minted = commit_facts(&store, mint)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let entity_id = minted
        .entities
        .get(&EntityIdx(0))
        .ok_or("expected mint")?
        .id
        .clone();

    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![
            Decl::Existing {
                id: entity_id.clone(),
            },
            Decl::Existing {
                id: entity_id.clone(),
            },
        ],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "via-decl-0")?, name_fact(1, "via-decl-1")?]
            .into_iter()
            .collect(),
    };
    let errs = commit_err(&store, bundle).await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::DuplicateEntityDecl { id } if *id == entity_id)),
        "got {errs:?}"
    );
    Ok(())
}

// --- retract-commit target existence ---

/// A `RetractCommit` whose target was never recorded is rejected with
/// `CommitNotFound` carrying the id. The id is a valid 64-char hex
/// `CommitId` no commit hashes to in an empty store, so it exercises the
/// `commit_known(...) == false` arm rather than a structural pre-check.
pub async fn retract_commit_targeting_unrecorded_commit_rejected<S: FactStore>(
    store: S,
) -> TestResult {
    let phantom = CommitId::parse("0".repeat(64))?;

    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_commit_fact(phantom.clone())?]
            .into_iter()
            .collect(),
    };

    let err = match commit_facts(&store, bundle).await {
        Ok(_) => return Err("expected CommitNotFound rejection".into()),
        Err(e) => e,
    };

    let errs = submit_batch(err)?;
    assert_eq!(errs.len().get(), 1);
    let SubmitError::CommitNotFound { id } = errs.first() else {
        return Err(format!("expected CommitNotFound, got {:?}", errs.first()).into());
    };
    assert_eq!(*id, phantom);
    Ok(())
}

/// A `RetractCommit` targeting an already-recorded commit succeeds.
/// Commits a fact-bearing bundle for a real `CommitId`, then retracts it;
/// the `commit_known` check passes because the first commit recorded it.
pub async fn retract_commit_targeting_recorded_commit_succeeds<S: FactStore>(
    store: S,
) -> TestResult {
    let first: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "to-be-retracted")?].into_iter().collect(),
    };
    let first_result = commit_facts(&store, first)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let target = first_result.commit_id.clone();

    let retraction: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_commit_fact(target)?].into_iter().collect(),
    };
    let retraction_result = commit_facts(&store, retraction)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        retraction_result.fact_ids.len(),
        1,
        "the retraction's meta-fact must be recorded"
    );
    assert!(!retraction_result.previously_committed);
    Ok(())
}

// --- transaction brand ---

/// Positive control for the brand pattern: a tx handle is usable across
/// multiple `submit_commit` calls inside its own closure. The pattern
/// blocks cross-instance misuse, not within-instance re-use.
pub async fn two_commits_share_one_with_tx_brand<S: FactStore>(store: S) -> TestResult {
    let bundle1: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "first")?].into_iter().collect(),
    };
    let bundle2: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "second")?].into_iter().collect(),
    };

    let (r1, r2) = store
        .with_tx(|s, tx| {
            Box::pin(async move {
                let r1 = s
                    .submit_commit(tx, bundle1)
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                let r2 = s
                    .submit_commit(tx, bundle2)
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                Ok::<_, String>((r1, r2))
            })
        })
        .await
        .map_err(|e| format!("{e:?}"))??;

    assert_eq!(r1.fact_ids.len(), 1);
    assert_eq!(r2.fact_ids.len(), 1);
    assert_ne!(r1.commit_id, r2.commit_id);
    Ok(())
}

// --- transaction semantics ---

/// A `with_tx` closure that fails after a successful submit rolls the whole
/// transaction back: the store is unchanged and a later resubmit of the same
/// bundle is a first commit, not a replay.
pub async fn err_from_with_tx_closure_rolls_back_submitted_commit<S: FactStore>(
    store: S,
) -> TestResult {
    commit_name(&store, "prior").await?;
    let watermark = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;

    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "rolled-back")?].into_iter().collect(),
    };
    let replay = bundle.clone();

    let outcome: Result<(), String> = store
        .with_tx(|s, tx| {
            Box::pin(async move {
                let result = s
                    .submit_commit(tx, bundle)
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                assert!(!result.previously_committed);
                Err("deliberate failure after the submit".to_owned())
            })
        })
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert!(outcome.is_err());

    // The submitted fact never landed and the clock never advanced.
    let after = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(after, watermark);
    let mut view = store
        .no_later_than(FactId::new(watermark.get() + 1))
        .await
        .map_err(|e| format!("{e:?}"))?;
    let lookup = view.fact(watermark).await.map_err(|e| format!("{e:?}"))?;
    assert!(matches!(lookup, FactLookup::Unknown), "got {lookup:?}");

    // Resubmitting is a first commit — the rolled-back result cache is gone.
    let result = commit_facts(&store, replay)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert!(!result.previously_committed);
    Ok(())
}

/// A commit recorded earlier in the same transaction is a valid
/// `RetractCommit` target: its staged metadata is visible to the second
/// submit's validation and retractor expansion, so the retraction lands and
/// takes effect.
pub async fn retract_commit_of_earlier_commit_in_same_tx_lands<S: FactStore>(
    store: S,
) -> TestResult {
    let first: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "retracted-in-tx")?].into_iter().collect(),
    };

    let (first_result, second_result) = store
        .with_tx(|s, tx| {
            Box::pin(async move {
                let first_result = s
                    .submit_commit(tx, first)
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                let retraction: SubmitCommitInput<S> = SubmitBundle {
                    author: user_author().map_err(|e| e.to_string())?,
                    recorded_at: fixed_time() + chrono::Duration::seconds(10),
                    entities: Vec::new(),
                    events: Vec::new(),
                    images: Vec::new(),
                    facts: [retract_commit_fact(first_result.commit_id.clone())
                        .map_err(|e| e.to_string())?]
                    .into_iter()
                    .collect(),
                };
                let second_result = s
                    .submit_commit(tx, retraction)
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                Ok::<_, String>((first_result, second_result))
            })
        })
        .await
        .map_err(|e| format!("{e:?}"))??;

    let target_fid = *first_result
        .fact_ids
        .first()
        .ok_or("first commit minted no fact")?;
    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let lookup = view.fact(target_fid).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Retracted { by } = lookup else {
        return Err(format!("expected Retracted, got {lookup:?}").into());
    };
    assert_eq!(Some(&by), second_result.fact_ids.first());
    Ok(())
}

/// Nothing from a rejected submit is ever durable, even when the `with_tx`
/// closure swallows the rejection and returns `Ok`. How a backend refuses is
/// its own business — unwinding the submit boundary and committing an empty
/// transaction, or refusing the whole transaction at apply — so the case
/// accepts either `with_tx` outcome and pins what both must guarantee: the
/// store stays empty, the rejected commit stays unknown, and a fresh
/// transaction is fully usable.
pub async fn swallowed_submit_rejection_leaves_nothing_durable<S: FactStore>(
    store: S,
) -> TestResult {
    // Decl 1 is never referenced: the bundle stages its one fact, then
    // rejects with UnusedDeclaration.
    let doomed: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local, Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "staged-then-rejected")?]
            .into_iter()
            .collect(),
    };
    let doomed_id = doomed.id()?;
    // The with_tx outcome is deliberately unasserted: rejection recovery is
    // backend-defined — memory refuses the whole transaction, sqlite unwinds
    // the submit scope and commits clean.
    let _outcome = store
        .with_tx(|s, tx| {
            Box::pin(async move {
                let Err(SubmitCommitError::Submit(_)) = s.submit_commit(tx, doomed).await else {
                    return Err("expected the submit to be rejected".to_owned());
                };
                Ok::<_, String>(())
            })
        })
        .await;

    // Nothing landed: the clock never advanced and the rejected commit was
    // never recorded.
    let after = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(after, FactId::new(0));
    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    assert!(
        !view
            .commit_known(&doomed_id)
            .await
            .map_err(|e| format!("{e:?}"))?
    );

    // A fresh transaction sees no leftovers: its commit is a first commit
    // and its fact takes the first id.
    let result = commit_name(&store, "after-the-rejection").await?;
    assert!(!result.previously_committed);
    assert_eq!(result.fact_ids.first(), Some(&FactId::new(0)));
    Ok(())
}

/// Record a synthetic single-fact commit through the [`FactWrite`] surface,
/// with `byte` repeated as the commit id.
async fn record_synthetic<S, W>(
    tx: &mut W,
    byte: &str,
    author: CommitAuthor,
    fid: FactId,
) -> Result<(), String>
where
    S: FactStore,
    W: FactWrite<S>,
{
    let commit_id = CommitId::parse(byte.repeat(32)).map_err(|e| format!("{e:?}"))?;
    let stored = StoredCommit {
        commit_id: commit_id.clone(),
        author,
        recorded_at: fixed_time(),
        fact_ids: vec![fid],
    };
    let result = SubmitResult::<S::Ids> {
        commit_id,
        previously_committed: false,
        fact_ids: vec![fid],
        entities: HashMap::new(),
        events: HashMap::new(),
        images: HashMap::new(),
        companion_commit_id: None,
    };
    tx.record_commit(stored, &result)
        .await
        .map_err(|e| format!("{e:?}"))
}

/// Read back a committed fact's stored body, to re-stage through the
/// [`FactWrite`] primitives without synthesising a fresh `StoredFact`.
async fn stored_fact_of<S: FactStore>(
    store: &S,
    fid: FactId,
) -> Result<crate::store::StoredFactOf<S>, super::TestError> {
    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let lookup = view.fact(fid).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Active(stored) = lookup else {
        return Err(format!("expected the seed fact Active, got {lookup:?}").into());
    };
    Ok(*stored)
}

/// `record_commit` marks exactly its own commit's facts committed: another
/// commit's staged facts stay `InFlight` until their own record, even after
/// a later-staged commit records first.
pub async fn record_commit_marks_only_its_own_facts_committed<S: FactStore>(
    store: S,
) -> TestResult {
    let seed = commit_name(&store, "seed").await?;
    let seed_fid = *seed.fact_ids.first().ok_or("no seed fact id")?;
    let seed_stored = stored_fact_of(&store, seed_fid).await?;

    let author = user_author()?;
    store
        .with_tx(|_s, tx| {
            Box::pin(async move {
                let fid_a = tx
                    .stage_fact(seed_stored.clone())
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                let fid_b = tx
                    .stage_fact(seed_stored)
                    .await
                    .map_err(|e| format!("{e:?}"))?;

                // Recording B's commit leaves the earlier-staged A in flight.
                record_synthetic::<S, _>(tx, "cd", author.clone(), fid_b).await?;
                let a = tx.placement(fid_a).await.map_err(|e| format!("{e:?}"))?;
                let b = tx.placement(fid_b).await.map_err(|e| format!("{e:?}"))?;
                assert_eq!(a, FactPlacement::InFlight);
                assert_eq!(b, FactPlacement::Committed);

                record_synthetic::<S, _>(tx, "ef", author, fid_a).await?;
                let a = tx.placement(fid_a).await.map_err(|e| format!("{e:?}"))?;
                assert_eq!(a, FactPlacement::Committed);
                Ok::<_, String>(())
            })
        })
        .await
        .map_err(|e| format!("{e:?}"))??;
    Ok(())
}

/// Two recorded commits claiming one staged fact never commit: a fact
/// belongs to exactly one commit, and a double claim would otherwise resolve
/// arbitrarily. Where the refusal lands is backend-defined — the claiming
/// `record_commit` or the transaction's commit step — so the case accepts
/// either failure and pins that nothing becomes durable.
pub async fn overlapping_recorded_commits_never_commit<S: FactStore>(store: S) -> TestResult {
    let seed = commit_name(&store, "seed").await?;
    let seed_fid = *seed.fact_ids.first().ok_or("no seed fact id")?;
    let watermark = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;
    let seed_stored = stored_fact_of(&store, seed_fid).await?;

    let author = user_author()?;
    let outcome = store
        .with_tx(|_s, tx| {
            Box::pin(async move {
                let fid = tx
                    .stage_fact(seed_stored)
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                record_synthetic::<S, _>(tx, "cd", author.clone(), fid).await?;
                record_synthetic::<S, _>(tx, "ef", author, fid).await?;
                Ok::<_, String>(())
            })
        })
        .await;
    assert!(
        !matches!(outcome, Ok(Ok(()))),
        "a doubly-claimed staged fact must refuse the record or the transaction, got {outcome:?}"
    );
    // Nothing landed.
    let after = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(after, watermark);
    Ok(())
}

// --- self-referential meta rules + retraction visibility ---

/// A `RetractFact` whose target is a fact in the same commit (its own
/// meta-fact id) is rejected with `MetaTargetInSameCommit`.
pub async fn retract_fact_targeting_same_commit_fact_rejected<S: FactStore>(
    store: S,
) -> TestResult {
    commit_name(&store, "prior").await?;
    // The id this retraction's own meta-fact will take — an in-commit target.
    let in_commit = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;

    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_fact(in_commit)?].into_iter().collect(),
    };
    let err = match commit_facts(&store, bundle).await {
        Ok(_) => return Err("expected MetaTargetInSameCommit".into()),
        Err(e) => e,
    };
    let errs = submit_batch(err)?;
    assert_eq!(errs.len().get(), 1);
    let SubmitError::MetaTargetInSameCommit { target } = errs.first() else {
        return Err(format!("expected MetaTargetInSameCommit, got {:?}", errs.first()).into());
    };
    assert_eq!(*target, in_commit);
    Ok(())
}

/// A `RetractFact` targeting a prior committed fact is accepted.
pub async fn retract_fact_targeting_prior_fact_accepted<S: FactStore>(store: S) -> TestResult {
    let prior = commit_name(&store, "to-retract").await?;
    let target = *prior.fact_ids.first().ok_or("no prior fact id")?;

    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_fact(target)?].into_iter().collect(),
    };
    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        result.fact_ids.len(),
        1,
        "the retraction meta-fact must be recorded"
    );
    Ok(())
}

/// A `SupersedeFact` whose target is in the same commit (with a prior
/// replacement) is rejected with `MetaTargetInSameCommit`.
pub async fn supersede_fact_targeting_same_commit_fact_rejected<S: FactStore>(
    store: S,
) -> TestResult {
    let prior = commit_name(&store, "replacement").await?;
    let replacement = *prior.fact_ids.first().ok_or("no prior fact id")?;
    let in_commit = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;

    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [supersede_fact(in_commit, replacement)?]
            .into_iter()
            .collect(),
    };
    let err = match commit_facts(&store, bundle).await {
        Ok(_) => return Err("expected MetaTargetInSameCommit".into()),
        Err(e) => e,
    };
    let errs = submit_batch(err)?;
    assert_eq!(errs.len().get(), 1);
    let SubmitError::MetaTargetInSameCommit { target } = errs.first() else {
        return Err(format!("expected MetaTargetInSameCommit, got {:?}", errs.first()).into());
    };
    assert_eq!(*target, in_commit);
    Ok(())
}

/// A `SupersedeFact` whose target equals its replacement is rejected with
/// `SupersedeReplacementEqualsTarget`, before any target existence lookup.
pub async fn supersede_fact_with_equal_target_and_replacement_rejected<S: FactStore>(
    store: S,
) -> TestResult {
    let prior = commit_name(&store, "self-target").await?;
    let fact_id = *prior.fact_ids.first().ok_or("no prior fact id")?;

    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [supersede_fact(fact_id, fact_id)?].into_iter().collect(),
    };
    let err = match commit_facts(&store, bundle).await {
        Ok(_) => return Err("expected SupersedeReplacementEqualsTarget".into()),
        Err(e) => e,
    };
    let errs = submit_batch(err)?;
    assert_eq!(errs.len().get(), 1);
    let SubmitError::SupersedeReplacementEqualsTarget { target } = errs.first() else {
        return Err(format!(
            "expected SupersedeReplacementEqualsTarget, got {:?}",
            errs.first()
        )
        .into());
    };
    assert_eq!(*target, fact_id);
    Ok(())
}

/// A retracted fact reads `Active` at a snapshot before the retraction and
/// `Retracted` by it once the retraction is visible.
pub async fn retract_fact_hides_target_only_after_its_commit<S: FactStore>(store: S) -> TestResult {
    let original = commit_name(&store, "fact-x").await?;
    let target = *original.fact_ids.first().ok_or("no original fact id")?;

    let retraction_bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_fact(target)?].into_iter().collect(),
    };
    let retraction = commit_facts(&store, retraction_bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let retractor = *retraction.fact_ids.first().ok_or("no retractor fact id")?;

    let mut before = store
        .no_later_than(retractor)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let lookup = before.fact(target).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Active(_) = lookup else {
        return Err(format!("expected Active before retraction, got {lookup:?}").into());
    };

    let mut after = store.now().await.map_err(|e| format!("{e:?}"))?;
    let lookup = after.fact(target).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Retracted { by } = lookup else {
        return Err(format!("expected Retracted after retraction, got {lookup:?}").into());
    };
    assert_eq!(by, retractor);
    Ok(())
}

/// A read snapshot classifies a committed fact as `Committed` and an id at or
/// past the snapshot as `Absent`, and never reports `InFlight` — it has no
/// in-flight commit.
pub async fn read_snapshot_placement_is_committed_or_absent<S: FactStore>(store: S) -> TestResult {
    let first = commit_name(&store, "alpha").await?;
    let committed = *first.fact_ids.first().ok_or("no committed fact id")?;
    let snapshot = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;
    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;

    assert_eq!(
        view.placement(committed)
            .await
            .map_err(|e| format!("{e:?}"))?,
        FactPlacement::Committed
    );
    // The next id to be minted is past the snapshot — no fact lives there yet.
    assert_eq!(
        view.placement(snapshot)
            .await
            .map_err(|e| format!("{e:?}"))?,
        FactPlacement::Absent
    );
    Ok(())
}

/// A read snapshot whose watermark sits far past the committed fact count
/// reports `Absent`, not `InFlight`, for ids between the committed count and the
/// watermark — a read view has no in-flight commit, so `InFlight` is
/// structurally unreachable.
pub async fn read_snapshot_placement_never_inflight_past_watermark<S: FactStore>(
    store: S,
) -> TestResult {
    commit_name(&store, "alpha").await?;
    commit_name(&store, "beta").await?;
    let committed_count = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(committed_count, FactId::new(2));

    // An "everything" view whose watermark is well past the committed count.
    let mut view = store
        .no_later_than(FactId::new(1_000))
        .await
        .map_err(|e| format!("{e:?}"))?;

    // An id between the committed count and the watermark names no fact — the
    // old lower-bound-only `InFlight` branch wrongly reported `InFlight` here.
    assert_eq!(
        view.placement(FactId::new(500))
            .await
            .map_err(|e| format!("{e:?}"))?,
        FactPlacement::Absent
    );
    // An id below the committed count is `Committed`.
    assert_eq!(
        view.placement(FactId::new(0))
            .await
            .map_err(|e| format!("{e:?}"))?,
        FactPlacement::Committed
    );
    Ok(())
}

/// A `RetractCommit` hides every fact of its target commit.
pub async fn retract_commit_hides_every_fact_of_target<S: FactStore>(store: S) -> TestResult {
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "Pantheon")?, construction_started_fact(0)?]
            .into_iter()
            .collect(),
    };
    let original = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(original.fact_ids.len(), 2);
    let target_commit = original.commit_id.clone();

    let retraction_bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_commit_fact(target_commit)?].into_iter().collect(),
    };
    let retraction = commit_facts(&store, retraction_bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let retractor = *retraction.fact_ids.first().ok_or("no retractor fact id")?;

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    for fact_id in &original.fact_ids {
        let lookup = view.fact(*fact_id).await.map_err(|e| format!("{e:?}"))?;
        let FactLookup::Retracted { by } = lookup else {
            return Err(format!("expected Retracted for {fact_id:?}, got {lookup:?}").into());
        };
        assert_eq!(by, retractor);
    }
    Ok(())
}

/// A `SupersedeFact` hides its target but leaves the replacement `Active`.
pub async fn supersede_fact_hides_target_and_keeps_replacement<S: FactStore>(
    store: S,
) -> TestResult {
    let original = commit_name(&store, "old-value").await?;
    let target = *original.fact_ids.first().ok_or("no target fact id")?;
    let replacement_result = commit_name(&store, "new-value").await?;
    let replacement = *replacement_result
        .fact_ids
        .first()
        .ok_or("no replacement fact id")?;

    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(20),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [supersede_fact(target, replacement)?].into_iter().collect(),
    };
    let supersession = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let supersede_id = *supersession
        .fact_ids
        .first()
        .ok_or("no supersede fact id")?;

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let target_lookup = view.fact(target).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Retracted { by } = target_lookup else {
        return Err(format!("expected superseded target Retracted, got {target_lookup:?}").into());
    };
    assert_eq!(by, supersede_id);

    let replacement_lookup = view.fact(replacement).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Active(_) = replacement_lookup else {
        return Err(format!("expected replacement Active, got {replacement_lookup:?}").into());
    };
    Ok(())
}

/// Retracting a retraction restores the original fact's visibility. F1 is
/// active at a C1-era snapshot, retracted at a C2-era snapshot, and active
/// again at a C3-era snapshot once the retraction is itself retracted.
pub async fn retraction_of_retraction_restores_visibility<S: FactStore>(store: S) -> TestResult {
    let c1 = commit_name(&store, "f1").await?;
    let f1 = *c1.fact_ids.first().ok_or("no f1 id")?;

    let c2_bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_fact(f1)?].into_iter().collect(),
    };
    let c2 = commit_facts(&store, c2_bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let r1 = *c2.fact_ids.first().ok_or("no r1 id")?;

    let c3_bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(20),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_fact(r1)?].into_iter().collect(),
    };
    let c3 = commit_facts(&store, c3_bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let r2 = *c3.fact_ids.first().ok_or("no r2 id")?;

    let mut era1 = store
        .no_later_than(r1)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let lookup = era1.fact(f1).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Active(_) = lookup else {
        return Err(format!("C1-era: expected Active, got {lookup:?}").into());
    };

    let mut era2 = store
        .no_later_than(r2)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let lookup = era2.fact(f1).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Retracted { by } = lookup else {
        return Err(format!("C2-era: expected Retracted, got {lookup:?}").into());
    };
    assert_eq!(by, r1);

    let mut era3 = store.now().await.map_err(|e| format!("{e:?}"))?;
    let lookup = era3.fact(f1).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Active(_) = lookup else {
        return Err(format!("C3-era: expected Active again, got {lookup:?}").into());
    };
    Ok(())
}

/// A `SupersedeFact` naming a never-minted replacement is rejected with
/// `FactNotFound` for the dangling replacement id.
pub async fn supersede_fact_with_unminted_replacement_rejected<S: FactStore>(
    store: S,
) -> TestResult {
    let prior = commit_name(&store, "supersede-target").await?;
    let target = *prior.fact_ids.first().ok_or("no prior fact id")?;
    let phantom = FactId::new(999_999);

    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [supersede_fact(target, phantom)?].into_iter().collect(),
    };
    let err = match commit_facts(&store, bundle).await {
        Ok(_) => return Err("expected FactNotFound for replacement".into()),
        Err(e) => e,
    };
    let errs = submit_batch(err)?;
    assert_eq!(errs.len().get(), 1);
    let SubmitError::FactNotFound { id } = errs.first() else {
        return Err(format!("expected FactNotFound, got {:?}", errs.first()).into());
    };
    assert_eq!(*id, phantom);
    Ok(())
}

/// When a fact has several retractors and the lowest-id one is itself
/// retracted, `fact()` reports the lowest STILL-effective retractor.
pub async fn retracted_by_reports_lowest_still_effective_retractor<S: FactStore>(
    store: S,
) -> TestResult {
    let original = commit_name(&store, "multiply-retracted").await?;
    let f = *original.fact_ids.first().ok_or("no original fact id")?;

    // Two independent retractions of F (ra has the lower id).
    let first = commit_retract(&store, f, 10).await?;
    let ra = *first.fact_ids.first().ok_or("no ra id")?;
    let second = commit_retract(&store, f, 20).await?;
    let rb = *second.fact_ids.first().ok_or("no rb id")?;
    // Retract ra, cancelling it.
    commit_retract(&store, ra, 30).await?;

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let lookup = view.fact(f).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Retracted { by } = lookup else {
        return Err(format!("expected Retracted, got {lookup:?}").into());
    };
    assert_eq!(
        by, rb,
        "ra is cancelled, so rb is the lowest still-effective retractor"
    );
    Ok(())
}

// --- accumulation ---

/// One bundle violating three independent rules — a disjunctive stored date, an
/// inverted name window, and a self-parent subimage — is rejected with all three
/// in a single batch, accumulation rather than first-failure.
pub async fn multi_rule_violations_accumulate_in_one_batch<S: FactStore>(store: S) -> TestResult {
    let bundle = local_bundle(
        2,
        0,
        1,
        0,
        vec![
            started_with_date(0, disjunctive_date()?)?,
            name_window_fact(1, Some(1900), Some(1800))?,
            subimage_fact(0, 0)?,
        ],
    )?;
    let errs = commit_err(&store, bundle).await?;
    assert_eq!(
        errs.len().get(),
        3,
        "expected three rule violations, got {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::NonSingleIntervalDate { .. })),
        "missing NonSingleIntervalDate: {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::NameWindowInverted { .. })),
        "missing NameWindowInverted: {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::CompositeSelfParent { .. })),
        "missing CompositeSelfParent: {errs:?}"
    );
    Ok(())
}

/// The resolvability check batches every out-of-range index and unknown
/// `Decl::Existing` together and gates the rules out: a rule-violating fact in
/// the same bundle is not reported, because its references can't resolve.
pub async fn resolvability_gate_batches_and_skips_rules<S: UnmintedIds>(store: S) -> TestResult {
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Existing {
            id: S::unminted_entity(),
        }],
        events: Vec::new(),
        images: vec![Decl::Local],
        facts: [
            name_fact(5, "out-of-range-entity")?,
            medium_picture_fact(7)?,
            started_with_date(0, disjunctive_date()?)?,
        ]
        .into_iter()
        .collect(),
    };
    let errs = commit_err(&store, bundle).await?;
    assert_eq!(
        errs.len().get(),
        3,
        "expected three resolvability errors only, got {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::EntityIdxOutOfRange { idx: 5, .. })),
        "missing EntityIdxOutOfRange: {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::ImageIdxOutOfRange { idx: 7, .. })),
        "missing ImageIdxOutOfRange: {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::UnknownExistingEntity { .. })),
        "missing UnknownExistingEntity: {errs:?}"
    );
    // The would-be single-interval-date and unused-declaration violations only
    // run after resolvability passes, so the early return suppresses them.
    assert!(
        !errs
            .iter()
            .any(|e| matches!(e, SubmitError::NonSingleIntervalDate { .. })),
        "a rule fired despite the resolvability gate: {errs:?}"
    );
    Ok(())
}

// --- backlink reads (active-only, snapshot-scoped) ---

/// `all_facts_about_image` returns only active facts: a retracted fact about
/// the image is excluded.
pub async fn all_facts_about_image_excludes_retracted<S: FactStore>(store: S) -> TestResult {
    let c1 = commit_facts(
        &store,
        local_bundle(0, 0, 1, 0, vec![medium_picture_fact(0)?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let image = c1
        .images
        .get(&ImageIdx(0))
        .ok_or("missing image")?
        .id
        .clone();
    let target = *c1.fact_ids.first().ok_or("no fact id")?;

    let retraction: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_fact(target)?].into_iter().collect(),
    };
    commit_facts(&store, retraction)
        .await
        .map_err(|e| format!("{e:?}"))?;

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let page = view
        .all_facts_about_image(&image, None, PAGE_100)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert!(
        !page.items.iter().any(|item| item.fact_id == target),
        "retracted fact must not appear in all_facts_about_image; got {:?}",
        page.items
    );
    Ok(())
}

/// `all_facts_about_image` is snapshot-scoped: a fact about the image committed
/// after the snapshot is absent from a view pinned at it.
pub async fn all_facts_about_image_respects_snapshot<S: FactStore>(store: S) -> TestResult {
    let c1 = commit_facts(
        &store,
        local_bundle(0, 0, 1, 0, vec![medium_picture_fact(0)?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let image = c1
        .images
        .get(&ImageIdx(0))
        .ok_or("missing image")?
        .id
        .clone();
    let early = *c1.fact_ids.first().ok_or("no fact id")?;
    let snapshot = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;

    let c2: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Existing { id: image.clone() }],
        facts: [captured_date_fact(0)?].into_iter().collect(),
    };
    let c2 = commit_facts(&store, c2)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let late = *c2.fact_ids.first().ok_or("no fact id")?;

    let mut view = store
        .no_later_than(snapshot)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let page = view
        .all_facts_about_image(&image, None, PAGE_100)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let returned: BTreeSet<FactId> = page.items.iter().map(|item| item.fact_id).collect();
    assert!(returned.contains(&early), "pre-snapshot fact must appear");
    assert!(
        !returned.contains(&late),
        "post-snapshot fact must be absent; got {returned:?}"
    );
    Ok(())
}

// --- cluster rules ---

/// A construction bookend carrying a location is accepted.
pub async fn construction_location_accepted<S: FactStore>(store: S) -> TestResult {
    commit_ok(
        &store,
        local_bundle(1, 0, 0, 0, vec![construction_location_fact(0)?])?,
    )
    .await
}

/// A stored location that conjoins two far-apart resolved circles denotes
/// nothing — its `conflict_status` is `Conflict` — and is rejected as an empty
/// location, the geometric extension of the bare-`Empty` guard.
pub async fn disjoint_conjunction_location_rejected<S: FactStore>(store: S) -> TestResult {
    let paris = UnresolvedLocation::Resolved(Location::circle(
        GeoPoint::new(48.8566, 2.3522)?,
        Meters::new_unchecked(1000.0),
    )?);
    let tokyo = UnresolvedLocation::Resolved(Location::circle(
        GeoPoint::new(35.6762, 139.6503)?,
        Meters::new_unchecked(1000.0),
    )?);
    let disjoint = UnresolvedLocation::all_of(vec![paris, tokyo])?;
    let errs = commit_err(
        &store,
        local_bundle(1, 0, 0, 0, vec![construction_with_location(0, disjoint)?])?,
    )
    .await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::EmptyLocation { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// A stored location conjoining a circle with an unresolved reference is
/// `Pending` — its emptiness can't be decided before the reference resolves — so
/// the guard accepts it.
pub async fn pending_conjunction_location_accepted<S: FactStore>(store: S) -> TestResult {
    let circle = UnresolvedLocation::Resolved(Location::circle(
        GeoPoint::new(40.0, -74.0)?,
        Meters::new_unchecked(1000.0),
    )?);
    let reference = UnresolvedLocation::Reference(LocationReference::NamedPlace {
        name: Text::new("Paris")?,
    });
    let pending = UnresolvedLocation::all_of(vec![circle, reference])?;
    commit_ok(
        &store,
        local_bundle(1, 0, 0, 0, vec![construction_with_location(0, pending)?])?,
    )
    .await
}

/// A stored location conjoining two overlapping resolved circles is
/// `Consistent` and accepted.
pub async fn consistent_conjunction_location_accepted<S: FactStore>(store: S) -> TestResult {
    let a = UnresolvedLocation::Resolved(Location::circle(
        GeoPoint::new(40.0, -74.0)?,
        Meters::new_unchecked(5000.0),
    )?);
    let b = UnresolvedLocation::Resolved(Location::circle(
        GeoPoint::new(40.005, -74.0)?,
        Meters::new_unchecked(5000.0),
    )?);
    let consistent = UnresolvedLocation::all_of(vec![a, b])?;
    commit_ok(
        &store,
        local_bundle(1, 0, 0, 0, vec![construction_with_location(0, consistent)?])?,
    )
    .await
}

/// A stored location naming more than `MAX_LOCATION_CIRCLES` circles is rejected;
/// one exactly at the cap commits. The bound guards the emptiness check's
/// candidate-point cost against machine-generated junk.
pub async fn over_complex_location_rejected_at_cap_accepted<S: FactStore>(store: S) -> TestResult {
    use crate::submit::pipeline::MAX_LOCATION_CIRCLES;

    let over = n_circle_location(MAX_LOCATION_CIRCLES + 1)?;
    let errs = commit_err(
        &store,
        local_bundle(1, 0, 0, 0, vec![construction_with_location(0, over)?])?,
    )
    .await?;
    assert!(
        errs.iter().any(|e| matches!(
            e,
            SubmitError::LocationTooComplex { circles, limit, .. }
                if *circles == MAX_LOCATION_CIRCLES + 1 && *limit == MAX_LOCATION_CIRCLES
        )),
        "got {errs:?}"
    );

    let at_cap = n_circle_location(MAX_LOCATION_CIRCLES)?;
    commit_ok(
        &store,
        local_bundle(1, 0, 0, 0, vec![construction_with_location(0, at_cap)?])?,
    )
    .await
}

/// A minted event with one `HasEvent` and payloads consistent with its declared
/// kind commits and stores.
pub async fn event_with_has_event_and_consistent_payloads_stored<S: FactStore>(
    store: S,
) -> TestResult {
    commit_ok(
        &store,
        local_bundle(
            1,
            1,
            0,
            0,
            vec![
                has_event_fact(0, 0, damaged_kind())?,
                event_damage_cause_fact(0)?,
                event_durational_date_fact(0)?,
            ],
        )?,
    )
    .await
}

/// A minted event with no `HasEvent` is typeless and rejected: a payload alone
/// doesn't declare the event's subject or kind.
pub async fn event_without_has_event_rejected<S: FactStore>(store: S) -> TestResult {
    let errs = commit_err(
        &store,
        local_bundle(1, 1, 0, 0, vec![event_durational_date_fact(0)?])?,
    )
    .await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::EventMissingHasEvent { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// An event referenced only as a `Gap` endpoint, with no `HasEvent`, is still
/// rejected as typeless: the rule's event set spans every referenced id, not
/// just typing/payload subjects, so a gap endpoint can't smuggle in an untyped
/// event.
pub async fn event_referenced_only_as_gap_endpoint_without_has_event_rejected<S: FactStore>(
    store: S,
) -> TestResult {
    let errs = commit_err(
        &store,
        local_bundle(1, 1, 0, 0, vec![gap_from_event_fact(0, 0)?])?,
    )
    .await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::EventMissingHasEvent { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// Two `HasEvent` facts of different kinds on one minted event id is a
/// self-contradiction — an event has one kind — and is rejected, not stored.
/// Disagreement belongs on separate event ids.
pub async fn event_two_has_event_kinds_rejected<S: FactStore>(store: S) -> TestResult {
    let errs = commit_err(
        &store,
        local_bundle(
            1,
            1,
            0,
            0,
            vec![
                has_event_fact(0, 0, damaged_kind())?,
                has_event_fact(0, 0, moved_kind())?,
            ],
        )?,
    )
    .await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::EventMultipleHasEvent { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// Two `HasEvent` facts naming different subject entities on one event id is the
/// same self-contradiction over the entity rather than the kind: an event has
/// one subject. Rejected as `EventMultipleHasEvent`.
pub async fn event_two_has_event_entities_rejected<S: FactStore>(store: S) -> TestResult {
    let errs = commit_err(
        &store,
        local_bundle(
            2,
            1,
            0,
            0,
            vec![
                has_event_fact(0, 0, damaged_kind())?,
                has_event_fact(0, 1, damaged_kind())?,
            ],
        )?,
    )
    .await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::EventMultipleHasEvent { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// A payload contradicting the commit's `HasEvent` kind is rejected: a
/// `MoveMethod` on a `Damaged` event doesn't suit the declared kind.
pub async fn event_payload_contradicts_declared_kind_rejected<S: FactStore>(
    store: S,
) -> TestResult {
    let errs = commit_err(
        &store,
        local_bundle(
            1,
            1,
            0,
            0,
            vec![
                has_event_fact(0, 0, damaged_kind())?,
                event_move_method_fact(0)?,
            ],
        )?,
    )
    .await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::EventFactKindMismatch { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// A `PointDate` on a durational-kinded event is a category mismatch, rejected
/// the same way a typed payload mismatch is.
pub async fn event_date_contradicts_declared_category_rejected<S: FactStore>(
    store: S,
) -> TestResult {
    let errs = commit_err(
        &store,
        local_bundle(
            1,
            1,
            0,
            0,
            vec![
                has_event_fact(0, 0, damaged_kind())?,
                event_point_date_fact(0)?,
            ],
        )?,
    )
    .await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::EventFactKindMismatch { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// Two sources disagreeing on an event's kind store fine as *separate* events:
/// C1 mints event E `Damaged`, C2 mints event F `Moved` on the same entity.
/// Each commit carries one `HasEvent` per minted id, so neither is rejected —
/// the kind rules bind per event id, not per entity — and the disagreement
/// sits in the store, read back per id.
pub async fn cross_source_kind_conflict_stores_as_separate_events<S: FactStore>(
    store: S,
) -> TestResult {
    let c1 = commit_facts(
        &store,
        local_bundle(1, 1, 0, 0, vec![has_event_fact(0, 0, damaged_kind())?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let entity = c1
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing entity")?
        .id
        .clone();

    let c2: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![Decl::Existing { id: entity }],
        events: vec![Decl::Local],
        images: Vec::new(),
        facts: [has_event_fact(0, 0, moved_kind())?].into_iter().collect(),
    };
    commit_ok(&store, c2).await
}

/// A commit adding facts to an already-existing event need not restate its
/// `HasEvent`: the exactly-one rule reads the cumulative neighbourhood, so
/// the prior commit's typing claim covers the new fact. C2 declares the event
/// `Existing`, adds only a description, lands clean, and the fact reads back
/// active.
pub async fn existing_event_accepts_new_facts_without_restating_has_event<S: FactStore>(
    store: S,
) -> TestResult {
    let c1 = commit_facts(
        &store,
        local_bundle(
            1,
            1,
            0,
            0,
            vec![
                has_event_fact(0, 0, designated_kind())?,
                event_point_date_fact(0)?,
            ],
        )?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let event = c1
        .events
        .get(&EventIdx(0))
        .ok_or("missing event")?
        .id
        .clone();

    let c2: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: vec![Decl::Existing { id: event }],
        images: Vec::new(),
        facts: [event_description_fact(0)?].into_iter().collect(),
    };
    let second = commit_result(&store, c2).await?;
    let fact_id = *second.fact_ids.first().ok_or("no fact id")?;

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let lookup = view.fact(fact_id).await.map_err(|e| format!("{e:?}"))?;
    assert!(
        matches!(lookup, FactLookup::Active(_)),
        "expected the new event fact active, got {lookup:?}"
    );
    Ok(())
}

/// A `SameEvent` identity fact is rejected at submit: reads answer singleton
/// event classes — event equivalence is not resolved — so a stored edge would
/// be silently ignored. The bundle is otherwise clean, so the rejection is
/// exactly the `SameEvent` arm.
pub async fn same_event_identity_fact_rejected<S: FactStore>(store: S) -> TestResult {
    let errs = commit_err(
        &store,
        local_bundle(
            1,
            2,
            0,
            0,
            vec![
                has_event_fact(0, 0, designated_kind())?,
                has_event_fact(1, 0, designated_kind())?,
                same_event_fact(0, 1)?,
            ],
        )?,
    )
    .await?;
    assert_eq!(errs.len().get(), 1, "got {errs:?}");
    assert!(
        matches!(errs.first(), SubmitError::SameEventUnresolvable { .. }),
        "got {errs:?}"
    );
    Ok(())
}

/// Re-typing an event across a same-commit retraction is rejected by ownership
/// immutability. C1 types event E `Moved`; C2 retracts that `HasEvent` and adds
/// `HasEvent { Damaged }`. The same-commit retraction is visible, so the stale
/// `Moved` claim is *not* double-counted — no `EventMultipleHasEvent` — but the
/// event's `{entity, kind}` is pinned at its first-ever `HasEvent`, so the
/// re-type to `Damaged` trips `EventOwnershipImmutable` instead.
pub async fn event_retype_across_same_commit_retraction_rejected<S: FactStore>(
    store: S,
) -> TestResult {
    let c1 = commit_facts(
        &store,
        local_bundle(1, 1, 0, 0, vec![has_event_fact(0, 0, moved_kind())?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let entity = c1
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing entity")?
        .id
        .clone();
    let event = c1
        .events
        .get(&EventIdx(0))
        .ok_or("missing event")?
        .id
        .clone();
    let has_event_moved = *c1.fact_ids.first().ok_or("no has-event fact id")?;

    let c2: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![Decl::Existing { id: entity }],
        events: vec![Decl::Existing { id: event }],
        images: Vec::new(),
        facts: [
            retract_fact(has_event_moved)?,
            has_event_fact(0, 0, damaged_kind())?,
        ]
        .into_iter()
        .collect(),
    };
    let errs = commit_err(&store, c2).await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::EventOwnershipImmutable { .. })),
        "the re-type must trip ownership immutability: {errs:?}"
    );
    assert!(
        !errs
            .iter()
            .any(|e| matches!(e, SubmitError::EventMultipleHasEvent { .. })),
        "the same-commit retraction is visible, so the stale claim is not double-counted: {errs:?}"
    );
    Ok(())
}

/// Re-homing an event to a different entity across retraction is rejected. C1
/// types event V `Moved` on entity X; C2 retracts that `HasEvent`; C3 declares V
/// `Existing` and adds `HasEvent { Moved }` on a fresh entity Y. V's only active
/// claim is now Y, so the active-only rules pass — but the pin from V's
/// retracted first-ever `HasEvent` still names X, so ownership rejects.
pub async fn event_rehome_across_retraction_rejected<S: FactStore>(store: S) -> TestResult {
    let c1 = commit_facts(
        &store,
        local_bundle(1, 1, 0, 0, vec![has_event_fact(0, 0, moved_kind())?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let event = c1
        .events
        .get(&EventIdx(0))
        .ok_or("missing event")?
        .id
        .clone();
    let has_event_x = *c1.fact_ids.first().ok_or("no has-event fact id")?;

    commit_retract(&store, has_event_x, 10).await?;

    // C3: mint a fresh entity Y, re-declare V as Existing, re-home to Y.
    let c3: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(20),
        entities: vec![Decl::Local],
        events: vec![Decl::Existing { id: event }],
        images: Vec::new(),
        facts: [has_event_fact(0, 0, moved_kind())?].into_iter().collect(),
    };
    let errs = commit_err(&store, c3).await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::EventOwnershipImmutable { .. })),
        "re-homing V to a new entity must trip ownership immutability: {errs:?}"
    );
    Ok(())
}

/// Re-typing an event across retraction, split over commits, is rejected. Like
/// [`event_rehome_across_retraction_rejected`] but C3 keeps the same entity X
/// and changes only the kind (`Moved` → `Damaged`). The pin's kind is immutable.
pub async fn event_retype_across_retraction_rejected<S: FactStore>(store: S) -> TestResult {
    let c1 = commit_facts(
        &store,
        local_bundle(1, 1, 0, 0, vec![has_event_fact(0, 0, moved_kind())?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let entity = c1
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing entity")?
        .id
        .clone();
    let event = c1
        .events
        .get(&EventIdx(0))
        .ok_or("missing event")?
        .id
        .clone();
    let has_event_moved = *c1.fact_ids.first().ok_or("no has-event fact id")?;

    commit_retract(&store, has_event_moved, 10).await?;

    let c3: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(20),
        entities: vec![Decl::Existing { id: entity }],
        events: vec![Decl::Existing { id: event }],
        images: Vec::new(),
        facts: [has_event_fact(0, 0, damaged_kind())?]
            .into_iter()
            .collect(),
    };
    let errs = commit_err(&store, c3).await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::EventOwnershipImmutable { .. })),
        "re-typing V's kind must trip ownership immutability: {errs:?}"
    );
    Ok(())
}

/// Re-asserting the identical `{entity, kind}` after a retraction is accepted —
/// the pin is unchanged. C1 types event V `Moved` on X; C2 retracts it; C3
/// re-declares V `Existing` and re-asserts `HasEvent { Moved }` on X (a revive
/// with a fresh commit). The active claim equals the pin, so ownership passes.
pub async fn event_reassert_identical_has_event_after_retraction_accepted<S: FactStore>(
    store: S,
) -> TestResult {
    let c1 = commit_facts(
        &store,
        local_bundle(1, 1, 0, 0, vec![has_event_fact(0, 0, moved_kind())?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let entity = c1
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing entity")?
        .id
        .clone();
    let event = c1
        .events
        .get(&EventIdx(0))
        .ok_or("missing event")?
        .id
        .clone();
    let has_event_moved = *c1.fact_ids.first().ok_or("no has-event fact id")?;

    commit_retract(&store, has_event_moved, 10).await?;

    let c3: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(20),
        entities: vec![Decl::Existing { id: entity }],
        events: vec![Decl::Existing { id: event }],
        images: Vec::new(),
        facts: [has_event_fact(0, 0, moved_kind())?].into_iter().collect(),
    };
    commit_ok(&store, c3).await
}

/// A brand-new event id sets its own pin: minting event V with `HasEvent` on X
/// is accepted, whatever the `{entity, kind}` — there is no prior ever-asserted
/// claim to conflict with, so ownership never fires on a first-ever `HasEvent`.
pub async fn event_fresh_id_sets_its_own_pin<S: FactStore>(store: S) -> TestResult {
    commit_ok(
        &store,
        local_bundle(1, 1, 0, 0, vec![has_event_fact(0, 0, moved_kind())?])?,
    )
    .await
}

/// Re-adopting a retracted event id under a different `{entity, kind}` is
/// rejected — the case the retraction-inclusive read is for. C1 types event V
/// `Moved` on X; C2 retracts the `HasEvent`, orphaning V; C3 declares V
/// `Existing` and adopts it under a fresh entity Y with kind `Damaged`. V has no
/// active `HasEvent` between C2 and C3, so the active-only rules see only C3's
/// lone claim and would accept it — but the pin from V's retracted first-ever
/// `HasEvent` names X/`Moved`, so ownership rejects the re-adoption.
pub async fn event_readopt_retracted_id_under_new_owner_rejected<S: FactStore>(
    store: S,
) -> TestResult {
    let c1 = commit_facts(
        &store,
        local_bundle(1, 1, 0, 0, vec![has_event_fact(0, 0, moved_kind())?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let event = c1
        .events
        .get(&EventIdx(0))
        .ok_or("missing event")?
        .id
        .clone();
    let has_event_x = *c1.fact_ids.first().ok_or("no has-event fact id")?;

    commit_retract(&store, has_event_x, 10).await?;

    let c3: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(20),
        entities: vec![Decl::Local],
        events: vec![Decl::Existing { id: event }],
        images: Vec::new(),
        facts: [has_event_fact(0, 0, damaged_kind())?]
            .into_iter()
            .collect(),
    };
    let errs = commit_err(&store, c3).await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::EventOwnershipImmutable { .. })),
        "re-adopting a retracted event id under a new owner must trip ownership immutability: {errs:?}"
    );
    Ok(())
}

/// Re-typing an event must atomically retract the payloads the new kind doesn't
/// admit. C1 mints event E `Moved` with a `MovedToLocation` payload; C2 retracts
/// the `HasEvent { Moved }` and adds `HasEvent { Damaged }` but leaves the
/// `MovedToLocation` active. The consistency rule reads the cumulative active
/// payloads — not just C2's candidates — so the orphaned `MovedToLocation` meets
/// the newly-declared `Damaged` kind and the commit is rejected. Without the
/// cumulative read the stale payload would never be re-checked.
pub async fn event_retype_without_retracting_stale_payload_rejected<S: FactStore>(
    store: S,
) -> TestResult {
    let c1 = commit_facts(
        &store,
        local_bundle(
            1,
            1,
            0,
            0,
            vec![
                has_event_fact(0, 0, moved_kind())?,
                event_moved_to_location_fact(0)?,
            ],
        )?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let entity = c1
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing entity")?
        .id
        .clone();
    let event = c1
        .events
        .get(&EventIdx(0))
        .ok_or("missing event")?
        .id
        .clone();
    let has_event_moved = *c1.fact_ids.first().ok_or("no has-event fact id")?;

    let c2: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![Decl::Existing { id: entity }],
        events: vec![Decl::Existing { id: event }],
        images: Vec::new(),
        facts: [
            retract_fact(has_event_moved)?,
            has_event_fact(0, 0, damaged_kind())?,
        ]
        .into_iter()
        .collect(),
    };
    let errs = commit_err(&store, c2).await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::EventFactKindMismatch { .. })),
        "the stale MovedToLocation must mismatch the new Damaged kind: {errs:?}"
    );
    Ok(())
}

/// A same-commit retraction of the only depiction unmasks the gap: C1 records
/// the sole depiction tying X to I; C2 retracts it and adds an
/// `ImageObservation` of X on I. The retraction is visible to the rule read in
/// C2, so the depiction no longer satisfies the pairing and the commit is
/// rejected as `ObservationWithoutDepiction`. Without the pending-retractor
/// overlay the doomed depiction reads active and the commit is falsely accepted.
pub async fn observation_depiction_retracted_in_same_commit_rejected<S: FactStore>(
    store: S,
) -> TestResult {
    let c1 = commit_facts(
        &store,
        local_bundle(1, 0, 1, 0, vec![depiction_fact(0, 0)?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let entity = c1
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing entity")?
        .id
        .clone();
    let image = c1
        .images
        .get(&ImageIdx(0))
        .ok_or("missing image")?
        .id
        .clone();
    let depiction = *c1.fact_ids.first().ok_or("no depiction fact id")?;

    let c2: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![Decl::Existing { id: entity }],
        events: Vec::new(),
        images: vec![Decl::Existing { id: image }],
        facts: [
            retract_fact(depiction)?,
            observation_feature_fact(0, image_observation_citation(0)?)?,
        ]
        .into_iter()
        .collect(),
    };
    let errs = commit_err(&store, c2).await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::ObservationWithoutDepiction { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// An inverted name window is rejected.
pub async fn name_window_inverted_rejected<S: FactStore>(store: S) -> TestResult {
    let errs = commit_err(
        &store,
        local_bundle(
            1,
            0,
            0,
            0,
            vec![name_window_fact(0, Some(1900), Some(1800))?],
        )?,
    )
    .await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::NameWindowInverted { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// An equal-bound window (earliest == latest year) is accepted.
pub async fn name_window_equal_accepted<S: FactStore>(store: S) -> TestResult {
    commit_ok(
        &store,
        local_bundle(
            1,
            0,
            0,
            0,
            vec![name_window_fact(0, Some(1850), Some(1850))?],
        )?,
    )
    .await
}

/// An open upper bound can't prove inversion, so it's accepted.
pub async fn name_window_open_bound_accepted<S: FactStore>(store: S) -> TestResult {
    commit_ok(
        &store,
        local_bundle(1, 0, 0, 0, vec![name_window_fact(0, Some(1900), None)?])?,
    )
    .await
}

/// A subimage that is its own parent is rejected.
pub async fn composite_self_parent_rejected<S: FactStore>(store: S) -> TestResult {
    let errs = commit_err(
        &store,
        local_bundle(0, 0, 1, 0, vec![subimage_fact(0, 0)?])?,
    )
    .await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::CompositeSelfParent { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// Distinct subimage and parent are accepted.
pub async fn composite_distinct_accepted<S: FactStore>(store: S) -> TestResult {
    commit_ok(
        &store,
        local_bundle(0, 0, 2, 0, vec![subimage_fact(0, 1)?])?,
    )
    .await
}

/// A second parent for the same subimage, arriving in a later commit, is
/// rejected via the image backlink.
pub async fn composite_multiple_parents_across_commits_rejected<S: FactStore>(
    store: S,
) -> TestResult {
    let c1 = commit_facts(
        &store,
        local_bundle(0, 0, 2, 0, vec![subimage_fact(0, 1)?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let subimage = c1
        .images
        .get(&ImageIdx(0))
        .ok_or("missing subimage")?
        .id
        .clone();

    let c2: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Existing { id: subimage }, Decl::Local],
        facts: [subimage_fact(0, 1)?].into_iter().collect(),
    };
    let errs = commit_err(&store, c2).await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::CompositeMultipleParents { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// A self-loop edge `{s←s}` isn't a genuine parent, so `{s←s}` alongside `{s←p}`
/// reports only the self-parent error, not a spurious multiple-parents error.
/// `s` has exactly one real parent, `p`.
pub async fn composite_self_loop_does_not_trip_multiple_parents<S: FactStore>(
    store: S,
) -> TestResult {
    let errs = commit_err(
        &store,
        local_bundle(0, 0, 2, 0, vec![subimage_fact(0, 0)?, subimage_fact(0, 1)?])?,
    )
    .await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::CompositeSelfParent { .. })),
        "expected the self-parent error: {errs:?}"
    );
    assert!(
        !errs
            .iter()
            .any(|e| matches!(e, SubmitError::CompositeMultipleParents { .. })),
        "self-loop must not count as a second parent: {errs:?}"
    );
    Ok(())
}

/// A chain formed across commits (A←X in C1, then X←B in C2) is rejected: X is
/// both a subimage and a parent.
pub async fn composite_chain_across_commits_rejected<S: FactStore>(store: S) -> TestResult {
    // C1: A (img 0) is a subimage of X (img 1).
    let c1 = commit_facts(
        &store,
        local_bundle(0, 0, 2, 0, vec![subimage_fact(0, 1)?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let x = c1.images.get(&ImageIdx(1)).ok_or("missing X")?.id.clone();

    // C2: X is a subimage of a fresh B (img 1).
    let c2: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Existing { id: x }, Decl::Local],
        facts: [subimage_fact(0, 1)?].into_iter().collect(),
    };
    let errs = commit_err(&store, c2).await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::CompositeChain { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// A capture date submits cleanly — capture metadata is a general image
/// attribute, gated by no medium.
pub async fn captured_date_submits_cleanly<S: FactStore>(store: S) -> TestResult {
    commit_ok(
        &store,
        local_bundle(0, 0, 1, 0, vec![captured_date_fact(0)?])?,
    )
    .await
}

/// A depiction alongside a `Medium` on the same image submits cleanly: the
/// medium is a non-gating render hint, constraining no other fact.
pub async fn depiction_with_medium_submits_cleanly<S: FactStore>(store: S) -> TestResult {
    commit_ok(
        &store,
        local_bundle(
            1,
            0,
            1,
            0,
            vec![medium_picture_fact(0)?, depiction_fact(0, 0)?],
        )?,
    )
    .await
}

/// A combination the former role-coherence rule rejected — a map-medium image
/// also carrying a capture date and a depiction — now submits. Medium gates
/// nothing, so the bundle is well-formed; any tension is the projection's to
/// surface, not submit's to reject.
pub async fn former_role_conflict_combination_now_submits<S: FactStore>(store: S) -> TestResult {
    commit_ok(
        &store,
        local_bundle(
            1,
            0,
            1,
            0,
            vec![
                map_medium_fact(0)?,
                captured_date_fact(0)?,
                depiction_fact(0, 0)?,
            ],
        )?,
    )
    .await
}

/// Two disagreeing media on one image submit cleanly — `Medium` gates nothing,
/// even self-contradicting, so the conflict surfaces at projection.
pub async fn disagreeing_media_submit_cleanly<S: FactStore>(store: S) -> TestResult {
    commit_ok(
        &store,
        local_bundle(
            0,
            0,
            1,
            0,
            vec![medium_picture_fact(0)?, map_medium_fact(0)?],
        )?,
    )
    .await
}

// --- observation -> depiction pairing ---

/// An image-observation of an entity with no paired depiction is rejected.
pub async fn observation_without_depiction_rejected<S: FactStore>(store: S) -> TestResult {
    let bundle = local_bundle(
        1,
        0,
        1,
        0,
        vec![observation_feature_fact(0, image_observation_citation(0)?)?],
    )?;
    let errs = commit_err(&store, bundle).await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::ObservationWithoutDepiction { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// A depiction of the entity on the observed image, in the same commit,
/// satisfies the pairing.
pub async fn observation_with_same_commit_depiction_accepted<S: FactStore>(store: S) -> TestResult {
    let bundle = local_bundle(
        1,
        0,
        1,
        0,
        vec![
            observation_feature_fact(0, image_observation_citation(0)?)?,
            depiction_fact(0, 0)?,
        ],
    )?;
    commit_ok(&store, bundle).await
}

/// A depiction in a prior commit satisfies the pairing, via the image backlink.
pub async fn observation_with_prior_commit_depiction_accepted<S: FactStore>(
    store: S,
) -> TestResult {
    let c1 = commit_facts(
        &store,
        local_bundle(1, 0, 1, 0, vec![depiction_fact(0, 0)?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let entity = c1
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing entity")?
        .id
        .clone();
    let image = c1
        .images
        .get(&ImageIdx(0))
        .ok_or("missing image")?
        .id
        .clone();

    let c2: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![Decl::Existing { id: entity }],
        events: Vec::new(),
        images: vec![Decl::Existing { id: image }],
        facts: [observation_feature_fact(0, image_observation_citation(0)?)?]
            .into_iter()
            .collect(),
    };
    commit_ok(&store, c2).await
}

/// An observation cited by `External` carries no observed image, so the pairing
/// requirement doesn't apply.
pub async fn observation_cited_external_accepted<S: FactStore>(store: S) -> TestResult {
    let bundle = local_bundle(
        1,
        0,
        0,
        0,
        vec![observation_feature_fact(0, external_judgment_citation()?)?],
    )?;
    commit_ok(&store, bundle).await
}

// --- single-interval stored-date rule ---

/// A bookend carrying a single interval is accepted (the storable shape).
pub async fn single_interval_bookend_accepted<S: FactStore>(store: S) -> TestResult {
    commit_ok(
        &store,
        local_bundle(1, 0, 0, 0, vec![started_with_date(0, year_date(1900)?)?])?,
    )
    .await
}

/// A bookend carrying a disjunction is rejected at the fact-payload host.
pub async fn disjunctive_bookend_date_rejected<S: FactStore>(store: S) -> TestResult {
    let errs = commit_err(
        &store,
        local_bundle(1, 0, 0, 0, vec![started_with_date(0, disjunctive_date()?)?])?,
    )
    .await?;
    assert!(
        errs.iter().any(|e| matches!(
            e,
            SubmitError::NonSingleIntervalDate {
                role: DateRole::BookendBound
            }
        )),
        "got {errs:?}"
    );
    Ok(())
}

/// A bookend carrying the empty date (⊥) is rejected — ⊥ is no honest claim.
pub async fn empty_bookend_date_rejected<S: FactStore>(store: S) -> TestResult {
    let errs = commit_err(
        &store,
        local_bundle(
            1,
            0,
            0,
            0,
            vec![started_with_date(0, UncertainDate::empty())?],
        )?,
    )
    .await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::NonSingleIntervalDate { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// A `JudgmentSource::External` citation carrying a disjunctive `published`
/// date is rejected — the rule reaches citation dates, not just fact payloads.
pub async fn disjunctive_judgment_citation_date_rejected<S: FactStore>(store: S) -> TestResult {
    let errs = commit_err(
        &store,
        local_bundle(
            1,
            0,
            0,
            0,
            vec![observation_external_published(
                0,
                Some(disjunctive_date()?),
            )?],
        )?,
    )
    .await?;
    assert!(
        errs.iter().any(|e| matches!(
            e,
            SubmitError::NonSingleIntervalDate {
                role: DateRole::CitationDate
            }
        )),
        "got {errs:?}"
    );
    Ok(())
}

/// A `MetaSource::External` citation carrying a disjunctive `created` date is
/// rejected — the third `ExternalSource` host the traversal must reach.
pub async fn disjunctive_meta_citation_date_rejected<S: FactStore>(store: S) -> TestResult {
    // Seed a commit so the retraction has a real target.
    let seed = commit_facts(
        &store,
        local_bundle(1, 0, 0, 0, vec![name_fact(0, "seed")?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;

    let retraction = crate::submit::SubmitFact::Meta {
        assertion: crate::grammar::assertions::MetaAssertion::RetractCommit {
            target: seed.commit_id,
            reason: crate::grammar::assertions::RetractionReason::FactualError,
        },
        citation: crate::grammar::citations::MetaSource::External {
            source: ExternalSource::Archive {
                collection: Text::new("fonds")?,
                catalog_id: None,
                created: Some(disjunctive_date()?),
            },
        },
    };
    let bundle: SubmitCommitInput<S> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retraction].into_iter().collect(),
    };
    let errs = commit_err(&store, bundle).await?;
    assert!(
        errs.iter().any(|e| matches!(
            e,
            SubmitError::NonSingleIntervalDate {
                role: DateRole::CitationDate
            }
        )),
        "got {errs:?}"
    );
    Ok(())
}

// --- spatial class walk (InViewport) ---

/// `InViewport` surfaces every entity the box holds — a construction bookend inside
/// it, or a `MovedToLocation` inside it attributed through its `HasEvent` owner —
/// and nothing else. An out-of-box construction is excluded, and a move whose
/// `HasEvent` owner is retracted attributes to no entity.
pub async fn walk_entity_classes_in_viewport_surfaces_located_and_moved_in_entities<
    S: FactStore,
>(
    store: S,
) -> TestResult {
    let inside = (40.5, -73.5);
    let outside = (10.0, 10.0);

    // A: built inside the box.
    let a = commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![construction_at(0, inside.0, inside.1)?])?,
    )
    .await?;
    let a_id = a.entities.get(&EntityIdx(0)).ok_or("missing a")?.id.clone();

    // B: built outside the box.
    let b = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![construction_at(0, outside.0, outside.1)?])?,
    )
    .await?;
    let b_id = b.entities.get(&EntityIdx(0)).ok_or("missing b")?.id.clone();

    // C: built outside, then a `Moved` event lands it inside the box.
    let c = commit_result(
        &store,
        local_bundle(
            1,
            1,
            0,
            20,
            vec![
                construction_at(0, outside.0, outside.1)?,
                has_event_fact(0, 0, moved_kind())?,
                moved_to(0, inside.0, inside.1)?,
            ],
        )?,
    )
    .await?;
    let c_id = c.entities.get(&EntityIdx(0)).ok_or("missing c")?.id.clone();

    // D: a `Moved` inside the box whose `HasEvent` owner is retracted below, so
    // the orphaned move attributes to no entity.
    let d = commit_result(
        &store,
        local_bundle(
            1,
            1,
            0,
            30,
            vec![
                has_event_fact(0, 0, moved_kind())?,
                moved_to(0, inside.0, inside.1)?,
            ],
        )?,
    )
    .await?;
    let d_event = d
        .events
        .get(&EventIdx(0))
        .ok_or("missing d event")?
        .id
        .clone();
    let mut pre_retract = store.now().await.map_err(|e| format!("{e:?}"))?;
    let d_facts = pre_retract
        .all_facts_about_event(&d_event, None, PAGE_100)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let has_event_fid = d_facts
        .items
        .iter()
        .find(|item| {
            matches!(
                item.fact.event_fact(),
                Some(crate::grammar::event::Fact::HasEvent { .. })
            )
        })
        .ok_or("D's HasEvent is missing")?
        .fact_id;
    commit_retract(&store, has_event_fid, 40).await?;

    let viewport = sample_viewport()?;
    let stream = EntityStream::InViewport(&viewport);
    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let rows: Vec<ClassRow<EntityIdOf<S>>> =
        drain_entity_classes::<S, _>(&mut view, &stream, PAGE_100)
            .await
            .map_err(|e| format!("{e:?}"))?;

    let reps: BTreeSet<EntityIdOf<S>> = rows.iter().map(|r| r.representative.clone()).collect();
    let want: BTreeSet<EntityIdOf<S>> = [a_id.clone(), c_id.clone()].into_iter().collect();
    assert_eq!(
        reps, want,
        "InViewport surfaces the in-box construction (a={a_id:?}) and the moved-in entity \
         (c={c_id:?}); the out-of-box construction (b={b_id:?}) and the orphaned move \
         (d's event={d_event:?}) are excluded; got {rows:?}"
    );
    Ok(())
}

/// A location is a region, not a point: a construction circle whose center
/// sits outside the viewport but whose radius reaches across its edge is
/// surfaced by `InViewport`, while an equal-radius circle that falls short is
/// not. Kills a center-only point-in-box check.
pub async fn walk_entity_classes_in_viewport_surfaces_cap_overlapping_viewport_edge<
    S: FactStore,
>(
    store: S,
) -> TestResult {
    // The viewport is lat [40, 41], lon [-74, -73]. Both centers sit east of
    // it at mid latitude: ~11 km out (within a 20 km radius) and ~84 km out
    // (well past it).
    let overlapping = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            0,
            vec![construction_circle_at(0, 40.5, -72.9, 20_000.0)?],
        )?,
    )
    .await?;
    let overlapping_id = overlapping
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing overlapping entity")?
        .id
        .clone();
    commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            10,
            vec![construction_circle_at(0, 40.5, -72.0, 20_000.0)?],
        )?,
    )
    .await?;

    let viewport = sample_viewport()?;
    let stream = EntityStream::InViewport(&viewport);
    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let rows: Vec<ClassRow<EntityIdOf<S>>> =
        drain_entity_classes::<S, _>(&mut view, &stream, PAGE_100)
            .await
            .map_err(|e| format!("{e:?}"))?;

    let reps: BTreeSet<EntityIdOf<S>> = rows.iter().map(|r| r.representative.clone()).collect();
    let want: BTreeSet<EntityIdOf<S>> = [overlapping_id.clone()].into_iter().collect();
    assert_eq!(
        reps, want,
        "the edge-overlapping circle (center outside, {overlapping_id:?}) is surfaced and \
         the out-of-reach circle is not; got {rows:?}"
    );
    Ok(())
}

/// The image `InViewport` stream surfaces images whose capture location meets the
/// viewport — a point inside it, or a circle overlapping its edge from
/// outside — and excludes a capture location clear of it.
pub async fn walk_image_classes_in_viewport_surfaces_captured_locations<S: FactStore>(
    store: S,
) -> TestResult {
    // A: captured inside the viewport.
    let a = commit_result(
        &store,
        local_bundle(0, 0, 1, 0, vec![captured_location_at(0, 40.5, -73.5, 0.0)?])?,
    )
    .await?;
    let a_id = a.images.get(&ImageIdx(0)).ok_or("missing a")?.id.clone();

    // B: captured well outside.
    commit_result(
        &store,
        local_bundle(0, 0, 1, 10, vec![captured_location_at(0, 10.0, 10.0, 0.0)?])?,
    )
    .await?;

    // C: capture circle centered outside the east edge, radius reaching in.
    let c = commit_result(
        &store,
        local_bundle(
            0,
            0,
            1,
            20,
            vec![captured_location_at(0, 40.5, -72.9, 20_000.0)?],
        )?,
    )
    .await?;
    let c_id = c.images.get(&ImageIdx(0)).ok_or("missing c")?.id.clone();

    let viewport = sample_viewport()?;
    let stream = ImageStream::InViewport(&viewport);
    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let rows: Vec<ClassRow<ImageIdOf<S>>> =
        drain_image_classes::<S, _>(&mut view, &stream, PAGE_100)
            .await
            .map_err(|e| format!("{e:?}"))?;

    let reps: BTreeSet<ImageIdOf<S>> = rows.iter().map(|r| r.representative.clone()).collect();
    let want: BTreeSet<ImageIdOf<S>> = [a_id.clone(), c_id.clone()].into_iter().collect();
    assert_eq!(
        reps, want,
        "image InViewport surfaces the in-box capture (a={a_id:?}) and the edge-overlapping \
         capture circle (c={c_id:?}), excluding the far one; got {rows:?}"
    );
    Ok(())
}

/// A partially-resolved conjunction still carries spatial evidence: an entity
/// located `AllOf(Reference, circle inside the viewport)` is surfaced — the
/// unresolved member removes nothing from the intersection — while the same
/// shape with its circle far away is excluded on that circle's evidence.
pub async fn walk_entity_classes_in_viewport_surfaces_conjunction_with_unresolved_member<
    S: FactStore,
>(
    store: S,
) -> TestResult {
    let conjunction_at = |lat: f64, lon: f64| -> Result<UnresolvedLocation, TestError> {
        Ok(UnresolvedLocation::all_of(vec![
            UnresolvedLocation::Reference(LocationReference::NamedPlace {
                name: Text::new("lot 12")?,
            }),
            UnresolvedLocation::Resolved(Location::circle(
                GeoPoint::new(lat, lon)?,
                Meters::new_unchecked(10.0),
            )?),
        ])?)
    };

    // A: reference + circle inside the viewport.
    let a = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            0,
            vec![construction_with_location(0, conjunction_at(40.5, -73.5)?)?],
        )?,
    )
    .await?;
    let a_id = a.entities.get(&EntityIdx(0)).ok_or("missing a")?.id.clone();

    // B: reference + circle far away.
    commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            10,
            vec![construction_with_location(0, conjunction_at(10.0, 10.0)?)?],
        )?,
    )
    .await?;

    let viewport = sample_viewport()?;
    let stream = EntityStream::InViewport(&viewport);
    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let rows: Vec<ClassRow<EntityIdOf<S>>> =
        drain_entity_classes::<S, _>(&mut view, &stream, PAGE_100)
            .await
            .map_err(|e| format!("{e:?}"))?;

    let reps: BTreeSet<EntityIdOf<S>> = rows.iter().map(|r| r.representative.clone()).collect();
    let want: BTreeSet<EntityIdOf<S>> = [a_id.clone()].into_iter().collect();
    assert_eq!(
        reps, want,
        "the conjunction with an in-viewport circle ({a_id:?}) is surfaced despite its \
         unresolved member, and the far conjunction is not; got {rows:?}"
    );
    Ok(())
}

/// Two entities, each with two construction bookends, walked over the `All`
/// stream one row at a time. `next` walks every row (each representative
/// twice); `next_class` skips the emitted representative's remaining rows, so
/// paging on it visits each representative once. Row order follows the
/// representative's `Ord`, so the two minted ids are sorted into walk order
/// first.
pub async fn class_walk_next_class_cursor_skips_to_the_next_representative<S: FactStore>(
    store: S,
) -> TestResult {
    let a = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            0,
            vec![
                construction_started_in(0, 1700)?,
                construction_started_in(0, 1710)?,
            ],
        )?,
    )
    .await?;
    let a_id = a.entities.get(&EntityIdx(0)).ok_or("missing a")?.id.clone();
    let b = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            10,
            vec![
                construction_started_in(0, 1800)?,
                construction_started_in(0, 1810)?,
            ],
        )?,
    )
    .await?;
    let b_id = b.entities.get(&EntityIdx(0)).ok_or("missing b")?.id.clone();
    assert_ne!(a_id, b_id, "the two commits must mint distinct entities");
    let (first, second) = if a_id < b_id {
        (a_id, b_id)
    } else {
        (b_id, a_id)
    };

    let stream = EntityStream::All;
    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let one = std::num::NonZeroUsize::MIN;

    // First page: one row under the first representative, with both cursors
    // live.
    let page1 = view
        .walk_entity_classes(&stream, None, one)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(page1.rows.len(), 1);
    assert_eq!(page1.rows[0].representative, first);
    let next = page1.next.ok_or("page1 has more rows, so next is set")?;
    let next_class = page1
        .next_class
        .ok_or("the second entity remains, so next_class is set")?;

    // Resuming on `next` stays within the first representative (its second
    // row).
    let page_next = view
        .walk_entity_classes(&stream, Some(next), one)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(page_next.rows.len(), 1);
    assert_eq!(
        page_next.rows[0].representative, first,
        "next stays within the first representative"
    );

    // Resuming on `next_class` skips the first representative's tail and lands
    // on the second; it is the final class, so its `next_class` is exhausted.
    let page_class = view
        .walk_entity_classes(&stream, Some(next_class), one)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(page_class.rows.len(), 1);
    assert_eq!(
        page_class.rows[0].representative, second,
        "next_class skips to the second representative"
    );
    assert!(
        page_class.next_class.is_none(),
        "the second representative is the last"
    );

    // The row cursor walks every row: first, first, second, second.
    let by_row: Vec<EntityIdOf<S>> = drain_entity_classes::<S, _>(&mut view, &stream, one)
        .await
        .map_err(|e| format!("{e:?}"))?
        .into_iter()
        .map(|row| row.representative)
        .collect();
    assert_eq!(
        by_row,
        vec![first.clone(), first.clone(), second.clone(), second.clone()],
        "the row cursor walks every row"
    );

    // The class cursor visits each representative once: first, second.
    let mut by_class: Vec<EntityIdOf<S>> = Vec::new();
    let mut cursor = None;
    loop {
        let page = view
            .walk_entity_classes(&stream, cursor, one)
            .await
            .map_err(|e| format!("{e:?}"))?;
        let Some(row) = page.rows.first() else {
            break;
        };
        by_class.push(row.representative.clone());
        match page.next_class {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    assert_eq!(
        by_class,
        vec![first, second],
        "the class cursor visits each representative once"
    );
    Ok(())
}

// --- viewport clustering ---

/// The geographic centre of tile `(tx, ty)` at `level` — safely inside the
/// tile's finest (level-24) cell, so its [`quadkey`] lands in that tile. Used by
/// the per-tile clustering cases to place entities in chosen sub-tiles.
fn tile_center(level: QuadLevel, tx: u32, ty: u32) -> Result<GeoPoint, TestError> {
    let n = f64::from(1u32 << level.get());
    let ux = (f64::from(tx) + 0.5) / n;
    let uy = (f64::from(ty) + 0.5) / n;
    Ok(GeoPoint::new(mercator_y_to_lat(uy), mercator_x_to_lon(ux))?)
}

/// The per-tile fold is viewport-free: container tile `(z, cx, cy)`'s stand-alone
/// cells (`cluster_tile_cells`) equal the same sub-tiles' cells folded inside a
/// viewport that also spans the eastern neighbour container
/// (`cluster_entities_in_viewport` at `z + CELL_DEPTH`), once the viewport cells
/// are clipped to the container's Morton block. An entity in the neighbour proves
/// the clip removes a real out-of-container cell.
pub async fn cluster_tile_cells_match_the_same_tile_inside_a_viewport<S: FactStore>(
    store: S,
) -> TestResult {
    let z = QuadLevel::new(8)?;
    let cell_level = QuadLevel::saturating(z.get() + CELL_DEPTH);
    // An arbitrary interior container tile; the geography is irrelevant since
    // points are derived from tile coordinates.
    let (cx, cy) = (75u32, 96u32);
    let factor = 1u32 << CELL_DEPTH; // sub-tiles per axis in a container
    let (base_x, base_y) = (cx * factor, cy * factor);

    // Two lone entities in distinct sub-tiles, a co-located pair sharing one
    // sub-tile (identical point → one finest tile), and one entity in the
    // eastern neighbour container.
    let p_lone1 = tile_center(cell_level, base_x + 1, base_y + 2)?;
    let p_lone2 = tile_center(cell_level, base_x + 5, base_y + 6)?;
    let p_pair = tile_center(cell_level, base_x + 3, base_y + 4)?;
    let p_neighbour = tile_center(cell_level, (cx + 1) * factor + 2, base_y + 2)?;

    let place = |p: &GeoPoint, secs: i64| {
        local_bundle::<S::Ids>(1, 0, 0, secs, vec![construction_at(0, p.lat(), p.lon())?])
    };
    commit_result(&store, place(&p_lone1, 0)?).await?;
    commit_result(&store, place(&p_lone2, 10)?).await?;
    commit_result(&store, place(&p_pair, 20)?).await?;
    commit_result(&store, place(&p_pair, 30)?).await?;
    commit_result(&store, place(&p_neighbour, 40)?).await?;

    let tile = TileId::new(z, cx, cy)?;
    let container = tile.range();
    let in_container = |p: &GeoPoint| {
        let q = quadkey(p);
        container.lo <= q && q <= container.hi
    };
    // Precondition: the three in-container points sit in the block; the neighbour
    // does not — so the clip below has something real to remove.
    for p in [&p_lone1, &p_lone2, &p_pair] {
        assert!(
            in_container(p),
            "an in-container point must sit in the block"
        );
    }
    assert!(
        !in_container(&p_neighbour),
        "the neighbour point must sit outside the container block"
    );

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;

    let mut tile_cells = view
        .cluster_tile_cells(tile, RankKey::Unranked)
        .await
        .map_err(|e| format!("{e:?}"))?;

    // A viewport spanning both containers, folded at the sub-tile level. The
    // corner divisor is the level's tiles-per-axis, derived from `z` so editing
    // the level can't leave a stale constant behind.
    let tiles_per_axis = f64::from(1u32 << z.get());
    let viewport = Viewport::new(
        GeoPoint::new(
            mercator_y_to_lat(f64::from(cy + 1) / tiles_per_axis),
            mercator_x_to_lon(f64::from(cx) / tiles_per_axis),
        )?,
        GeoPoint::new(
            mercator_y_to_lat(f64::from(cy) / tiles_per_axis),
            mercator_x_to_lon(f64::from(cx + 2) / tiles_per_axis),
        )?,
    )?;
    let vp_all = view
        .cluster_entities_in_viewport(&viewport, cell_level, RankKey::Unranked)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        vp_all.len(),
        4,
        "the viewport spans three in-container cells plus the neighbour's, got {vp_all:?}"
    );

    let mut vp_cells: Vec<ClusterCell<EntityIdOf<S>>> = vp_all
        .into_iter()
        .filter(|c| in_container(&c.point))
        .collect();

    tile_cells.sort_by(|a, b| a.representative.cmp(&b.representative));
    vp_cells.sort_by(|a, b| a.representative.cmp(&b.representative));

    assert_eq!(
        tile_cells.len(),
        3,
        "the container folds two singletons and one co-located cell, got {tile_cells:?}"
    );
    assert_eq!(
        tile_cells, vp_cells,
        "a container's stand-alone tile cells must equal its clipped viewport cells (the fold is \
         viewport-free)"
    );
    Ok(())
}

/// The per-sub-tile top-N budget: a container whose low-Morton corner sub-tile
/// holds more than [`CLUSTER_TILE_N`] entities still yields a cell for a
/// high-Morton corner sub-tile. A single container-wide `LIMIT` would spend its
/// whole budget on the low corner (whose quadkeys all sort first) and drop the
/// high corner — the headline truncation bug the per-sub-tile budget guards.
pub async fn cluster_tile_cells_keep_high_sub_tiles_under_a_dense_low_corner<S: FactStore>(
    store: S,
) -> TestResult {
    let z = QuadLevel::new(8)?;
    let cell_level = QuadLevel::saturating(z.get() + CELL_DEPTH);
    let (cx, cy) = (40u32, 50u32);
    let factor = 1u32 << CELL_DEPTH;
    let (base_x, base_y) = (cx * factor, cy * factor);

    let low = tile_center(cell_level, base_x, base_y)?;
    let high = tile_center(cell_level, base_x + factor - 1, base_y + factor - 1)?;

    // Fill the low corner with more than the per-sub-tile budget, all at the
    // identical point (one finest tile), so a container-wide LIMIT would exhaust
    // itself here before ever reaching the high corner.
    let dense = CLUSTER_TILE_N + 5;
    for i in 0..dense {
        commit_result(
            &store,
            local_bundle::<S::Ids>(
                1,
                0,
                0,
                i as i64,
                vec![construction_at(0, low.lat(), low.lon())?],
            )?,
        )
        .await?;
    }
    let high_res = commit_result(
        &store,
        local_bundle::<S::Ids>(
            1,
            0,
            0,
            10_000,
            vec![construction_at(0, high.lat(), high.lon())?],
        )?,
    )
    .await?;
    let high_id = high_res
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing high-corner entity")?
        .id
        .clone();

    // Preconditions: both corners sit in the container, and the dense low corner
    // sorts entirely before the high corner.
    let tile = TileId::new(z, cx, cy)?;
    let container = tile.range();
    for p in [&low, &high] {
        let q = quadkey(p);
        assert!(
            container.lo <= q && q <= container.hi,
            "a corner must sit in the container block"
        );
    }
    assert!(
        quadkey(&low) < quadkey(&high),
        "the low corner must sort before the high corner"
    );

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let cells = view
        .cluster_tile_cells(tile, RankKey::Unranked)
        .await
        .map_err(|e| format!("{e:?}"))?;

    assert_eq!(
        cells.len(),
        2,
        "the per-sub-tile budget keeps both the dense low corner and the high corner, got {cells:?}"
    );
    let high_cell = cells
        .iter()
        .find(|c| c.representative == high_id)
        .ok_or("the high-corner entity must surface despite the dense low corner")?;
    assert_eq!(
        high_cell.kind,
        CellKind::Singleton,
        "the high corner holds one entity — a singleton"
    );
    Ok(())
}

/// The clustering read buckets located entities by tile and folds each tile to
/// one [`ClusterCell`], classified three ways: two distinct entities in one tile
/// but distinct finest tiles fold to a [`Cluster`](CellKind::Cluster); a
/// `SameEntity`-merged pair sharing a tile is a [`Singleton`](CellKind::Singleton)
/// (distinct representatives, not fact count, drive the kind); a lone entity is a
/// singleton; and an entity just outside the viewport but inside an overhang tile
/// still yields its own cell — the fold is viewport-free. The `(quadkey,
/// fact_id)`-minimal survivor sets each cell's representative and point. Runs
/// against both backends, so it also pins that their bounded per-tile folds agree
/// cell-for-cell.
pub async fn cluster_entities_in_viewport_buckets_by_tile<S: FactStore>(store: S) -> TestResult {
    let viewport = Viewport::new(GeoPoint::new(40.0, -74.0)?, GeoPoint::new(40.1, -73.9)?)?;
    let level = QuadLevel::new(14)?;

    // Two two-point clusters and a lone point well inside, plus an overhang
    // point just north of the viewport. Within each cluster the first point is
    // north-and-west of the second, so its quadkey is the lower and its fact
    // wins the tile's representative.
    let (a_lat, a_lon) = (40.021, -73.981);
    let (b_lat, b_lon) = (40.020, -73.980);
    let (c_lat, c_lon) = (40.061, -73.941);
    let (d_lat, d_lon) = (40.060, -73.940);
    let (e_lat, e_lon) = (40.030, -73.930);
    let (f_lat, f_lon) = (40.1005, -73.950);

    // A and B: distinct entities in one tile at distinct finest tiles → a Cluster cell.
    let a = commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![construction_at(0, a_lat, a_lon)?])?,
    )
    .await?;
    let a_id = a.entities.get(&EntityIdx(0)).ok_or("missing a")?.id.clone();
    let b = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![construction_at(0, b_lat, b_lon)?])?,
    )
    .await?;
    let b_id = b.entities.get(&EntityIdx(0)).ok_or("missing b")?.id.clone();

    // C and D: a SameEntity-merged pair meant to share a second tile → a
    // singleton cell (one representative, though two facts).
    let cd = commit_result(
        &store,
        local_bundle(
            2,
            0,
            0,
            20,
            vec![
                construction_at(0, c_lat, c_lon)?,
                construction_at(1, d_lat, d_lon)?,
                same_entity_fact(0, 1)?,
            ],
        )?,
    )
    .await?;
    let c_id = cd
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing c")?
        .id
        .clone();
    let d_id = cd
        .entities
        .get(&EntityIdx(1))
        .ok_or("missing d")?
        .id
        .clone();
    let cd_rep = c_id.clone().min(d_id.clone());

    // E: a lone entity in a third tile → a singleton cell.
    let e = commit_result(
        &store,
        local_bundle(1, 0, 0, 30, vec![construction_at(0, e_lat, e_lon)?])?,
    )
    .await?;
    let e_id = e.entities.get(&EntityIdx(0)).ok_or("missing e")?.id.clone();

    // F: just north of the viewport, inside an overhang tile → its own
    // singleton cell, since the fold is viewport-free.
    let f = commit_result(
        &store,
        local_bundle(1, 0, 0, 40, vec![construction_at(0, f_lat, f_lon)?])?,
    )
    .await?;
    let f_id = f.entities.get(&EntityIdx(0)).ok_or("missing f")?.id.clone();

    // Pin the geometry the case leans on before trusting the fold: A/B share a
    // tile, C/D share a distinct tile, E is a third, all inside the viewport;
    // F is outside but inside a viewport-overhang tile.
    let a_pt = GeoPoint::new(a_lat, a_lon)?;
    let b_pt = GeoPoint::new(b_lat, b_lon)?;
    let c_pt = GeoPoint::new(c_lat, c_lon)?;
    let d_pt = GeoPoint::new(d_lat, d_lon)?;
    let e_pt = GeoPoint::new(e_lat, e_lon)?;
    let f_pt = GeoPoint::new(f_lat, f_lon)?;
    let ranges = viewport_tiles(&viewport, level)?;
    let tile_of = |p: &GeoPoint| -> Option<usize> {
        let q = quadkey(p);
        ranges.iter().position(|r| r.lo <= q && q <= r.hi)
    };
    let t_ab = tile_of(&a_pt).ok_or("A must fall in a viewport tile")?;
    assert_eq!(Some(t_ab), tile_of(&b_pt), "A and B must share a tile");
    // A and B occupy distinct finest tiles, so their shared query-level tile
    // folds to a Cluster rather than a co-located group.
    assert_ne!(
        quadkey(&a_pt),
        quadkey(&b_pt),
        "A and B must occupy distinct finest tiles"
    );
    let t_cd = tile_of(&c_pt).ok_or("C must fall in a viewport tile")?;
    assert_eq!(Some(t_cd), tile_of(&d_pt), "C and D must share a tile");
    let t_e = tile_of(&e_pt).ok_or("E must fall in a viewport tile")?;
    assert!(
        t_ab != t_cd && t_ab != t_e && t_cd != t_e,
        "the three inside tiles must be distinct: A/B={t_ab}, C/D={t_cd}, E={t_e}"
    );
    let t_f = tile_of(&f_pt).ok_or("F must fall in a viewport-overhang tile")?;
    assert!(
        t_f != t_ab && t_f != t_cd && t_f != t_e,
        "F's overhang tile must be distinct from the inside tiles"
    );
    for p in [&a_pt, &b_pt, &c_pt, &d_pt, &e_pt] {
        assert!(
            viewport.contains(p),
            "the inside points must be in the viewport"
        );
    }
    // F sits outside the viewport, yet its overhang tile still yields a cell —
    // the fold is viewport-free.
    assert!(!viewport.contains(&f_pt), "F must sit outside the viewport");

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let mut got = view
        .cluster_entities_in_viewport(&viewport, level, RankKey::Unranked)
        .await
        .map_err(|e| format!("{e:?}"))?;
    got.sort_by(|x, y| x.representative.cmp(&y.representative));

    let mut want = vec![
        ClusterCell {
            representative: a_id.clone(),
            point: a_pt,
            // The fold derives the split level from the survivors' quadkeys; A/B
            // are the two survivors, so recompute it from their points here.
            kind: CellKind::Cluster {
                split_level: split_level(quadkey(&a_pt), quadkey(&b_pt)),
            },
        },
        ClusterCell {
            representative: cd_rep.clone(),
            point: c_pt,
            kind: CellKind::Singleton,
        },
        ClusterCell {
            representative: e_id.clone(),
            point: e_pt,
            kind: CellKind::Singleton,
        },
        ClusterCell {
            representative: f_id.clone(),
            point: f_pt,
            kind: CellKind::Singleton,
        },
    ];
    want.sort_by(|x, y| x.representative.cmp(&y.representative));

    assert_eq!(
        got, want,
        "clustering folds A+B into a Cluster cell at A ({a_id:?}, b={b_id:?}), the merged C/D \
         into a singleton at C (rep {cd_rep:?}), E ({e_id:?}) into a singleton, and F ({f_id:?}) \
         into its own singleton though outside the viewport; got {got:?}"
    );
    Ok(())
}

/// A clustering read pinned to an earlier snapshot ignores facts committed
/// after it. Entity A (one tile) exists below the snapshot; entity B (a
/// distinct tile) is committed after. Reading at the snapshot yields only A's
/// singleton cell — B adds neither its own cell nor a second representative.
pub async fn cluster_entities_in_viewport_respects_snapshot<S: FactStore>(store: S) -> TestResult {
    let viewport = Viewport::new(GeoPoint::new(40.0, -74.0)?, GeoPoint::new(40.1, -73.9)?)?;
    let level = QuadLevel::new(14)?;

    let (a_lat, a_lon) = (40.021, -73.981);
    let (b_lat, b_lon) = (40.061, -73.941);

    // A: committed below the snapshot.
    let a = commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![construction_at(0, a_lat, a_lon)?])?,
    )
    .await?;
    let a_id = a.entities.get(&EntityIdx(0)).ok_or("missing a")?.id.clone();

    let snapshot = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;

    // B: committed after the snapshot, in a distinct tile.
    let b = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![construction_at(0, b_lat, b_lon)?])?,
    )
    .await?;
    let b_id = b.entities.get(&EntityIdx(0)).ok_or("missing b")?.id.clone();

    // Preconditions: A and B sit in distinct viewport tiles, both inside.
    let a_pt = GeoPoint::new(a_lat, a_lon)?;
    let b_pt = GeoPoint::new(b_lat, b_lon)?;
    let ranges = viewport_tiles(&viewport, level)?;
    let tile_of = |p: &GeoPoint| -> Option<usize> {
        let q = quadkey(p);
        ranges.iter().position(|r| r.lo <= q && q <= r.hi)
    };
    let t_a = tile_of(&a_pt).ok_or("A must fall in a viewport tile")?;
    let t_b = tile_of(&b_pt).ok_or("B must fall in a viewport tile")?;
    assert_ne!(t_a, t_b, "A and B must occupy distinct tiles");
    assert!(viewport.contains(&a_pt) && viewport.contains(&b_pt));

    // At the snapshot: only A's singleton cell; B is invisible.
    let mut pinned = store
        .no_later_than(snapshot)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let got = pinned
        .cluster_entities_in_viewport(&viewport, level, RankKey::Unranked)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        got,
        vec![ClusterCell {
            representative: a_id.clone(),
            point: a_pt,
            kind: CellKind::Singleton,
        }],
        "pinned read must see only A ({a_id:?}), never post-snapshot B ({b_id:?}); got {got:?}"
    );

    // At head: both A and B appear as singleton cells.
    let mut head = store.now().await.map_err(|e| format!("{e:?}"))?;
    let mut head_cells = head
        .cluster_entities_in_viewport(&viewport, level, RankKey::Unranked)
        .await
        .map_err(|e| format!("{e:?}"))?;
    head_cells.sort_by(|x, y| x.representative.cmp(&y.representative));
    let mut want_head = vec![
        ClusterCell {
            representative: a_id.clone(),
            point: a_pt,
            kind: CellKind::Singleton,
        },
        ClusterCell {
            representative: b_id.clone(),
            point: b_pt,
            kind: CellKind::Singleton,
        },
    ];
    want_head.sort_by(|x, y| x.representative.cmp(&y.representative));
    assert_eq!(
        head_cells, want_head,
        "at head both A and B appear as singletons; got {head_cells:?}"
    );
    Ok(())
}

/// Two distinct entities constructed at one point share a finest (level-24)
/// tile, so their tile folds to a co-located cell rather than a splittable
/// cluster — zooming can't separate them. The cell carries both entity ids as
/// sorted members, and its representative is the `(quadkey, fact_id)`-minimal
/// survivor: with the quadkey shared, the earlier commit's lower fact id breaks
/// the tie.
pub async fn cluster_entities_in_viewport_groups_colocated_entities<S: FactStore>(
    store: S,
) -> TestResult {
    let viewport = Viewport::new(GeoPoint::new(40.0, -74.0)?, GeoPoint::new(40.1, -73.9)?)?;
    let level = QuadLevel::new(14)?;

    let (p_lat, p_lon) = (40.021, -73.981);

    // A and B: distinct entities pinned to the same point.
    let a = commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![construction_at(0, p_lat, p_lon)?])?,
    )
    .await?;
    let a_id = a.entities.get(&EntityIdx(0)).ok_or("missing a")?.id.clone();
    let b = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![construction_at(0, p_lat, p_lon)?])?,
    )
    .await?;
    let b_id = b.entities.get(&EntityIdx(0)).ok_or("missing b")?.id.clone();

    // Preconditions: two distinct entities on the one in-viewport point.
    let p_pt = GeoPoint::new(p_lat, p_lon)?;
    assert_ne!(a_id, b_id, "A and B must be distinct entities");
    assert!(
        viewport.contains(&p_pt),
        "the shared point must be in the viewport"
    );

    // The fold sorts members by id (BTreeSet order); the representative is A,
    // whose earlier commit gives the lower fact id under the shared quadkey.
    let mut members = vec![a_id.clone(), b_id.clone()];
    members.sort();

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let got = view
        .cluster_entities_in_viewport(&viewport, level, RankKey::Unranked)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        got,
        vec![ClusterCell {
            representative: a_id.clone(),
            point: p_pt,
            kind: CellKind::Colocated {
                members: members.clone()
            },
        }],
        "A ({a_id:?}) and B ({b_id:?}) at one point fold to a single Colocated cell carrying \
         sorted members {members:?}; got {got:?}"
    );
    Ok(())
}

/// A `Cluster` cell collapses to a `Singleton` when one of its two members is
/// retracted. A and B share a query-level tile but sit in distinct finest tiles
/// (a cluster); retracting B's placing fact leaves A alone, and the tile folds
/// to a singleton at A.
pub async fn cluster_cluster_becomes_singleton_when_a_member_is_retracted<S: FactStore>(
    store: S,
) -> TestResult {
    let viewport = Viewport::new(GeoPoint::new(40.0, -74.0)?, GeoPoint::new(40.1, -73.9)?)?;
    let level = QuadLevel::new(14)?;

    let (a_lat, a_lon) = (40.021, -73.981);
    let (b_lat, b_lon) = (40.020, -73.980);

    let a = commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![construction_at(0, a_lat, a_lon)?])?,
    )
    .await?;
    let a_id = a.entities.get(&EntityIdx(0)).ok_or("missing a")?.id.clone();
    let b = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![construction_at(0, b_lat, b_lon)?])?,
    )
    .await?;
    let b_id = b.entities.get(&EntityIdx(0)).ok_or("missing b")?.id.clone();
    let b_fact = *b.fact_ids.first().ok_or("missing b construction fact id")?;

    // Preconditions: A and B share a query-level tile but distinct finest
    // tiles, both inside the viewport → a cluster before retraction.
    let a_pt = GeoPoint::new(a_lat, a_lon)?;
    let b_pt = GeoPoint::new(b_lat, b_lon)?;
    assert_ne!(a_id, b_id, "A and B must be distinct entities");
    assert_ne!(
        quadkey(&a_pt),
        quadkey(&b_pt),
        "A and B must occupy distinct finest tiles"
    );
    let ranges = viewport_tiles(&viewport, level)?;
    let tile_of = |p: &GeoPoint| -> Option<usize> {
        let q = quadkey(p);
        ranges.iter().position(|r| r.lo <= q && q <= r.hi)
    };
    let t_a = tile_of(&a_pt).ok_or("A must fall in a viewport tile")?;
    assert_eq!(
        Some(t_a),
        tile_of(&b_pt),
        "A and B must share a query-level tile"
    );

    // Before retraction: one Cluster cell at A.
    {
        let mut before = store.now().await.map_err(|e| format!("{e:?}"))?;
        let cells_before = before
            .cluster_entities_in_viewport(&viewport, level, RankKey::Unranked)
            .await
            .map_err(|e| format!("{e:?}"))?;
        assert_eq!(
            cells_before,
            vec![ClusterCell {
                representative: a_id.clone(),
                point: a_pt,
                kind: CellKind::Cluster {
                    split_level: split_level(quadkey(&a_pt), quadkey(&b_pt)),
                },
            }],
            "before retraction A ({a_id:?}) and B ({b_id:?}) form one Cluster cell; got \
             {cells_before:?}"
        );
    }

    commit_retract(&store, b_fact, 20).await?;

    // After retraction: B is gone, the tile folds to a singleton at A.
    let mut after = store.now().await.map_err(|e| format!("{e:?}"))?;
    let cells_after = after
        .cluster_entities_in_viewport(&viewport, level, RankKey::Unranked)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        cells_after,
        vec![ClusterCell {
            representative: a_id.clone(),
            point: a_pt,
            kind: CellKind::Singleton,
        }],
        "retracting B ({b_id:?}) leaves A ({a_id:?}) as a singleton; got {cells_after:?}"
    );
    Ok(())
}

/// A `Colocated` cell collapses to a `Singleton` when one of its two members is
/// retracted. A and B share one point (a co-located group); retracting B's
/// placing fact leaves A alone, and the tile folds to a singleton at A.
pub async fn cluster_colocated_becomes_singleton_when_a_member_is_retracted<S: FactStore>(
    store: S,
) -> TestResult {
    let viewport = Viewport::new(GeoPoint::new(40.0, -74.0)?, GeoPoint::new(40.1, -73.9)?)?;
    let level = QuadLevel::new(14)?;

    let (p_lat, p_lon) = (40.021, -73.981);

    let a = commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![construction_at(0, p_lat, p_lon)?])?,
    )
    .await?;
    let a_id = a.entities.get(&EntityIdx(0)).ok_or("missing a")?.id.clone();
    let b = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![construction_at(0, p_lat, p_lon)?])?,
    )
    .await?;
    let b_id = b.entities.get(&EntityIdx(0)).ok_or("missing b")?.id.clone();
    let b_fact = *b.fact_ids.first().ok_or("missing b construction fact id")?;

    // Preconditions: two distinct entities on the one in-viewport point → a
    // co-located group before retraction.
    let p_pt = GeoPoint::new(p_lat, p_lon)?;
    assert_ne!(a_id, b_id, "A and B must be distinct entities");
    assert!(
        viewport.contains(&p_pt),
        "the shared point must be in the viewport"
    );

    // Before retraction: one Colocated cell carrying both members.
    {
        let mut members = vec![a_id.clone(), b_id.clone()];
        members.sort();
        let mut before = store.now().await.map_err(|e| format!("{e:?}"))?;
        let cells_before = before
            .cluster_entities_in_viewport(&viewport, level, RankKey::Unranked)
            .await
            .map_err(|e| format!("{e:?}"))?;
        assert_eq!(
            cells_before,
            vec![ClusterCell {
                representative: a_id.clone(),
                point: p_pt,
                kind: CellKind::Colocated { members },
            }],
            "before retraction A ({a_id:?}) and B ({b_id:?}) form one Colocated cell; got \
             {cells_before:?}"
        );
    }

    commit_retract(&store, b_fact, 20).await?;

    // After retraction: B is gone, the tile folds to a singleton at A.
    let mut after = store.now().await.map_err(|e| format!("{e:?}"))?;
    let cells_after = after
        .cluster_entities_in_viewport(&viewport, level, RankKey::Unranked)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        cells_after,
        vec![ClusterCell {
            representative: a_id.clone(),
            point: p_pt,
            kind: CellKind::Singleton,
        }],
        "retracting B ({b_id:?}) leaves A ({a_id:?}) as a singleton; got {cells_after:?}"
    );
    Ok(())
}

/// An entity positioned by a `MovedToLocation` event clusters under the entity,
/// through the event's `HasEvent` owner: the located fact's subject is the
/// *event*, so a read that folds located facts by their own subject attributes
/// the cell to nothing. A is built far outside the viewport and moved inside, so
/// the move is the only fact that can produce a cell here — and it must produce
/// one whose representative is A.
pub async fn cluster_entities_in_viewport_places_a_moved_entity_under_its_owner<S: FactStore>(
    store: S,
) -> TestResult {
    let viewport = Viewport::new(GeoPoint::new(40.0, -74.0)?, GeoPoint::new(40.1, -73.9)?)?;
    let level = QuadLevel::new(14)?;

    let (built_lat, built_lon) = (10.0, 10.0);
    let (moved_lat, moved_lon) = (40.021, -73.981);

    let a = commit_result(
        &store,
        local_bundle(
            1,
            1,
            0,
            0,
            vec![
                construction_at(0, built_lat, built_lon)?,
                has_event_fact(0, 0, moved_kind())?,
                moved_to(0, moved_lat, moved_lon)?,
            ],
        )?,
    )
    .await?;
    let a_id = a.entities.get(&EntityIdx(0)).ok_or("missing a")?.id.clone();

    // Preconditions: the construction sits outside every tile the viewport
    // spans, the move inside one of them — so the move alone decides the answer.
    let built_pt = GeoPoint::new(built_lat, built_lon)?;
    let moved_pt = GeoPoint::new(moved_lat, moved_lon)?;
    let ranges = viewport_tiles(&viewport, level)?;
    let in_viewport_tile = |p: &GeoPoint| {
        let q = quadkey(p);
        ranges.iter().any(|r| r.lo <= q && q <= r.hi)
    };
    assert!(
        !in_viewport_tile(&built_pt),
        "A's construction must fall outside the viewport's tiles"
    );
    assert!(
        in_viewport_tile(&moved_pt),
        "A's move must land inside a viewport tile"
    );

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let got = view
        .cluster_entities_in_viewport(&viewport, level, RankKey::Unranked)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        got,
        vec![ClusterCell {
            representative: a_id.clone(),
            point: moved_pt,
            kind: CellKind::Singleton,
        }],
        "the move must place A ({a_id:?}) at the moved-to point via its HasEvent owner; got {got:?}"
    );
    Ok(())
}

/// A viewport spanning more than [`CLUSTER_TILE_CAP`] tiles at the requested
/// level is refused — the level is too fine for the span, and the caller needs
/// to hear that rather than read an empty answer as "nothing is here". The same
/// entity and viewport cluster fine one level coarser, so the refusal is the
/// cap's doing and not a clustering read that fails on everything.
pub async fn cluster_entities_in_viewport_refuses_a_level_too_fine_for_the_span<
    S: FactStore + RefusalKinds,
>(
    store: S,
) -> TestResult {
    let viewport = Viewport::new(GeoPoint::new(40.0, -74.0)?, GeoPoint::new(40.1, -73.9)?)?;
    let too_fine = QuadLevel::new(17)?;
    let within_cap = QuadLevel::new(14)?;

    let (a_lat, a_lon) = (40.021, -73.981);
    let a = commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![construction_at(0, a_lat, a_lon)?])?,
    )
    .await?;
    let a_id = a.entities.get(&EntityIdx(0)).ok_or("missing a")?.id.clone();

    // The refusal has to come from the clustering cap, not the shared
    // `viewport_tiles` guard above it: enumerating the tiles succeeds, and there
    // are more of them than one clustering read may enumerate.
    let spanned = viewport_tiles(&viewport, too_fine)?.len();
    assert!(
        spanned > CLUSTER_TILE_CAP,
        "the level must span more tiles ({spanned}) than the clustering cap \
         ({CLUSTER_TILE_CAP}) for this case to test the cap"
    );
    assert!(
        viewport_tiles(&viewport, within_cap)?.len() <= CLUSTER_TILE_CAP,
        "the coarser level must stay inside the cap for its read to answer"
    );

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let refused = view
        .cluster_entities_in_viewport(&viewport, too_fine, RankKey::Unranked)
        .await;
    let Err(error) = refused else {
        return Err(format!(
            "a viewport spanning {spanned} tiles at level {} must be refused, never answered with \
             a cell list; got {refused:?}",
            too_fine.get()
        )
        .into());
    };
    assert!(
        S::is_cluster_tile_cap_refusal(&error),
        "the refusal must be the tile-cap refusal, not some other backend failure; got {error:?}"
    );

    // The same viewport at a level inside the cap answers with A's cell: the
    // refusal above is the cap talking, and the entity is really there to find.
    let a_pt = GeoPoint::new(a_lat, a_lon)?;
    let got = view
        .cluster_entities_in_viewport(&viewport, within_cap, RankKey::Unranked)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        got,
        vec![ClusterCell {
            representative: a_id.clone(),
            point: a_pt,
            kind: CellKind::Singleton,
        }],
        "at a level inside the cap the same viewport must cluster A ({a_id:?}); got {got:?}"
    );
    Ok(())
}

/// A capture location never spends an entity's share of a tile's candidate
/// budget. A full budget of them sorts ahead of the one entity sharing their
/// tile, so a read that let image subjects into the candidate cut would spend
/// the whole budget on them, drop the entity, and answer with nothing there.
pub async fn cluster_entities_in_viewport_excludes_image_capture_locations<S: FactStore>(
    store: S,
) -> TestResult {
    let viewport = Viewport::new(GeoPoint::new(40.0, -74.0)?, GeoPoint::new(40.1, -73.9)?)?;
    let level = QuadLevel::new(14)?;

    let (image_lat, image_lon) = (40.021, -73.981);
    let (entity_lat, entity_lon) = (40.020, -73.980);

    // A whole tile budget of capture locations, committed first so their fact
    // ids sort first too.
    let crowd = (0..CLUSTER_TILE_N)
        .map(|idx| captured_location_at(idx, image_lat, image_lon, 0.0))
        .collect::<Result<Vec<_>, TestError>>()?;
    commit_result(&store, local_bundle(0, 0, CLUSTER_TILE_N, 0, crowd)?).await?;

    let a = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            20,
            vec![construction_at(0, entity_lat, entity_lon)?],
        )?,
    )
    .await?;
    let a_id = a.entities.get(&EntityIdx(0)).ok_or("missing a")?.id.clone();

    // Preconditions: the crowd shares the entity's tile and sorts ahead of it,
    // so it would exhaust the budget.
    let image_pt = GeoPoint::new(image_lat, image_lon)?;
    let entity_pt = GeoPoint::new(entity_lat, entity_lon)?;
    let ranges = viewport_tiles(&viewport, level)?;
    let tile_of = |p: &GeoPoint| -> Option<usize> {
        let q = quadkey(p);
        ranges.iter().position(|r| r.lo <= q && q <= r.hi)
    };
    let entity_tile = tile_of(&entity_pt).ok_or("the entity must fall in a viewport tile")?;
    assert_eq!(
        Some(entity_tile),
        tile_of(&image_pt),
        "the capture crowd must share the entity's tile"
    );
    assert!(
        quadkey(&image_pt) < quadkey(&entity_pt),
        "the capture crowd must sort ahead of the entity to contest its budget"
    );

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let got = view
        .cluster_entities_in_viewport(&viewport, level, RankKey::Unranked)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        got,
        vec![ClusterCell {
            representative: a_id.clone(),
            point: entity_pt,
            kind: CellKind::Singleton,
        }],
        "capture locations must not crowd out A ({a_id:?}); got {got:?}"
    );
    Ok(())
}
