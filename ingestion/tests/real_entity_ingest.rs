//! Real-entity round trip: ingest the curated Wikidata set into the fact store
//! and assert the read side — bbox viewport listing over the projection —
//! reflects it.
//!
//! `WIKIDATA_ENTITIES_JSONL` points at the fetched `entities.jsonl` (the
//! `wikidata-curated-entities` FOD). The hermetic test / llvm-cov checks and the
//! api/web dev shells set it via `apiRuntimeEnv`, so this runs automatically in
//! `just check`. Unset — e.g. a bare `cargo test` outside the nix shell — the
//! tests skip.
//!
//! Assertions are grounded in the pinned 2022-01-03 snapshot: the Colosseum is
//! antiquity, the Eiffel Tower is an 1880s build, and demolished-and-rebuilt
//! Chioggia Cathedral splits into two dated entities. Re-pinning the FOD is a
//! deliberate act that updates these like a golden.

use chrono::{Datelike, TimeZone, Utc};
use chronoscope_core::facts::ids::IngesterRunId;
use chronoscope_core::facts::listing::{EntitySummary, summaries_in_bbox};
use chronoscope_core::facts::memory::{MemoryEntityId, MemoryFactStore, MemoryImageId};
use chronoscope_core::facts::store::FactStore;
use chronoscope_core::facts::submit::commit_facts;
use chronoscope_core::geo::{Bbox, GeoPoint};
use chronoscope_ingestion::wikidata::commits::build_commit;
use chronoscope_integrations::wikidata::WikidataEntity;
use std::num::NonZeroUsize;

type BoxError = Box<dyn std::error::Error>;

/// Ingest every curated entity into a fresh store, asserting each builds and
/// submits cleanly. `None` when `WIKIDATA_ENTITIES_JSONL` is unset (the caller
/// then skips).
async fn ingest_curated() -> Result<Option<MemoryFactStore>, BoxError> {
    let Ok(path) = std::env::var("WIKIDATA_ENTITIES_JSONL") else {
        eprintln!("WIKIDATA_ENTITIES_JSONL unset — skipping real-entity round trip");
        return Ok(None);
    };
    let content = std::fs::read_to_string(&path)?;
    let run = IngesterRunId::new("round-trip");
    let recorded_at = Utc
        .with_ymd_and_hms(2022, 1, 3, 0, 0, 0)
        .single()
        .ok_or("unambiguous timestamp")?;
    let store = MemoryFactStore::new();

    let mut committed = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for (i, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entity: WikidataEntity =
            serde_json::from_str(line).map_err(|e| format!("line {i}: {e}"))?;
        let qid = entity.id.as_str().to_owned();
        match build_commit(&entity, &run, recorded_at, &mut Vec::new()) {
            Ok(None) => {}
            Ok(Some(commit)) => match commit_facts(&store, commit).await {
                Ok(_) => committed += 1,
                Err(e) => failures.push(format!("{qid}: submit: {e:?}")),
            },
            Err(e) => failures.push(format!("{qid}: build: {e}")),
        }
    }
    assert!(
        failures.is_empty(),
        "{} curated entities failed to ingest:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(
        committed > 0,
        "expected to ingest at least one curated entity"
    );
    Ok(Some(store))
}

/// The placeable entity summaries whose current marker lands in the box spanned
/// by `sw`..`ne`. Asserts the spatial filter's contract: every surfaced summary
/// really is inside the query box.
async fn placeable_in(
    store: &MemoryFactStore,
    sw: (f64, f64),
    ne: (f64, f64),
) -> Result<Vec<EntitySummary<MemoryEntityId, MemoryImageId>>, BoxError> {
    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let bbox = Bbox::new(GeoPoint::new(sw.0, sw.1)?, GeoPoint::new(ne.0, ne.1)?)?;
    let limit = NonZeroUsize::new(64).ok_or("nonzero limit")?;
    let page = summaries_in_bbox::<MemoryFactStore, _>(&view, &bbox, None, limit)
        .await
        .map_err(|e| format!("{e:?}"))?;
    for s in &page.summaries {
        assert!(
            bbox.contains(&s.point),
            "summary {:?} surfaced outside the query box at {:?}",
            s.id,
            s.point
        );
    }
    Ok(page.summaries)
}

fn has_name(s: &EntitySummary<MemoryEntityId, MemoryImageId>, name: &str) -> bool {
    s.names.iter().any(|n| n.text == name)
}

#[tokio::test]
async fn every_curated_entity_builds_and_submits() -> Result<(), BoxError> {
    // `ingest_curated` asserts each entity builds and submits without a rule
    // rejection; this names that broad gate.
    let _ = ingest_curated().await?;
    Ok(())
}

#[tokio::test]
async fn bbox_listing_projects_landmarks_with_their_dates() -> Result<(), BoxError> {
    let Some(store) = ingest_curated().await? else {
        return Ok(());
    };

    // Central Rome — the ancient landmarks cluster here.
    let rome = placeable_in(&store, (41.85, 12.44), (41.95, 12.50)).await?;
    assert!(
        rome.iter().all(|s| !s.names.is_empty()),
        "every listed summary carries at least one name"
    );
    let colosseum = rome
        .iter()
        .find(|s| has_name(s, "Colosseum"))
        .ok_or("the Colosseum surfaces in a Rome viewport")?;
    assert!(
        colosseum
            .earliest
            .map(|d| d.year())
            .is_some_and(|y| y < 200),
        "the Colosseum projects an antiquity construction date, got {:?}",
        colosseum.earliest
    );
    assert!(
        !rome.iter().any(|s| has_name(s, "Eiffel Tower")),
        "a Rome viewport does not surface the Paris Eiffel Tower"
    );

    // Central Paris — the Eiffel Tower's construction window.
    let paris = placeable_in(&store, (48.84, 2.28), (48.87, 2.36)).await?;
    let eiffel = paris
        .iter()
        .find(|s| has_name(s, "Eiffel Tower"))
        .ok_or("the Eiffel Tower surfaces in a Paris viewport")?;
    let within_1880s = |d: Option<chrono::NaiveDate>| {
        d.map(|d| d.year())
            .is_some_and(|y| (1880..1890).contains(&y))
    };
    assert!(
        within_1880s(eiffel.earliest) && within_1880s(eiffel.latest),
        "the Eiffel Tower projects an 1880s construction window, got {:?}..{:?}",
        eiffel.earliest,
        eiffel.latest
    );
    Ok(())
}

#[tokio::test]
async fn notre_dame_founding_survives_as_construction_start() -> Result<(), BoxError> {
    let Some(store) = ingest_curated().await? else {
        return Ok(());
    };

    // Notre-Dame de Paris (Q2981) carries a P571 founding of 1160 alongside a
    // P793 construction (1163–1345) that already has a completion. The founding
    // must survive as a competing construction start bound — the earlier
    // "fill the empty completion" reading dropped it, pushing `earliest` to 1163.
    let paris = placeable_in(&store, (48.84, 2.28), (48.87, 2.36)).await?;
    let notre_dame = paris
        .iter()
        .find(|s| has_name(s, "Notre-Dame de Paris"))
        .ok_or("Notre-Dame surfaces in a Paris viewport")?;
    assert_eq!(
        notre_dame.earliest.map(|d| d.year()),
        Some(1160),
        "the 1160 founding is the earliest date, ahead of the 1163 build start, got {:?}",
        notre_dame.earliest
    );
    Ok(())
}

#[tokio::test]
async fn demolish_rebuild_splits_into_two_dated_entities() -> Result<(), BoxError> {
    let Some(store) = ingest_curated().await? else {
        return Ok(());
    };

    // Chioggia Cathedral was demolished and rebuilt, so it splits into two
    // entities sharing one location, each with its own lifetime.
    let chioggia = placeable_in(&store, (45.20, 12.26), (45.23, 12.29)).await?;
    let cathedrals: Vec<_> = chioggia
        .iter()
        .filter(|s| has_name(s, "Chioggia Cathedral"))
        .collect();
    assert!(
        cathedrals.len() >= 2,
        "the demolish-rebuild splits into at least two entities, got {}",
        cathedrals.len()
    );
    let years: std::collections::BTreeSet<i32> = cathedrals
        .iter()
        .filter_map(|s| s.earliest.map(|d| d.year()))
        .collect();
    assert!(
        years.len() >= 2,
        "the split entities carry distinct construction dates, got {years:?}"
    );
    Ok(())
}
