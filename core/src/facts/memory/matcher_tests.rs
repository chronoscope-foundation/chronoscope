//! Matcher scenarios through `submit_commit`: a match mints fresh and
//! asserts identity as a machine-authored judgment in a companion commit;
//! no-match and ambiguity mint with no edge.

use std::collections::BTreeSet;

use super::tests::{
    TestBundle, TestResult, commit_result, construction_location_in, construction_started_fact,
    construction_started_in, demolition_completed_in, designated_kind, event_point_date_fact,
    external_reference_fact, fixed_time, has_event_fact, image_source_fact, local_bundle,
    name_fact, name_fact_in_language, retract_commit_fact, same_entity_fact, user_author,
};
use super::{MemoryEntityId, MemoryEventId, MemoryFactStore, MemoryImageId};
use crate::facts::assertions::{FactualAssertion, JudgmentAssertion};
use crate::facts::attribute;
use crate::facts::citations::JudgmentSource;
use crate::facts::identity;
use crate::facts::ids::{CommitId, FactId};
use crate::facts::schema::EquivClass;
use crate::facts::store::{EntityView, FactStore, FactView, ImageView};
use crate::facts::submit::result::{StoredFactualFact, StoredJudgmentFact};
use crate::facts::submit::{
    Commit as SubmitBundle, CommitAuthor, Decl, EntityIdx, EventIdx, FactLookup, ImageIdx,
    ResolutionOrigin, StoredCommit, StoredFact, matcher,
};

// --- read helpers ---

/// The entity class of `member` at the store's latest snapshot.
async fn entity_class_now(
    store: &MemoryFactStore,
    member: MemoryEntityId,
) -> Result<EquivClass<MemoryEntityId>, Box<dyn std::error::Error>> {
    Ok(store.now().await?.entity_class(&member).await?)
}

/// The image class of `member` at the store's latest snapshot.
async fn image_class_now(
    store: &MemoryFactStore,
    member: MemoryImageId,
) -> Result<EquivClass<MemoryImageId>, Box<dyn std::error::Error>> {
    Ok(store.now().await?.image_class(&member).await?)
}

/// The stored commit record under `id`.
async fn stored_commit(
    store: &MemoryFactStore,
    id: &CommitId,
) -> Result<StoredCommit, Box<dyn std::error::Error>> {
    let inner = store.lock_inner().await;
    Ok(inner.commits.get(id).ok_or("commit not recorded")?.clone())
}

/// The single judgment fact of `commit_id`, unwrapped to its stored form.
async fn sole_judgment_of(
    store: &MemoryFactStore,
    commit_id: &CommitId,
) -> Result<
    StoredJudgmentFact<MemoryEntityId, MemoryEventId, MemoryImageId>,
    Box<dyn std::error::Error>,
> {
    let commit = stored_commit(store, commit_id).await?;
    if commit.fact_ids.len() != 1 {
        return Err(format!("expected one fact, got {}", commit.fact_ids.len()).into());
    }
    let fid = *commit.fact_ids.first().ok_or("missing fact id")?;
    let view = store.now().await?;
    let FactLookup::Active(stored) = view.fact(fid).await? else {
        return Err("judgment must be active".into());
    };
    let StoredFact::Judgment(judgment) = *stored else {
        return Err("expected a judgment fact".into());
    };
    Ok(judgment)
}

/// The fact id among `candidates` whose stored fact is an
/// `ExternalReference` attribute.
async fn external_ref_fact_id(
    store: &MemoryFactStore,
    candidates: &[FactId],
) -> Result<FactId, Box<dyn std::error::Error>> {
    let view = store.now().await?;
    for &fid in candidates {
        if let FactLookup::Active(stored) = view.fact(fid).await?
            && matches!(
                *stored,
                StoredFact::Factual(StoredFactualFact {
                    assertion: FactualAssertion::Attribute {
                        fact: attribute::Fact::ExternalReference { .. },
                    },
                    ..
                })
            )
        {
            return Ok(fid);
        }
    }
    Err("no external-reference fact among candidates".into())
}

// --- match scenarios ---

/// A later commit's `Local` decl with the same `(name, language)` mints a
/// fresh id and joins the existing entity's class. The matcher's verdict
/// lands as a `SameEntity` judgment in a machine-authored companion commit,
/// citing the matched name fact and the snapshot it judged at, timestamped
/// with the producer's `recorded_at`. The name compare is normalized (trim +
/// Unicode lowercase), so a case/whitespace variant still matches.
#[tokio::test]
async fn match_by_name_joins_existing_entity_class() -> TestResult {
    let store = MemoryFactStore::new();
    let first = commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![name_fact(0, "Pantheon")?])?,
    )
    .await?;
    let existing = first.entities.get(&EntityIdx(0)).ok_or("missing decl")?.id;
    let anchor = *first.fact_ids.first().ok_or("missing anchor fact")?;
    let snapshot = store.next_fact_id().await?;

    let second = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![name_fact(0, "  PANTHEON ")?])?,
    )
    .await?;
    let resolution = second.entities.get(&EntityIdx(0)).ok_or("missing decl")?;
    assert_eq!(
        resolution.origin,
        ResolutionOrigin::MatchedExisting { matched: existing }
    );
    let fresh = resolution.id;
    assert_ne!(fresh, existing, "a match still mints a fresh id");

    let class = entity_class_now(&store, fresh).await?;
    assert!(
        class.members.contains(&existing),
        "the fresh id must class with the matched entity; got {:?}",
        class.members
    );

    let companion_id = second
        .companion_commit_id
        .clone()
        .ok_or("expected a companion commit")?;
    let companion = stored_commit(&store, &companion_id).await?;
    let (process, version) = matcher::matcher_identity();
    assert_eq!(
        companion.author,
        CommitAuthor::Analyzer {
            process: process.clone(),
            version: version.clone(),
        }
    );
    assert_eq!(
        companion.recorded_at,
        fixed_time() + chrono::Duration::seconds(10),
        "the companion borrows the producer's timestamp"
    );

    let judgment = sole_judgment_of(&store, &companion_id).await?;
    assert_eq!(
        judgment.assertion,
        JudgmentAssertion::Identity {
            fact: identity::Fact::same_entity(fresh, existing)?,
        }
    );
    assert_eq!(
        judgment.source,
        JudgmentSource::Derivation {
            process,
            version,
            basis: [anchor].into_iter().collect(),
            snapshot,
        }
    );
    Ok(())
}

/// A mixed-case tag canonicalizes at construction, so two casings of one tag
/// form the same matcher name key: C1 mints under `("London", en-us → en-US)`
/// and C2's `Local` decl built from `EN-US` matches it, classing its fresh
/// mint with the existing entity.
#[tokio::test]
async fn mixed_case_language_tags_canonicalize_to_one_name_key() -> TestResult {
    let store = MemoryFactStore::new();
    let first = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            0,
            vec![name_fact_in_language(0, "London", "en-us")?],
        )?,
    )
    .await?;
    let existing = first.entities.get(&EntityIdx(0)).ok_or("missing decl")?.id;

    let second = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            10,
            vec![name_fact_in_language(0, "London", "EN-US")?],
        )?,
    )
    .await?;
    let resolution = second.entities.get(&EntityIdx(0)).ok_or("missing decl")?;
    assert_eq!(
        resolution.origin,
        ResolutionOrigin::MatchedExisting { matched: existing }
    );
    let class = entity_class_now(&store, resolution.id).await?;
    assert!(class.members.contains(&existing));
    Ok(())
}

/// Name text canonicalizes to NFC at construction, so the decomposed and
/// precomposed spellings of one name form the same matcher name key: C1 mints
/// under "Panthéon" spelled with a combining acute, C2's `Local` decl built
/// from the precomposed spelling matches it, classing its fresh mint with the
/// existing entity.
#[tokio::test]
async fn decomposed_and_precomposed_name_forms_match() -> TestResult {
    let store = MemoryFactStore::new();
    let first = commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![name_fact(0, "Panthe\u{0301}on")?])?,
    )
    .await?;
    let existing = first.entities.get(&EntityIdx(0)).ok_or("missing decl")?.id;

    let second = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![name_fact(0, "Panth\u{e9}on")?])?,
    )
    .await?;
    let resolution = second.entities.get(&EntityIdx(0)).ok_or("missing decl")?;
    assert_eq!(
        resolution.origin,
        ResolutionOrigin::MatchedExisting { matched: existing }
    );
    let class = entity_class_now(&store, resolution.id).await?;
    assert!(class.members.contains(&existing));
    Ok(())
}

/// A `Local` decl whose name matches no existing entity mints fresh, with no
/// companion commit.
#[tokio::test]
async fn different_name_mints_fresh_entity() -> TestResult {
    let store = MemoryFactStore::new();
    let first = commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![name_fact(0, "Pantheon")?])?,
    )
    .await?;
    let existing = first.entities.get(&EntityIdx(0)).ok_or("missing decl")?.id;

    let second = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![name_fact(0, "Colosseum")?])?,
    )
    .await?;
    let resolution = second.entities.get(&EntityIdx(0)).ok_or("missing decl")?;
    assert_eq!(resolution.origin, ResolutionOrigin::NewlyMinted);
    assert_ne!(resolution.id, existing);
    assert_eq!(
        second.companion_commit_id, None,
        "no match, so no companion commit"
    );
    Ok(())
}

/// A later commit's `Local` decl carrying the same `ExternalReference` mints
/// fresh and classes with the existing entity through the companion edge.
#[tokio::test]
async fn match_by_external_reference_joins_existing_entity_class() -> TestResult {
    let store = MemoryFactStore::new();
    let first = commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![external_reference_fact(0, 243)?])?,
    )
    .await?;
    let existing = first.entities.get(&EntityIdx(0)).ok_or("missing decl")?.id;

    let second = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![external_reference_fact(0, 243)?])?,
    )
    .await?;
    let resolution = second.entities.get(&EntityIdx(0)).ok_or("missing decl")?;
    assert_eq!(
        resolution.origin,
        ResolutionOrigin::MatchedExisting { matched: existing }
    );
    assert_ne!(resolution.id, existing);
    let class = entity_class_now(&store, resolution.id).await?;
    assert!(class.members.contains(&existing));
    assert!(second.companion_commit_id.is_some());
    Ok(())
}

/// A decl carrying both a matching external reference and a name that matches
/// a *different* entity resolves on the reference alone — one candidate, no
/// name-union and no spurious ambiguity. The fresh mint classes with the
/// reference-matched entity, and the companion's basis cites the
/// external-ref anchor, not a name fact.
#[tokio::test]
async fn external_reference_takes_precedence_over_name() -> TestResult {
    let store = MemoryFactStore::new();
    let by_ref = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            0,
            vec![external_reference_fact(0, 243)?, name_fact(0, "Pantheon")?],
        )?,
    )
    .await?;
    let ref_id = by_ref.entities.get(&EntityIdx(0)).ok_or("missing decl")?.id;
    let ref_anchor = external_ref_fact_id(&store, &by_ref.fact_ids).await?;
    let by_name = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![name_fact(0, "Basilica")?])?,
    )
    .await?;
    let name_id = by_name
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing decl")?
        .id;
    assert_ne!(ref_id, name_id);

    let third = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            20,
            vec![external_reference_fact(0, 243)?, name_fact(0, "Basilica")?],
        )?,
    )
    .await?;
    let resolution = third.entities.get(&EntityIdx(0)).ok_or("missing decl")?;
    assert_eq!(
        resolution.origin,
        ResolutionOrigin::MatchedExisting { matched: ref_id }
    );
    let class = entity_class_now(&store, resolution.id).await?;
    assert!(class.members.contains(&ref_id));
    assert!(
        !class.members.contains(&name_id),
        "the fresh id must not class with the name-hit entity"
    );

    let companion_id = third
        .companion_commit_id
        .clone()
        .ok_or("expected a companion commit")?;
    let judgment = sole_judgment_of(&store, &companion_id).await?;
    let JudgmentSource::Derivation { basis, .. } = judgment.source else {
        return Err(format!("expected a Derivation source, got {:?}", judgment.source).into());
    };
    let expected: BTreeSet<FactId> = [ref_anchor].into_iter().collect();
    assert_eq!(
        basis, expected,
        "the basis cites the external-ref anchor, not a name fact"
    );
    Ok(())
}

/// A `Local` decl with no name or external-reference anchor mints fresh, even
/// with other entities in the store.
#[tokio::test]
async fn anchorless_local_decl_mints_fresh() -> TestResult {
    let store = MemoryFactStore::new();
    commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![name_fact(0, "Pantheon")?])?,
    )
    .await?;

    let second = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![construction_started_fact(0)?])?,
    )
    .await?;
    let resolution = second.entities.get(&EntityIdx(0)).ok_or("missing decl")?;
    assert_eq!(resolution.origin, ResolutionOrigin::NewlyMinted);
    Ok(())
}

/// Two same-named entities minted in one commit (the matcher consults only
/// the view, so neither decl sees the other — there is no in-bundle
/// matching), then a later decl with that name mints fresh and reports both
/// as ambiguous candidates, excluding its own minted fallback id. No identity
/// is asserted for an ambiguous match, so there is no companion commit.
#[tokio::test]
async fn ambiguous_name_match_mints_with_candidates() -> TestResult {
    let store = MemoryFactStore::new();
    let first = commit_result(
        &store,
        local_bundle(
            2,
            0,
            0,
            0,
            vec![name_fact(0, "Pantheon")?, name_fact(1, "Pantheon")?],
        )?,
    )
    .await?;
    let a = first.entities.get(&EntityIdx(0)).ok_or("missing decl 0")?;
    let b = first.entities.get(&EntityIdx(1)).ok_or("missing decl 1")?;
    assert_eq!(a.origin, ResolutionOrigin::NewlyMinted);
    assert_eq!(b.origin, ResolutionOrigin::NewlyMinted);
    assert_ne!(a.id, b.id, "same-commit decls must not match each other");

    let second = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![name_fact(0, "Pantheon")?])?,
    )
    .await?;
    let resolution = second.entities.get(&EntityIdx(0)).ok_or("missing decl")?;
    let ResolutionOrigin::Ambiguous { candidates } = &resolution.origin else {
        return Err(format!("expected Ambiguous, got {:?}", resolution.origin).into());
    };
    let got: Vec<MemoryEntityId> = candidates.iter().copied().collect();
    let expected = {
        let mut v = vec![a.id, b.id];
        v.sort();
        v
    };
    assert_eq!(got, expected, "candidates are the two existing entities");
    assert_ne!(resolution.id, a.id);
    assert_ne!(resolution.id, b.id);
    assert!(
        !got.contains(&resolution.id),
        "the minted fallback id must not appear among the candidates"
    );
    assert_eq!(
        second.companion_commit_id, None,
        "an ambiguous match asserts no identity edge"
    );
    Ok(())
}

/// A later commit's `Local` image decl with the same source `url` mints fresh
/// and classes with the existing image; the companion carries the
/// `SameArtifact` judgment.
#[tokio::test]
async fn match_by_source_url_joins_existing_image_class() -> TestResult {
    let store = MemoryFactStore::new();
    let first = commit_result(
        &store,
        local_bundle(
            0,
            0,
            1,
            0,
            vec![image_source_fact(0, "https://example.com/photo.jpg")?],
        )?,
    )
    .await?;
    let existing = first.images.get(&ImageIdx(0)).ok_or("missing decl")?.id;

    let second = commit_result(
        &store,
        local_bundle(
            0,
            0,
            1,
            10,
            vec![image_source_fact(0, "https://example.com/photo.jpg")?],
        )?,
    )
    .await?;
    let resolution = second.images.get(&ImageIdx(0)).ok_or("missing decl")?;
    assert_eq!(
        resolution.origin,
        ResolutionOrigin::MatchedExisting { matched: existing }
    );
    assert_ne!(resolution.id, existing);
    let class = image_class_now(&store, resolution.id).await?;
    assert!(class.members.contains(&existing));

    let companion_id = second
        .companion_commit_id
        .clone()
        .ok_or("expected a companion commit")?;
    let judgment = sole_judgment_of(&store, &companion_id).await?;
    assert_eq!(
        judgment.assertion,
        JudgmentAssertion::Identity {
            fact: identity::Fact::same_artifact(resolution.id, existing)?,
        }
    );
    Ok(())
}

/// A `Local` image decl with a different source `url` mints fresh.
#[tokio::test]
async fn different_source_url_mints_fresh_image() -> TestResult {
    let store = MemoryFactStore::new();
    let first = commit_result(
        &store,
        local_bundle(
            0,
            0,
            1,
            0,
            vec![image_source_fact(0, "https://example.com/photo.jpg")?],
        )?,
    )
    .await?;
    let existing = first.images.get(&ImageIdx(0)).ok_or("missing decl")?.id;

    let second = commit_result(
        &store,
        local_bundle(
            0,
            0,
            1,
            10,
            vec![image_source_fact(0, "https://example.com/other.jpg")?],
        )?,
    )
    .await?;
    let resolution = second.images.get(&ImageIdx(0)).ok_or("missing decl")?;
    assert_eq!(resolution.origin, ResolutionOrigin::NewlyMinted);
    assert_ne!(resolution.id, existing);
    Ok(())
}

/// Same-shaped event facts in two commits each mint their own event — events
/// have no matcher.
#[tokio::test]
async fn events_always_mint() -> TestResult {
    let store = MemoryFactStore::new();
    let first = commit_result(
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
    .await?;
    let a = first.events.get(&EventIdx(0)).ok_or("missing decl")?;
    assert_eq!(a.origin, ResolutionOrigin::NewlyMinted);

    let second = commit_result(
        &store,
        local_bundle(
            1,
            1,
            0,
            10,
            vec![
                has_event_fact(0, 0, designated_kind())?,
                event_point_date_fact(0)?,
            ],
        )?,
    )
    .await?;
    let b = second.events.get(&EventIdx(0)).ok_or("missing decl")?;
    assert_eq!(b.origin, ResolutionOrigin::NewlyMinted);
    assert_ne!(a.id, b.id);
    Ok(())
}

/// A name hit on a non-representative `SameEntity` member reports the class
/// representative as the matched subject — the match canonicalises across
/// the equivalence class — and the fresh mint grows the class to three.
#[tokio::test]
async fn match_canonicalises_to_class_representative() -> TestResult {
    let store = MemoryFactStore::new();
    let first = commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![name_fact(0, "Pantheon")?])?,
    )
    .await?;
    let a = first.entities.get(&EntityIdx(0)).ok_or("missing decl")?.id;

    // Mint B under a different name and link it to A.
    let second_bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![Decl::Existing { id: a }, Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(1, "Rotonda")?, same_entity_fact(0, 1)?]
            .into_iter()
            .collect(),
    };
    let second = commit_result(&store, second_bundle).await?;
    let b = second.entities.get(&EntityIdx(1)).ok_or("missing decl")?.id;
    assert_ne!(a, b);

    // "Rotonda" lands on B, the non-representative member; the matched
    // subject is the class representative.
    let third = commit_result(
        &store,
        local_bundle(1, 0, 0, 20, vec![name_fact(0, "Rotonda")?])?,
    )
    .await?;
    let resolution = third.entities.get(&EntityIdx(0)).ok_or("missing decl")?;
    assert_eq!(
        resolution.origin,
        ResolutionOrigin::MatchedExisting { matched: a.min(b) }
    );
    let class = entity_class_now(&store, resolution.id).await?;
    let expected: BTreeSet<MemoryEntityId> = [a, b, resolution.id].into_iter().collect();
    assert_eq!(
        class.members, expected,
        "the fresh mint grows the class to three members"
    );
    Ok(())
}

/// The exact-name matcher cannot tell Pantheon-in-Rome from
/// Pantheon-in-Paris: the second decl still classes with the first entity
/// despite conflicting construction evidence. A known over-match degradation,
/// now soft — the collapse is one companion-commit edge, repairable by a
/// single retraction.
/// [`pantheon_in_rome_and_paris_resolve_to_distinct_entities`] pins the
/// intended behavior, this pins the current one so an unannounced change is
/// caught.
#[tokio::test]
async fn exact_name_matcher_collapses_distinct_namesakes() -> TestResult {
    let store = MemoryFactStore::new();
    let rome = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            0,
            vec![name_fact(0, "Pantheon")?, construction_started_in(0, 113)?],
        )?,
    )
    .await?;
    let rome_id = rome.entities.get(&EntityIdx(0)).ok_or("missing decl")?.id;

    let paris = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            10,
            vec![name_fact(0, "Pantheon")?, construction_started_in(0, 1758)?],
        )?,
    )
    .await?;
    let resolution = paris.entities.get(&EntityIdx(0)).ok_or("missing decl")?;
    assert_eq!(
        resolution.origin,
        ResolutionOrigin::MatchedExisting { matched: rome_id }
    );
    assert_ne!(resolution.id, rome_id);
    let class = entity_class_now(&store, rome_id).await?;
    assert!(
        class.members.contains(&resolution.id),
        "the namesakes collapse into one class"
    );
    Ok(())
}

/// Two `Local` decls that both match one existing entity land cleanly: each
/// mints its own fresh id, the producer's `SameEntity` pair links the two
/// mints, and the companion's edges link both to the existing entity — one
/// three-member class, no duplicate-decl or self-equivalence rejection.
#[tokio::test]
async fn two_local_decls_matching_one_entity_join_its_class() -> TestResult {
    let store = MemoryFactStore::new();
    let first = commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![name_fact(0, "Pantheon")?])?,
    )
    .await?;
    let existing = first.entities.get(&EntityIdx(0)).ok_or("missing decl")?.id;

    let bundle = local_bundle(
        2,
        0,
        0,
        10,
        vec![
            name_fact(0, "Pantheon")?,
            name_fact(1, "Pantheon")?,
            same_entity_fact(0, 1)?,
        ],
    )?;
    let second = commit_result(&store, bundle).await?;
    let a = second.entities.get(&EntityIdx(0)).ok_or("missing decl 0")?;
    let b = second.entities.get(&EntityIdx(1)).ok_or("missing decl 1")?;
    assert_eq!(
        a.origin,
        ResolutionOrigin::MatchedExisting { matched: existing }
    );
    assert_eq!(
        b.origin,
        ResolutionOrigin::MatchedExisting { matched: existing }
    );
    assert_ne!(a.id, b.id, "each matched decl mints its own fresh id");

    let companion_id = second
        .companion_commit_id
        .clone()
        .ok_or("expected a companion commit")?;
    let companion = stored_commit(&store, &companion_id).await?;
    assert_eq!(
        companion.fact_ids.len(),
        2,
        "one identity judgment per matched decl"
    );

    let class = entity_class_now(&store, existing).await?;
    let expected: BTreeSet<MemoryEntityId> = [existing, a.id, b.id].into_iter().collect();
    assert_eq!(
        class.members, expected,
        "both mints class with the matched entity"
    );
    Ok(())
}

/// Re-submitting a matched bundle is idempotent end to end: the cached
/// result reports the same companion commit and the store mints no new
/// facts.
#[tokio::test]
async fn resubmitted_match_reports_same_companion_without_new_facts() -> TestResult {
    let store = MemoryFactStore::new();
    commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![name_fact(0, "Pantheon")?])?,
    )
    .await?;

    let first = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![name_fact(0, "Pantheon")?])?,
    )
    .await?;
    assert!(first.companion_commit_id.is_some());
    let watermark = store.next_fact_id().await?;

    let second = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![name_fact(0, "Pantheon")?])?,
    )
    .await?;
    assert!(second.previously_committed);
    assert_eq!(second.commit_id, first.commit_id);
    assert_eq!(second.companion_commit_id, first.companion_commit_id);
    assert_eq!(
        store.next_fact_id().await?,
        watermark,
        "a re-submission mints nothing"
    );
    Ok(())
}

/// Retracting the companion commit dissolves the matcher's identity edge:
/// the matched pair reads as one class before the retraction and as
/// singleton classes after. The one-retraction repair for a bad match.
#[tokio::test]
async fn retracting_companion_commit_dissolves_the_class() -> TestResult {
    let store = MemoryFactStore::new();
    let first = commit_result(
        &store,
        local_bundle(1, 0, 0, 0, vec![name_fact(0, "Pantheon")?])?,
    )
    .await?;
    let existing = first.entities.get(&EntityIdx(0)).ok_or("missing decl")?.id;

    let second = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![name_fact(0, "Pantheon")?])?,
    )
    .await?;
    let fresh = second.entities.get(&EntityIdx(0)).ok_or("missing decl")?.id;
    let companion_id = second
        .companion_commit_id
        .clone()
        .ok_or("expected a companion commit")?;
    let linked_at = store.next_fact_id().await?;

    let retraction: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(20),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_commit_fact(companion_id)?].into_iter().collect(),
    };
    commit_result(&store, retraction).await?;

    let before = store.no_later_than(linked_at).entity_class(&fresh).await?;
    assert!(
        before.members.contains(&existing),
        "before the retraction the pair shares a class"
    );

    let after = entity_class_now(&store, fresh).await?;
    let fresh_only: BTreeSet<MemoryEntityId> = [fresh].into_iter().collect();
    assert_eq!(
        after.members, fresh_only,
        "the retraction dissolves the edge"
    );
    let existing_class = entity_class_now(&store, existing).await?;
    let existing_only: BTreeSet<MemoryEntityId> = [existing].into_iter().collect();
    assert_eq!(existing_class.members, existing_only);
    Ok(())
}

// --- matcher forcing functions ---
//
// These assert the desired disambiguating behavior — same-named entities
// with conflicting evidence resolve distinctly — and stay ignored while the
// exact-match matcher collapses them. Un-ignoring them is the acceptance
// check for the disambiguating matcher; each ignore message names the
// signal it must weigh.

/// Same name, conflicting location evidence: the Roman and Parisian Pantheon
/// must resolve to distinct classes.
#[tokio::test]
#[ignore = "tier-7: matcher disambiguates by location"]
async fn pantheon_in_rome_and_paris_resolve_to_distinct_entities() -> TestResult {
    let store = MemoryFactStore::new();
    let rome = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            0,
            vec![
                name_fact(0, "Pantheon")?,
                construction_location_in(0, "Rome")?,
            ],
        )?,
    )
    .await?;
    let rome_id = rome.entities.get(&EntityIdx(0)).ok_or("missing decl")?.id;

    let paris = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            10,
            vec![
                name_fact(0, "Pantheon")?,
                construction_location_in(0, "Paris")?,
            ],
        )?,
    )
    .await?;
    let resolution = paris.entities.get(&EntityIdx(0)).ok_or("missing decl")?;
    assert_eq!(
        resolution.origin,
        ResolutionOrigin::NewlyMinted,
        "conflicting location evidence must yield a fresh entity"
    );
    let class = entity_class_now(&store, rome_id).await?;
    assert!(
        !class.members.contains(&resolution.id),
        "conflicting location evidence must keep the namesakes in distinct classes"
    );
    Ok(())
}

/// Same name, disjoint construction-era evidence: a nineteenth- and a
/// twentieth-century namesake must resolve to distinct classes.
#[tokio::test]
#[ignore = "tier-7: matcher disambiguates by date evidence"]
async fn nineteenth_and_twentieth_century_namesakes_resolve_distinctly() -> TestResult {
    let store = MemoryFactStore::new();
    let older = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            0,
            vec![
                name_fact(0, "Grand Hotel")?,
                construction_started_in(0, 1850)?,
            ],
        )?,
    )
    .await?;
    let older_id = older.entities.get(&EntityIdx(0)).ok_or("missing decl")?.id;

    let newer = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            10,
            vec![
                name_fact(0, "Grand Hotel")?,
                construction_started_in(0, 1950)?,
            ],
        )?,
    )
    .await?;
    let resolution = newer.entities.get(&EntityIdx(0)).ok_or("missing decl")?;
    assert_eq!(
        resolution.origin,
        ResolutionOrigin::NewlyMinted,
        "disjoint construction-era evidence must yield a fresh entity"
    );
    let class = entity_class_now(&store, older_id).await?;
    assert!(
        !class.members.contains(&resolution.id),
        "disjoint construction-era evidence must keep the namesakes in distinct classes"
    );
    Ok(())
}

/// Same name, one demolished in 1904 and one with no demolition bookend: the
/// destroyed and the still-standing namesake must resolve to distinct
/// classes.
#[tokio::test]
#[ignore = "tier-7: matcher disambiguates by lifecycle bookends"]
async fn destroyed_and_still_standing_namesakes_resolve_distinctly() -> TestResult {
    let store = MemoryFactStore::new();
    let destroyed = commit_result(
        &store,
        local_bundle(
            1,
            0,
            0,
            0,
            vec![
                name_fact(0, "Grand Hotel")?,
                demolition_completed_in(0, 1904)?,
            ],
        )?,
    )
    .await?;
    let destroyed_id = destroyed
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing decl")?
        .id;

    let standing = commit_result(
        &store,
        local_bundle(1, 0, 0, 10, vec![name_fact(0, "Grand Hotel")?])?,
    )
    .await?;
    let resolution = standing.entities.get(&EntityIdx(0)).ok_or("missing decl")?;
    assert_eq!(
        resolution.origin,
        ResolutionOrigin::NewlyMinted,
        "a demolition bookend on the prior namesake must yield a fresh entity"
    );
    let class = entity_class_now(&store, destroyed_id).await?;
    assert!(
        !class.members.contains(&resolution.id),
        "a demolition bookend on the prior namesake must keep the pair in distinct classes"
    );
    Ok(())
}
