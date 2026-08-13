//! SQLite fact-store tests: the conformance suite stamped against
//! [`SqliteFactStore`], plus what is genuinely backend-specific — file-backed
//! persistence across reopen, counter rollback, submit-boundary savepoints
//! over real SQL, unminted-id reads, `BEGIN IMMEDIATE` writer exclusion, and
//! the query-plan gate.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sqlx::sqlite::SqlitePool;

use chronoscope_core::geo::{
    GeoPoint, QuadLevel, TileId, Viewport, mercator_x_to_lon, mercator_y_to_lat, quadkey,
};
use chronoscope_core::grammar::assertions::FactualAssertion;
use chronoscope_core::grammar::attribute;
use chronoscope_core::grammar::citations::Language;
use chronoscope_core::grammar::ids::FactId;
use chronoscope_core::store::conformance::fixtures::{
    commit_err, commit_name, commit_result, construction_at, construction_started_fact, fixed_time,
    local_bundle, name_fact, retract_fact, same_entity_fact, sample_viewport, user_author,
};
use chronoscope_core::store::conformance::{RefusalKinds, TestError, TestResult, UnmintedIds};
use chronoscope_core::store::schema::{
    CELL_DEPTH, CLUSTER_TILE_N, CellKind, ClusterCell, EntityStream, RankKey,
};
use chronoscope_core::store::{EntityView, FactStore, FactView, FactWrite};
use chronoscope_core::submit::{
    Commit as SubmitBundle, Decl, EntityIdx, FactLookup, ResolutionOrigin, StoredFact, SubmitError,
};

use super::*;
use crate::DbError;

/// Counters mint dense from zero, so `i64::MAX` is never assigned.
impl UnmintedIds for SqliteFactStore {
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

impl RefusalKinds for SqliteFactStore {
    fn is_cluster_tile_cap_refusal(error: &SqliteFactStoreError) -> bool {
        matches!(error, SqliteFactStoreError::ClusterTiles(_))
    }
}

/// A fresh pool over the facts file at `overlay` (a filesystem path): the
/// file is created + migrated if absent, then attached as `ovl` on a
/// throwaway in-memory `main` — the two-file layout every fact-store test
/// rides. Routes the path through [`FactStoreLocations::standalone_at`], the
/// one place a tempdir path becomes the overlay string. No plan verification
/// (that is its own test), so the conformance suite stays fast.
async fn fresh_pool(overlay: &std::path::Path) -> Result<SqlitePool, DbError> {
    let overlay = super::path_string(overlay, "overlay")?;
    crate::create_facts_file(&overlay).await?;
    crate::create_facts_pool(
        "sqlite::memory:",
        Some(crate::FactMount {
            overlay: &overlay,
            base: None,
        }),
    )
    .await
}

/// A fresh file-backed store per case, plus the tempdir holding its overlay
/// facts file. Views hold read transactions on the overlay (WAL) for their
/// lifetime; only a file-backed overlay gives WAL's reader/writer
/// independence — an in-memory overlay would serialize them at table locks.
/// The witness homomorphism suite reuses it for the same reader/writer reason.
pub(super) async fn fresh_store() -> Result<(SqliteFactStore, tempfile::TempDir), TestError> {
    let dir = tempfile::tempdir()?;
    let pool = fresh_pool(&dir.path().join("facts.sqlite3")).await?;
    Ok((SqliteFactStore::new(pool), dir))
}

chronoscope_core::fact_store_conformance!(fresh_store());

/// `open` yields a migrated, usable store, and `close` tears it down inside the
/// runtime — the teardown that keeps SpatiaLite's dlclose off the exit path.
#[tokio::test]
async fn open_migrates_the_store_and_close_tears_it_down() -> TestResult {
    let dir = tempfile::tempdir()?;
    let store = SqliteFactStore::open(FactStoreLocations::standalone_at(
        &dir.path().join("facts.sqlite3"),
    )?)
    .await?;
    assert_eq!(
        store.next_fact_id().await?,
        FactId::new(0),
        "a freshly opened store starts before any fact"
    );
    store.close().await;
    Ok(())
}

// --- file-backed persistence ---

/// A committed fact survives dropping the store and pool: a reopened
/// file-backed store serves the fact, knows the commit, and replays the
/// cached result on re-submit.
#[tokio::test]
async fn reopened_file_store_serves_committed_facts() -> TestResult {
    let dir = tempfile::tempdir()?;
    let overlay = dir.path().join("facts.sqlite3");

    let (commit_id, fact_id) = {
        let pool = fresh_pool(&overlay).await?;
        let store = SqliteFactStore::new(pool.clone());
        let result = commit_name(&store, "durable").await?;
        let fact_id = *result.fact_ids.first().ok_or("no fact id")?;
        pool.close().await;
        (result.commit_id, fact_id)
    };

    let pool = fresh_pool(&overlay).await?;
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
        SqlEntityId(0),
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
    let doomed = local_bundle::<SqlIds>(2, 0, 0, 0, vec![name_fact(0, "shared-name")?])?;
    // The same name on the healthy bundle: leaked staging would hand the
    // matcher a candidate.
    let healthy = local_bundle::<SqlIds>(1, 0, 0, 10, vec![name_fact(0, "shared-name")?])?;

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
    let doomed = local_bundle::<SqlIds>(2, 0, 0, 0, vec![name_fact(0, "staged-then-rejected")?])?;
    let doomed_id = doomed.id()?;
    let healthy = local_bundle::<SqlIds>(1, 0, 0, 10, vec![name_fact(0, "kept")?])?;

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

    let huge = SqlEntityId(i64::MAX - 1);
    assert_eq!(view.entity_representative(&huge).await?, huge);
    let class = view.entity_class(&huge).await?;
    assert_eq!(class.members.len(), 1);

    let negative = SqlEntityId(-7);
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

    let bundle: SubmitBundle<SqlIds> = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![Decl::Existing {
            id: SqlEntityId(-1),
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
    let pool = fresh_pool(&dir.path().join("facts.sqlite3")).await?;
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
                    .entity_known(&SqlEntityId(1))
                    .await
                    .map_err(|e| e.to_string())?;
                let two = tx
                    .entity_known(&SqlEntityId(2))
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
/// migrated schema, with the fact tables in the attached `ovl` overlay — the
/// unqualified reads and the `ovl.`-qualified writes must all resolve there.
#[tokio::test]
async fn fact_store_query_plans_use_indexes() -> TestResult {
    let dir = tempfile::tempdir()?;
    let pool = fresh_pool(&dir.path().join("facts.sqlite3")).await?;
    let fact_queries = super::queries::FactQueries::resolve(false);
    super::queries::verify_query_plans(&pool, &fact_queries).await?;
    Ok(())
}

/// Every fact-store query still plans without a full table scan over a mounted
/// base ∪ overlay — the reads resolve through the temp union views and the
/// spatial candidate SQL through its two-branch rtree union, so the no-full-scan
/// gate keeps its teeth across the layers, not just overlay-only. `open` runs
/// the plan verification internally, so a full-scan plan would fail this mount.
#[tokio::test]
async fn fact_store_query_plans_use_indexes_over_a_mounted_base() -> TestResult {
    let dir = tempfile::tempdir()?;
    let base_path = dir.path().join("base.sqlite3");
    let base = open_base(&base_path).await?;
    commit_result(
        &base,
        local_bundle::<SqlIds>(1, 0, 0, 0, vec![construction_at(0, 40.5, -73.5)?])?,
    )
    .await?;
    finish_base(base).await?;
    let overlay = dir.path().join("overlay.sqlite3");
    let store =
        SqliteFactStore::open(FactStoreLocations::mounted_at(&base_path, &overlay)?).await?;
    store.close().await;
    Ok(())
}

// --- base build helpers ---

/// Open a fresh overlay-only store to build a base artifact at `path`. The
/// caller submits into it, then finishes it with [`finish_base`].
async fn open_base(path: &std::path::Path) -> Result<SqliteFactStore, TestError> {
    Ok(SqliteFactStore::open(FactStoreLocations::standalone_at(path)?).await?)
}

/// Stamp and close a base artifact so it is a valid, immutable-ready pin. The
/// stamp's TRUNCATE checkpoint folds the overlay's WAL into the file, so an
/// immutable reader (which ignores the WAL) later sees everything.
async fn finish_base(store: SqliteFactStore) -> Result<(), TestError> {
    store.stamp_codec_version().await?;
    store.close().await;
    Ok(())
}

/// Mount `base_path` beneath a fresh scratch overlay in `dir`.
async fn mount_over(
    dir: &tempfile::TempDir,
    base_path: &std::path::Path,
) -> Result<SqliteFactStore, TestError> {
    let overlay = dir.path().join("overlay.sqlite3");
    Ok(SqliteFactStore::open(FactStoreLocations::mounted_at(base_path, &overlay)?).await?)
}

// --- base validation ---

/// `open` validates any base pin rather than fabricating: a missing path and an
/// unstamped/partial build both fail loud, while a completed, codec-stamped
/// build mounts. A `create_facts_file`-only DB stands in for an interrupted
/// build — migrated but never stamped (`user_version` 0). The real consumer is
/// the dev server mounting a pinned artifact.
#[tokio::test]
async fn open_rejects_an_invalid_base_pin() -> TestResult {
    let dir = tempfile::tempdir()?;
    let base_path = dir.path().join("base.sqlite3");
    let bad = || FactStoreLocations::mounted_at(&base_path, &dir.path().join("overlay.sqlite3"));

    // Missing base: no fabrication, a loud config error.
    let missing = SqliteFactStore::open(bad()?).await;
    assert!(
        matches!(missing, Err(DbError::Config(_))),
        "mounting a missing base must fail loud, got {missing:?}"
    );

    // Migrated but unstamped (the interrupted-build state): still rejected.
    crate::create_facts_file(super::path_string(&base_path, "base")?.as_str()).await?;
    let unstamped = SqliteFactStore::open(bad()?).await;
    assert!(
        matches!(unstamped, Err(DbError::Config(_))),
        "mounting an unstamped/partial base must fail loud, got {unstamped:?}"
    );
    assert!(matches!(
        crate::validate_facts_file(&base_path),
        Err(crate::FactsFileError::CodecMismatch { found: 0, .. })
    ));

    // Stamp it (what `ingest build-db` does after a successful ingest), and the
    // mount now succeeds.
    finish_base(open_base(&base_path).await?).await?;
    let store = mount_over(&dir, &base_path).await?;
    assert_eq!(store.next_fact_id().await?, FactId::new(0));
    store.close().await;
    Ok(())
}

/// `stamp_codec_version`'s TRUNCATE checkpoint folds the stamp into the file
/// header immediately, so [`validate_facts_file`](crate::validate_facts_file) —
/// which reads the raw header bytes (`user_version` at offset 60..64,
/// big-endian), never opening a connection — sees the current codec version
/// while the writing connection is still open, before any close-time
/// checkpoint. A bare `PRAGMA user_version` without the truncate checkpoint
/// would leave the stamp in the WAL until close and fail this pre-close read —
/// the exact WAL-durability regression the assertion guards.
#[tokio::test]
async fn stamp_reaches_file_header_before_close() -> TestResult {
    let dir = tempfile::tempdir()?;
    let base_path = dir.path().join("base.sqlite3");
    let store = open_base(&base_path).await?;
    commit_name(&store, "resident").await?;
    store.stamp_codec_version().await?;
    // Read the raw header before close: the stamp must already be in the file.
    crate::validate_facts_file(&base_path)
        .map_err(|e| format!("stamp did not reach the file header before close: {e}"))?;
    store.close().await;
    // And it survives the close.
    crate::validate_facts_file(&base_path).map_err(|e| format!("stamped file rejected: {e}"))?;
    Ok(())
}

// --- cross-layer reads ---

/// A mounted store's unqualified reads span the base through the union views: a
/// base fact reads Active and turns up in its subject's backlink. The union is
/// load-bearing — the same fact id in a fresh overlay-only store (empty overlay,
/// no base) reads Future, so the base data reaches the reader only through the
/// union.
#[tokio::test]
async fn mounted_reads_span_base_facts() -> TestResult {
    let dir = tempfile::tempdir()?;
    let base_path = dir.path().join("base.sqlite3");
    let base = open_base(&base_path).await?;
    let base_result = commit_name(&base, "base-entity").await?;
    let base_fact = *base_result.fact_ids.first().ok_or("no base fact id")?;
    let base_entity = base_result
        .entities
        .get(&EntityIdx(0))
        .ok_or("base entity resolution missing")?
        .id;
    finish_base(base).await?;

    let store = mount_over(&dir, &base_path).await?;
    let mut view = store.now().await?;
    assert!(
        matches!(view.fact(base_fact).await?, FactLookup::Active(_)),
        "the mounted store must serve the base fact through the union view"
    );
    let limit = std::num::NonZeroUsize::new(100).ok_or("limit is nonzero")?;
    let page = view
        .all_facts_about_entity(&base_entity, None, limit)
        .await?;
    assert_eq!(
        page.items.len(),
        1,
        "the base entity's backlink must span the base facts"
    );
    drop(view);
    store.close().await;

    // Overlay-only, no base: the same fact id is unknown, so the base data
    // above reached the reader only through the union.
    let (bare, _bare_dir) = fresh_store().await?;
    let mut bare_view = bare.now().await?;
    assert!(
        matches!(bare_view.fact(base_fact).await?, FactLookup::Future),
        "an overlay-only store has no base fact to serve"
    );
    Ok(())
}

/// A fresh overlay over a populated base mints past the base's ids rather than
/// colliding: the seed lifts the overlay's counters and the union `MAX`
/// continues the fact ids. Three base entities minted 0..2, so the overlay's
/// first fresh entity is 3 and its first fact continues past the base's.
#[tokio::test]
async fn overlay_mint_continues_past_base_ids() -> TestResult {
    let dir = tempfile::tempdir()?;
    let base_path = dir.path().join("base.sqlite3");
    let base = open_base(&base_path).await?;
    let base_result = commit_result(
        &base,
        local_bundle::<SqlIds>(
            3,
            0,
            0,
            0,
            vec![name_fact(0, "a")?, name_fact(1, "b")?, name_fact(2, "c")?],
        )?,
    )
    .await?;
    let base_next_fact = base_result.fact_ids.len() as u64;
    finish_base(base).await?;

    let store = mount_over(&dir, &base_path).await?;
    assert_eq!(
        store.next_fact_id().await?,
        FactId::new(base_next_fact),
        "the overlay's clock must continue past the base's facts"
    );
    let result = commit_name(&store, "overlay-entity").await?;
    let minted = result
        .entities
        .get(&EntityIdx(0))
        .ok_or("entity resolution missing")?
        .id;
    assert_eq!(
        minted,
        SqlEntityId(3),
        "a fresh overlay mint must continue past the base's three entities, not collide at 0"
    );
    assert_eq!(
        result.fact_ids.first(),
        Some(&FactId::new(base_next_fact)),
        "the overlay's fact ids continue past the base's"
    );
    store.close().await;
    Ok(())
}

/// An overlay `SameEntity` merging a base entity into a base class re-points the
/// whole losing class — including members whose only rep row lives in the base.
/// The base merges entities 1,2 (rep 1); the overlay then merges 0 with that
/// class, and member 2 (base-only rep row) must follow to representative 0.
/// Without the union, the merge's class gather would miss the base row and
/// leave member 2 stranded at representative 1.
#[tokio::test]
async fn overlay_same_entity_merges_a_base_class() -> TestResult {
    let dir = tempfile::tempdir()?;
    let base_path = dir.path().join("base.sqlite3");
    let base = open_base(&base_path).await?;
    commit_result(
        &base,
        local_bundle::<SqlIds>(
            3,
            0,
            0,
            0,
            vec![
                name_fact(0, "a")?,
                name_fact(1, "b")?,
                name_fact(2, "c")?,
                same_entity_fact(1, 2)?,
            ],
        )?,
    )
    .await?;
    finish_base(base).await?;

    let store = mount_over(&dir, &base_path).await?;
    // Merge base entity 0 with the base class {1, 2} via existing-id decls.
    commit_result(
        &store,
        SubmitBundle::<SqlIds> {
            author: user_author()?,
            recorded_at: fixed_time() + chrono::Duration::seconds(10),
            entities: vec![
                Decl::Existing { id: SqlEntityId(0) },
                Decl::Existing { id: SqlEntityId(1) },
            ],
            events: Vec::new(),
            images: Vec::new(),
            facts: [same_entity_fact(0, 1)?].into_iter().collect(),
        },
    )
    .await?;

    let mut view = store.now().await?;
    let class = view.entity_class(&SqlEntityId(0)).await?;
    let members: std::collections::BTreeSet<SqlEntityId> = class.members.iter().copied().collect();
    assert_eq!(
        members,
        [SqlEntityId(0), SqlEntityId(1), SqlEntityId(2)]
            .into_iter()
            .collect(),
        "the overlay merge must pull the base member 2 into the class across the union"
    );
    assert_eq!(
        view.entity_representative(&SqlEntityId(2)).await?,
        SqlEntityId(0),
        "base member 2 must resolve to the merged representative 0"
    );
    drop(view);
    store.close().await;
    Ok(())
}

/// An overlay retraction hides a base fact: the retractor row lives in the
/// overlay, its target in the base, and the closure spans both — seeding from
/// the base fact and reaching the overlay retractor through the union. Without
/// the union the base seed would not be found and the fact would read Active.
#[tokio::test]
async fn overlay_retraction_hides_a_base_fact() -> TestResult {
    let dir = tempfile::tempdir()?;
    let base_path = dir.path().join("base.sqlite3");
    let base = open_base(&base_path).await?;
    let base_result = commit_result(
        &base,
        local_bundle::<SqlIds>(
            1,
            0,
            0,
            0,
            vec![name_fact(0, "alpha")?, construction_started_fact(0)?],
        )?,
    )
    .await?;
    let target = *base_result
        .fact_ids
        .get(1)
        .ok_or("no base construction fact")?;
    finish_base(base).await?;

    let store = mount_over(&dir, &base_path).await?;
    {
        let mut view = store.now().await?;
        assert!(
            matches!(view.fact(target).await?, FactLookup::Active(_)),
            "the base construction fact starts Active"
        );
    }
    commit_result(
        &store,
        SubmitBundle::<SqlIds> {
            author: user_author()?,
            recorded_at: fixed_time() + chrono::Duration::seconds(10),
            entities: Vec::new(),
            events: Vec::new(),
            images: Vec::new(),
            facts: [retract_fact(target)?].into_iter().collect(),
        },
    )
    .await?;

    let mut view = store.now().await?;
    assert!(
        matches!(view.fact(target).await?, FactLookup::Retracted { .. }),
        "the overlay retraction must hide the base fact across the union"
    );
    drop(view);
    store.close().await;
    Ok(())
}

/// An overlay split shadows a base rep with a later overlay row while earlier
/// snapshots stay merged. The base merges 0,1 (member 1 → rep 0); the overlay
/// retracts that edge, splitting them, and appends a later rep row (member 1 →
/// self). At `now` member 1 resolves to itself; a snapshot pinned between the
/// base merge and the overlay split still resolves it to 0 through the base
/// row — the append-only, historically-immutable rep log across the union.
#[tokio::test]
async fn overlay_split_shadows_base_rep_earlier_snapshots_stay_merged() -> TestResult {
    let dir = tempfile::tempdir()?;
    let base_path = dir.path().join("base.sqlite3");
    let base = open_base(&base_path).await?;
    let base_result = commit_result(
        &base,
        local_bundle::<SqlIds>(
            2,
            0,
            0,
            0,
            vec![
                name_fact(0, "a")?,
                name_fact(1, "b")?,
                same_entity_fact(0, 1)?,
            ],
        )?,
    )
    .await?;
    // The identity edge is the last base fact; retracting it splits the class.
    let edge = *base_result.fact_ids.last().ok_or("no base identity fact")?;
    let merged_snapshot = FactId::new(edge.get() + 1);
    finish_base(base).await?;

    let store = mount_over(&dir, &base_path).await?;
    {
        let mut view = store.now().await?;
        assert_eq!(
            view.entity_representative(&SqlEntityId(1)).await?,
            SqlEntityId(0),
            "the base merge holds before the overlay splits it"
        );
    }
    commit_result(
        &store,
        SubmitBundle::<SqlIds> {
            author: user_author()?,
            recorded_at: fixed_time() + chrono::Duration::seconds(10),
            entities: Vec::new(),
            events: Vec::new(),
            images: Vec::new(),
            facts: [retract_fact(edge)?].into_iter().collect(),
        },
    )
    .await?;

    let mut now_view = store.now().await?;
    assert_eq!(
        now_view.entity_representative(&SqlEntityId(1)).await?,
        SqlEntityId(1),
        "the overlay split shadows the base rep at now"
    );
    drop(now_view);

    let mut past_view = store.no_later_than(merged_snapshot).await?;
    assert_eq!(
        past_view.entity_representative(&SqlEntityId(1)).await?,
        SqlEntityId(0),
        "a snapshot before the overlay split still reads the base merge through the union"
    );
    drop(past_view);
    store.close().await;
    Ok(())
}

/// The class and spatial walks span both layers: a `ByName` walk surfaces a base
/// entity, and an `InViewport` walk surfaces a base-located entity through the
/// two-branch rtree union — over a 0444 base pin (no room for WAL/-shm sidecars)
/// attached `mode=ro&immutable=1`, the shape the dev server serves. The overlay
/// then accepts a submit on top, so serving a frozen pin still takes writes.
#[tokio::test]
async fn walks_span_base_and_overlay_over_a_read_only_pin() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir()?;
    let base_path = dir.path().join("base.sqlite3");
    let base = open_base(&base_path).await?;
    let base_result = commit_result(
        &base,
        local_bundle::<SqlIds>(
            1,
            0,
            0,
            0,
            vec![name_fact(0, "landmark")?, construction_at(0, 40.5, -73.5)?],
        )?,
    )
    .await?;
    let base_entity = base_result
        .entities
        .get(&EntityIdx(0))
        .ok_or("base entity resolution missing")?
        .id;
    finish_base(base).await?;

    // Pin the base read-only, as a 0444 nix-store facts DB is served.
    let mut perms = std::fs::metadata(&base_path)?.permissions();
    perms.set_mode(0o444);
    std::fs::set_permissions(&base_path, perms)?;

    let store = mount_over(&dir, &base_path).await?;
    let limit = std::num::NonZeroUsize::new(100).ok_or("limit is nonzero")?;

    let mut view = store.now().await?;
    let named = view
        .walk_entity_classes(
            &EntityStream::ByName {
                name: "landmark",
                language: &Language::new("en")?,
            },
            None,
            limit,
        )
        .await?;
    let named_reps: Vec<SqlEntityId> = named.rows.iter().map(|r| r.representative).collect();
    assert!(
        named_reps.contains(&base_entity),
        "the ByName walk must surface the base entity across the union, got {named_reps:?}"
    );

    let viewport = sample_viewport()?;
    let spatial = view
        .walk_entity_classes(&EntityStream::InViewport(&viewport), None, limit)
        .await?;
    let spatial_reps: Vec<SqlEntityId> = spatial.rows.iter().map(|r| r.representative).collect();
    assert!(
        spatial_reps.contains(&base_entity),
        "the InViewport walk must surface the base-located entity through the two-branch rtree, \
         got {spatial_reps:?}"
    );
    drop(view);

    // Serving a frozen pin still takes writes: the submit lands in the overlay.
    let result = commit_name(&store, "overlay-addition").await?;
    assert!(!result.previously_committed);
    store.close().await;
    Ok(())
}

// --- cross-layer clustering ---

/// The geographic centre of tile `(tx, ty)` at `level` — inside its finest cell,
/// so its `quadkey` lands in that tile. Places entities in chosen sub-tiles.
fn tile_center(level: QuadLevel, tx: u32, ty: u32) -> Result<GeoPoint, TestError> {
    let n = f64::from(1u32 << level.get());
    let ux = (f64::from(tx) + 0.5) / n;
    let uy = (f64::from(ty) + 0.5) / n;
    Ok(GeoPoint::new(mercator_y_to_lat(uy), mercator_x_to_lon(ux))?)
}

/// The clustering read spans base ∪ overlay through the two-branch quadkey union,
/// and the container's stand-alone tile cells equal its clipped viewport cells
/// even when a sub-tile's entities straddle the seam. The co-located pair has one
/// member in the base and one in the overlay, so a single bucket is folded from
/// both branches — the union path the overlay-only conformance run can't reach.
#[tokio::test]
async fn cluster_tile_and_viewport_agree_across_a_mounted_base() -> TestResult {
    let z = QuadLevel::new(8)?;
    let cell_level = QuadLevel::saturating(z.get() + CELL_DEPTH);
    let (cx, cy) = (75u32, 96u32);
    let factor = 1u32 << CELL_DEPTH;
    let (base_x, base_y) = (cx * factor, cy * factor);

    let p_lone1 = tile_center(cell_level, base_x + 1, base_y + 2)?;
    let p_lone2 = tile_center(cell_level, base_x + 5, base_y + 6)?;
    let p_pair = tile_center(cell_level, base_x + 3, base_y + 4)?;
    let p_neighbour = tile_center(cell_level, (cx + 1) * factor + 2, base_y + 2)?;

    let place = |p: &GeoPoint, secs: i64| {
        local_bundle::<SqlIds>(1, 0, 0, secs, vec![construction_at(0, p.lat(), p.lon())?])
    };

    // Base holds one lone entity and one half of the co-located pair.
    let dir = tempfile::tempdir()?;
    let base_path = dir.path().join("base.sqlite3");
    let base = open_base(&base_path).await?;
    commit_result(&base, place(&p_lone1, 0)?).await?;
    commit_result(&base, place(&p_pair, 20)?).await?;
    finish_base(base).await?;

    // Overlay holds the other lone entity, the pair's other half, and the
    // out-of-container neighbour.
    let store = mount_over(&dir, &base_path).await?;
    commit_result(&store, place(&p_lone2, 10)?).await?;
    commit_result(&store, place(&p_pair, 30)?).await?;
    commit_result(&store, place(&p_neighbour, 40)?).await?;

    let tile = TileId::new(z, cx, cy)?;
    let container = tile.range();
    let in_container = |p: &GeoPoint| {
        let q = quadkey(p);
        container.lo <= q && q <= container.hi
    };
    assert!(
        !in_container(&p_neighbour),
        "the neighbour must sit outside the container block"
    );

    let mut view = store.now().await?;
    let mut tile_cells = view.cluster_tile_cells(tile, RankKey::Unranked).await?;

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
        .await?;
    assert_eq!(
        vp_all.len(),
        4,
        "the viewport spans three in-container cells plus the neighbour's, got {vp_all:?}"
    );
    let mut vp_cells: Vec<ClusterCell<SqlEntityId>> = vp_all
        .into_iter()
        .filter(|c| in_container(&c.point))
        .collect();

    tile_cells.sort_by_key(|a| a.representative);
    vp_cells.sort_by_key(|a| a.representative);
    assert_eq!(
        tile_cells.len(),
        3,
        "two singletons and one seam-straddling co-located cell, got {tile_cells:?}"
    );
    assert!(
        tile_cells
            .iter()
            .any(|c| matches!(c.kind, CellKind::Colocated { ref members } if members.len() == 2)),
        "the seam-straddling pair must fold to a two-member co-located cell, got {tile_cells:?}"
    );
    assert_eq!(
        tile_cells, vp_cells,
        "over a mounted base the stand-alone tile cells must equal the clipped viewport cells"
    );
    drop(view);
    store.close().await;
    Ok(())
}

/// The overlay union re-truncates each bucket to its `(quadkey, fact_id)`-lowest
/// `CLUSTER_TILE_N` in Rust before the fold. A sub-tile is packed with
/// `CLUSTER_TILE_N` co-located entities in the base plus three more in the
/// overlay: each layer branch returns its own top-N (base N, overlay 3), so the
/// union hands the fold `N + 3` rows. Without the Rust re-truncate the co-located
/// cell would carry all `N + 3` members; the cut recovers the single-table top-N,
/// so exactly the base's `N` (lowest fact ids under the shared quadkey) survive
/// and the three overlay members drop — the same cells the memory oracle folds.
#[tokio::test]
async fn cluster_union_retruncates_a_bucket_to_top_n() -> TestResult {
    let viewport = Viewport::new(GeoPoint::new(40.0, -74.0)?, GeoPoint::new(40.1, -73.9)?)?;
    let level = QuadLevel::new(14)?;
    let (p_lat, p_lon) = (40.021, -73.981);

    // Base: exactly CLUSTER_TILE_N co-located entities at one point (fact ids
    // 0..N-1, all sharing one quadkey).
    let dir = tempfile::tempdir()?;
    let base_path = dir.path().join("base.sqlite3");
    let base = open_base(&base_path).await?;
    for i in 0..CLUSTER_TILE_N {
        commit_result(
            &base,
            local_bundle::<SqlIds>(1, 0, 0, i as i64, vec![construction_at(0, p_lat, p_lon)?])?,
        )
        .await?;
    }
    finish_base(base).await?;

    // Overlay: three more co-located entities at the same point — higher fact
    // ids, so the top-N-by-(quadkey, fact_id) cut drops them.
    let store = mount_over(&dir, &base_path).await?;
    let mut overlay_ids = Vec::new();
    for i in 0..3 {
        let res = commit_result(
            &store,
            local_bundle::<SqlIds>(1, 0, 0, 10_000 + i, vec![construction_at(0, p_lat, p_lon)?])?,
        )
        .await?;
        overlay_ids.push(
            res.entities
                .get(&EntityIdx(0))
                .ok_or("missing overlay id")?
                .id,
        );
    }

    let mut view = store.now().await?;
    let cells = view
        .cluster_entities_in_viewport(&viewport, level, RankKey::Unranked)
        .await?;
    assert_eq!(
        cells.len(),
        1,
        "one bucket, one co-located cell; got {cells:?}"
    );
    let CellKind::Colocated { members } = &cells[0].kind else {
        return Err(format!("expected a co-located cell, got {:?}", cells[0].kind).into());
    };
    assert_eq!(
        members.len(),
        CLUSTER_TILE_N,
        "the re-truncate keeps exactly the top-N; a missing cut would carry all N+3 members"
    );
    assert!(
        members.iter().all(|m| m.0 < CLUSTER_TILE_N as i64),
        "only the base's N lowest-fact-id entities survive; the overlay members drop, got {members:?}"
    );
    assert!(
        overlay_ids.iter().all(|o| !members.contains(o)),
        "no overlay entity may appear once the bucket is re-truncated to top-N"
    );
    assert_eq!(
        cells[0].representative,
        SqlEntityId(0),
        "the (quadkey, fact_id)-minimal survivor is the base's first entity"
    );
    drop(view);
    store.close().await;
    Ok(())
}
