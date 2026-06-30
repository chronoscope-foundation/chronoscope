//! The generic merge: `inject` a fact to a sparse entity, then fold by join.
//!
//! Every projected field is a [`Slot`], so the whole merge is
//! `facts.map(inject).fold(bottom, join)` — one fold over the product, no
//! per-field projectors. [`inject`] turns one stored fact into a mostly-`bottom`
//! entity with a single leaf set, tagging that leaf's support through the
//! caller's `provenance` closure.
//!
//! Provenance is two-argument: the closure sees the *source id* the fact spoke
//! to alongside the fact's citation. The source id is the minted entity the
//! claim belongs to — intrinsic for an entity-subject fact (its own subject),
//! the reaching member for an interior event (which member's `HasEvent` bridged
//! to it). Keeping the id in the atom lets the read side compute load-bearing
//! (Green, Karvounarakis & Tannen 2007).

use std::collections::{BTreeMap, BTreeSet};

use crate::algebra::lattice::BoundedLattice;
use crate::algebra::monoid::CommutativeMonoid;
use crate::algebra::semiring::Semiring;
use crate::claimed::Claimed;
use crate::date::UncertainDate;
use crate::facts::assertions::{FactualAssertion, JudgmentAssertion};
use crate::facts::attribute;
use crate::facts::bookend;
use crate::facts::event;
use crate::facts::identity;
use crate::facts::ids::{FactId, IdScheme};
use crate::facts::lifecycle::DurationalRole;
use crate::facts::submit::StoredFact;

use super::bracket::Bracket;
use super::provenance::{Citation, Cited};
use super::types::{Bookend, Entity, Event, NameKey, NameRecord};

/// The citation a single fact warrants, when it warrants one. A meta fact backs
/// no value, so it cites nothing.
pub(super) fn citation_of<R: IdScheme>(fact: &StoredFact<R>) -> Option<Citation<R::Image>> {
    match fact {
        StoredFact::Factual(f) => Some(Citation::Factual {
            citation: f.citation.clone(),
        }),
        StoredFact::Judgment(j) => Some(Citation::Judgment {
            source: j.source.clone(),
        }),
        StoredFact::Meta(_) => None,
    }
}

/// Each event's one owning entity, named by its `HasEvent`.
///
/// An interior event fact carries no entity ref of its own. Submit's
/// `rule_event_has_one_kind` rejects a second distinct `HasEvent` on an event id
/// (`EventMultipleHasEvent`) across the cumulative neighbourhood, so a reachable
/// event has exactly one owner — one source id, not a set.
pub(crate) fn event_reachers<R: IdScheme>(
    facts: &BTreeMap<FactId, StoredFact<R>>,
) -> BTreeMap<R::Event, R::Entity> {
    let mut reachers: BTreeMap<R::Event, R::Entity> = BTreeMap::new();
    for fact in facts.values() {
        if let Some(event::Fact::HasEvent { entity, event, .. }) = fact.event_fact() {
            reachers.insert(event.clone(), entity.clone());
        }
    }
    reachers
}

/// Merge an entity's facts into a [`Entity`]. The generic fold over the
/// product of slots, with provenance carried by the semiring `T`.
///
/// `provenance` lifts a `(source id, citation)` pair into the semiring. The
/// **member-aware lineage** (`BTreeSet<(EntId, Citation)>`) is the first
/// instance, and the generic is deliberate: the roadmap swaps in richer `T` over
/// the same fold — the raw expression tree `N[A]`, why-provenance, `PosBool`/ATMS
/// nogoods, support counts — each answering a finer provenance question with no
/// change here.
///
/// `reachers` maps each interior event id to the one entity whose `HasEvent`
/// named it (see [`event_reachers`]); that entity is the event fact's source id.
pub(crate) fn project_facts<R: IdScheme, T>(
    facts: &BTreeMap<FactId, StoredFact<R>>,
    reachers: &BTreeMap<R::Event, R::Entity>,
    provenance: impl Fn(&R::Entity, &Citation<R::Image>) -> T,
) -> Entity<R::Entity, R::Event, T>
where
    T: Semiring + Clone,
{
    facts
        .values()
        .map(|fact| inject(fact, reachers, &provenance))
        .fold(
            <Entity<R::Entity, R::Event, T> as CommutativeMonoid>::identity(),
            CommutativeMonoid::combine,
        )
}

/// One fact as a sparse, mostly-identity entity. A factual fact lands a single
/// leaf set, its support the fact's citation lifted through `provenance` against
/// the source id the fact spoke to. A `SameEntity` judgment lands one `sameness`
/// edge. A meta fact backs nothing.
fn inject<R: IdScheme, T>(
    fact: &StoredFact<R>,
    reachers: &BTreeMap<R::Event, R::Entity>,
    provenance: &impl Fn(&R::Entity, &Citation<R::Image>) -> T,
) -> Entity<R::Entity, R::Event, T>
where
    T: Semiring + Clone,
{
    match fact {
        StoredFact::Factual(f) => {
            let Some(citation) = citation_of(fact) else {
                return Entity::identity();
            };
            inject_factual(&f.assertion, reachers, &citation, provenance)
        }
        StoredFact::Judgment(j) => {
            let Some(citation) = citation_of(fact) else {
                return Entity::identity();
            };
            inject_judgment(&j.assertion, &citation, provenance)
        }
        StoredFact::Meta(_) => Entity::identity(),
    }
}

/// A factual assertion's contribution. The source id is the fact's own subject
/// entity for entity-level claims; for an interior event it is the one entity
/// whose `HasEvent` owns the event.
fn inject_factual<R: IdScheme, T>(
    assertion: &FactualAssertion<R>,
    reachers: &BTreeMap<R::Event, R::Entity>,
    citation: &Citation<R::Image>,
    provenance: &impl Fn(&R::Entity, &Citation<R::Image>) -> T,
) -> Entity<R::Entity, R::Event, T>
where
    T: Semiring + Clone,
{
    match assertion {
        FactualAssertion::Attribute { fact } => {
            let support = provenance(fact.subject(), citation);
            let mut entity = Entity::identity();
            inject_attribute(fact, support, &mut entity);
            entity
        }
        FactualAssertion::Construction { fact } => {
            let support = provenance(fact.subject(), citation);
            let mut entity = Entity::identity();
            inject_bookend(fact, support, &mut entity.construction);
            entity
        }
        FactualAssertion::Demolition { fact } => {
            let support = provenance(fact.subject(), citation);
            let mut entity = Entity::identity();
            inject_bookend(fact, support, &mut entity.demolition);
            entity
        }
        FactualAssertion::Event { fact } => inject_event_fact(fact, reachers, citation, provenance),
        // A gap is an ordering relationship, not a single subject's field.
        FactualAssertion::Gap { .. } => Entity::identity(),
        // Image facts feed the sibling image projection, not an entity field.
        FactualAssertion::Image { .. } => Entity::identity(),
    }
}

/// A `SameEntity` judgment's contribution: one glue edge keyed by its endpoint
/// pair. The judgment's support tags the edge symmetrically — against both `a`
/// and `b` — so a field either endpoint asserted into can find the edge by id,
/// and `Citation::Judgment` stays live. Every other judgment kind backs no
/// entity field.
fn inject_judgment<R: IdScheme, T>(
    assertion: &JudgmentAssertion<R>,
    citation: &Citation<R::Image>,
    provenance: &impl Fn(&R::Entity, &Citation<R::Image>) -> T,
) -> Entity<R::Entity, R::Event, T>
where
    T: Semiring,
{
    let JudgmentAssertion::Identity {
        fact: identity::Fact::SameEntity { pair },
    } = assertion
    else {
        return Entity::identity();
    };
    let support = provenance(pair.a(), citation).plus(provenance(pair.b(), citation));
    let mut entity = Entity::identity();
    entity
        .sameness
        .insert(pair.clone(), Cited { value: (), support });
    entity
}

fn inject_attribute<EntId, EvtId, T>(
    fact: &attribute::Fact<EntId>,
    support: T,
    entity: &mut Entity<EntId, EvtId, T>,
) where
    EntId: Ord + Clone,
    EvtId: Ord,
    T: Semiring + Clone,
{
    match fact {
        attribute::Fact::Name {
            name,
            language,
            name_type,
            valid_from,
            valid_to,
            ..
        } => {
            let key = NameKey {
                name: name.clone(),
                language: language.clone(),
                name_type: *name_type,
            };
            let record = NameRecord {
                valid_from: optional_date(valid_from.as_ref(), support.clone()),
                valid_to: optional_date(valid_to.as_ref(), support.clone()),
            };
            entity.names.insert(
                key,
                Cited {
                    value: record,
                    support,
                },
            );
        }
        attribute::Fact::ExternalReference { reference, .. } => {
            entity
                .refs
                .insert(reference.clone(), Cited { value: (), support });
        }
        attribute::Fact::Relationship { pair, relation } => {
            let mut kinds = BTreeMap::new();
            kinds.insert(
                *relation,
                Cited {
                    value: (),
                    support: support.clone(),
                },
            );
            entity.relations.insert(
                pair.to().clone(),
                Cited {
                    value: kinds,
                    support,
                },
            );
        }
    }
}

fn inject_bookend<EntId, T>(fact: &bookend::Fact<EntId>, support: T, bookend: &mut Bookend<T>)
where
    T: Semiring + Clone,
{
    match fact {
        bookend::Fact::Started { bound, .. } => {
            bookend.started_at = Bracket::from((bound.clone(), support));
        }
        bookend::Fact::Completed { bound, .. } => {
            bookend.completed_at = Bracket::from((bound.clone(), support));
        }
        bookend::Fact::Location { location, .. } => {
            bookend.location = Bracket::from((location.clone(), support));
        }
    }
}

/// An interior event fact's contribution, tagged with the one entity whose
/// `HasEvent` owns the event. An event with no owner (no `HasEvent` in the bag)
/// contributes nothing.
fn inject_event_fact<EntId, EvtId, ImgId, T>(
    fact: &event::Fact<EntId, EvtId>,
    reachers: &BTreeMap<EvtId, EntId>,
    citation: &Citation<ImgId>,
    provenance: &impl Fn(&EntId, &Citation<ImgId>) -> T,
) -> Entity<EntId, EvtId, T>
where
    EntId: Ord + Clone,
    EvtId: Ord + Clone,
    ImgId: Ord + Clone,
    T: Semiring + Clone,
{
    let Some(owner) = reachers.get(fact.subject()) else {
        return Entity::identity();
    };
    let support = provenance(owner, citation);
    let mut entity = Entity::identity();
    inject_event(fact, support, &mut entity);
    entity
}

fn inject_event<EntId, EvtId, T>(
    fact: &event::Fact<EntId, EvtId>,
    support: T,
    entity: &mut Entity<EntId, EvtId, T>,
) where
    EntId: Ord,
    EvtId: Ord + Clone,
    T: Semiring + Clone,
{
    let mut record = Event::identity();
    match fact {
        event::Fact::HasEvent { kind, .. } => {
            record.kind = claimed_of(*kind, support.clone());
        }
        event::Fact::DurationalDate {
            role: DurationalRole::Started,
            bound,
            ..
        } => record.started_at = Bracket::from((bound.clone(), support.clone())),
        event::Fact::DurationalDate {
            role: DurationalRole::Completed,
            bound,
            ..
        } => record.completed_at = Bracket::from((bound.clone(), support.clone())),
        event::Fact::PointDate { bound, .. } => {
            record.occurred_at = Bracket::from((bound.clone(), support.clone()));
        }
        event::Fact::MovedToLocation { location, .. } => {
            record.location = Bracket::from((location.clone(), support.clone()));
        }
        event::Fact::DamageCause { cause, .. } => {
            record.cause = claimed_of(cause.clone(), support.clone());
        }
        event::Fact::MoveMethod { method, .. } => {
            record.method = claimed_of(*method, support.clone());
        }
        event::Fact::UsageChange { new_usages, .. } => {
            record.usages = claimed_of(new_usages.clone(), support.clone());
        }
        event::Fact::Designation { designation, .. } => {
            record.designation = claimed_of(designation.clone(), support.clone());
        }
        event::Fact::Description { text, .. } => {
            record.descriptions.insert(
                text.clone(),
                Cited {
                    value: (),
                    support: support.clone(),
                },
            );
        }
    }
    entity.events.insert(
        fact.subject().clone(),
        Cited {
            value: record,
            support,
        },
    );
}

/// A bracket over an optional date claim: the supplied bound when present, an
/// unsupported ⊤/⊥ seed (no claim) when absent. A name window is `Option`, so a
/// missing endpoint contributes no constraint.
fn optional_date<T: Semiring + Clone>(
    date: Option<&UncertainDate>,
    support: T,
) -> Bracket<UncertainDate, T> {
    match date {
        Some(d) => Bracket::from((d.clone(), support)),
        None => Bracket::identity(),
    }
}

/// A value-mode bracket pinning one claimed value: `Claimed::Of { values: {value} }`.
/// The whole value is the atom — two facts claiming different values meet to the
/// empty set, the over-determined conflict.
fn claimed_of<A, T>(value: A, support: T) -> Bracket<Claimed<A>, T>
where
    A: Ord + Clone,
    Claimed<A>: BoundedLattice,
    T: Semiring + Clone,
{
    Bracket::from((
        Claimed::Of {
            values: BTreeSet::from([value]),
        },
        support,
    ))
}
