use std::collections::BTreeSet;

use chrono::NaiveDate;

use crate::claimed::Claimed;
use crate::date::UncertainDate;
use crate::facts::attribute::{EntityRelationType, NameType};
use crate::facts::citations::{ExternalReference, Language};
use crate::facts::event::EventPayload;
use crate::facts::lifecycle::{
    DamageCause, DurationalKind, LifetimeEventKind, MoveMethod, PointKind, Usage,
};
use crate::facts::schema::EquivClass;
use crate::location::UnresolvedLocation;

use crate::facts::projection::{
    self, Bookend, Bracket, Citation, FactMap, FactSet, MemberLineage, NameKey, NameRecord,
};

use super::*;

/// A name claim, flattened: the dedup triple plus its validity window and the
/// citations behind its presence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Name<ImgId> {
    pub text: String,
    pub language: Language,
    pub name_type: NameType,
    pub valid_from: Bounded<UncertainDate, ImgId>,
    pub valid_to: Bounded<UncertainDate, ImgId>,
    pub sources: Vec<Citation<ImgId>>,
}

/// A directed relationship to a neighbor: the bare target id plus the relation
/// kinds asserted, each attributed. Label resolution is a later ids→names pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Relation<EntId, ImgId> {
    pub other: EntId,
    pub kinds: Vec<Attributed<EntityRelationType, ImgId>>,
}

/// A start/completion span over two independently-bounded endpoints — a consumer
/// renders the span rather than collapsing it into one range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Period<ImgId> {
    pub started: Bounded<UncertainDate, ImgId>,
    pub completed: Bounded<UncertainDate, ImgId>,
}

/// The flattened mirror of [`projection::Event`] (the semiring param removed), kept
/// whole on an [`InteriorEvent::Ambiguous`] entry whose kind didn't settle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventFacts<ImgId> {
    pub started: Bounded<UncertainDate, ImgId>,
    pub completed: Bounded<UncertainDate, ImgId>,
    pub occurred: Bounded<UncertainDate, ImgId>,
    pub location: Bounded<UnresolvedLocation, ImgId>,
    pub cause: Bounded<Claimed<DamageCause>, ImgId>,
    pub method: Bounded<Claimed<MoveMethod>, ImgId>,
    pub usages: Bounded<Claimed<BTreeSet<Usage>>, ImgId>,
    pub designation: Bounded<Claimed<String>, ImgId>,
}

/// One lifecycle timeline entry: a bookend (construction/demolition) or an
/// interior event with its id, descriptions, and parsed kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum EventDetail<EvtId, ImgId> {
    /// Synthesized from the `construction` bookend, with its own location slot.
    Constructed {
        period: Period<ImgId>,
        location: Bounded<UnresolvedLocation, ImgId>,
    },
    /// Synthesized from the `demolition` bookend. Carries no location — a
    /// demolition derives the entity's last-known position.
    Demolished { period: Period<ImgId> },
    /// One interior lifetime event: its id, its descriptions, and its parsed
    /// kind.
    Interior {
        id: EvtId,
        descriptions: Vec<Attributed<String, ImgId>>,
        kind: InteriorEvent<ImgId>,
    },
}

/// One interior event's kind, parsed into a typed variant when its kind settled
/// to a singleton, else [`Ambiguous`](InteriorEvent::Ambiguous).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InteriorEvent<ImgId> {
    Modified {
        period: Period<ImgId>,
    },
    Repaired {
        period: Period<ImgId>,
    },
    Damaged {
        period: Period<ImgId>,
        cause: Bounded<Claimed<DamageCause>, ImgId>,
    },
    Moved {
        period: Period<ImgId>,
        method: Bounded<Claimed<MoveMethod>, ImgId>,
        to: Bounded<UnresolvedLocation, ImgId>,
    },
    UsageChanged {
        at: Bounded<UncertainDate, ImgId>,
        usages: Bounded<Claimed<BTreeSet<Usage>>, ImgId>,
    },
    Designated {
        at: Bounded<UncertainDate, ImgId>,
        designation: Bounded<Claimed<String>, ImgId>,
    },
    /// The kind didn't settle to a singleton — a conflict, an absent or `Any`
    /// kind. Carries the rival kinds and the whole flattened fact set, boxed so
    /// this rare arm doesn't widen every entry. The kind's citations ride the
    /// enclosing entry's `sources`.
    Ambiguous {
        candidates: Claimed<LifetimeEventKind>,
        facts: Box<EventFacts<ImgId>>,
    },
}

/// One sorted timeline entry: the lifecycle detail and the citations behind it.
///
/// `sources` is universal: an interior event carries its kind/existence
/// citations (so a bare `Modified` with no dates keeps its attribution); a
/// bookend carries the union of its date and location bracket citations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimelineEntry<EvtId, ImgId> {
    pub detail: EventDetail<EvtId, ImgId>,
    pub sources: Vec<Citation<ImgId>>,
}

/// The typed DTO for one entity: every restrictive field flattened to a
/// [`Bounded`], every membership attributed, the interior events parsed into a
/// sorted timeline, and the merge lineage surfaced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entity<EntId: Ord, EvtId, ImgId> {
    pub id: EntId,
    pub names: Vec<Name<ImgId>>,
    pub relations: Vec<Relation<EntId, ImgId>>,
    pub external_refs: Vec<Attributed<ExternalReference, ImgId>>,
    pub location: Bounded<UnresolvedLocation, ImgId>,
    pub timeline: Vec<TimelineEntry<EvtId, ImgId>>,
    /// Images depicting this entity, with their per-depiction linkage.
    pub depictions: Vec<Depiction<ImgId, ImgId>>,
    pub merged_from: MergeProvenance<EntId, ImgId>,
}

// ----------------------------------------------------------------------------
// The transform
// ----------------------------------------------------------------------------

impl<EntId, EvtId, ImgId> Entity<EntId, EvtId, ImgId>
where
    EntId: Ord + Clone,
    EvtId: Ord + Clone + std::fmt::Debug,
    ImgId: Ord + Clone,
{
    /// Flatten a [`projection::Entity`] into its typed DTO. Pure over the flat
    /// member-aware lineage; the `EquivClass` carries `mention_count = members.len()`.
    pub fn parse(
        projected: &projection::Entity<EntId, EvtId, ImgId, MemberLineage<EntId, ImgId>>,
        class: &EquivClass<EntId>,
    ) -> Self {
        let names = display_names(&projected.names);
        let relations = display_relations(&projected.relations);
        let external_refs = projected
            .refs
            .iter()
            .map(|(reference, entry)| factset(entry, reference.clone()))
            .collect();
        let timeline = timeline(projected);
        let location = entity_location(&projected.construction.location, &timeline);
        let depictions = projected
            .depictions
            .iter()
            .map(|(image, entry)| depiction(image.clone(), entry))
            .collect();
        let merged_from = merge_provenance(&projected.sameness, class);

        Self {
            id: class.representative.clone(),
            names,
            relations,
            external_refs,
            location,
            timeline,
            depictions,
            merged_from,
        }
    }
}

fn display_names<EntId, ImgId>(
    names: &FactMap<NameKey, NameRecord<MemberLineage<EntId, ImgId>>, MemberLineage<EntId, ImgId>>,
) -> Vec<Name<ImgId>>
where
    EntId: Ord + Clone,
    ImgId: Ord + Clone,
{
    names
        .iter()
        .map(|(key, entry)| Name {
            text: key.name.as_str().to_owned(),
            language: key.language.clone(),
            name_type: key.name_type,
            valid_from: bracket(&entry.value.valid_from),
            valid_to: bracket(&entry.value.valid_to),
            sources: sources(&entry.support),
        })
        .collect()
}

/// The projection's relations field: target id → coexisting relation kinds.
type ProjectedRelations<EntId, ImgId> = FactMap<
    EntId,
    FactSet<EntityRelationType, MemberLineage<EntId, ImgId>>,
    MemberLineage<EntId, ImgId>,
>;

fn display_relations<EntId, ImgId>(
    relations: &ProjectedRelations<EntId, ImgId>,
) -> Vec<Relation<EntId, ImgId>>
where
    EntId: Ord + Clone,
    ImgId: Ord + Clone,
{
    relations
        .iter()
        .map(|(other, entry)| Relation {
            other: other.clone(),
            kinds: entry
                .value
                .iter()
                .map(|(kind, kind_entry)| factset(kind_entry, *kind))
                .collect(),
        })
        .collect()
}

/// Flatten the bookend dates into a [`Period`].
fn bookend_period<EntId, ImgId>(bookend: &Bookend<MemberLineage<EntId, ImgId>>) -> Period<ImgId>
where
    EntId: Ord + Clone,
    ImgId: Ord + Clone,
{
    Period {
        started: bracket(&bookend.started_at),
        completed: bracket(&bookend.completed_at),
    }
}

/// Whether a bookend carries any claim — its dates or its location.
fn bookend_present<EntId, ImgId>(bookend: &Bookend<MemberLineage<EntId, ImgId>>) -> bool
where
    EntId: Ord,
    ImgId: Ord,
{
    touched(&bookend.started_at) || touched(&bookend.completed_at) || touched(&bookend.location)
}

/// The flattened [`EventFacts`] of one event record.
fn event_facts<EntId, ImgId>(
    record: &projection::Event<MemberLineage<EntId, ImgId>>,
) -> EventFacts<ImgId>
where
    EntId: Ord + Clone,
    ImgId: Ord + Clone,
{
    EventFacts {
        started: bracket(&record.started_at),
        completed: bracket(&record.completed_at),
        occurred: bracket(&record.occurred_at),
        location: bracket(&record.location),
        cause: bracket(&record.cause),
        method: bracket(&record.method),
        usages: bracket(&record.usages),
        designation: bracket(&record.designation),
    }
}

/// The one kind a settled consensus agreed on: a singleton `Reached { Of({k}) }`.
/// Every other shape leaves the event's kind unsettled.
fn settled_kind(consensus: &Consensus<Claimed<LifetimeEventKind>>) -> Option<LifetimeEventKind> {
    match consensus {
        Consensus::Reached {
            value: Claimed::Of { values },
        } => single(values),
        _ => None,
    }
}

/// The restrictive fields the record touches whose [`EventPayload`] rule excludes
/// the settled kind `k`. Submit enforces the per-kind set, so a non-empty result
/// marks a malformed record; the caller routes it to `Ambiguous` (whose
/// `EventFacts` keeps the stray claims visible) and names the offending fields in
/// its log. `descriptions` is additive and rides every kind, so it sits outside
/// this set.
fn off_kind_fields<EntId, ImgId>(
    record: &projection::Event<MemberLineage<EntId, ImgId>>,
    k: LifetimeEventKind,
) -> Vec<&'static str>
where
    EntId: Ord,
    ImgId: Ord,
{
    [
        (
            "started_at",
            touched(&record.started_at),
            EventPayload::DurationalDate,
        ),
        (
            "completed_at",
            touched(&record.completed_at),
            EventPayload::DurationalDate,
        ),
        (
            "occurred_at",
            touched(&record.occurred_at),
            EventPayload::PointDate,
        ),
        (
            "location",
            touched(&record.location),
            EventPayload::MovedToLocation,
        ),
        ("cause", touched(&record.cause), EventPayload::DamageCause),
        ("method", touched(&record.method), EventPayload::MoveMethod),
        ("usages", touched(&record.usages), EventPayload::UsageChange),
        (
            "designation",
            touched(&record.designation),
            EventPayload::Designation,
        ),
    ]
    .into_iter()
    .filter(|(_, is_touched, payload)| *is_touched && !payload.allowed_kinds().contains(&k))
    .map(|(field, _, _)| field)
    .collect()
}

/// Parse one event record's kind into a typed [`InteriorEvent`], returning the kind's
/// citations alongside so the timeline entry keeps its attribution even when the
/// kind is the event's only fact.
///
/// The single predicate selecting a typed variant is a settled singleton kind
/// (`Reached { Of({k}) }`); every other shape routes to `Ambiguous` carrying the
/// kind's extent as `candidates`.
fn interior_event<EntId, EvtId, ImgId>(
    record: &projection::Event<MemberLineage<EntId, ImgId>>,
    event_id: &EvtId,
) -> (InteriorEvent<ImgId>, Vec<Citation<ImgId>>)
where
    EntId: Ord + Clone,
    EvtId: std::fmt::Debug,
    ImgId: Ord + Clone,
{
    let kind = bracket(&record.kind);
    let kind_sources = kind.sources.clone();
    let facts = event_facts(record);

    // A typed variant forms only when the kind settled and the record fits that
    // kind's read set. Submit enforces the per-kind set; honoring it here keeps a
    // malformed record lossless, since `Ambiguous` carries the whole `EventFacts`.
    let settled = match settled_kind(&kind.consensus) {
        Some(k) => {
            let off_kind = off_kind_fields(record, k);
            if off_kind.is_empty() {
                Some(k)
            } else {
                tracing::warn!(
                    event = ?event_id,
                    kind = ?k,
                    off_kind = ?off_kind,
                    "event kind settled but carries off-kind payload; routing to Ambiguous (a submit-layer invariant should forbid this)"
                );
                None
            }
        }
        None => None,
    };

    let Some(k) = settled else {
        let candidates = kind.possible;
        debug_assert!(
            !kind.sources.is_empty() || matches!(kind.consensus, Consensus::Absent),
            "a present kind must cite a fact"
        );
        let parsed = InteriorEvent::Ambiguous {
            candidates,
            facts: Box::new(facts),
        };
        return (parsed, kind_sources);
    };

    let period = Period {
        started: facts.started,
        completed: facts.completed,
    };
    let parsed = match k {
        LifetimeEventKind::Durational { kind } => match kind {
            DurationalKind::Modified => InteriorEvent::Modified { period },
            DurationalKind::Repaired => InteriorEvent::Repaired { period },
            DurationalKind::Damaged => InteriorEvent::Damaged {
                period,
                cause: facts.cause,
            },
            DurationalKind::Moved => InteriorEvent::Moved {
                period,
                method: facts.method,
                to: facts.location,
            },
        },
        LifetimeEventKind::Point { kind } => match kind {
            PointKind::UsageChanged => InteriorEvent::UsageChanged {
                at: facts.occurred,
                usages: facts.usages,
            },
            PointKind::Designated => InteriorEvent::Designated {
                at: facts.occurred,
                designation: facts.designation,
            },
        },
    };
    (parsed, kind_sources)
}

/// The single element of a set, or `None` for empty or larger.
fn single<A: Clone>(set: &BTreeSet<A>) -> Option<A> {
    let mut it = set.iter();
    let first = it.next()?;
    match it.next() {
        Some(_) => None,
        None => Some(first.clone()),
    }
}

/// The descriptions hoisted onto a timeline entry.
fn descriptions<EntId, ImgId>(
    descriptions: &FactSet<String, MemberLineage<EntId, ImgId>>,
) -> Vec<Attributed<String, ImgId>>
where
    EntId: Ord + Clone,
    ImgId: Ord + Clone,
{
    descriptions
        .iter()
        .map(|(text, entry)| factset(entry, text.clone()))
        .collect()
}

/// An interior event's best-known date — started, falling back to completed,
/// falling back to occurred — for ordering. The bookends carry none; they anchor
/// by lifecycle position instead.
fn interior_date<ImgId>(kind: &InteriorEvent<ImgId>) -> Option<NaiveDate> {
    let period_date = |p: &Period<ImgId>| {
        p.started
            .possible
            .earliest()
            .or_else(|| p.completed.possible.earliest())
    };
    match kind {
        InteriorEvent::Modified { period }
        | InteriorEvent::Repaired { period }
        | InteriorEvent::Damaged { period, .. }
        | InteriorEvent::Moved { period, .. } => period_date(period),
        InteriorEvent::UsageChanged { at, .. } | InteriorEvent::Designated { at, .. } => {
            at.possible.earliest()
        }
        InteriorEvent::Ambiguous { facts, .. } => facts
            .started
            .possible
            .earliest()
            .or_else(|| facts.completed.possible.earliest())
            .or_else(|| facts.occurred.possible.earliest()),
    }
}

/// The total timeline order: construction first, demolition last, interior by
/// best-known date with undated entries (`None` last) after the dated ones.
fn entry_sort_key<EvtId, ImgId>(
    detail: &EventDetail<EvtId, ImgId>,
) -> (u8, bool, Option<NaiveDate>) {
    match detail {
        EventDetail::Constructed { .. } => (0, false, None),
        EventDetail::Demolished { .. } => (2, false, None),
        EventDetail::Interior { kind, .. } => {
            let date = interior_date(kind);
            (1, date.is_none(), date)
        }
    }
}

/// The union of a bookend's date and location bracket citations — a present
/// bookend entry's attribution, so it never surfaces empty-sourced.
fn bookend_sources<EntId, ImgId>(
    bookend: &Bookend<MemberLineage<EntId, ImgId>>,
) -> Vec<Citation<ImgId>>
where
    EntId: Ord + Clone,
    ImgId: Ord + Clone,
{
    bracket(&bookend.started_at)
        .sources
        .into_iter()
        .chain(bracket(&bookend.completed_at).sources)
        .chain(bracket(&bookend.location).sources)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Build the sorted timeline: construction first, demolition last, the interior
/// events parsed and ordered by earliest known date with undated entries last.
fn timeline<EntId, EvtId, ImgId>(
    projected: &projection::Entity<EntId, EvtId, ImgId, MemberLineage<EntId, ImgId>>,
) -> Vec<TimelineEntry<EvtId, ImgId>>
where
    EntId: Ord + Clone,
    EvtId: Ord + Clone + std::fmt::Debug,
    ImgId: Ord + Clone,
{
    let mut entries: Vec<TimelineEntry<EvtId, ImgId>> = Vec::new();

    if bookend_present(&projected.construction) {
        entries.push(TimelineEntry {
            detail: EventDetail::Constructed {
                period: bookend_period(&projected.construction),
                location: bracket(&projected.construction.location),
            },
            sources: bookend_sources(&projected.construction),
        });
    }

    for (event_id, entry) in &projected.events {
        let (kind, sources) = interior_event(&entry.value, event_id);
        entries.push(TimelineEntry {
            detail: EventDetail::Interior {
                id: event_id.clone(),
                descriptions: descriptions(&entry.value.descriptions),
                kind,
            },
            sources,
        });
    }

    if bookend_present(&projected.demolition) {
        entries.push(TimelineEntry {
            detail: EventDetail::Demolished {
                period: bookend_period(&projected.demolition),
            },
            sources: bookend_sources(&projected.demolition),
        });
    }

    entries.sort_by_key(|entry| entry_sort_key(&entry.detail));
    entries
}

/// The best-known landing date of a `Moved` entry: its completion if dated,
/// else its start — a move with only a start date stays orderable.
fn landing_date<ImgId>(period: &Period<ImgId>) -> Option<NaiveDate> {
    period
        .completed
        .possible
        .earliest()
        .or_else(|| period.started.possible.earliest())
}

/// The entity-level location: the destination of the latest `Moved` entry in the
/// built timeline by best-known landing date, else the construction location. The
/// `Option<NaiveDate>` ordering puts a dated move above an undated one and the
/// later landing on top.
fn entity_location<EntId, EvtId, ImgId>(
    construction_location: &Bracket<UnresolvedLocation, MemberLineage<EntId, ImgId>>,
    timeline: &[TimelineEntry<EvtId, ImgId>],
) -> Bounded<UnresolvedLocation, ImgId>
where
    EntId: Ord + Clone,
    ImgId: Ord + Clone,
{
    timeline
        .iter()
        .filter_map(|entry| match &entry.detail {
            EventDetail::Interior {
                kind: InteriorEvent::Moved { period, to, .. },
                ..
            } => Some((landing_date(period), to)),
            _ => None,
        })
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, to)| to.clone())
        .unwrap_or_else(|| bracket(construction_location))
}

#[cfg(test)]
mod tests {
    use crate::facts::attribute::NameText;
    use crate::location::Location;

    use super::*;
    use crate::facts::typed::test_support::*;

    fn resolved_point(
        lat: f64,
        lon: f64,
    ) -> Result<UnresolvedLocation, Box<dyn std::error::Error>> {
        use crate::geo::{GeoPoint, Meters};
        Ok(UnresolvedLocation::Resolved(Location::circle(
            GeoPoint::new(lat, lon)?,
            Meters(10.0),
        )?))
    }

    fn date_kind(
        kind: LifetimeEventKind,
        support: Lin,
    ) -> Bracket<Claimed<LifetimeEventKind>, Lin> {
        claim(
            Claimed::Of {
                values: [kind].into_iter().collect(),
            },
            support,
        )
    }

    /// A minimal empty event record — every slot untouched.
    fn empty_event() -> Result<projection::Event<Lin>, Box<dyn std::error::Error>> {
        Ok(projection::Event {
            kind: untouched(),
            started_at: untouched(),
            completed_at: untouched(),
            occurred_at: untouched(),
            location: untouched(),
            cause: untouched(),
            method: untouched(),
            usages: untouched(),
            designation: untouched(),
            descriptions: FactMap::new(),
        })
    }

    fn empty_bookend() -> Bookend<Lin> {
        Bookend {
            started_at: untouched(),
            completed_at: untouched(),
            location: untouched(),
        }
    }

    /// An entity with empty everything — the per-test base to populate.
    fn empty_entity() -> projection::Entity<EntId, EvtId, ImgId, Lin> {
        projection::Entity {
            names: FactMap::new(),
            relations: FactMap::new(),
            refs: FactMap::new(),
            construction: empty_bookend(),
            demolition: empty_bookend(),
            events: FactMap::new(),
            depictions: FactMap::new(),
            sameness: FactMap::new(),
        }
    }

    /// The singleton class of one id — every entity's own subject is a member.
    fn solo_class(id: EntId) -> EquivClass<EntId> {
        EquivClass {
            representative: id,
            members: [id].into_iter().collect(),
        }
    }

    // ---- the kind parse ----

    #[test]
    fn settled_kind_parses_to_typed_variant() -> TestResult {
        let mut record = empty_event()?;
        record.kind = date_kind(
            LifetimeEventKind::Durational {
                kind: DurationalKind::Damaged,
            },
            lin(1, "https://a")?,
        );
        record.cause = claim(
            Claimed::Of {
                values: [DamageCause::Fire].into_iter().collect(),
            },
            lin(1, "https://a")?,
        );
        let (kind, _sources) = interior_event(&record, &1u64);
        let InteriorEvent::Damaged { cause, .. } = kind else {
            return Err("expected a Damaged variant".into());
        };
        assert_eq!(
            cause.consensus,
            Consensus::Reached {
                value: Claimed::Of {
                    values: [DamageCause::Fire].into_iter().collect()
                }
            },
            "the Damaged variant carries its settled cause"
        );
        Ok(())
    }

    #[test]
    fn settled_kind_with_off_kind_field_routes_to_ambiguous() -> TestResult {
        // A Damaged event whose record also carries a `method` claim — a slot
        // Damaged never reads. Submit forbids this; the typed projection must route it to
        // Ambiguous so the stray claim stays visible instead of being dropped.
        let mut record = empty_event()?;
        record.kind = date_kind(
            LifetimeEventKind::Durational {
                kind: DurationalKind::Damaged,
            },
            lin(1, "https://a")?,
        );
        record.cause = claim(
            Claimed::Of {
                values: [DamageCause::Fire].into_iter().collect(),
            },
            lin(1, "https://a")?,
        );
        record.method = claim(
            Claimed::Of {
                values: [MoveMethod::Whole].into_iter().collect(),
            },
            lin(2, "https://m")?,
        );

        let (kind, _sources) = interior_event(&record, &1u64);
        let InteriorEvent::Ambiguous { candidates, facts } = kind else {
            return Err("an off-kind payload must route to Ambiguous, not Damaged".into());
        };
        assert_eq!(
            candidates,
            Claimed::Of {
                values: [LifetimeEventKind::Durational {
                    kind: DurationalKind::Damaged,
                }]
                .into_iter()
                .collect()
            },
            "the kind still settled — candidates carry the lone Damaged kind"
        );
        assert_eq!(
            facts.method.consensus,
            Consensus::Reached {
                value: Claimed::Of {
                    values: [MoveMethod::Whole].into_iter().collect()
                }
            },
            "the stray method claim survives in the Ambiguous facts"
        );
        Ok(())
    }

    #[test]
    fn conflicting_kind_routes_to_ambiguous_with_candidates() -> TestResult {
        use crate::algebra::monoid::CommutativeMonoid;
        let mut record = empty_event()?;
        // Two sources disagree on the kind: the meet empties, the extent keeps
        // both rivals.
        record.kind = date_kind(
            LifetimeEventKind::Durational {
                kind: DurationalKind::Modified,
            },
            lin(1, "https://a")?,
        )
        .combine(date_kind(
            LifetimeEventKind::Durational {
                kind: DurationalKind::Repaired,
            },
            lin(2, "https://b")?,
        ));
        let (kind, _sources) = interior_event(&record, &1u64);
        let InteriorEvent::Ambiguous { candidates, .. } = kind else {
            return Err("expected Ambiguous".into());
        };
        assert_eq!(
            candidates,
            Claimed::Of {
                values: [
                    LifetimeEventKind::Durational {
                        kind: DurationalKind::Modified
                    },
                    LifetimeEventKind::Durational {
                        kind: DurationalKind::Repaired
                    },
                ]
                .into_iter()
                .collect()
            },
            "candidates carry the rival kinds, the extent — never the conflicting meet"
        );
        Ok(())
    }

    #[test]
    fn any_kind_routes_to_ambiguous() -> TestResult {
        let mut record = empty_event()?;
        record.kind = claim(Claimed::Any, lin(1, "https://a")?);
        let (kind, _sources) = interior_event(&record, &1u64);
        let InteriorEvent::Ambiguous { candidates, .. } = kind else {
            return Err("expected Ambiguous for Any".into());
        };
        assert_eq!(candidates, Claimed::Any);
        Ok(())
    }

    #[test]
    fn bare_typed_event_surfaces_kind_citation() -> TestResult {
        // An event whose only fact is its kind — a dateless `Modified` — keeps
        // its attribution through the entry's `sources`, not just `Ambiguous`.
        let mut entity = empty_entity();
        let mut record = empty_event()?;
        record.kind = date_kind(
            LifetimeEventKind::Durational {
                kind: DurationalKind::Modified,
            },
            lin(1, "https://k")?,
        );
        entity.events.insert(1, cited(record, lin(1, "https://k")?));

        let out = Entity::<EntId, EvtId, ImgId>::parse(&entity, &solo_class(1));
        let entry = out
            .timeline
            .iter()
            .find(|e| {
                matches!(
                    &e.detail,
                    EventDetail::Interior {
                        kind: InteriorEvent::Modified { .. },
                        ..
                    }
                )
            })
            .ok_or("no Modified entry")?;
        assert_eq!(
            entry.sources.len(),
            1,
            "a settled typed event surfaces its kind citation"
        );
        Ok(())
    }

    // ---- merge provenance ----

    #[test]
    fn unmerged_entity_is_single_mention_no_bridges() -> TestResult {
        let entity = empty_entity();
        let out = Entity::<EntId, EvtId, ImgId>::parse(&entity, &solo_class(7));
        assert_eq!(
            out.merged_from.mention_count,
            NonZeroUsize::MIN,
            "an unmerged entity is its own one mention"
        );
        assert!(
            out.merged_from.bridges.is_empty(),
            "no sameness edges, no bridges"
        );
        Ok(())
    }

    #[test]
    fn merged_class_carries_count_and_bridge() -> TestResult {
        let mut entity = empty_entity();
        entity.sameness.insert(
            OrderedDistinctPair::new(1, 2)?,
            cited((), lin(1, "https://judgment")?),
        );
        let class = EquivClass {
            representative: 1,
            members: [1, 2].into_iter().collect(),
        };
        let out = Entity::<EntId, EvtId, ImgId>::parse(&entity, &class);
        assert_eq!(out.merged_from.mention_count.get(), 2);
        assert_eq!(out.merged_from.bridges.len(), 1);
        let bridge = out.merged_from.bridges.first().ok_or("no bridge")?;
        assert_eq!(bridge.endpoints, OrderedDistinctPair::new(1, 2)?);
        assert_eq!(bridge.judgment.len(), 1, "the bridge cites its judgment");
        Ok(())
    }

    // ---- location: latest Moved else construction ----

    #[test]
    fn location_prefers_latest_move_over_construction() -> TestResult {
        let mut entity = empty_entity();
        let built = resolved_point(41.0, 12.0)?;
        let moved_to = resolved_point(45.0, 9.0)?;
        entity.construction.location = claim(built.clone(), lin(1, "https://built")?);

        let mut moved = empty_event()?;
        moved.kind = date_kind(
            LifetimeEventKind::Durational {
                kind: DurationalKind::Moved,
            },
            lin(1, "https://m")?,
        );
        moved.completed_at = claim(year(1900)?, lin(1, "https://m")?);
        moved.location = claim(moved_to.clone(), lin(1, "https://m")?);
        entity.events.insert(10, cited(moved, lin(1, "https://m")?));

        let out = Entity::<EntId, EvtId, ImgId>::parse(&entity, &solo_class(1));
        assert_eq!(
            out.location.possible, moved_to,
            "the move destination wins over the construction location"
        );
        Ok(())
    }

    #[test]
    fn location_falls_back_to_construction_without_moves() -> TestResult {
        let mut entity = empty_entity();
        let built = resolved_point(41.0, 12.0)?;
        entity.construction.location = claim(built.clone(), lin(1, "https://built")?);
        let out = Entity::<EntId, EvtId, ImgId>::parse(&entity, &solo_class(1));
        assert_eq!(out.location.possible, built);
        Ok(())
    }

    #[test]
    fn location_picks_latest_of_two_moves() -> TestResult {
        let mut entity = empty_entity();
        let early = resolved_point(45.0, 9.0)?;
        let late = resolved_point(48.0, 2.0)?;

        let mut move_early = empty_event()?;
        move_early.kind = date_kind(
            LifetimeEventKind::Durational {
                kind: DurationalKind::Moved,
            },
            lin(1, "https://e")?,
        );
        move_early.completed_at = claim(year(1880)?, lin(1, "https://e")?);
        move_early.location = claim(early, lin(1, "https://e")?);
        entity
            .events
            .insert(10, cited(move_early, lin(1, "https://e")?));

        let mut move_late = empty_event()?;
        move_late.kind = date_kind(
            LifetimeEventKind::Durational {
                kind: DurationalKind::Moved,
            },
            lin(1, "https://l")?,
        );
        move_late.completed_at = claim(year(1920)?, lin(1, "https://l")?);
        move_late.location = claim(late.clone(), lin(1, "https://l")?);
        entity
            .events
            .insert(11, cited(move_late, lin(1, "https://l")?));

        let out = Entity::<EntId, EvtId, ImgId>::parse(&entity, &solo_class(1));
        assert_eq!(
            out.location.possible, late,
            "the later move's destination is the entity location"
        );
        Ok(())
    }

    #[test]
    fn location_picks_dated_move_over_undated_move() -> TestResult {
        let mut entity = empty_entity();
        let dated = resolved_point(45.0, 9.0)?;
        let undated = resolved_point(48.0, 2.0)?;

        let mut move_dated = empty_event()?;
        move_dated.kind = date_kind(
            LifetimeEventKind::Durational {
                kind: DurationalKind::Moved,
            },
            lin(1, "https://d")?,
        );
        move_dated.completed_at = claim(year(1900)?, lin(1, "https://d")?);
        move_dated.location = claim(dated.clone(), lin(1, "https://d")?);
        entity
            .events
            .insert(10, cited(move_dated, lin(1, "https://d")?));

        let mut move_undated = empty_event()?;
        move_undated.kind = date_kind(
            LifetimeEventKind::Durational {
                kind: DurationalKind::Moved,
            },
            lin(1, "https://u")?,
        );
        move_undated.location = claim(undated, lin(1, "https://u")?);
        entity
            .events
            .insert(11, cited(move_undated, lin(1, "https://u")?));

        let out = Entity::<EntId, EvtId, ImgId>::parse(&entity, &solo_class(1));
        assert_eq!(
            out.location.possible, dated,
            "a dated move wins over an undated one"
        );
        Ok(())
    }

    // ---- timeline sort ----

    /// Build a designated point event at a given year (or undated).
    fn designated_event(
        year_opt: Option<i32>,
    ) -> Result<projection::Event<Lin>, Box<dyn std::error::Error>> {
        let mut record = empty_event()?;
        record.kind = date_kind(
            LifetimeEventKind::Point {
                kind: PointKind::Designated,
            },
            lin(1, "https://d")?,
        );
        if let Some(y) = year_opt {
            record.occurred_at = claim(year(y)?, lin(1, "https://d")?);
        }
        Ok(record)
    }

    #[test]
    fn timeline_orders_construction_first_demolition_last_undated_late() -> TestResult {
        let mut entity = empty_entity();
        entity.construction.started_at = claim(year(1800)?, lin(1, "https://c")?);
        entity.demolition.completed_at = claim(year(1990)?, lin(1, "https://x")?);
        // Two interior events: one dated 1900, one undated. Insert undated at a
        // lower event id so a stable sort would otherwise place it first.
        entity
            .events
            .insert(1, cited(designated_event(None)?, lin(1, "https://u")?));
        entity.events.insert(
            2,
            cited(designated_event(Some(1900))?, lin(1, "https://p")?),
        );

        let out = Entity::<EntId, EvtId, ImgId>::parse(&entity, &solo_class(1));
        let kinds: Vec<&str> = out
            .timeline
            .iter()
            .map(|e| match &e.detail {
                EventDetail::Constructed { .. } => "constructed",
                EventDetail::Demolished { .. } => "demolished",
                EventDetail::Interior {
                    kind: InteriorEvent::Designated { at, .. },
                    ..
                } => {
                    if matches!(at.consensus, Consensus::Absent) {
                        "designated-undated"
                    } else {
                        "designated-dated"
                    }
                }
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "constructed",
                "designated-dated",
                "designated-undated",
                "demolished"
            ],
            "construction first, dated interior, undated interior, demolition last"
        );
        Ok(())
    }

    // ---- absent field ----

    #[test]
    fn absent_name_validity_is_honest() -> TestResult {
        let mut entity = empty_entity();
        entity.names.insert(
            NameKey {
                name: NameText::new("Pantheon"),
                language: Language::new("en")?,
                name_type: NameType::Common,
            },
            cited(
                NameRecord {
                    valid_from: untouched(),
                    valid_to: untouched(),
                },
                lin(1, "https://n")?,
            ),
        );
        let out = Entity::<EntId, EvtId, ImgId>::parse(&entity, &solo_class(1));
        let name = out.names.first().ok_or("no name")?;
        assert_eq!(name.text, "Pantheon");
        assert_eq!(
            name.valid_from.consensus,
            Consensus::Absent,
            "an unstated validity endpoint is Absent, not faked"
        );
        assert_eq!(
            name.sources.len(),
            1,
            "the name presence still cites its fact"
        );
        Ok(())
    }

    // ---- entity-side depictions ----

    #[test]
    fn entity_parse_populates_depictions_from_projection() -> TestResult {
        let geom = geometry()?;
        let mut entity = empty_entity();
        // Key the depiction by an image id (9) distinct from the entity id (1),
        // so the assertion genuinely pins keying by the image map key.
        entity.depictions.insert(
            9,
            cited(
                depiction_record(
                    Some(geom.clone()),
                    Some(Perspective::Interior),
                    lin(1, "https://d")?,
                ),
                lin(1, "https://d")?,
            ),
        );

        let out = Entity::<EntId, EvtId, ImgId>::parse(&entity, &solo_class(1));
        assert_eq!(
            out.depictions.len(),
            1,
            "the projected depiction surfaces on the entity"
        );
        let dep = out.depictions.first().ok_or("no depiction")?;
        assert_eq!(dep.other, 9, "the depiction is keyed by its image map key");
        assert_eq!(
            dep.localization.consensus,
            Consensus::Reached {
                value: Claimed::Of {
                    values: [geom].into_iter().collect()
                }
            },
            "the entity-side depiction flattens its localization"
        );
        assert_eq!(
            dep.perspective.consensus,
            Consensus::Reached {
                value: Claimed::Of {
                    values: [Perspective::Interior].into_iter().collect()
                }
            },
            "the entity-side depiction flattens its perspective"
        );
        Ok(())
    }
}
