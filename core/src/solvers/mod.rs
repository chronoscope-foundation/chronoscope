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

use crate::algebra::lattice::JoinSemilattice;
use crate::algebra::monoid::CommutativeMonoid;
use crate::algebra::semiring::{Label, Semiring, Support};
use crate::conflicts::{FactAtom, fact_date, is_construction_start};
use crate::date::{DateBound, UncertainDate};
use crate::grammar::assertions::FactualAssertion;
use crate::grammar::bookend::DemolitionFact;
use crate::grammar::ids::{FactId, IdScheme};
use crate::nonempty::NonEmptyVec;
use crate::projection::{self, Bracket};
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
/// floor ([`ConstructionFact::Started`](crate::grammar::bookend::ConstructionFact::Started), read earliest) and the demolition
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

/// The live temporal witnesses and bookends of an entity's class at one
/// snapshot, grouped for [`conflicts_via_index`].
///
/// The composed-read output the fact store produces by scanning the per-subject
/// witness indexes — keyed by each witness's immutable subject — instead of
/// projecting the whole entity. `construction_starts` / `demolition_completions`
/// are the live bookend facts the floor / ceiling join over; `below_floor` /
/// `above_ceiling` are the value-range violators, each inner group the witnesses
/// the enumeration bundles into one conflict (existence facts grouped by exact
/// date, an event's date facts grouped by event in projection slot order).
#[derive(Debug, Clone, Default)]
pub struct WitnessScan {
    /// The live `ConstructionFact::Started` facts and their dates.
    pub construction_starts: Vec<(FactId, UncertainDate)>,
    /// The live `DemolitionFact::Completed` facts and their dates.
    pub demolition_completions: Vec<(FactId, UncertainDate)>,
    /// Witness groups whose latest instant falls below the construction floor.
    pub below_floor: Vec<Vec<(FactId, UncertainDate)>>,
    /// Witness groups whose earliest instant rises above the demolition ceiling.
    pub above_ceiling: Vec<Vec<(FactId, UncertainDate)>>,
}

/// The entity-level temporal contradictions read straight off the witness
/// indexes — the detect-then-enumerate counterpart of
/// [`temporal_conflicts`] over [`project_entity`](crate::projection::project_entity).
///
/// **Detect:** the floor and ceiling come from the class's bookend facts; a
/// [`WitnessScan`] whose value-range scans found no violator carries no
/// `below_floor` / `above_ceiling` group, so an in-bounds entity yields no
/// conflict without the whole-entity projection. **Enumerate:** each violator
/// group runs today's `bundle_conflicts` against the bound it crossed, sharing
/// one dedup set — so event endpoints unite, disputed bookends are all named, and
/// repeated fact sets collapse exactly as the oracle does. The homomorphism
/// `conflicts_via_index(E, T) == temporal_conflicts(project_entity_at(E, T))` as
/// sets, at every snapshot, is the proptest that guards the two paths from drift.
pub fn conflicts_via_index(scan: &WitnessScan) -> Vec<TemporalConflict> {
    let floor = witness_floor(&scan.construction_starts);
    let ceiling = witness_ceiling(&scan.demolition_completions);
    let mut conflicts: Vec<TemporalConflict> = Vec::new();
    let mut seen: BTreeSet<BTreeSet<FactId>> = BTreeSet::new();
    if let Some(floor) = floor.as_ref() {
        for group in &scan.below_floor {
            bundle_conflicts(&mut conflicts, &mut seen, group, Some(floor), None);
        }
    }
    if let Some(ceiling) = ceiling.as_ref() {
        for group in &scan.above_ceiling {
            bundle_conflicts(&mut conflicts, &mut seen, group, None, Some(ceiling));
        }
    }
    conflicts
}

/// The construction floor from the class's live construction-start facts —
/// [`construction_floor`]'s reading over the raw facts: the extent's earliest
/// instant (the join over every hypothesis) and every start fact that could set
/// it. Absent when no start dates it, or when a start is open below (the join is
/// then unbounded below, exactly as the projected floor bails).
fn witness_floor(starts: &[(FactId, UncertainDate)]) -> Option<LifetimeBound> {
    if starts.is_empty() {
        return None;
    }
    let envelope = UncertainDate::join_all(starts.iter().map(|(_, date)| date.clone()));
    let instant = envelope.earliest()?;
    Some(LifetimeBound {
        instant,
        envelope,
        facts: starts.iter().map(|(id, _)| *id).collect(),
    })
}

/// The demolition ceiling from the class's live demolition-completion facts —
/// [`demolition_ceiling`]'s reading over the raw facts: the extent's latest
/// instant and every completion fact behind it.
fn witness_ceiling(completions: &[(FactId, UncertainDate)]) -> Option<LifetimeBound> {
    if completions.is_empty() {
        return None;
    }
    let envelope = UncertainDate::join_all(completions.iter().map(|(_, date)| date.clone()));
    let instant = envelope.latest()?;
    Some(LifetimeBound {
        instant,
        envelope,
        facts: completions.iter().map(|(id, _)| *id).collect(),
    })
}

/// Inject a derived "built by" bound into an empty construction start. Reads the
/// same witnesses [`temporal_conflicts`] does — existence facts and interior-event
/// dates — derives `construction ≤ earliest-witness` preserving that witness's
/// precision, and folds it into the empty slot with support a premise on the
/// binding witness fact(s).
///
/// Only an empty construction start is touched. An asserted start a witness
/// predates is the detector's alarm, not the producer's; one a witness agrees
/// with makes the derived bound redundant. So no asserted value ever absorbs a
/// witness into its provenance, and the detector is never blinded — the inference
/// only fills a gap, it never rewrites an asserted bookend.
pub fn inject_derived_bounds<R: IdScheme>(entity: &mut CitedEntity<R>) {
    if !entity.construction.started_at.extent.support.is_zero() {
        return;
    }

    let injection: Option<(UncertainDate, Label<FactAtom<R>>)> = {
        // Every witness with a definite upper bound, paired with the precision-
        // keeping bound and the fact it rests on. An open-above witness ("after
        // 1800") floors the start at +∞ — no information — so it contributes none.
        let mut witnesses: Vec<(NaiveDate, DateBound, &FactAtom<R>)> = Vec::new();
        for (date, entry) in &entity.existence {
            if let (Some(latest), Some(bound)) = (date.latest(), date.latest_bound().copied()) {
                for atom in entry.support.atoms() {
                    witnesses.push((latest, bound, atom));
                }
            }
        }
        for entry in entity.events.values() {
            let event = &entry.value;
            for slot in [&event.occurred_at, &event.started_at, &event.completed_at] {
                for atom in slot.extent.support.atoms() {
                    if let Some(date) = fact_date(&atom.fact)
                        && let (Some(latest), Some(bound)) =
                            (date.latest(), date.latest_bound().copied())
                    {
                        witnesses.push((latest, bound, atom));
                    }
                }
            }
        }

        // The earliest witness sets the "built by" bound; every witness tied at it
        // binds the bound jointly, so the support names each one.
        match witnesses.iter().map(|(latest, _, _)| *latest).min() {
            None => None,
            Some(earliest) => {
                let mut derived_bound: Option<DateBound> = None;
                let mut support = Label::empty();
                for (latest, bound, atom) in &witnesses {
                    if *latest == earliest {
                        // Ties share the instant — keep the finest precision.
                        derived_bound = Some(match derived_bound {
                            Some(current) if current.precision() <= bound.precision() => current,
                            _ => *bound,
                        });
                        support = support.plus(Label::premise((*atom).clone()));
                    }
                }
                derived_bound
                    .and_then(|bound| UncertainDate::bounded(None, Some(bound)).ok())
                    .map(|derived| (derived, support))
            }
        }
    };

    if let Some((derived, support)) = injection {
        entity.construction.started_at = entity
            .construction
            .started_at
            .clone()
            .combine(Bracket::from((derived, support)));
    }
}

/// ∃-satisfiability of a derived date constraint against a slot's asserted
/// envelope: `derived ⊓ envelope ≠ ⊥`. Some hypothesis of the envelope can still
/// hold under `derived`. Its unsatisfiable face (`!envelope_satisfies`) is a
/// relational conflict — a witness that no bookend hypothesis admits; its
/// satisfiable face is what an inference producer injects.
fn envelope_satisfies(derived: &UncertainDate, envelope: &UncertainDate) -> bool {
    derived.overlaps(envelope)
}

/// One end of the entity's lifetime window: the bounding instant, the asserted
/// envelope a witness is checked against, and the bookend fact(s) that pin it,
/// so a conflict can name them.
struct LifetimeBound {
    /// The extremum instant — the earliest a construction could have started, or
    /// the latest a demolition could have reached — carried into the conflict kind.
    instant: NaiveDate,
    /// The bookend slot's extent (join): every hypothesis the asserted dates
    /// admit. A witness's derived one-sided bound is checked against it.
    envelope: UncertainDate,
    /// The bookend fact(s) setting the extremum.
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
    (!facts.is_empty()).then(|| LifetimeBound {
        instant,
        envelope: started.extent.value.clone(),
        facts,
    })
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
    (!facts.is_empty()).then(|| LifetimeBound {
        instant,
        envelope: completed.extent.value.clone(),
        facts,
    })
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
            // "construction ≤ this witness" — the bound the witness derives on the
            // start. Unsatisfiable against the asserted envelope exactly when the
            // witness predates every construction hypothesis, i.e. below the floor.
            let derived = UncertainDate::bounded(None, date.latest_bound().copied());
            let below_floor =
                derived.is_ok_and(|derived| !envelope_satisfies(&derived, &floor.envelope));
            if below_floor && let Some(latest) = date.latest() {
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
            // "demolition ≥ this witness" — unsatisfiable against the asserted
            // envelope exactly when the witness outlasts every demolition
            // hypothesis, i.e. above the ceiling.
            let derived = UncertainDate::bounded(date.earliest_bound().copied(), None);
            let above_ceiling =
                derived.is_ok_and(|derived| !envelope_satisfies(&derived, &ceiling.envelope));
            if above_ceiling && let Some(earliest) = date.earliest() {
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
    use crate::grammar::bookend::ConstructionFact;
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
    use crate::typed;

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

    // ---- derived-bound producer ----

    /// A `UsageChanged` point event on entity 0 / event 0, dated `y` — an
    /// interior event whose date is an existence witness (the Colosseum opening).
    fn point_event_at(y: i32) -> Result<Vec<SubmitFact>, Box<dyn std::error::Error>> {
        Ok(vec![
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
                        bound: year(y)?,
                    },
                },
                citation: citation("https://example.com/opening-date")?,
            },
        ])
    }

    /// The one-sided "before `y`" bound the producer derives from a year-`y`
    /// witness — construction ≤ end of `y`, preserving the witness's precision.
    fn before(y: i32) -> Result<UncertainDate, Box<dyn std::error::Error>> {
        Ok(UncertainDate::bounded(
            None,
            year(y)?.latest_bound().copied(),
        )?)
    }

    /// The Colosseum shape: an existence witness at 81 and no construction start.
    /// The producer fills the empty construction slot with the derived "before 81"
    /// bound resting on the witness, and the flatten marks the row inferred.
    #[tokio::test]
    async fn empty_construction_gains_inferred_built_by_from_existence_witness() -> TestResult {
        let store = MemoryFactStore::new();
        let commit = Commit::<MemoryIds> {
            author: CommitAuthor::User(UserId::new("test")),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Local],
            events: Vec::new(),
            images: Vec::new(),
            facts: [existence_at(81)?].into_iter().collect(),
        };
        let result = commit_facts(&store, commit)
            .await
            .map_err(|e| format!("{e:?}"))?;
        let id = result.entities.get(&EntityIdx(0)).ok_or("entity 0")?.id;
        let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
        let (class, mut entity) =
            project_entity::<MemoryFactStore, _, _>(&mut view, id, fact_lineage)
                .await
                .map_err(|e| format!("{e:?}"))?
                .ok_or("known id should project")?;

        let witness = witness_id(&entity).ok_or("existence witness fact")?;
        assert!(
            entity.construction.started_at.extent.support.is_zero(),
            "the construction start is empty before inference"
        );

        inject_derived_bounds::<MemoryIds>(&mut entity);

        let started = &entity.construction.started_at;
        assert_eq!(
            started.extent.value,
            before(81)?,
            "the empty slot gains construction ≤ 81"
        );
        let atoms: BTreeSet<FactId> = started.extent.support.atoms().map(|atom| atom.id).collect();
        assert_eq!(
            atoms,
            BTreeSet::from([witness]),
            "the derived bound rests on the witness fact alone"
        );

        // The flatten surfaces the bound inline on the construction row, marked
        // inferred and sourced to the witness.
        let typed = typed::Entity::parse(&entity, &class);
        let period = typed
            .timeline
            .events()
            .iter()
            .find_map(|event| match &event.detail {
                typed::EventDetail::Constructed { period, .. } => Some(period),
                _ => None,
            })
            .ok_or("a constructed row")?;
        assert_eq!(
            period.started.derivation,
            Some(typed::Derivation::ExistenceWitness),
            "the inferred start is marked derived by the existence-witness rule"
        );
        assert_eq!(
            period.started.possible,
            before(81)?,
            "the inferred row carries the before-81 bound"
        );
        assert_eq!(
            period.started.facts,
            vec![witness],
            "the inferred row names the witness fact"
        );
        assert_eq!(
            period.started.sources.len(),
            1,
            "the inferred row cites the witness source"
        );
        Ok(())
    }

    /// An interior event's date is a witness too: a point event dated 81 with no
    /// construction start infers the same "before 81" bound on its own.
    #[tokio::test]
    async fn interior_event_date_infers_built_by_bound() -> TestResult {
        let store = MemoryFactStore::new();
        let commit = Commit::<MemoryIds> {
            author: CommitAuthor::User(UserId::new("test")),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Local],
            events: vec![Decl::Local],
            images: Vec::new(),
            facts: point_event_at(81)?.into_iter().collect(),
        };
        let mut entity = project(&store, commit).await?;

        let event_date = entity
            .events
            .values()
            .next()
            .ok_or("one event")?
            .value
            .occurred_at
            .extent
            .support
            .atoms()
            .next()
            .ok_or("event date fact")?
            .id;

        inject_derived_bounds::<MemoryIds>(&mut entity);

        let started = &entity.construction.started_at;
        assert_eq!(
            started.extent.value,
            before(81)?,
            "the event date floors the built-by bound"
        );
        let atoms: BTreeSet<FactId> = started.extent.support.atoms().map(|atom| atom.id).collect();
        assert_eq!(
            atoms,
            BTreeSet::from([event_date]),
            "the bound rests on the event's date fact"
        );
        Ok(())
    }

    /// An asserted construction start is the producer's boundary: a witness
    /// consistent with it makes the derived bound redundant, so the asserted slot
    /// stays byte-for-byte untouched and its flattened row carries no derivation.
    #[tokio::test]
    async fn asserted_construction_start_blocks_inference() -> TestResult {
        let store = MemoryFactStore::new();
        let commit = Commit::<MemoryIds> {
            author: CommitAuthor::User(UserId::new("test")),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Local],
            events: Vec::new(),
            images: Vec::new(),
            facts: [existence_at(81)?, construction_started(70)?]
                .into_iter()
                .collect(),
        };
        let result = commit_facts(&store, commit)
            .await
            .map_err(|e| format!("{e:?}"))?;
        let id = result.entities.get(&EntityIdx(0)).ok_or("entity 0")?.id;
        let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
        let (class, mut entity) =
            project_entity::<MemoryFactStore, _, _>(&mut view, id, fact_lineage)
                .await
                .map_err(|e| format!("{e:?}"))?
                .ok_or("known id should project")?;

        let before_inject = entity.construction.started_at.clone();
        inject_derived_bounds::<MemoryIds>(&mut entity);
        assert_eq!(
            entity.construction.started_at, before_inject,
            "an asserted construction start is untouched by inference"
        );

        let typed = typed::Entity::parse(&entity, &class);
        let period = typed
            .timeline
            .events()
            .iter()
            .find_map(|event| match &event.detail {
                typed::EventDetail::Constructed { period, .. } => Some(period),
                _ => None,
            })
            .ok_or("a constructed row")?;
        assert_eq!(
            period.started.derivation, None,
            "an asserted start is not marked derived"
        );
        Ok(())
    }

    /// Witnesses tied at the earliest date all bind the inferred bound: an
    /// existence fact and an interior event both dated 81 leave the derived bound
    /// resting on both facts, so every binder surfaces as provenance.
    #[tokio::test]
    async fn witnesses_tied_at_earliest_all_bind_the_inferred_bound() -> TestResult {
        let store = MemoryFactStore::new();
        let mut facts: BTreeSet<SubmitFact> = BTreeSet::new();
        facts.insert(existence_at(81)?);
        facts.extend(point_event_at(81)?);
        let commit = Commit::<MemoryIds> {
            author: CommitAuthor::User(UserId::new("test")),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Local],
            events: vec![Decl::Local],
            images: Vec::new(),
            facts,
        };
        let mut entity = project(&store, commit).await?;

        let witness = witness_id(&entity).ok_or("existence witness fact")?;
        let event_date = entity
            .events
            .values()
            .next()
            .ok_or("one event")?
            .value
            .occurred_at
            .extent
            .support
            .atoms()
            .next()
            .ok_or("event date fact")?
            .id;

        inject_derived_bounds::<MemoryIds>(&mut entity);

        let atoms: BTreeSet<FactId> = entity
            .construction
            .started_at
            .extent
            .support
            .atoms()
            .map(|atom| atom.id)
            .collect();
        assert_eq!(
            atoms,
            BTreeSet::from([witness, event_date]),
            "both witnesses tied at 81 bind the inferred bound"
        );
        Ok(())
    }

    /// Witnesses tied at one instant but differing precision: the derived bound
    /// keeps the finest, so "built by 81-12-31" wins over "built by 81".
    #[tokio::test]
    async fn tie_break_keeps_the_finest_witness_precision() -> TestResult {
        let store = MemoryFactStore::new();
        let precise_day = SubmitFact::Factual {
            assertion: FactualAssertion::Existence {
                fact: existence::Fact {
                    entity: EntityIdx(0),
                    at: UncertainDate::with_precision(
                        chrono::NaiveDate::from_ymd_opt(81, 12, 31).ok_or("valid day")?,
                        DatePrecision::Day,
                    )?,
                },
            },
            citation: citation("https://example.com/precise-witness")?,
        };
        let commit = Commit::<MemoryIds> {
            author: CommitAuthor::User(UserId::new("test")),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Local],
            events: Vec::new(),
            images: Vec::new(),
            // A year-81 witness ends Dec 31, tying the day-81-12-31 witness.
            facts: [existence_at(81)?, precise_day].into_iter().collect(),
        };
        let mut entity = project(&store, commit).await?;
        inject_derived_bounds::<MemoryIds>(&mut entity);

        let bound = entity
            .construction
            .started_at
            .extent
            .value
            .latest_bound()
            .ok_or("the derived bound has an upper edge")?;
        assert_eq!(
            bound.precision(),
            DatePrecision::Day,
            "the finest-precision tied witness sets the bound's precision"
        );
        Ok(())
    }
}
