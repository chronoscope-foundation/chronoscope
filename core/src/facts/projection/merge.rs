//! Per-field merge helpers — the no-winners projectors that fold an entity's
//! facts into the projected value and its citation sidecar.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::date::UncertainDate;
use crate::facts::assertions::FactualAssertion;
use crate::facts::attribute::{self, EntityRelationType, NameText, NameType};
use crate::facts::bookend;
use crate::facts::citations::{ExternalReference, Language};
use crate::facts::event;
use crate::facts::ids::FactId;
use crate::facts::lifecycle::{DurationalKind, DurationalRole, LifetimeEventKind, PointKind};
use crate::facts::submit::StoredFact;
use crate::lattice::JoinSemilattice;
use crate::location::UnresolvedLocation;

use super::{
    CitationMap, JsonPath, ProjectedBookend, ProjectedCitation, ProjectedEntity,
    ProjectedLifetimeEvent, ProjectedName, ProjectedRelation,
};

/// The judgment sources of the visible `SameEntity` edges — the class-root
/// provenance ("why these members are one entity"). A singleton class has no
/// edge, so this is empty and root stays unaddressed.
pub(super) fn root_supports<EntId, EvtId, ImgId>(
    facts: &BTreeMap<FactId, StoredFact<EntId, EvtId, ImgId>>,
) -> Vec<ProjectedCitation<ImgId>>
where
    EntId: Ord,
    EvtId: Ord,
    ImgId: Ord + Clone,
{
    use crate::facts::assertions::JudgmentAssertion;
    use crate::facts::identity;
    let mut out = Vec::new();
    for fact in facts.values() {
        if let StoredFact::Judgment(jf) = fact
            && let JudgmentAssertion::Identity {
                fact: identity::Fact::SameEntity { .. },
            } = &jf.assertion
        {
            out.push(ProjectedCitation::Judgment {
                source: jf.source.clone(),
            });
        }
    }
    out
}

/// The factual citation of a stored fact, when it has one. A judgment or meta
/// fact backing a value would be a different arm; the value fields here are all
/// factual, so a non-factual fact contributes nothing.
fn factual_citation<EntId, EvtId, ImgId>(
    fact: &StoredFact<EntId, EvtId, ImgId>,
) -> Option<ProjectedCitation<ImgId>>
where
    EntId: Ord,
    EvtId: Ord,
    ImgId: Ord + Clone,
{
    match fact {
        StoredFact::Factual(f) => Some(ProjectedCitation::Factual {
            citation: f.citation.clone(),
        }),
        StoredFact::Judgment(_) | StoredFact::Meta(_) => None,
    }
}

/// Names: one slot per distinct `(name, language, name_type)`; a collapsed slot
/// cites every fact that fed it. The fact set is keyed by `FactId`, so each fact
/// contributes once; order follows ascending `FactId`.
pub(super) fn project_names<EntId, EvtId, ImgId>(
    facts: &BTreeMap<FactId, StoredFact<EntId, EvtId, ImgId>>,
    names: &mut Vec<ProjectedName>,
    citations: &mut CitationMap<ImgId>,
) where
    EntId: Ord,
    EvtId: Ord,
    ImgId: Ord + Clone,
{
    // Slot key is the dedup triple; the value is the slot's index plus its
    // accumulated supports, so a later duplicate appends to the first slot.
    let mut slots: HashMap<(NameText, Language, NameType), usize> = HashMap::new();
    let mut supports: Vec<Vec<ProjectedCitation<ImgId>>> = Vec::new();
    for fact in facts.values() {
        let StoredFact::Factual(f) = fact else {
            continue;
        };
        let FactualAssertion::Attribute {
            fact:
                attribute::Fact::Name {
                    name,
                    language,
                    name_type,
                    valid_from,
                    valid_to,
                    ..
                },
        } = &f.assertion
        else {
            continue;
        };
        let key = (name.clone(), language.clone(), *name_type);
        let idx = *slots.entry(key).or_insert_with(|| {
            names.push(ProjectedName {
                name: name.clone(),
                language: language.clone(),
                name_type: *name_type,
                valid_from: valid_from.clone(),
                valid_to: valid_to.clone(),
            });
            supports.push(Vec::new());
            names.len() - 1
        });
        if let Some(c) = factual_citation(fact) {
            supports[idx].push(c);
        }
    }
    for (idx, s) in supports.into_iter().enumerate() {
        citations.insert_supports(JsonPath::root().field("names").index(idx), s);
    }
}

/// Relations: one slot per distinct `(to, kind)`; a collapsed slot cites every
/// contributing fact.
pub(super) fn project_relations<EntId, EvtId, ImgId>(
    facts: &BTreeMap<FactId, StoredFact<EntId, EvtId, ImgId>>,
    relations: &mut Vec<ProjectedRelation<EntId>>,
    citations: &mut CitationMap<ImgId>,
) where
    EntId: Ord + Clone + std::hash::Hash,
    EvtId: Ord,
    ImgId: Ord + Clone,
{
    let mut slots: HashMap<(EntId, EntityRelationType), usize> = HashMap::new();
    let mut supports: Vec<Vec<ProjectedCitation<ImgId>>> = Vec::new();
    for fact in facts.values() {
        let StoredFact::Factual(f) = fact else {
            continue;
        };
        let FactualAssertion::Attribute {
            fact: attribute::Fact::Relationship { pair, relation },
        } = &f.assertion
        else {
            continue;
        };
        let key = (pair.to().clone(), *relation);
        let idx = *slots.entry(key).or_insert_with(|| {
            relations.push(ProjectedRelation {
                to: pair.to().clone(),
                kind: *relation,
            });
            supports.push(Vec::new());
            relations.len() - 1
        });
        if let Some(c) = factual_citation(fact) {
            supports[idx].push(c);
        }
    }
    for (idx, s) in supports.into_iter().enumerate() {
        citations.insert_supports(JsonPath::root().field("relations").index(idx), s);
    }
}

/// External references: one slot per distinct [`ExternalReference`], sorted for
/// a deterministic order independent of fact ids; a collapsed slot cites every
/// contributing fact.
pub(super) fn project_external_references<EntId, EvtId, ImgId>(
    facts: &BTreeMap<FactId, StoredFact<EntId, EvtId, ImgId>>,
    refs: &mut Vec<ExternalReference>,
    citations: &mut CitationMap<ImgId>,
) where
    EntId: Ord,
    EvtId: Ord,
    ImgId: Ord + Clone,
{
    let mut by_ref: BTreeMap<ExternalReference, Vec<ProjectedCitation<ImgId>>> = BTreeMap::new();
    for fact in facts.values() {
        let StoredFact::Factual(f) = fact else {
            continue;
        };
        let FactualAssertion::Attribute {
            fact: attribute::Fact::ExternalReference { reference, .. },
        } = &f.assertion
        else {
            continue;
        };
        let entry = by_ref.entry(reference.clone()).or_default();
        if let Some(c) = factual_citation(fact) {
            entry.push(c);
        }
    }
    for (idx, (reference, s)) in by_ref.into_iter().enumerate() {
        refs.push(reference);
        citations.insert_supports(JsonPath::root().field("external_references").index(idx), s);
    }
}

/// One single-valued slot built from several claims of one lattice type: the
/// contributing values plus the facts that fed them. The join (⊔) of zero claims
/// is no slot at all, so [`joined`](Self::joined) returns `None`.
struct JoinSlot<'f, T: JoinSemilattice, EntId: Ord, EvtId: Ord, ImgId: Ord> {
    values: Vec<T>,
    facts: Vec<&'f StoredFact<EntId, EvtId, ImgId>>,
}

impl<T: JoinSemilattice, EntId: Ord, EvtId: Ord, ImgId: Ord> Default
    for JoinSlot<'_, T, EntId, EvtId, ImgId>
{
    fn default() -> Self {
        Self {
            values: Vec::new(),
            facts: Vec::new(),
        }
    }
}

impl<'f, T, EntId, EvtId, ImgId> JoinSlot<'f, T, EntId, EvtId, ImgId>
where
    T: JoinSemilattice + Clone,
    EntId: Ord,
    EvtId: Ord,
    ImgId: Ord + Clone,
{
    fn add(&mut self, value: T, fact: &'f StoredFact<EntId, EvtId, ImgId>) {
        self.values.push(value);
        self.facts.push(fact);
    }

    /// The joined value: the lattice join (⊔) of every contributing claim. Zero
    /// claims → `None` (no slot to fill); otherwise `join_all`, which seeds from
    /// ⊥ so a real claim is never poisoned and a singleton folds to itself.
    fn joined(&self) -> Option<T> {
        if self.values.is_empty() {
            return None;
        }
        Some(T::join_all(self.values.iter().cloned()))
    }

    fn supports(&self) -> Vec<ProjectedCitation<ImgId>> {
        self.facts
            .iter()
            .filter_map(|f| factual_citation(f))
            .collect()
    }
}

/// Bookends: `Construction::*` and `Demolition::*` facts fold into explicit
/// per-phase slots, each endpoint and the location joined independently.
pub(super) fn project_bookends<EntId, EvtId, ImgId>(
    facts: &BTreeMap<FactId, StoredFact<EntId, EvtId, ImgId>>,
    entity: &mut ProjectedEntity<EntId, EvtId>,
    citations: &mut CitationMap<ImgId>,
) where
    EntId: Ord,
    EvtId: Ord,
    ImgId: Ord + Clone,
{
    let mut construction = PhaseSlots::default();
    let mut demolition = PhaseSlots::default();
    for fact in facts.values() {
        let StoredFact::Factual(f) = fact else {
            continue;
        };
        let (phase, bookend_fact) = match &f.assertion {
            FactualAssertion::Construction { fact } => (&mut construction, fact),
            FactualAssertion::Demolition { fact } => (&mut demolition, fact),
            _ => continue,
        };
        match bookend_fact {
            bookend::Fact::Started { bound, .. } => phase.started.add(bound.clone(), fact),
            bookend::Fact::Completed { bound, .. } => phase.completed.add(bound.clone(), fact),
            bookend::Fact::Location { location, .. } => {
                phase.location.add(location.clone(), fact);
            }
        }
    }
    entity.construction = build_phase(
        &construction,
        JsonPath::root().field("construction"),
        citations,
    );
    entity.demolition = build_phase(&demolition, JsonPath::root().field("demolition"), citations);
}

/// The three join slots of one bookend phase: two dates and a location.
struct PhaseSlots<'f, EntId: Ord, EvtId: Ord, ImgId: Ord> {
    started: JoinSlot<'f, UncertainDate, EntId, EvtId, ImgId>,
    completed: JoinSlot<'f, UncertainDate, EntId, EvtId, ImgId>,
    location: JoinSlot<'f, UnresolvedLocation, EntId, EvtId, ImgId>,
}

impl<EntId: Ord, EvtId: Ord, ImgId: Ord> Default for PhaseSlots<'_, EntId, EvtId, ImgId> {
    fn default() -> Self {
        Self {
            started: JoinSlot::default(),
            completed: JoinSlot::default(),
            location: JoinSlot::default(),
        }
    }
}

/// Build one phase's [`ProjectedBookend`], addressing each populated slot. A
/// phase no fact spoke to projects as `None`.
fn build_phase<EntId, EvtId, ImgId>(
    slots: &PhaseSlots<'_, EntId, EvtId, ImgId>,
    base: JsonPath,
    citations: &mut CitationMap<ImgId>,
) -> Option<ProjectedBookend>
where
    EntId: Ord,
    EvtId: Ord,
    ImgId: Ord + Clone,
{
    let bookend = ProjectedBookend {
        started_at: slots.started.joined(),
        completed_at: slots.completed.joined(),
        location: slots.location.joined(),
    };
    if bookend.is_empty() {
        return None;
    }
    citations.insert_supports(base.clone().field("started_at"), slots.started.supports());
    citations.insert_supports(
        base.clone().field("completed_at"),
        slots.completed.supports(),
    );
    citations.insert_supports(base.field("location"), slots.location.supports());
    Some(bookend)
}

/// A kind slot: the distinct kinds the event's `HasEvent` claims declare plus
/// the facts declaring them. The set joins by union; per event id the submit
/// layer guarantees one `HasEvent`, so it is a singleton today.
struct KindSlot<'f, K, EntId: Ord, EvtId: Ord, ImgId: Ord> {
    kinds: BTreeSet<K>,
    facts: Vec<&'f StoredFact<EntId, EvtId, ImgId>>,
}

impl<K, EntId: Ord, EvtId: Ord, ImgId: Ord> Default for KindSlot<'_, K, EntId, EvtId, ImgId> {
    fn default() -> Self {
        Self {
            kinds: BTreeSet::new(),
            facts: Vec::new(),
        }
    }
}

impl<'f, K, EntId, EvtId, ImgId> KindSlot<'f, K, EntId, EvtId, ImgId>
where
    K: Ord + Copy,
    EntId: Ord,
    EvtId: Ord,
    ImgId: Ord + Clone,
{
    fn add(&mut self, kind: K, fact: &'f StoredFact<EntId, EvtId, ImgId>) {
        self.kinds.insert(kind);
        self.facts.push(fact);
    }

    /// The declared kinds, joined by union.
    fn joined(&self) -> BTreeSet<K> {
        self.kinds.clone()
    }

    fn supports(&self) -> Vec<ProjectedCitation<ImgId>> {
        self.facts
            .iter()
            .filter_map(|f| factual_citation(f))
            .collect()
    }
}

/// One durational interior event under construction.
struct DurationalSlot<'f, EntId: Ord, EvtId: Ord, ImgId: Ord> {
    kind: KindSlot<'f, DurationalKind, EntId, EvtId, ImgId>,
    started: JoinSlot<'f, UncertainDate, EntId, EvtId, ImgId>,
    completed: JoinSlot<'f, UncertainDate, EntId, EvtId, ImgId>,
    location: JoinSlot<'f, UnresolvedLocation, EntId, EvtId, ImgId>,
}

impl<EntId: Ord, EvtId: Ord, ImgId: Ord> Default for DurationalSlot<'_, EntId, EvtId, ImgId> {
    fn default() -> Self {
        Self {
            kind: KindSlot::default(),
            started: JoinSlot::default(),
            completed: JoinSlot::default(),
            location: JoinSlot::default(),
        }
    }
}

/// One point interior event under construction.
struct PointSlot<'f, EntId: Ord, EvtId: Ord, ImgId: Ord> {
    kind: KindSlot<'f, PointKind, EntId, EvtId, ImgId>,
    occurred: JoinSlot<'f, UncertainDate, EntId, EvtId, ImgId>,
}

impl<EntId: Ord, EvtId: Ord, ImgId: Ord> Default for PointSlot<'_, EntId, EvtId, ImgId> {
    fn default() -> Self {
        Self {
            kind: KindSlot::default(),
            occurred: JoinSlot::default(),
        }
    }
}

/// One interior event, its arm chosen by the category of its `HasEvent` kind.
/// The submit layer requires exactly one `HasEvent` per event id, so the arm is
/// determined; an event reached with no visible `HasEvent` forms no slot.
enum EventSlot<'f, EntId: Ord, EvtId: Ord, ImgId: Ord> {
    Durational(DurationalSlot<'f, EntId, EvtId, ImgId>),
    Point(PointSlot<'f, EntId, EvtId, ImgId>),
}

/// Interior events: facts grouped by [`LifetimeEventId`](crate::facts::ids), each
/// projected to its category arm. The arm and `kind` come from the event's
/// `HasEvent` claim; the payload facts feed only their data slots. Durational
/// events split their date claims by role into independent `started_at` /
/// `completed_at` slots; point events join into one `occurred_at`. The event list
/// is sorted by a stable deterministic key (earliest claimed date, then kind,
/// then id) so the projection and its addressed paths reproduce.
pub(super) fn project_events<EntId, EvtId, ImgId>(
    facts: &BTreeMap<FactId, StoredFact<EntId, EvtId, ImgId>>,
    events: &mut Vec<ProjectedLifetimeEvent<EvtId>>,
    citations: &mut CitationMap<ImgId>,
) where
    EntId: Ord,
    EvtId: Ord + Clone,
    ImgId: Ord + Clone,
{
    // First pass: each event's HasEvent claim picks its arm and kind. An event
    // with no visible HasEvent isn't projected — a payload reached without its
    // typing claim has no category to slot under. The arm follows the kind's
    // category, and the kind feeds the matching arm's set.
    let mut slots: BTreeMap<EvtId, EventSlot<'_, EntId, EvtId, ImgId>> = BTreeMap::new();
    for fact in facts.values() {
        let Some(event::Fact::HasEvent { event, kind, .. }) = fact.event_fact() else {
            continue;
        };
        match kind {
            LifetimeEventKind::Durational { kind } => {
                let EventSlot::Durational(d) = slots
                    .entry(event.clone())
                    .or_insert_with(|| EventSlot::Durational(DurationalSlot::default()))
                else {
                    // The event already holds an opposite-category claim. A single
                    // event being both durational and point is a contradiction;
                    // conflict detection owns surfacing it, so no kind is assigned.
                    continue;
                };
                d.kind.add(*kind, fact);
            }
            LifetimeEventKind::Point { kind } => {
                let EventSlot::Point(p) = slots
                    .entry(event.clone())
                    .or_insert_with(|| EventSlot::Point(PointSlot::default()))
                else {
                    // The event already holds an opposite-category claim. A single
                    // event being both durational and point is a contradiction;
                    // conflict detection owns surfacing it, so no kind is assigned.
                    continue;
                };
                p.kind.add(*kind, fact);
            }
        }
    }

    // Second pass: payloads feed their event's date / location slots.
    for fact in facts.values() {
        let Some(ev) = fact.event_fact() else {
            continue;
        };
        let Some(slot) = slots.get_mut(ev.subject()) else {
            continue;
        };
        match (slot, ev) {
            (
                EventSlot::Durational(d),
                event::Fact::DurationalDate {
                    role: DurationalRole::Started,
                    bound,
                    ..
                },
            ) => d.started.add(bound.clone(), fact),
            (
                EventSlot::Durational(d),
                event::Fact::DurationalDate {
                    role: DurationalRole::Completed,
                    bound,
                    ..
                },
            ) => d.completed.add(bound.clone(), fact),
            (EventSlot::Durational(d), event::Fact::MovedToLocation { location, .. }) => {
                d.location.add(location.clone(), fact);
            }
            (EventSlot::Point(p), event::Fact::PointDate { bound, .. }) => {
                p.occurred.add(bound.clone(), fact);
            }
            // The data-free payloads (a damage cause, a move method, a usage
            // change, a designation) and `Description` carry no date or location
            // to slot. A payload whose category contradicts its event's declared
            // kind can't reach here: the submit consistency rule rejects it
            // cumulatively, so a `MovedToLocation` only ever meets a durational
            // slot and a `PointDate` only a point slot.
            _ => {}
        }
    }

    let mut built: Vec<(
        ProjectedLifetimeEvent<EvtId>,
        EventSlot<'_, EntId, EvtId, ImgId>,
    )> = slots
        .into_iter()
        .map(|(event, slot)| {
            let projected = match &slot {
                EventSlot::Durational(d) => ProjectedLifetimeEvent::Durational {
                    event,
                    kind: d.kind.joined(),
                    started_at: d.started.joined(),
                    completed_at: d.completed.joined(),
                    location: d.location.joined(),
                },
                EventSlot::Point(p) => ProjectedLifetimeEvent::Point {
                    event,
                    kind: p.kind.joined(),
                    occurred_at: p.occurred.joined(),
                },
            };
            (projected, slot)
        })
        .collect();
    built.sort_by(|(a, _), (b, _)| event_sort_key(a).cmp(&event_sort_key(b)));

    for (idx, (event, slot)) in built.into_iter().enumerate() {
        let base = JsonPath::root().field("events").index(idx);
        match &slot {
            EventSlot::Durational(d) => {
                citations.insert_supports(base.clone().field("started_at"), d.started.supports());
                citations
                    .insert_supports(base.clone().field("completed_at"), d.completed.supports());
                citations.insert_supports(base.clone().field("kind"), d.kind.supports());
                citations.insert_supports(base.field("location"), d.location.supports());
            }
            EventSlot::Point(p) => {
                citations.insert_supports(base.clone().field("occurred_at"), p.occurred.supports());
                citations.insert_supports(base.field("kind"), p.kind.supports());
            }
        }
        events.push(event);
    }
}

/// A stable, deterministic ordering key for an interior event: earliest claimed
/// date first (events with no date sort last), then category and kind, then id.
/// The kind rank is the minimum over the declared-kind set (a singleton today,
/// so the minimum is that one kind's rank); an empty set ranks last.
fn event_sort_key<EvtId: Ord + Clone>(
    ev: &ProjectedLifetimeEvent<EvtId>,
) -> (bool, Option<chrono::NaiveDate>, u8, EvtId) {
    let (date, kind_rank) = match ev {
        ProjectedLifetimeEvent::Durational {
            started_at,
            completed_at,
            kind,
            ..
        } => {
            let date = started_at
                .as_ref()
                .or(completed_at.as_ref())
                .and_then(UncertainDate::earliest);
            (date, min_rank(kind, durational_rank))
        }
        ProjectedLifetimeEvent::Point {
            occurred_at, kind, ..
        } => {
            let date = occurred_at.as_ref().and_then(UncertainDate::earliest);
            (date, min_rank(kind, point_rank))
        }
    };
    // Dateless events sort last: the presence flag (`false` before `true`)
    // leads, so `Some(_)` events precede `None` regardless of the date itself.
    (date.is_none(), date, kind_rank, ev.event().clone())
}

/// The minimum rank over a declared-kind set; an empty set ranks last.
fn min_rank<K: Copy>(kinds: &BTreeSet<K>, rank: impl Fn(K) -> u8) -> u8 {
    kinds.iter().map(|k| rank(*k)).min().unwrap_or(u8::MAX)
}

fn durational_rank(kind: DurationalKind) -> u8 {
    match kind {
        DurationalKind::Modified => 0,
        DurationalKind::Damaged => 1,
        DurationalKind::Repaired => 2,
        DurationalKind::Moved => 3,
    }
}

fn point_rank(kind: PointKind) -> u8 {
    match kind {
        PointKind::UsageChanged => 0,
        PointKind::Designated => 1,
    }
}
