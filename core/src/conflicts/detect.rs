//! The projection-layer conflict detector: walk a projected entity's
//! restrictive date slots, and for each whose consensus bottomed out, enumerate
//! the minimal fighting fact sets and emit one [`ConflictReport`] apiece.
//!
//! This is the first consumer of the conflict model. It runs over a projection
//! whose provenance atom carries the whole stored fact ([`CitedFact`]), so the
//! detector is a pure function of the projected entity — the fighting dates read
//! straight off each slot's consensus support, with no separate fact-map lookup.

use crate::algebra::semiring::Lineage;
use crate::date::UncertainDate;
use crate::grammar::assertions::FactualAssertion;
use crate::grammar::ids::{FactId, IdScheme};
use crate::grammar::{bookend, event};
use crate::location::ConflictStatus;
use crate::nonempty::NonEmptyVec;
use crate::projection::{Bracket, ConsensusConflict, Entity};
use crate::submit::StoredFact;

use super::minimize::minimize;
use super::report::{
    AnyConflictReport, BookendEndpoint, ConflictLocation, ConflictPath, ConflictReport,
    EventEndpoint,
};

/// The provenance atom for the conflict projection: one whole stored fact,
/// carried beside its id so the detector reads the fighting dates straight off a
/// slot's support.
///
/// Identity is the [`FactId`] alone — fact ids are unique, so keying on the id
/// dedups a fact to one atom in the [`Lineage`] set and spares [`StoredFact`] an
/// `Ord`/`Hash` it doesn't carry.
#[derive(Debug, Clone)]
pub struct CitedFact<R: IdScheme> {
    /// The fact's id — the atom's whole identity.
    pub id: FactId,
    /// The stored fact the id names.
    pub fact: StoredFact<R>,
}

impl<R: IdScheme> PartialEq for CitedFact<R> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl<R: IdScheme> Eq for CitedFact<R> {}

impl<R: IdScheme> PartialOrd for CitedFact<R> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<R: IdScheme> Ord for CitedFact<R> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.id.cmp(&other.id)
    }
}

impl<R: IdScheme> std::hash::Hash for CitedFact<R> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// The provenance closure for a conflict-carrying projection: the fact's atom is
/// the singleton `{(id, fact)}`. A caller passes this to
/// [`project_entity`](crate::projection::project_entity) to get a projection whose
/// slot support carries the whole fighting facts, which [`detect_conflicts`] then
/// reads without a second store round-trip.
pub fn cited_lineage<R: IdScheme>(
    fact_id: &FactId,
    _subject: &R::Entity,
    fact: &StoredFact<R>,
) -> Lineage<CitedFact<R>> {
    Lineage::Of(
        [CitedFact {
            id: *fact_id,
            fact: fact.clone(),
        }]
        .into_iter()
        .collect(),
    )
}

/// The date a bookend or event date fact carries. The detector needs each
/// fighting fact's own interval back: the consensus meet reports only that a slot
/// bottomed out, not which facts' intervals are mutually disjoint.
///
/// Reading it side-blind (started vs completed) is sound because the projection
/// already sorts the facts by side — each routes to its own endpoint slot, so a
/// slot's support holds one side only, and the caller is always inside one slot.
fn fact_date<R: IdScheme>(fact: &StoredFact<R>) -> Option<UncertainDate> {
    let StoredFact::Factual(f) = fact else {
        return None;
    };
    let bound = match &f.assertion {
        FactualAssertion::Construction {
            fact:
                bookend::ConstructionFact::Started { bound, .. }
                | bookend::ConstructionFact::Completed { bound, .. },
        }
        | FactualAssertion::Demolition {
            fact:
                bookend::DemolitionFact::Started { bound, .. }
                | bookend::DemolitionFact::Completed { bound, .. },
        }
        | FactualAssertion::Event {
            fact: event::Fact::DurationalDate { bound, .. } | event::Fact::PointDate { bound, .. },
        } => bound,
        _ => return None,
    };
    Some(bound.clone())
}

/// A projected entity whose slot support carries whole stored facts ([`CitedFact`])
/// — the projection shape [`detect_conflicts`] reads conflicts off directly.
pub type CitedEntity<R> = Entity<
    <R as IdScheme>::Entity,
    <R as IdScheme>::Event,
    <R as IdScheme>::Image,
    Lineage<CitedFact<R>>,
>;

/// The date conflicts in a projected entity — pure over the projection.
///
/// The fold has already done the detecting: each slot's consensus meet bottomed
/// out where sources disagreed, and its support unioned the facts that fought.
/// This reads that off, adding the two things the fold doesn't keep — the
/// structural path to each slot, and the split of a slot's fighting facts into
/// minimal sets (the meet retains the ⊥, not which subsets caused it).
pub fn detect_conflicts<R: IdScheme>(
    entity_id: &R::Entity,
    entity: &CitedEntity<R>,
) -> Vec<AnyConflictReport<R::Entity, R::Event>> {
    let mut reports = Vec::new();

    let bookend_slots = [
        (
            ConflictPath::Construction {
                endpoint: BookendEndpoint::Started,
            },
            &entity.construction.started_at,
        ),
        (
            ConflictPath::Construction {
                endpoint: BookendEndpoint::Completed,
            },
            &entity.construction.completed_at,
        ),
        (
            ConflictPath::Demolition {
                endpoint: BookendEndpoint::Started,
            },
            &entity.demolition.started_at,
        ),
        (
            ConflictPath::Demolition {
                endpoint: BookendEndpoint::Completed,
            },
            &entity.demolition.completed_at,
        ),
    ];
    for (path, bracket) in bookend_slots {
        collect_slot(entity_id, path, bracket, &mut reports);
    }

    for (event_id, entry) in &entity.events {
        let record = &entry.value;
        let event_slots = [
            (EventEndpoint::Started, &record.started_at),
            (EventEndpoint::Completed, &record.completed_at),
            (EventEndpoint::Occurred, &record.occurred_at),
        ];
        for (endpoint, bracket) in event_slots {
            let path = ConflictPath::EventDate {
                event: event_id.clone(),
                endpoint,
            };
            collect_slot(entity_id, path, bracket, &mut reports);
        }
    }

    reports
}

/// Emit the reports for one date slot: nothing unless its consensus bottomed out,
/// then one report per minimal fighting set over the facts backing the slot.
fn collect_slot<R: IdScheme>(
    entity_id: &R::Entity,
    path: ConflictPath<R::Event>,
    bracket: &Bracket<UncertainDate, Lineage<CitedFact<R>>>,
    reports: &mut Vec<AnyConflictReport<R::Entity, R::Event>>,
) {
    if bracket.consensus.value.conflict() != ConflictStatus::Conflict {
        return;
    }
    let premises: Vec<(FactId, UncertainDate)> = bracket
        .consensus
        .support
        .iter()
        .filter_map(|cited| fact_date(&cited.fact).map(|date| (cited.id, date)))
        .collect();

    for contributing in minimal_fighting_sets(&premises) {
        let location = ConflictLocation {
            entity: entity_id.clone(),
            path: path.clone(),
        };
        reports.push(AnyConflictReport::Date(ConflictReport::date_conflict(
            location,
            contributing,
        )));
    }
}

/// The minimal fighting sets among a slot's date premises, whose joint meet is
/// already ⊥.
///
/// Pairwise first: any two premises whose meet is empty are a minimal fighting
/// set on their own. For single-interval dates this is complete — Helly's theorem
/// in one dimension forces an empty total intersection to already contain a
/// disjoint pair — so every real projection resolves here.
///
/// The [`minimize`] backstop fires only when no pair is disjoint yet the whole
/// slot still conflicts: multi-interval (disjunctive) values, where three or more
/// premises can pairwise overlap and meet to empty only together. It returns the
/// one irreducible set. The two paths never both fire, so the result is dup-free.
fn minimal_fighting_sets(premises: &[(FactId, UncertainDate)]) -> Vec<NonEmptyVec<FactId>> {
    let mut sets: Vec<NonEmptyVec<FactId>> = Vec::new();
    for (i, (id_a, date_a)) in premises.iter().enumerate() {
        for (id_b, date_b) in premises.iter().skip(i + 1) {
            if !date_a.overlaps(date_b) {
                let mut pair = NonEmptyVec::singleton(*id_a);
                pair.push(*id_b);
                sets.push(pair);
            }
        }
    }
    if sets.is_empty()
        && let Some(set) = minimize(premises)
    {
        sets.push(set);
    }
    sets
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeSet;

    use crate::date::DatePrecision;
    use crate::grammar::citations::{Excerpt, ExternalSource, FactualCitation};
    use crate::grammar::ids::UserId;
    use crate::grammar::lifecycle::{LifetimeEventKind, PointKind};
    use crate::projection::project_entity;
    use crate::store::memory::{MemoryFactStore, MemoryIds};
    use crate::store::{EntityIdOf, EventIdOf, FactStore};
    use crate::submit::{
        Commit, CommitAuthor, Decl, EntityIdx, EventIdx, SubmitFact, SubmitResult, commit_facts,
    };
    use chrono::TimeZone;
    use url::Url;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type MemEntId = EntityIdOf<MemoryFactStore>;
    type MemEvtId = EventIdOf<MemoryFactStore>;

    fn fixed_time() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc
            .with_ymd_and_hms(2024, 1, 1, 12, 0, 0)
            .single()
            .unwrap_or_default()
    }

    fn year(y: i32) -> Result<UncertainDate, Box<dyn std::error::Error>> {
        Ok(UncertainDate::with_precision(
            chrono::NaiveDate::from_ymd_opt(y, 1, 1).ok_or("date")?,
            DatePrecision::Year,
        )?)
    }

    /// A factual citation distinguished by source url, so two same-shaped date
    /// claims from different sources stay distinct stored facts.
    fn citation(url: &str) -> Result<FactualCitation, Box<dyn std::error::Error>> {
        Ok(FactualCitation::new(
            ExternalSource::Url {
                url: Url::parse(url)?,
                published: None,
            },
            vec![Excerpt::new("source-text")?],
        )?)
    }

    fn construction_started(
        entity: usize,
        bound: UncertainDate,
        url: &str,
    ) -> Result<SubmitFact, Box<dyn std::error::Error>> {
        Ok(SubmitFact::Factual {
            assertion: FactualAssertion::Construction {
                fact: bookend::ConstructionFact::Started {
                    entity: EntityIdx(entity),
                    bound,
                },
            },
            citation: citation(url)?,
        })
    }

    fn has_point_event(
        entity: usize,
        event: usize,
    ) -> Result<SubmitFact, Box<dyn std::error::Error>> {
        Ok(SubmitFact::Factual {
            assertion: FactualAssertion::Event {
                fact: event::Fact::HasEvent {
                    entity: EntityIdx(entity),
                    event: EventIdx(event),
                    kind: LifetimeEventKind::Point {
                        kind: PointKind::Designated,
                    },
                },
            },
            citation: citation("https://example.com/has-event")?,
        })
    }

    fn point_date(
        event: usize,
        bound: UncertainDate,
        url: &str,
    ) -> Result<SubmitFact, Box<dyn std::error::Error>> {
        Ok(SubmitFact::Factual {
            assertion: FactualAssertion::Event {
                fact: event::Fact::PointDate {
                    event: EventIdx(event),
                    bound,
                },
            },
            citation: citation(url)?,
        })
    }

    async fn submit(
        store: &MemoryFactStore,
        entities: usize,
        events: usize,
        facts: Vec<SubmitFact>,
    ) -> Result<SubmitResult<MemoryIds>, Box<dyn std::error::Error>> {
        let bundle: Commit<MemoryIds> = Commit {
            author: CommitAuthor::User(UserId::new("alice")),
            recorded_at: fixed_time(),
            entities: (0..entities).map(|_| Decl::Local).collect(),
            events: (0..events).map(|_| Decl::Local).collect(),
            images: Vec::new(),
            facts: facts.into_iter().collect(),
        };
        commit_facts(store, bundle)
            .await
            .map_err(|e| format!("{e:?}").into())
    }

    /// Project an entity's class through the conflict-carrying lineage.
    async fn project(
        store: &MemoryFactStore,
        id: MemEntId,
    ) -> Result<CitedEntity<MemoryIds>, Box<dyn std::error::Error>> {
        let view = store.now().await.map_err(|e| format!("{e:?}"))?;
        let (_, entity) = project_entity::<MemoryFactStore, _, _>(&view, id, cited_lineage)
            .await?
            .ok_or("known id should project")?;
        Ok(entity)
    }

    fn date_report(
        report: &AnyConflictReport<MemEntId, MemEvtId>,
    ) -> &ConflictReport<super::super::report::DateConflict, MemEntId, MemEvtId> {
        match report {
            AnyConflictReport::Date(r) => r,
        }
    }

    // ------------------------------------------------------------------
    // Competing bookend dates → one date conflict at the Started slot
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn competing_construction_dates_conflict_at_the_started_slot() -> TestResult {
        let store = MemoryFactStore::new();
        // Two distinct sources place the start in disjoint years — the consensus
        // meet empties, the over-determined slot.
        let result = submit(
            &store,
            1,
            0,
            vec![
                construction_started(0, year(1887)?, "https://a.example/src")?,
                construction_started(0, year(1889)?, "https://b.example/src")?,
            ],
        )
        .await?;
        let id = result.entities.get(&EntityIdx(0)).ok_or("missing")?.id;

        let entity = project(&store, id).await?;
        let reports = detect_conflicts::<MemoryIds>(&id, &entity);

        assert_eq!(reports.len(), 1, "one over-determined slot, one report");
        let report = date_report(reports.first().ok_or("no report")?);
        assert_eq!(
            report.location.path,
            ConflictPath::Construction {
                endpoint: BookendEndpoint::Started,
            },
            "the conflict anchors at the construction-started slot"
        );

        // The two Started facts are the whole commit, so the minted fact ids are
        // exactly the fighting set.
        let expected: BTreeSet<FactId> = result.fact_ids.iter().copied().collect();
        let contributing: BTreeSet<FactId> = report.data.contributing.iter().copied().collect();
        assert_eq!(contributing, expected, "both start claims fight");

        assert_eq!(report.resolutions.len(), 2, "one retract per contributor");
        let retracted: BTreeSet<FactId> = report
            .resolutions
            .iter()
            .filter_map(|r| match r {
                super::super::report::Resolution::Retract { fact } => Some(*fact),
                super::super::report::Resolution::Custom { .. } => None,
            })
            .collect();
        assert_eq!(retracted, expected, "each contributor is retractable");
        Ok(())
    }

    // ------------------------------------------------------------------
    // A consistent slot → no report
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn agreeing_construction_dates_produce_no_report() -> TestResult {
        let store = MemoryFactStore::new();
        // Two sources agree on the same year — the meet is that year, consistent.
        let result = submit(
            &store,
            1,
            0,
            vec![
                construction_started(0, year(1887)?, "https://a.example/src")?,
                construction_started(0, year(1887)?, "https://b.example/src")?,
            ],
        )
        .await?;
        let id = result.entities.get(&EntityIdx(0)).ok_or("missing")?.id;

        let entity = project(&store, id).await?;
        let reports = detect_conflicts::<MemoryIds>(&id, &entity);
        assert!(reports.is_empty(), "an agreed date is no conflict");
        Ok(())
    }

    // ------------------------------------------------------------------
    // Coverage: a bookend conflict AND an event date conflict in one entity
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn bookend_and_event_slots_each_report_their_own_conflict() -> TestResult {
        let store = MemoryFactStore::new();
        let result = submit(
            &store,
            1,
            1,
            vec![
                construction_started(0, year(1887)?, "https://a.example/src")?,
                construction_started(0, year(1889)?, "https://b.example/src")?,
                has_point_event(0, 0)?,
                point_date(0, year(1950)?, "https://c.example/src")?,
                point_date(0, year(1960)?, "https://d.example/src")?,
            ],
        )
        .await?;
        let id = result.entities.get(&EntityIdx(0)).ok_or("missing")?.id;

        let entity = project(&store, id).await?;
        let reports = detect_conflicts::<MemoryIds>(&id, &entity);

        assert_eq!(reports.len(), 2, "one report per over-determined slot");
        let has_construction = reports.iter().any(|report| {
            matches!(
                date_report(report).location.path,
                ConflictPath::Construction {
                    endpoint: BookendEndpoint::Started,
                }
            )
        });
        let has_event = reports.iter().any(|report| {
            matches!(
                date_report(report).location.path,
                ConflictPath::EventDate {
                    endpoint: EventEndpoint::Occurred,
                    ..
                }
            )
        });
        assert!(has_construction, "the construction slot conflict surfaces");
        assert!(has_event, "the event occurred-at conflict surfaces");
        Ok(())
    }

    // ------------------------------------------------------------------
    // Multi-interval backstop — three disjunctive dates that overlap
    // pairwise but meet to empty. Unreachable through the real projection
    // (submit stores only single intervals; Helly makes pairwise complete),
    // so it exercises `minimal_fighting_sets` on hand-built disjunctions.
    // ------------------------------------------------------------------

    #[test]
    fn pairwise_overlapping_disjunctions_fall_through_to_minimize() -> TestResult {
        // A = {1000, 2000}, B = {2000, 3000}, C = {1000, 3000}: each pair shares
        // one year (overlaps), all three share none (meet empty). No disjoint
        // pair exists, so the backstop must attribute all three together.
        let a = year(1000)?.join(&year(2000)?);
        let b = year(2000)?.join(&year(3000)?);
        let c = year(1000)?.join(&year(3000)?);
        assert!(
            a.overlaps(&b) && a.overlaps(&c) && b.overlaps(&c),
            "pairwise overlap"
        );

        let premises = vec![
            (FactId::new(1), a),
            (FactId::new(2), b),
            (FactId::new(3), c),
        ];
        let sets = minimal_fighting_sets(&premises);
        assert_eq!(
            sets.len(),
            1,
            "no disjoint pair, so the backstop fires once"
        );
        let contributing: BTreeSet<FactId> =
            sets.first().ok_or("no set")?.iter().copied().collect();
        assert_eq!(
            contributing,
            BTreeSet::from([FactId::new(1), FactId::new(2), FactId::new(3)]),
            "every disjunction is essential to the empty meet"
        );
        Ok(())
    }
}
