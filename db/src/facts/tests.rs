//! SQLite fact-store tests: the conformance suite stamped against
//! [`SqliteFactStore`], plus what is genuinely backend-specific — file-backed
//! persistence across reopen, counter rollback, submit-boundary savepoints
//! over real SQL, unminted-id reads, `BEGIN IMMEDIATE` writer exclusion, and
//! the query-plan gate.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sqlx::sqlite::SqlitePool;

use chronoscope_core::grammar::assertions::FactualAssertion;
use chronoscope_core::grammar::attribute;
use chronoscope_core::grammar::ids::FactId;
use chronoscope_core::store::conformance::fixtures::{
    commit_err, commit_name, fixed_time, local_bundle, name_fact, user_author,
};
use chronoscope_core::store::conformance::{TestError, TestResult, UnmintedIds};
use chronoscope_core::store::{EntityView, FactStore, FactView, FactWrite};
use chronoscope_core::submit::{
    Commit as SubmitBundle, Decl, EntityIdx, FactLookup, ResolutionOrigin, StoredFact, SubmitError,
};

use super::*;
use crate::DbError;

/// Counters mint dense from zero, so `i64::MAX` is never assigned.
impl UnmintedIds for SqliteFactStore {
    fn unminted_entity() -> SqliteEntityId {
        SqliteEntityId(i64::MAX)
    }
    fn unminted_event() -> SqliteEventId {
        SqliteEventId(i64::MAX)
    }
    fn unminted_image() -> SqliteImageId {
        SqliteImageId(i64::MAX)
    }
}

/// A fresh migrated pool with the crate's standard shape (SpatiaLite loaded).
async fn fresh_pool(database_url: &str) -> Result<SqlitePool, DbError> {
    let pool = crate::create_pool(database_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

/// A fresh file-backed store per case, plus the tempdir holding its
/// database. Views hold read transactions for their lifetime, and only a
/// file-backed database gives WAL's reader/writer independence — a
/// shared-cache in-memory database serializes them at table locks.
async fn fresh_store() -> Result<(SqliteFactStore, tempfile::TempDir), TestError> {
    let dir = tempfile::tempdir()?;
    let url = format!("sqlite:{}", dir.path().join("facts.sqlite3").display());
    let pool = fresh_pool(&url).await?;
    Ok((SqliteFactStore::new(pool), dir))
}

chronoscope_core::fact_store_conformance!(
    fresh_store(),
    ignore(
        walk_entity_classes_group_submitted_facts_into_one_class:
            "walk_entity_classes answers an empty page on this backend; flips green once the class-stream walk reads the facts table",
        class_walk_pages_distinct_entities_as_contiguous_runs:
            "walk_entity_classes answers an empty page on this backend; flips green once the class-stream walk reads the facts table",
        walk_image_classes_group_submitted_facts_into_one_class:
            "walk_image_classes answers an empty page on this backend; flips green once the class-stream walk reads the facts table",
        walk_entity_classes_in_bbox_surfaces_located_and_moved_in_entities:
            "the InBbox stream needs the populated facts_spatial rtree; flips green once the spatial walk lands",
        class_walk_next_class_cursor_skips_to_the_next_representative:
            "walk_entity_classes answers an empty page on this backend; flips green once the class-stream walk reads the facts table",
    )
);

// --- file-backed persistence ---

/// A committed fact survives dropping the store and pool: a reopened
/// file-backed store serves the fact, knows the commit, and replays the
/// cached result on re-submit.
#[tokio::test]
async fn reopened_file_store_serves_committed_facts() -> TestResult {
    let dir = tempfile::tempdir()?;
    let url = format!("sqlite:{}", dir.path().join("facts.sqlite3").display());

    let (commit_id, fact_id) = {
        let pool = fresh_pool(&url).await?;
        let store = SqliteFactStore::new(pool.clone());
        let result = commit_name(&store, "durable").await?;
        let fact_id = *result.fact_ids.first().ok_or("no fact id")?;
        pool.close().await;
        (result.commit_id, fact_id)
    };

    let pool = fresh_pool(&url).await?;
    let store = SqliteFactStore::new(pool.clone());
    assert_eq!(
        store.next_fact_id().await?,
        FactId::new(fact_id.get() + 1),
        "the reopened store's clock must sit one past the persisted fact"
    );

    let mut view = store.now().await?;
    assert!(view.commit_known(&commit_id).await?);
    let lookup = view.fact(fact_id).await?;
    let FactLookup::Active(stored) = lookup else {
        return Err(format!("expected the persisted fact Active, got {lookup:?}").into());
    };
    let StoredFact::Factual(factual) = stored.as_ref() else {
        return Err("expected a factual stored fact".into());
    };
    let FactualAssertion::Attribute {
        fact: attribute::Fact::Name { name, .. },
    } = &factual.assertion
    else {
        return Err("expected the persisted Name attribute".into());
    };
    assert_eq!(name.as_str(), "durable");
    // The view holds a pooled connection; close() below waits for it.
    drop(view);

    // Content-address dedup spans the reopen: the identical bundle replays.
    let replay = commit_name(&store, "durable").await?;
    assert!(replay.previously_committed);
    assert_eq!(replay.commit_id, commit_id);

    pool.close().await;
    Ok(())
}

// --- transaction rollback ---

/// A failed `with_tx` closure rolls back the mint counters along with the
/// staged facts: the next successful commit mints entity id 0.
#[tokio::test]
async fn failed_transaction_rolls_back_mint_counters() -> TestResult {
    let (store, _dir) = fresh_store().await?;

    let outcome: Result<(), String> = store
        .with_tx(|_s, tx| {
            Box::pin(async move {
                tx.mint_entity().await.map_err(|e| e.to_string())?;
                tx.mint_event().await.map_err(|e| e.to_string())?;
                tx.mint_image().await.map_err(|e| e.to_string())?;
                Err("deliberate failure after the mints".to_owned())
            })
        })
        .await?;
    assert!(outcome.is_err());
    assert_eq!(store.next_fact_id().await?, FactId::new(0));

    let result = commit_name(&store, "first-after-rollback").await?;
    let minted = result
        .entities
        .get(&EntityIdx(0))
        .ok_or("entity resolution missing")?
        .id;
    assert_eq!(
        minted,
        SqliteEntityId(0),
        "rolled-back mints must not burn counter values"
    );
    Ok(())
}

// --- submit-boundary savepoints over real SQL ---

/// A rejected submit rolls back to its savepoint: the transaction's view
/// rewinds to the pre-submit watermark, a following submit in the same
/// transaction succeeds, sees none of the rejected staging, and its matcher
/// judges against clean state.
#[tokio::test]
async fn rejected_submit_unwinds_and_the_transaction_stays_usable() -> TestResult {
    let (store, _dir) = fresh_store().await?;
    // Decl 1 is never referenced: the bundle stages its one fact, then
    // rejects with UnusedDeclaration.
    let doomed = local_bundle::<SqliteIds>(2, 0, 0, 0, vec![name_fact(0, "shared-name")?])?;
    // The same name on the healthy bundle: leaked staging would hand the
    // matcher a candidate.
    let healthy = local_bundle::<SqliteIds>(1, 0, 0, 10, vec![name_fact(0, "shared-name")?])?;

    let result = store
        .with_tx(move |s, tx| {
            Box::pin(async move {
                if s.submit_commit(tx, doomed).await.is_ok() {
                    return Err("expected the submit to be rejected".to_owned());
                }
                // The savepoint unwound the staged rows, so the transaction
                // view is empty again.
                let bound = tx.snapshot().await.map_err(|e| e.to_string())?;
                if bound != FactId::new(0) {
                    return Err(format!("expected the snapshot rewound to 0, got {bound}"));
                }
                s.submit_commit(tx, healthy)
                    .await
                    .map_err(|e| format!("{e:?}"))
            })
        })
        .await??;

    // The healthy submit minted fresh — the rejected staging was gone, so
    // the matcher had no candidate — and its fact took the first id.
    let resolution = result
        .entities
        .get(&EntityIdx(0))
        .ok_or("entity resolution missing")?;
    assert!(
        matches!(resolution.origin, ResolutionOrigin::NewlyMinted),
        "expected a fresh mint against clean state, got {:?}",
        resolution.origin
    );
    assert_eq!(result.fact_ids.first(), Some(&FactId::new(0)));
    assert_eq!(store.next_fact_id().await?, FactId::new(1));
    Ok(())
}

/// A closure that swallows a rejected submit and returns `Ok` commits
/// cleanly: the savepoint unwound the rejected staging, so the pre-commit
/// audit finds nothing unclaimed and only the recorded commits are durable.
#[tokio::test]
async fn swallowed_rejection_commits_only_the_recorded_commits() -> TestResult {
    let (store, _dir) = fresh_store().await?;
    let doomed =
        local_bundle::<SqliteIds>(2, 0, 0, 0, vec![name_fact(0, "staged-then-rejected")?])?;
    let doomed_id = doomed.id()?;
    let healthy = local_bundle::<SqliteIds>(1, 0, 0, 10, vec![name_fact(0, "kept")?])?;

    store
        .with_tx(move |s, tx| {
            Box::pin(async move {
                if s.submit_commit(tx, doomed).await.is_ok() {
                    return Err("expected the submit to be rejected".to_owned());
                }
                s.submit_commit(tx, healthy)
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                Ok::<_, String>(())
            })
        })
        .await??;

    assert_eq!(store.next_fact_id().await?, FactId::new(1));
    let mut view = store.now().await?;
    assert!(!view.commit_known(&doomed_id).await?);
    let lookup = view.fact(FactId::new(0)).await?;
    assert!(
        matches!(lookup, FactLookup::Active(_)),
        "expected the healthy commit's fact Active, got {lookup:?}"
    );
    Ok(())
}

// --- view connections ---

/// A dropped view's read transaction rolls back and its connection returns
/// to the pool clean: cycling more views than the pool holds connections,
/// every later view opens a fresh transaction and sees newer commits.
#[tokio::test]
async fn dropped_views_return_clean_connections() -> TestResult {
    let (store, _dir) = fresh_store().await?;
    commit_name(&store, "first").await?;
    for _ in 0..8 {
        let mut view = store.now().await?;
        let lookup = view.fact(FactId::new(0)).await?;
        assert!(matches!(lookup, FactLookup::Active(_)), "got {lookup:?}");
    }
    commit_name(&store, "second").await?;
    for _ in 0..8 {
        let mut view = store.now().await?;
        let lookup = view.fact(FactId::new(1)).await?;
        assert!(matches!(lookup, FactLookup::Active(_)), "got {lookup:?}");
    }
    Ok(())
}

// --- unminted-id reads ---

/// Ids the store never minted read as domain answers — unknown fact,
/// singleton class — never as errors or bogus rows. Fact ids past the
/// storable `i64` range and subject ids outside the minted space (negative
/// or far above the counters) all land the same way.
#[tokio::test]
async fn unminted_ids_read_as_domain_answers() -> TestResult {
    let (store, _dir) = fresh_store().await?;
    commit_name(&store, "resident").await?;

    let mut view = store.no_later_than(FactId::new(u64::MAX)).await?;
    let lookup = view.fact(FactId::new(u64::MAX - 1)).await?;
    assert!(
        matches!(lookup, FactLookup::Unknown),
        "an unstorable fact id was never minted, got {lookup:?}"
    );

    let huge = SqliteEntityId(i64::MAX - 1);
    assert_eq!(view.entity_representative(&huge).await?, huge);
    let class = view.entity_class(&huge).await?;
    assert_eq!(class.members.len(), 1);

    let negative = SqliteEntityId(-7);
    assert_eq!(view.entity_representative(&negative).await?, negative);
    Ok(())
}

/// A `Decl::Existing` naming a negative id is rejected as unknown: the
/// mintable space is `0..counter`, and an upper bound alone would wave a
/// wire-supplied negative through once anything has been minted.
#[tokio::test]
async fn negative_existing_entity_id_rejected_as_unknown() -> TestResult {
    let (store, _dir) = fresh_store().await?;
    commit_name(&store, "resident").await?;

    let bundle: SubmitBundle<SqliteIds> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![Decl::Existing {
            id: SqliteEntityId(-1),
        }],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "phantom")?].into_iter().collect(),
    };
    let errs = commit_err(&store, bundle).await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::UnknownExistingEntity { .. })),
        "got {errs:?}"
    );
    Ok(())
}

// --- BEGIN IMMEDIATE writer exclusion ---

/// Two concurrent `with_tx` calls on one file-backed store serialize: the
/// second's `BEGIN IMMEDIATE` waits out the first instead of interleaving.
#[tokio::test]
async fn begin_immediate_serializes_concurrent_writers() -> TestResult {
    let dir = tempfile::tempdir()?;
    let url = format!("sqlite:{}", dir.path().join("facts.sqlite3").display());
    let pool = fresh_pool(&url).await?;
    let store = Arc::new(SqliteFactStore::new(pool.clone()));

    let first_finished = Arc::new(AtomicBool::new(false));
    let (entered_send, entered_recv) = tokio::sync::oneshot::channel::<()>();
    let (second_spawned_send, second_spawned_recv) = tokio::sync::oneshot::channel::<()>();

    let first = {
        let store = Arc::clone(&store);
        let finished = Arc::clone(&first_finished);
        tokio::spawn(async move {
            store
                .with_tx(move |_s, tx| {
                    Box::pin(async move {
                        let _ = entered_send.send(());
                        // Hold the write lock until the second writer's task
                        // is live, then yield so its BEGIN IMMEDIATE reaches
                        // SQLite and parks on this open transaction.
                        second_spawned_recv
                            .await
                            .map_err(|e| format!("spawn signal dropped: {e}"))?;
                        for _ in 0..64 {
                            tokio::task::yield_now().await;
                        }
                        tx.mint_entity().await.map_err(|e| e.to_string())?;
                        finished.store(true, Ordering::SeqCst);
                        Ok::<_, String>(())
                    })
                })
                .await
        })
    };

    entered_recv.await?;
    let second = {
        let store = Arc::clone(&store);
        let finished = Arc::clone(&first_finished);
        tokio::spawn(async move {
            store
                .with_tx(move |_s, tx| {
                    Box::pin(async move {
                        if !finished.load(Ordering::SeqCst) {
                            return Err(
                                "second writer entered while the first still held the write lock"
                                    .to_owned(),
                            );
                        }
                        tx.mint_entity().await.map_err(|e| e.to_string())?;
                        Ok::<_, String>(())
                    })
                })
                .await
        })
    };
    let _ = second_spawned_send.send(());

    second.await?.map_err(|e| format!("{e:?}"))??;
    first.await?.map_err(|e| format!("{e:?}"))??;

    // Both mints landed, in order: ids 0 and 1 are known, 2 is not.
    let known: Result<(bool, bool), String> = store
        .with_tx(|_s, tx| {
            Box::pin(async move {
                let one = tx
                    .entity_known(&SqliteEntityId(1))
                    .await
                    .map_err(|e| e.to_string())?;
                let two = tx
                    .entity_known(&SqliteEntityId(2))
                    .await
                    .map_err(|e| e.to_string())?;
                Ok((one, two))
            })
        })
        .await?;
    assert_eq!(known?, (true, false));

    pool.close().await;
    Ok(())
}

// --- query-plan gate ---

/// Every fact-store query plans without a full table scan against the real
/// migrated schema.
#[tokio::test]
async fn fact_store_query_plans_use_indexes() -> TestResult {
    let pool = fresh_pool("sqlite::memory:").await?;
    super::queries::verify_query_plans(&pool).await?;
    Ok(())
}
