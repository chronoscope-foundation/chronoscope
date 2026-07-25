//! Homomorphism test for the temporal-conflict witness read.
//!
//! The witness indexes and the `conflicts_via_index` read exist to answer the
//! same question as `temporal_conflicts` over a whole-entity projection, without
//! the projection. The coherence proof is the homomorphism law: at every
//! snapshot `T`,
//!
//! ```text
//! conflicts_via_index(E, T)  ==  temporal_conflicts(project_entity_at(E, T))
//! ```
//!
//! as sets. The proptest drives random submit sequences — existence witnesses,
//! bookends, point / durational events, identity merges, retractions — and
//! checks the law at every fact-id snapshot for every entity. Two edge-churn
//! fixtures ride alongside: retracting only the `HasEvent` while its date fact
//! stays live (an orphaned date), and attempting to re-home an event to a
//! different entity — now refused by ownership immutability, so the fixture pins
//! the rejection and that X keeps the event.

use std::collections::BTreeSet;
use std::num::NonZeroUsize;

use chrono::{Duration, NaiveDate};
use proptest::prelude::*;

use chronoscope_core::conflicts::fact_lineage;
use chronoscope_core::date::{DatePrecision, UncertainDate};
use chronoscope_core::grammar::assertions::FactualAssertion;
use chronoscope_core::grammar::bookend::{ConstructionFact, DemolitionFact};
use chronoscope_core::grammar::citations::{Excerpt, ExternalSource, FactualCitation};
use chronoscope_core::grammar::event::Fact as EventFact;
use chronoscope_core::grammar::existence;
use chronoscope_core::grammar::ids::{FactId, UserId};
use chronoscope_core::grammar::lifecycle::{
    DurationalKind, DurationalRole, LifetimeEventKind, PointKind,
};
use chronoscope_core::projection::project_entity;
use chronoscope_core::solvers::{TemporalConflict, TemporalConflictKind, temporal_conflicts};
use chronoscope_core::store::conformance::TestError;
use chronoscope_core::store::conformance::fixtures::{fixed_time, retract_fact, same_entity_fact};
use chronoscope_core::store::{EventView, FactStore};
use chronoscope_core::submit::{
    Commit, CommitAuthor, Decl, EntityIdx, EventIdx, StoredFact, SubmitFact, SubmitResult,
    commit_facts,
};

use super::{SqlEntityId, SqlEventId, SqlIds, SqliteFactStore, SqliteFactView};
// The homomorphism check opens a fresh read view per snapshot, so it needs the
// WAL reader/writer independence only a file-backed store gives — hence reusing
// the crate's file-backed `fresh_store` builder rather than a fresh in-memory one.
use super::tests::fresh_store;

// ============================================================================
// Fact builders — the bundle-local `SubmitFact` shapes the ops submit
// ============================================================================

fn citation(tag: &str) -> Result<FactualCitation, TestError> {
    let source = ExternalSource::Url {
        url: url::Url::parse(&format!("https://example.com/{tag}"))?,
        published: None,
    };
    Ok(FactualCitation::new(
        source,
        vec![Excerpt::new("evidence")?],
    )?)
}

/// A year-precision date.
fn year(y: i32) -> Result<UncertainDate, TestError> {
    Ok(UncertainDate::with_precision(
        NaiveDate::from_ymd_opt(y, 1, 1).ok_or("valid year")?,
        DatePrecision::Year,
    )?)
}

fn existence_fact(entity: usize, y: i32) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Existence {
            fact: existence::Fact {
                entity: EntityIdx(entity),
                at: year(y)?,
            },
        },
        citation: citation("witness")?,
    })
}

fn construction_fact(entity: usize, y: i32) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Construction {
            fact: ConstructionFact::Started {
                entity: EntityIdx(entity),
                bound: year(y)?,
            },
        },
        citation: citation("construction")?,
    })
}

fn demolition_fact(entity: usize, y: i32) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Demolition {
            fact: DemolitionFact::Completed {
                entity: EntityIdx(entity),
                bound: year(y)?,
            },
        },
        citation: citation("demolition")?,
    })
}

fn has_event_point_fact(entity: usize, event: usize) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Event {
            fact: EventFact::HasEvent {
                entity: EntityIdx(entity),
                event: EventIdx(event),
                kind: LifetimeEventKind::Point {
                    kind: PointKind::UsageChanged,
                },
            },
        },
        citation: citation("has-event-point")?,
    })
}

fn point_date_fact(event: usize, y: i32) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Event {
            fact: EventFact::PointDate {
                event: EventIdx(event),
                bound: year(y)?,
            },
        },
        citation: citation("point-date")?,
    })
}

fn has_event_durational_fact(entity: usize, event: usize) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Event {
            fact: EventFact::HasEvent {
                entity: EntityIdx(entity),
                event: EventIdx(event),
                kind: LifetimeEventKind::Durational {
                    kind: DurationalKind::Modified,
                },
            },
        },
        citation: citation("has-event-durational")?,
    })
}

fn durational_date_fact(
    event: usize,
    role: DurationalRole,
    y: i32,
) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Event {
            fact: EventFact::DurationalDate {
                event: EventIdx(event),
                role,
                bound: year(y)?,
            },
        },
        citation: citation("durational-date")?,
    })
}

fn name_fact(entity: usize, name: &str) -> Result<SubmitFact, TestError> {
    // A bare mention so a fresh `Decl::Local` entity is never unused.
    chronoscope_core::store::conformance::fixtures::name_fact(entity, name)
}

// ============================================================================
// Store plumbing
// ============================================================================

/// Submit one commit, returning its result or `None` if the bundle was rejected
/// (an interpreted op that doesn't type-check against current state is skipped,
/// not a test failure). `secs` offsets `recorded_at` so no two commits collide
/// on content address.
async fn try_commit(
    store: &SqliteFactStore,
    entities: Vec<Decl<SqlEntityId>>,
    events: Vec<Decl<SqlEventId>>,
    secs: i64,
    facts: Vec<SubmitFact>,
) -> Result<Option<SubmitResult<SqlIds>>, TestError> {
    let commit = Commit::<SqlIds> {
        author: CommitAuthor::User(UserId::new("prop")?),
        recorded_at: fixed_time() + Duration::seconds(secs),
        entities,
        events,
        images: Vec::new(),
        facts: facts.into_iter().collect(),
    };
    Ok(commit_facts(store, commit).await.ok())
}

/// Submit one commit that must land, returning its result. The fixtures build
/// known-good bundles, so a rejection is a test failure rather than a skip.
async fn commit_one(
    store: &SqliteFactStore,
    secs: i64,
    entities: Vec<Decl<SqlEntityId>>,
    events: Vec<Decl<SqlEventId>>,
    facts: Vec<SubmitFact>,
) -> Result<SubmitResult<SqlIds>, TestError> {
    try_commit(store, entities, events, secs, facts)
        .await?
        .ok_or_else(|| "commit rejected".into())
}

/// The resolved id at an entity declaration slot of a commit result.
fn entity_id(result: &SubmitResult<SqlIds>, idx: usize) -> Result<SqlEntityId, TestError> {
    Ok(result
        .entities
        .get(&EntityIdx(idx))
        .ok_or("entity resolution missing")?
        .id)
}

/// The resolved id at an event declaration slot of a commit result.
fn event_id(result: &SubmitResult<SqlIds>, idx: usize) -> Result<SqlEventId, TestError> {
    Ok(result
        .events
        .get(&EventIdx(idx))
        .ok_or("event resolution missing")?
        .id)
}

/// The live `HasEvent` fact id of an event at `now()`.
async fn has_event_fact_id(
    store: &SqliteFactStore,
    event: SqlEventId,
) -> Result<FactId, TestError> {
    let mut view = store.now().await?;
    let limit = NonZeroUsize::new(256).ok_or("nonzero limit")?;
    let page = view.all_facts_about_event(&event, None, limit).await?;
    for item in page.items {
        if let StoredFact::Factual(f) = &item.fact
            && let FactualAssertion::Event {
                fact: EventFact::HasEvent { .. },
            } = &f.assertion
        {
            return Ok(item.fact_id);
        }
    }
    Err("event has no live HasEvent".into())
}

// ============================================================================
// The homomorphism check
// ============================================================================

/// A conflict normalized for set comparison: the fact set (order-insensitive)
/// and the rendered kind. Distinct conflicts always carry distinct fact sets, so
/// the fact set alone canonicalizes the sort.
fn normalize(conflicts: &[TemporalConflict]) -> Vec<(BTreeSet<FactId>, TemporalConflictKind)> {
    let mut rows: Vec<(BTreeSet<FactId>, TemporalConflictKind)> = conflicts
        .iter()
        .map(|c| (c.facts.iter().copied().collect(), c.kind.clone()))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

/// Assert the witness read equals the projection oracle for `entity` on this
/// snapshot view. An id no fact names projects as `None` — no entity, no
/// conflicts — which the witness read also answers empty.
async fn assert_agrees(view: &mut SqliteFactView, entity: SqlEntityId) -> Result<(), TestError> {
    let oracle = match project_entity::<SqliteFactStore, _, _>(view, entity, fact_lineage).await? {
        Some((_, projected)) => temporal_conflicts::<SqlIds>(&projected),
        None => Vec::new(),
    };
    let indexed = view.temporal_conflicts_indexed(entity).await?;
    let oracle = normalize(&oracle);
    let indexed = normalize(&indexed);
    if oracle != indexed {
        return Err(format!(
            "homomorphism broke for {entity:?}:\n  oracle  = {oracle:?}\n  indexed = {indexed:?}"
        )
        .into());
    }
    Ok(())
}

/// Check the law for every entity at every fact-id snapshot (0 = before all
/// facts, `next_fact_id` = now). One read view per snapshot serves all entities.
async fn assert_homomorphism(
    store: &SqliteFactStore,
    entities: &[SqlEntityId],
) -> Result<(), TestError> {
    let next = store.next_fact_id().await?.get();
    for snapshot in 0..=next {
        let mut view = store.no_later_than(FactId::new(snapshot)).await?;
        for &entity in entities {
            assert_agrees(&mut view, entity).await?;
        }
    }
    Ok(())
}

// ============================================================================
// The random driver
// ============================================================================

/// One generated operation, each a single commit against a pool of three
/// pre-created entities. Slot fields index that pool (mod 3); `year` maps into a
/// narrow band so witnesses land on both sides of the bookends.
#[derive(Debug, Clone)]
enum Op {
    Existence { entity: u8, year: u8 },
    Construction { entity: u8, year: u8 },
    Demolition { entity: u8, year: u8 },
    PointEvent { entity: u8, year: u8 },
    DurationalEvent { entity: u8, start: u8, end: u8 },
    Merge { a: u8, b: u8 },
    Retract { fact: u8 },
}

/// Map a generated year byte into 1500..1560 — a band the bookends and
/// witnesses share, so a random sequence produces conflicts and non-conflicts.
fn band(y: u8) -> i32 {
    1500 + i32::from(y % 60)
}

/// The `Decl::Existing` for a pool slot (mod 3).
fn existing(entities: &[SqlEntityId], slot: u8) -> Decl<SqlEntityId> {
    Decl::Existing {
        id: entities[usize::from(slot) % 3],
    }
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u8..3, any::<u8>()).prop_map(|(entity, year)| Op::Existence { entity, year }),
        (0u8..3, any::<u8>()).prop_map(|(entity, year)| Op::Construction { entity, year }),
        (0u8..3, any::<u8>()).prop_map(|(entity, year)| Op::Demolition { entity, year }),
        (0u8..3, any::<u8>()).prop_map(|(entity, year)| Op::PointEvent { entity, year }),
        (0u8..3, any::<u8>(), any::<u8>()).prop_map(|(entity, start, end)| Op::DurationalEvent {
            entity,
            start,
            end
        }),
        (0u8..3, 0u8..3).prop_map(|(a, b)| Op::Merge { a, b }),
        any::<u8>().prop_map(|fact| Op::Retract { fact }),
    ]
}

/// Interpret an op sequence against a fresh store: three named entities, then
/// one commit per op referencing them as `Decl::Existing`. A rejected op is
/// skipped. Returns the entity pool and every committed fact id (retraction
/// targets).
async fn run_ops(
    store: &SqliteFactStore,
    ops: &[Op],
) -> Result<(Vec<SqlEntityId>, Vec<FactId>), TestError> {
    let mut secs: i64 = 0;
    let setup = commit_one(
        store,
        secs,
        vec![Decl::Local; 3],
        Vec::new(),
        vec![
            name_fact(0, "e0")?,
            name_fact(1, "e1")?,
            name_fact(2, "e2")?,
        ],
    )
    .await?;
    secs += 1;

    let entities: Vec<SqlEntityId> = (0..3)
        .map(|i| entity_id(&setup, i))
        .collect::<Result<_, _>>()?;
    let mut fact_ids: Vec<FactId> = setup.fact_ids.clone();

    for op in ops {
        let result = match op {
            Op::Existence { entity, year } => {
                try_commit(
                    store,
                    vec![existing(&entities, *entity)],
                    Vec::new(),
                    secs,
                    vec![existence_fact(0, band(*year))?],
                )
                .await?
            }
            Op::Construction { entity, year } => {
                try_commit(
                    store,
                    vec![existing(&entities, *entity)],
                    Vec::new(),
                    secs,
                    vec![construction_fact(0, band(*year))?],
                )
                .await?
            }
            Op::Demolition { entity, year } => {
                try_commit(
                    store,
                    vec![existing(&entities, *entity)],
                    Vec::new(),
                    secs,
                    vec![demolition_fact(0, band(*year))?],
                )
                .await?
            }
            Op::PointEvent { entity, year } => {
                try_commit(
                    store,
                    vec![existing(&entities, *entity)],
                    vec![Decl::Local],
                    secs,
                    vec![
                        has_event_point_fact(0, 0)?,
                        point_date_fact(0, band(*year))?,
                    ],
                )
                .await?
            }
            Op::DurationalEvent { entity, start, end } => {
                try_commit(
                    store,
                    vec![existing(&entities, *entity)],
                    vec![Decl::Local],
                    secs,
                    vec![
                        has_event_durational_fact(0, 0)?,
                        durational_date_fact(0, DurationalRole::Started, band(*start))?,
                        durational_date_fact(0, DurationalRole::Completed, band(*end))?,
                    ],
                )
                .await?
            }
            Op::Merge { a, b } => {
                if usize::from(*a) % 3 == usize::from(*b) % 3 {
                    None
                } else {
                    try_commit(
                        store,
                        vec![existing(&entities, *a), existing(&entities, *b)],
                        Vec::new(),
                        secs,
                        vec![same_entity_fact(0, 1)?],
                    )
                    .await?
                }
            }
            Op::Retract { fact } => {
                if fact_ids.is_empty() {
                    None
                } else {
                    let target = fact_ids[usize::from(*fact) % fact_ids.len()];
                    try_commit(
                        store,
                        Vec::new(),
                        Vec::new(),
                        secs,
                        vec![retract_fact(target)?],
                    )
                    .await?
                }
            }
        };
        secs += 1;
        if let Some(result) = result {
            fact_ids.extend(result.fact_ids.iter().copied());
        }
    }

    Ok((entities, fact_ids))
}

async fn run_and_check(ops: &[Op]) -> Result<(), TestError> {
    let (store, _dir) = fresh_store().await?;
    let (entities, _facts) = run_ops(&store, ops).await?;
    assert_homomorphism(&store, &entities).await?;
    store.close().await;
    Ok(())
}

fn runtime() -> Result<tokio::runtime::Runtime, TestCaseError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| TestCaseError::fail(format!("runtime: {e}")))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// The witness read matches the projection oracle over random submit
    /// sequences, at every snapshot, for every entity.
    #[test]
    fn conflicts_via_index_matches_projection(ops in prop::collection::vec(arb_op(), 1..=10)) {
        let rt = runtime()?;
        rt.block_on(run_and_check(&ops))
            .map_err(|e| TestCaseError::fail(format!("{e}")))?;
    }
}

// ============================================================================
// Regression fixtures — the two interleavings the read design turns on
// ============================================================================

/// Re-owning an event to a different entity is refused by ownership
/// immutability. An event dated before X's construction floor is owned by X;
/// re-homing it to Y — retracting the old `HasEvent` and adding one for Y in a
/// single commit — is rejected, because an event's `{entity, kind}` is pinned at
/// its first-ever `HasEvent`. The commit does not land, so X keeps the event and
/// the witness read still agrees with the projection at every snapshot.
#[tokio::test]
async fn reowning_an_event_to_a_different_entity_is_rejected() -> Result<(), TestError> {
    let (store, _dir) = fresh_store().await?;

    // X = entity 0, Y = entity 1, both built in 1600.
    let base = commit_one(
        &store,
        0,
        vec![Decl::Local, Decl::Local],
        Vec::new(),
        vec![construction_fact(0, 1600)?, construction_fact(1, 1600)?],
    )
    .await?;
    let x = entity_id(&base, 0)?;
    let y = entity_id(&base, 1)?;

    // A point event dated 1400 (before both floors), owned by X.
    let owned = commit_one(
        &store,
        1,
        vec![Decl::Existing { id: x }],
        vec![Decl::Local],
        vec![has_event_point_fact(0, 0)?, point_date_fact(0, 1400)?],
    )
    .await?;
    let event = event_id(&owned, 0)?;
    let old_edge = has_event_fact_id(&store, event).await?;

    // Attempt to re-home to Y in one commit (retract the old edge, add Y's).
    // The only otherwise-valid rejection reason is ownership immutability, so a
    // refused commit pins that the pin (X) survives the retraction.
    let reowned = try_commit(
        &store,
        vec![Decl::Existing { id: y }],
        vec![Decl::Existing { id: event }],
        2,
        vec![retract_fact(old_edge)?, has_event_point_fact(0, 0)?],
    )
    .await?;
    assert!(
        reowned.is_none(),
        "re-homing an event to a different entity must be rejected by ownership immutability"
    );

    // The event still belongs to X; the witness read agrees with the projection.
    assert_homomorphism(&store, &[x, y]).await?;
    store.close().await;
    Ok(())
}

/// Finding B: retracting only the `HasEvent`, leaving the date fact live. The
/// event's `PointDate` stays in the `event_witness` index but the event is no
/// longer owned, so it is not a witness — the read must gate the date on the
/// live edge, not surface an orphaned date.
#[tokio::test]
async fn orphaned_event_date_matches_projection_at_every_snapshot() -> Result<(), TestError> {
    let (store, _dir) = fresh_store().await?;

    // X built in 1600, with a point event dated 1400 (a conflict while owned).
    let base = commit_one(
        &store,
        0,
        vec![Decl::Local],
        Vec::new(),
        vec![construction_fact(0, 1600)?],
    )
    .await?;
    let x = entity_id(&base, 0)?;

    let owned = commit_one(
        &store,
        1,
        vec![Decl::Existing { id: x }],
        vec![Decl::Local],
        vec![has_event_point_fact(0, 0)?, point_date_fact(0, 1400)?],
    )
    .await?;
    let event = event_id(&owned, 0)?;
    let edge = has_event_fact_id(&store, event).await?;

    // Retract only the HasEvent — the PointDate stays live but orphaned.
    commit_one(&store, 2, Vec::new(), Vec::new(), vec![retract_fact(edge)?]).await?;

    assert_homomorphism(&store, &[x]).await?;
    store.close().await;
    Ok(())
}

/// BCE coverage: the lifetime window and its witnesses straddle year zero, so
/// the value-range scans compare [`num_days_from_ce`] day numbers across the
/// BC/AD boundary — where a stored date *string* would sort backwards. X is
/// built at 50 BCE and demolished at 10 BCE (both negative day numbers); an
/// existence witness at 100 BCE falls below the construction floor and one at
/// 50 CE rises above the demolition ceiling, so each scan fires across zero and
/// the read must still agree with the projection oracle.
///
/// [`num_days_from_ce`]: chrono::Datelike::num_days_from_ce
#[tokio::test]
async fn bce_witnesses_straddle_day_number_zero() -> Result<(), TestError> {
    let (store, _dir) = fresh_store().await?;

    // Floor at 50 BCE, ceiling at 10 BCE.
    let base = commit_one(
        &store,
        0,
        vec![Decl::Local],
        Vec::new(),
        vec![construction_fact(0, -50)?, demolition_fact(0, -10)?],
    )
    .await?;
    let x = entity_id(&base, 0)?;

    // A witness below the floor (100 BCE) and one above the ceiling (50 CE).
    commit_one(
        &store,
        1,
        vec![Decl::Existing { id: x }],
        Vec::new(),
        vec![existence_fact(0, -100)?],
    )
    .await?;
    commit_one(
        &store,
        2,
        vec![Decl::Existing { id: x }],
        Vec::new(),
        vec![existence_fact(0, 50)?],
    )
    .await?;

    assert_homomorphism(&store, &[x]).await?;

    // The oracle agreement above already pins correctness; this confirms the
    // scans genuinely fired across the boundary rather than both coming back
    // empty — one conflict below the floor, one above the ceiling.
    let mut view = store.now().await?;
    let conflicts = view.temporal_conflicts_indexed(x).await?;
    let before = conflicts
        .iter()
        .filter(|c| {
            matches!(
                c.kind,
                TemporalConflictKind::ExistedBeforeConstruction { .. }
            )
        })
        .count();
    let after = conflicts
        .iter()
        .filter(|c| matches!(c.kind, TemporalConflictKind::ExistedAfterDemolition { .. }))
        .count();
    drop(view);
    if (before, after) != (1, 1) {
        return Err(format!(
            "expected one before-construction and one after-demolition BCE conflict, got {conflicts:?}"
        )
        .into());
    }

    store.close().await;
    Ok(())
}
