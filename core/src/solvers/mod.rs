//! Read-time temporal reasoning over a projected entity.
//!
//! [`temporal_conflicts`] reads an entity's projection and reports existence
//! witnesses that fall outside its lifetime window. A witness is a date the
//! entity provably existed at — an [`Existence`](crate::grammar::assertions::FactualAssertion::Existence)
//! fact, or an interior event's date (an event implies the entity was there).
//! The window runs from the construction start to the demolition: a witness
//! whose latest instant precedes the earliest construction could have begun, or
//! whose earliest instant follows the latest the demolition could have reached,
//! can't jointly hold with that bookend. The contradiction spans two fields —
//! the witness's date and a construction or demolition bookend — so it has no
//! per-field home and surfaces as an entity-level [`TemporalConflict`], distinct
//! from the per-slot disputed consensus a single over-determined date carries.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::algebra::semiring::{Label, Support};
use crate::conflicts::{FactAtom, fact_date};
use crate::date::UncertainDate;
use crate::grammar::assertions::FactualAssertion;
use crate::grammar::bookend::{ConstructionFact, DemolitionFact};
use crate::grammar::ids::{FactId, IdScheme};
use crate::nonempty::NonEmptyVec;
use crate::projection;
use crate::submit::StoredFact;

/// An entity-level temporal contradiction: facts that can't jointly hold.
///
/// The facts are named by [`FactId`] so a consumer can route back to each one;
/// `kind` carries the witness's date and the lifetime bound it crossed, so a
/// display layer can phrase the contradiction in any locale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TemporalConflict {
    /// The facts on the contradiction — a flat, order-insensitive set of the
    /// witness's date fact(s) and the bookend fact(s) they clash with.
    pub facts: NonEmptyVec<FactId>,
    /// Which lifetime bound the witness fell outside, with the dates to render it.
    pub kind: TemporalConflictKind,
}

/// An existence witness landing outside the entity's lifetime window. The witness
/// keeps its [`UncertainDate`] so a renderer honors its precision; the bound is
/// the window's computed extremum — the earliest a construction could have
/// started, or the latest a demolition could have reached — a join over the
/// (possibly disputed) bookend facts, so it rides as a bare instant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TemporalConflictKind {
    /// The witness predates the earliest the construction could have started.
    ExistedBeforeConstruction {
        witness: UncertainDate,
        construction_started: NaiveDate,
    },
    /// The witness outlasts the latest the demolition could have reached.
    ExistedAfterDemolition {
        witness: UncertainDate,
        demolished: NaiveDate,
    },
}

/// An entity projected over its `fact_lineage` support — the whole facts behind
/// each slot ride as [`FactAtom`]s, so a read recovers each field's fighting
/// evidence by id.
type CitedEntity<R> = projection::Entity<
    <R as IdScheme>::Entity,
    <R as IdScheme>::Event,
    <R as IdScheme>::Image,
    Label<FactAtom<R>>,
>;

/// The entity-level temporal contradictions in one entity's projection.
///
/// Bounds existence witnesses by the entity's lifetime window: the construction
/// floor ([`ConstructionFact::Started`], read earliest) and the demolition
/// ceiling ([`DemolitionFact::Completed`], read latest). A witness whose latest
/// instant falls below the floor, or whose earliest instant rises above the
/// ceiling, can't hold with that bookend, and the [`TemporalConflict`] names the
/// witness's date fact(s) together with the bookend fact(s). Two witness kinds
/// share the window: an
/// [`Existence`](crate::grammar::assertions::FactualAssertion::Existence) fact's
/// date, and any interior event's date (an event implies the entity existed
/// then). Conflicts are deduplicated by their [`TemporalConflict::facts`] set, so
/// witnesses reaching the same facts collapse to one. The whole facts ride the
/// `fact_lineage` support, so the pass stays generic over the id scheme.
pub fn temporal_conflicts<R: IdScheme>(entity: &CitedEntity<R>) -> Vec<TemporalConflict> {
    let floor = construction_floor(entity);
    let ceiling = demolition_ceiling(entity);
    if floor.is_none() && ceiling.is_none() {
        return Vec::new();
    }

    let mut conflicts: Vec<TemporalConflict> = Vec::new();
    let mut seen: BTreeSet<BTreeSet<FactId>> = BTreeSet::new();

    // Existence witnesses: each dated attestation the entity was already there,
    // its facts all sharing the one attested date.
    for (date, entry) in &entity.existence {
        let dates: Vec<(FactId, UncertainDate)> = entry
            .support
            .atoms()
            .map(|atom| (atom.id, date.clone()))
            .collect();
        bundle_conflicts(
            &mut conflicts,
            &mut seen,
            &dates,
            floor.as_ref(),
            ceiling.as_ref(),
        );
    }

    // Interior events imply the entity existed at their date — a witness too.
    // Each event's endpoint dates bundle together, so a durational span with
    // both ends outside one bound reads as one conflict, not one per endpoint.
    for entry in entity.events.values() {
        let event = &entry.value;
        let dates: Vec<(FactId, UncertainDate)> =
            [&event.occurred_at, &event.started_at, &event.completed_at]
                .into_iter()
                .flat_map(|slot| slot.extent.support.atoms())
                .filter_map(|atom| fact_date(&atom.fact).map(|date| (atom.id, date)))
                .collect();
        bundle_conflicts(
            &mut conflicts,
            &mut seen,
            &dates,
            floor.as_ref(),
            ceiling.as_ref(),
        );
    }

    conflicts
}

/// One end of the entity's lifetime window: the bounding instant and the
/// bookend fact(s) that pin it, so a conflict can name them.
struct LifetimeBound {
    instant: NaiveDate,
    facts: Vec<FactId>,
}

/// The construction floor: the earliest the entity could have begun under any
/// hypothesis — the extent's earliest instant — paired with the
/// construction-start fact(s) that set it. Reading the extent (join) rather than
/// the consensus (meet) keeps the floor alive when sources dispute the start: two
/// disagreeing starts floor the window at the earlier of them, so a witness that
/// predates every hypothesis still contradicts. Absent when no source dates the
/// start, or when no start fact backs it.
fn construction_floor<R: IdScheme>(entity: &CitedEntity<R>) -> Option<LifetimeBound> {
    let started = &entity.construction.started_at;
    let instant = started.extent.value.earliest()?;
    let facts: Vec<FactId> = started
        .extent
        .support
        .atoms()
        .filter(|atom| is_construction_start(&atom.fact))
        .map(|atom| atom.id)
        .collect();
    (!facts.is_empty()).then_some(LifetimeBound { instant, facts })
}

/// The demolition ceiling: the latest the entity could have persisted to under
/// any hypothesis — the extent's latest instant — paired with the
/// demolition-completion fact(s) that set it. Reading the extent (join) rather
/// than the consensus (meet) keeps the ceiling alive when sources dispute the
/// completion: two disagreeing completions ceiling the window at the later of
/// them, so a witness that outlasts every hypothesis still contradicts. Absent
/// when no source dates the completion, or when no completion fact backs it.
fn demolition_ceiling<R: IdScheme>(entity: &CitedEntity<R>) -> Option<LifetimeBound> {
    let completed = &entity.demolition.completed_at;
    let instant = completed.extent.value.latest()?;
    let facts: Vec<FactId> = completed
        .extent
        .support
        .atoms()
        .filter(|atom| is_demolition_completed(&atom.fact))
        .map(|atom| atom.id)
        .collect();
    (!facts.is_empty()).then_some(LifetimeBound { instant, facts })
}

/// Bundle a witness's out-of-window dates into one conflict per violated bound.
/// An existence witness contributes its single shared date; a durational interior
/// event contributes each endpoint, so a span with both ends below the
/// construction floor names both endpoint facts and the floor in one conflict
/// where testing each endpoint alone would split it in two. The witness date the
/// conflict carries is the one closest to the bound — the latest below the floor,
/// the earliest above the ceiling.
fn bundle_conflicts(
    conflicts: &mut Vec<TemporalConflict>,
    seen: &mut BTreeSet<BTreeSet<FactId>>,
    dates: &[(FactId, UncertainDate)],
    floor: Option<&LifetimeBound>,
    ceiling: Option<&LifetimeBound>,
) {
    if let Some(floor) = floor {
        let mut ids = Vec::new();
        let mut witness: Option<&UncertainDate> = None;
        let mut witness_latest: Option<NaiveDate> = None;
        for (id, date) in dates {
            if let Some(latest) = date.latest()
                && latest < floor.instant
            {
                ids.push(*id);
                if witness_latest.is_none_or(|cur| latest > cur) {
                    witness_latest = Some(latest);
                    witness = Some(date);
                }
            }
        }
        if let Some(witness) = witness {
            push_conflict(
                conflicts,
                seen,
                &ids,
                &floor.facts,
                TemporalConflictKind::ExistedBeforeConstruction {
                    witness: witness.clone(),
                    construction_started: floor.instant,
                },
            );
        }
    }
    if let Some(ceiling) = ceiling {
        let mut ids = Vec::new();
        let mut witness: Option<&UncertainDate> = None;
        let mut witness_earliest: Option<NaiveDate> = None;
        for (id, date) in dates {
            if let Some(earliest) = date.earliest()
                && earliest > ceiling.instant
            {
                ids.push(*id);
                if witness_earliest.is_none_or(|cur| earliest < cur) {
                    witness_earliest = Some(earliest);
                    witness = Some(date);
                }
            }
        }
        if let Some(witness) = witness {
            push_conflict(
                conflicts,
                seen,
                &ids,
                &ceiling.facts,
                TemporalConflictKind::ExistedAfterDemolition {
                    witness: witness.clone(),
                    demolished: ceiling.instant,
                },
            );
        }
    }
}

/// Record one witness-vs-bookend conflict, unless a prior witness already reached
/// the same fact set. `facts` names the witness fact(s) first, then the bookend
/// fact(s) they clash with; the dedup key is the whole set, so the order within it
/// never splits an identical conflict.
fn push_conflict(
    conflicts: &mut Vec<TemporalConflict>,
    seen: &mut BTreeSet<BTreeSet<FactId>>,
    witness_ids: &[FactId],
    bound_ids: &[FactId],
    kind: TemporalConflictKind,
) {
    let Some((first, rest)) = witness_ids.split_first() else {
        return;
    };
    let mut facts = NonEmptyVec::singleton(*first);
    for id in rest {
        facts.push(*id);
    }
    for id in bound_ids {
        facts.push(*id);
    }
    let key: BTreeSet<FactId> = facts.iter().copied().collect();
    if seen.insert(key) {
        conflicts.push(TemporalConflict { facts, kind });
    }
}

/// Whether a stored fact is a [`ConstructionFact::Started`] claim — the fact a
/// temporal conflict names as the floor a witness fell below.
fn is_construction_start<R: IdScheme>(fact: &StoredFact<R>) -> bool {
    matches!(
        fact,
        StoredFact::Factual(f)
            if matches!(
                &f.assertion,
                FactualAssertion::Construction {
                    fact: ConstructionFact::Started { .. },
                }
            )
    )
}

/// Whether a stored fact is a [`DemolitionFact::Completed`] claim — the fact a
/// temporal conflict names as the ceiling a witness rose above.
fn is_demolition_completed<R: IdScheme>(fact: &StoredFact<R>) -> bool {
    matches!(
        fact,
        StoredFact::Factual(f)
            if matches!(
                &f.assertion,
                FactualAssertion::Demolition {
                    fact: DemolitionFact::Completed { .. },
                }
            )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeSet;

    use chrono::{DateTime, TimeZone, Utc};

    use crate::conflicts::fact_lineage;
    use crate::date::{DatePrecision, UncertainDate};
    use crate::grammar::citations::{Excerpt, ExternalSource, FactualCitation};
    use crate::grammar::event::Fact as EventFact;
    use crate::grammar::existence;
    use crate::grammar::ids::UserId;
    use crate::grammar::lifecycle::{DurationalKind, DurationalRole, LifetimeEventKind, PointKind};
    use crate::projection::project_entity;
    use crate::store::FactStore;
    use crate::store::memory::{MemoryFactStore, MemoryIds};
    use crate::submit::{
        Commit, CommitAuthor, Decl, EntityIdx, EventIdx, SubmitFact, commit_facts,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn fixed_time() -> Result<DateTime<Utc>, &'static str> {
        Utc.with_ymd_and_hms(2024, 1, 1, 12, 0, 0)
            .single()
            .ok_or("fixed timestamp is unambiguous")
    }

    fn citation(url: &str) -> Result<FactualCitation, Box<dyn std::error::Error>> {
        let source = ExternalSource::Url {
            url: url::Url::parse(url)?,
            published: None,
        };
        Ok(FactualCitation::new(source, vec![Excerpt::new("source")?])?)
    }

    fn year(y: i32) -> Result<UncertainDate, Box<dyn std::error::Error>> {
        Ok(UncertainDate::with_precision(
            chrono::NaiveDate::from_ymd_opt(y, 1, 1).ok_or("valid year")?,
            DatePrecision::Year,
        )?)
    }

    /// Project an entity's `SameEntity` class over `fact_lineage`, resolving its
    /// minted id off the commit result.
    async fn project(
        store: &MemoryFactStore,
        commit: Commit<MemoryIds>,
    ) -> Result<CitedEntity<MemoryIds>, Box<dyn std::error::Error>> {
        let result = commit_facts(store, commit)
            .await
            .map_err(|e| format!("{e:?}"))?;
        let id = result.entities.get(&EntityIdx(0)).ok_or("entity 0")?.id;
        let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
        let (_, entity) = project_entity::<MemoryFactStore, _, _>(&mut view, id, fact_lineage)
            .await
            .map_err(|e| format!("{e:?}"))?
            .ok_or("known id should project")?;
        Ok(entity)
    }

    /// A construction start fact for entity 0, dated `y`.
    fn construction_started(y: i32) -> Result<SubmitFact, Box<dyn std::error::Error>> {
        Ok(SubmitFact::Factual {
            assertion: FactualAssertion::Construction {
                fact: ConstructionFact::Started {
                    entity: EntityIdx(0),
                    bound: year(y)?,
                },
            },
            citation: citation("https://example.com/construction")?,
        })
    }

    /// An existence witness for entity 0, dated `y`.
    fn existence_at(y: i32) -> Result<SubmitFact, Box<dyn std::error::Error>> {
        Ok(SubmitFact::Factual {
            assertion: FactualAssertion::Existence {
                fact: existence::Fact {
                    entity: EntityIdx(0),
                    at: year(y)?,
                },
            },
            citation: citation("https://example.com/witness")?,
        })
    }

    /// A demolition completion fact for entity 0, dated `y` — the shape P576
    /// takes on ingestion.
    fn demolition_completed(y: i32) -> Result<SubmitFact, Box<dyn std::error::Error>> {
        Ok(SubmitFact::Factual {
            assertion: FactualAssertion::Demolition {
                fact: DemolitionFact::Completed {
                    entity: EntityIdx(0),
                    bound: year(y)?,
                },
            },
            citation: citation("https://example.com/demolition")?,
        })
    }

    /// A durational `Modified` event on entity 0 / event 0, spanning
    /// `started`–`completed`, as its `HasEvent` kind and two `DurationalDate`
    /// endpoints.
    fn modified_span(
        started: i32,
        completed: i32,
    ) -> Result<Vec<SubmitFact>, Box<dyn std::error::Error>> {
        Ok(vec![
            SubmitFact::Factual {
                assertion: FactualAssertion::Event {
                    fact: EventFact::HasEvent {
                        entity: EntityIdx(0),
                        event: EventIdx(0),
                        kind: LifetimeEventKind::Durational {
                            kind: DurationalKind::Modified,
                        },
                    },
                },
                citation: citation("https://example.com/modified")?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Event {
                    fact: EventFact::DurationalDate {
                        event: EventIdx(0),
                        role: DurationalRole::Started,
                        bound: year(started)?,
                    },
                },
                citation: citation("https://example.com/modified-start")?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Event {
                    fact: EventFact::DurationalDate {
                        event: EventIdx(0),
                        role: DurationalRole::Completed,
                        bound: year(completed)?,
                    },
                },
                citation: citation("https://example.com/modified-end")?,
            },
        ])
    }

    /// The construction-start fact id and the existence witness id off a
    /// projection, for asserting a conflict names both.
    fn started_id(entity: &CitedEntity<MemoryIds>) -> Option<FactId> {
        entity
            .construction
            .started_at
            .extent
            .support
            .atoms()
            .find(|atom| is_construction_start(&atom.fact))
            .map(|atom| atom.id)
    }

    fn witness_id(entity: &CitedEntity<MemoryIds>) -> Option<FactId> {
        entity
            .existence
            .values()
            .next()
            .and_then(|entry| entry.support.atoms().next())
            .map(|atom| atom.id)
    }

    /// The demolition-completion fact id off a projection, for asserting a
    /// conflict names it alongside the witness.
    fn demolished_id(entity: &CitedEntity<MemoryIds>) -> Option<FactId> {
        entity
            .demolition
            .completed_at
            .extent
            .support
            .atoms()
            .find(|atom| is_demolition_completed(&atom.fact))
            .map(|atom| atom.id)
    }

    /// An existence witness dated before the construction start can't hold with
    /// it: the entity is attested present before it was built.
    #[tokio::test]
    async fn existence_witness_before_construction_start_is_a_temporal_conflict() -> TestResult {
        let store = MemoryFactStore::new();
        let commit = Commit::<MemoryIds> {
            author: CommitAuthor::User(UserId::new("test")),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Local],
            events: Vec::new(),
            images: Vec::new(),
            // Notre-Dame's shape: a 1160 founding witness, a 1163 build start.
            facts: [existence_at(1160)?, construction_started(1163)?]
                .into_iter()
                .collect(),
        };
        let entity = project(&store, commit).await?;

        let started = started_id(&entity).ok_or("construction start fact")?;
        let witness = witness_id(&entity).ok_or("existence witness fact")?;

        let conflicts = temporal_conflicts::<MemoryIds>(&entity);
        assert_eq!(
            conflicts.len(),
            1,
            "the witness-before-start pair conflicts"
        );
        let named: BTreeSet<FactId> = conflicts
            .first()
            .ok_or("one conflict")?
            .facts
            .iter()
            .copied()
            .collect();
        assert_eq!(
            named,
            BTreeSet::from([witness, started]),
            "the conflict names the witness fact and the construction-start fact"
        );
        match &conflicts.first().ok_or("one conflict")?.kind {
            TemporalConflictKind::ExistedBeforeConstruction {
                witness,
                construction_started,
            } => {
                assert_eq!(
                    witness.earliest(),
                    NaiveDate::from_ymd_opt(1160, 1, 1),
                    "the kind carries the witness's own 1160 date"
                );
                assert_eq!(
                    *construction_started,
                    NaiveDate::from_ymd_opt(1163, 1, 1).ok_or("valid date")?,
                    "the kind carries the 1163 construction floor it crossed"
                );
            }
            other => {
                return Err(format!("expected a before-construction kind, got {other:?}").into());
            }
        }
        Ok(())
    }

    /// An existence witness dated after the construction start sits inside the
    /// lifetime window, so nothing conflicts.
    #[tokio::test]
    async fn existence_witness_after_construction_start_yields_no_conflict() -> TestResult {
        let store = MemoryFactStore::new();
        let commit = Commit::<MemoryIds> {
            author: CommitAuthor::User(UserId::new("test")),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Local],
            events: Vec::new(),
            images: Vec::new(),
            facts: [existence_at(1200)?, construction_started(1163)?]
                .into_iter()
                .collect(),
        };
        let entity = project(&store, commit).await?;

        assert!(
            temporal_conflicts::<MemoryIds>(&entity).is_empty(),
            "a witness inside the lifetime window is no contradiction"
        );
        Ok(())
    }

    /// An existence witness with no construction start to floor it raises no
    /// conflict — a lone witness dates presence, nothing contradicts it.
    #[tokio::test]
    async fn existence_witness_without_construction_start_yields_no_conflict() -> TestResult {
        let store = MemoryFactStore::new();
        let commit = Commit::<MemoryIds> {
            author: CommitAuthor::User(UserId::new("test")),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Local],
            events: Vec::new(),
            images: Vec::new(),
            facts: [existence_at(80)?].into_iter().collect(),
        };
        let entity = project(&store, commit).await?;

        assert!(
            temporal_conflicts::<MemoryIds>(&entity).is_empty(),
            "a witness with no construction start is the Colosseum shape — no conflict"
        );
        Ok(())
    }

    /// The Colosseum shape: a construction start and an interior point event
    /// dated a year before it — each fact fine alone, jointly impossible. An
    /// interior event's date is an existence witness too.
    #[tokio::test]
    async fn event_before_construction_start_is_a_temporal_conflict() -> TestResult {
        let store = MemoryFactStore::new();
        let commit = Commit::<MemoryIds> {
            author: CommitAuthor::User(UserId::new("test")),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Local],
            events: vec![Decl::Local],
            images: Vec::new(),
            facts: [
                construction_started(82)?,
                SubmitFact::Factual {
                    assertion: FactualAssertion::Event {
                        fact: EventFact::HasEvent {
                            entity: EntityIdx(0),
                            event: EventIdx(0),
                            kind: LifetimeEventKind::Point {
                                kind: PointKind::UsageChanged,
                            },
                        },
                    },
                    citation: citation("https://example.com/opening")?,
                },
                SubmitFact::Factual {
                    assertion: FactualAssertion::Event {
                        fact: EventFact::PointDate {
                            event: EventIdx(0),
                            bound: year(81)?,
                        },
                    },
                    citation: citation("https://example.com/opening-date")?,
                },
            ]
            .into_iter()
            .collect(),
        };
        let entity = project(&store, commit).await?;

        let started = started_id(&entity).ok_or("construction start fact")?;
        let event = entity.events.values().next().ok_or("one event")?;
        let event_date_id = event
            .value
            .occurred_at
            .extent
            .support
            .atoms()
            .next()
            .ok_or("event date fact")?
            .id;

        let conflicts = temporal_conflicts::<MemoryIds>(&entity);
        assert_eq!(conflicts.len(), 1, "the event-before-start pair conflicts");
        let named: BTreeSet<FactId> = conflicts
            .first()
            .ok_or("one conflict")?
            .facts
            .iter()
            .copied()
            .collect();
        assert_eq!(
            named,
            BTreeSet::from([event_date_id, started]),
            "the conflict names the event's date fact and the construction-start fact"
        );
        Ok(())
    }

    /// Chioggia Cathedral's shape: a 1623 demolition (P576) and a 1633 existence
    /// witness (P571), no construction. The witness sits after the entity was
    /// demolished, so the two can't jointly hold.
    #[tokio::test]
    async fn existence_witness_after_demolition_is_a_temporal_conflict() -> TestResult {
        let store = MemoryFactStore::new();
        let commit = Commit::<MemoryIds> {
            author: CommitAuthor::User(UserId::new("test")),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Local],
            events: Vec::new(),
            images: Vec::new(),
            facts: [existence_at(1633)?, demolition_completed(1623)?]
                .into_iter()
                .collect(),
        };
        let entity = project(&store, commit).await?;

        let demolished = demolished_id(&entity).ok_or("demolition fact")?;
        let witness = witness_id(&entity).ok_or("existence witness fact")?;

        let conflicts = temporal_conflicts::<MemoryIds>(&entity);
        assert_eq!(
            conflicts.len(),
            1,
            "the witness-after-demolition pair conflicts"
        );
        let named: BTreeSet<FactId> = conflicts
            .first()
            .ok_or("one conflict")?
            .facts
            .iter()
            .copied()
            .collect();
        assert_eq!(
            named,
            BTreeSet::from([witness, demolished]),
            "the conflict names the witness fact and the demolition fact"
        );
        match &conflicts.first().ok_or("one conflict")?.kind {
            TemporalConflictKind::ExistedAfterDemolition {
                witness,
                demolished,
            } => {
                assert_eq!(
                    witness.earliest(),
                    NaiveDate::from_ymd_opt(1633, 1, 1),
                    "the kind carries the witness's own 1633 date"
                );
                assert_eq!(
                    *demolished,
                    NaiveDate::from_ymd_opt(1623, 12, 31).ok_or("valid date")?,
                    "the kind carries the 1623 demolition ceiling it outlasted"
                );
            }
            other => return Err(format!("expected an after-demolition kind, got {other:?}").into()),
        }
        Ok(())
    }

    /// An existence witness dated inside the lifetime window — after the
    /// construction start, before the demolition — contradicts neither bookend.
    #[tokio::test]
    async fn existence_witness_within_lifetime_window_yields_no_conflict() -> TestResult {
        let store = MemoryFactStore::new();
        let commit = Commit::<MemoryIds> {
            author: CommitAuthor::User(UserId::new("test")),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Local],
            events: Vec::new(),
            images: Vec::new(),
            facts: [
                construction_started(1600)?,
                existence_at(1650)?,
                demolition_completed(1700)?,
            ]
            .into_iter()
            .collect(),
        };
        let entity = project(&store, commit).await?;

        assert!(
            temporal_conflicts::<MemoryIds>(&entity).is_empty(),
            "a witness between construction and demolition is no contradiction"
        );
        Ok(())
    }

    /// A durational interior event dated entirely before the construction start —
    /// both endpoints below the floor — is one conflict, not one per endpoint. The
    /// two out-of-window endpoint facts and the start fact bundle into a single
    /// contradiction.
    #[tokio::test]
    async fn durational_event_before_construction_is_one_conflict() -> TestResult {
        let store = MemoryFactStore::new();
        let mut facts: BTreeSet<SubmitFact> = BTreeSet::new();
        facts.insert(construction_started(1100)?);
        facts.extend(modified_span(1050, 1055)?);
        let commit = Commit::<MemoryIds> {
            author: CommitAuthor::User(UserId::new("test")),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Local],
            events: vec![Decl::Local],
            images: Vec::new(),
            facts,
        };
        let entity = project(&store, commit).await?;

        let conflicts = temporal_conflicts::<MemoryIds>(&entity);
        assert_eq!(
            conflicts.len(),
            1,
            "both endpoints before construction bundle into one conflict, got {conflicts:?}"
        );
        assert_eq!(
            conflicts.first().ok_or("one conflict")?.facts.len().get(),
            3,
            "the conflict names both endpoint facts and the construction start"
        );
        Ok(())
    }

    /// Two disputed construction starts (1900, 1901) collapse the consensus to ⊥,
    /// but the extent still floors the window at the earlier hypothesis (1900). A
    /// witness at 1700 predates every hypothesis, so it contradicts them all — one
    /// conflict naming the witness and both start facts.
    #[tokio::test]
    async fn witness_before_every_disputed_construction_start_is_one_conflict() -> TestResult {
        let store = MemoryFactStore::new();
        let commit = Commit::<MemoryIds> {
            author: CommitAuthor::User(UserId::new("test")),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Local],
            events: Vec::new(),
            images: Vec::new(),
            facts: [
                construction_started(1900)?,
                construction_started(1901)?,
                existence_at(1700)?,
            ]
            .into_iter()
            .collect(),
        };
        let entity = project(&store, commit).await?;

        let starts: BTreeSet<FactId> = entity
            .construction
            .started_at
            .extent
            .support
            .atoms()
            .filter(|atom| is_construction_start(&atom.fact))
            .map(|atom| atom.id)
            .collect();
        assert_eq!(
            starts.len(),
            2,
            "both disputed starts survive in the extent"
        );
        let witness = witness_id(&entity).ok_or("existence witness fact")?;

        let conflicts = temporal_conflicts::<MemoryIds>(&entity);
        assert_eq!(
            conflicts.len(),
            1,
            "the witness before every start is one conflict, got {conflicts:?}"
        );
        let named: BTreeSet<FactId> = conflicts
            .first()
            .ok_or("one conflict")?
            .facts
            .iter()
            .copied()
            .collect();
        let mut expected = starts;
        expected.insert(witness);
        assert_eq!(
            named, expected,
            "the conflict names the witness and both disputed construction starts"
        );
        Ok(())
    }
}
