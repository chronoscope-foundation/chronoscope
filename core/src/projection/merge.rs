//! The generic merge: `inject` a fact to a sparse entity, then fold by join.
//!
//! Every projected field is a [`Slot`], so the whole merge is
//! `facts.map(inject).fold(bottom, join)` — one fold over the product, no
//! per-field projectors. [`inject`] turns one stored fact into a mostly-`bottom`
//! entity with a single leaf set, tagging that leaf's support through the
//! caller's `provenance` closure.
//!
//! Provenance is three-argument: the closure sees the fact's own id and the
//! whole stored fact alongside the *source id* the fact spoke to. The source id
//! is the minted entity the claim belongs to — intrinsic for an entity-subject
//! fact (its own subject), the reaching member for an interior event (which
//! member's `HasEvent` bridged to it). Keeping the id in the atom lets the read
//! side compute load-bearing (Green, Karvounarakis & Tannen 2007).

use std::collections::{BTreeMap, BTreeSet};

use crate::algebra::lattice::BoundedLattice;
use crate::algebra::monoid::CommutativeMonoid;
use crate::algebra::semiring::Semiring;
use crate::date::UncertainDate;
use crate::grammar::assertions::{FactualAssertion, JudgmentAssertion};
use crate::grammar::attribute;
use crate::grammar::bookend;
use crate::grammar::composites;
use crate::grammar::depiction;
use crate::grammar::event;
use crate::grammar::identity;
use crate::grammar::ids::{FactId, IdScheme};
use crate::grammar::image;
use crate::grammar::lifecycle::DurationalRole;
use crate::lifespan::Lifespan;
use crate::projection::Claimed;
use crate::submit::StoredFact;

use super::bounds::{self, Chain};
use super::bracket::Bracket;
use super::provenance::{Citation, Cited, Stamp};
use super::types::{
    Bookend, DepictionRecord, Entity, Event, Image, NameKey, NameRecord, RegionRecord,
};

/// The citation a single fact warrants, when it warrants one. A meta fact backs
/// no value, so it cites nothing.
pub(crate) fn citation_of<R: IdScheme>(fact: &StoredFact<R>) -> Option<Citation<R::Image>> {
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
        if let Some((event, entity)) = fact.has_event_owner() {
            reachers.insert(event.clone(), entity.clone());
        }
    }
    reachers
}

/// Merge an entity's facts into a [`Entity`]. The generic fold over the
/// product of slots, with provenance carried by the semiring `T`.
///
/// `provenance` lifts a `(fact id, source id, stored fact)` triple into the
/// semiring. The **member-aware lineage** (`BTreeSet<(EntId, Citation)>`) is the first
/// instance, and the generic is deliberate: the roadmap swaps in richer `T` over
/// the same fold — the raw expression tree `N[A]`, why-provenance, `PosBool`/ATMS
/// nogoods, support counts — each answering a finer provenance question with no
/// change here.
///
/// `reachers` maps each interior event id to the one entity whose `HasEvent`
/// named it (see [`event_reachers`]); that entity is the event fact's source id.
///
/// `members` is the class being projected. A binary-relation fact folds as an
/// incident edge — keyed by its far endpoint, kept only when its near (subject)
/// endpoint is a member (see [`same_space_target`]) — so a relationship drained
/// through its target's backlinks lands on the source's projection, not as a
/// self-loop on the target.
///
/// A [pre-pass](bounds::chain) over the same bag computes the class's ordering
/// constraints first, and each bookend claim is narrowed against them on the way
/// in. It runs here rather than over the projection because rivals are
/// alternatives: by the time a slot has merged them, the per-assertion
/// distinction a narrowing turns on is gone.
pub(crate) fn project_facts<R: IdScheme, T>(
    facts: &BTreeMap<FactId, StoredFact<R>>,
    members: &BTreeSet<R::Entity>,
    reachers: &BTreeMap<R::Event, R::Entity>,
    provenance: impl Fn(&FactId, &R::Entity, &StoredFact<R>) -> T,
) -> Entity<R::Entity, R::Event, R::Image, T>
where
    T: Semiring + Clone + Stamp,
{
    let chain = bounds::chain(facts, reachers, &provenance);
    facts
        .iter()
        .map(|(fact_id, fact)| inject(fact_id, fact, members, reachers, &chain, &provenance))
        .fold(
            <Entity<R::Entity, R::Event, R::Image, T> as CommutativeMonoid>::identity(),
            CommutativeMonoid::combine,
        )
}

/// One fact as a sparse, mostly-identity entity. A factual fact lands a single
/// leaf set, its support the fact's citation lifted through `provenance` against
/// the source id the fact spoke to. A `SameEntity` judgment lands one `sameness`
/// edge; a `Depiction` judgment lands one `depictions` edge keyed by image. A
/// meta fact backs nothing.
fn inject<R: IdScheme, T>(
    fact_id: &FactId,
    fact: &StoredFact<R>,
    members: &BTreeSet<R::Entity>,
    reachers: &BTreeMap<R::Event, R::Entity>,
    chain: &Chain<T>,
    provenance: &impl Fn(&FactId, &R::Entity, &StoredFact<R>) -> T,
) -> Entity<R::Entity, R::Event, R::Image, T>
where
    T: Semiring + Clone + Stamp,
{
    match fact {
        StoredFact::Factual(f) => inject_factual(
            fact_id,
            &f.assertion,
            fact,
            members,
            reachers,
            chain,
            provenance,
        ),
        StoredFact::Judgment(j) => {
            inject_judgment(fact_id, &j.assertion, fact, members, provenance)
        }
        StoredFact::Meta(_) => Entity::identity(),
    }
}

/// A factual assertion's contribution. The source id is the fact's own subject
/// entity for entity-level claims; for an interior event it is the one entity
/// whose `HasEvent` owns the event.
///
/// A bookend claim meets the class's ordering constraints here, above both of
/// its readers. The claim feeds the slot's [`Bracket`], which a bound is
/// displayed from, and [`Lifespan`], which the served existence verdict folds;
/// narrowing above the split is what keeps a tightened bound and a tightened
/// render from disagreeing.
fn inject_factual<R: IdScheme, T>(
    fact_id: &FactId,
    assertion: &FactualAssertion<R>,
    stored: &StoredFact<R>,
    members: &BTreeSet<R::Entity>,
    reachers: &BTreeMap<R::Event, R::Entity>,
    chain: &Chain<T>,
    provenance: &impl Fn(&FactId, &R::Entity, &StoredFact<R>) -> T,
) -> Entity<R::Entity, R::Event, R::Image, T>
where
    T: Semiring + Clone + Stamp,
{
    match assertion {
        FactualAssertion::Attribute { fact } => {
            let support = provenance(fact_id, fact.subject(), stored);
            let mut entity = Entity::identity();
            inject_attribute(fact, members, support, &mut entity);
            entity
        }
        FactualAssertion::Construction { fact } => {
            let support = provenance(fact_id, fact.subject(), stored);
            let narrowing = bounds::narrow_construction(chain, fact);
            let (fact, support) = match narrowing.as_ref() {
                Some((narrowed, stamp)) => (narrowed, support.times(stamp.clone())),
                None => (fact, support),
            };
            let mut entity = Entity::identity();
            inject_construction(fact, support, &mut entity.construction);
            entity.lifespan = construction_lifespan(fact);
            entity
        }
        FactualAssertion::Demolition { fact } => {
            let support = provenance(fact_id, fact.subject(), stored);
            let narrowing = bounds::narrow_demolition(chain, fact);
            let (fact, support) = match narrowing.as_ref() {
                Some((narrowed, stamp)) => (narrowed, support.times(stamp.clone())),
                None => (fact, support),
            };
            let mut entity = Entity::identity();
            inject_demolition(fact, support, &mut entity.demolition);
            entity.lifespan = demolition_lifespan(fact);
            entity
        }
        FactualAssertion::Existence { fact } => {
            let support = provenance(fact_id, fact.subject(), stored);
            let mut entity = Entity::identity();
            entity
                .existence
                .insert(fact.at.clone(), Cited { value: (), support });
            entity.lifespan = Lifespan::witness(fact.at.clone());
            entity
        }
        FactualAssertion::Event { fact } => inject_event_fact(
            fact_id,
            fact,
            stored,
            reachers,
            provenance,
            event_lifespan(fact),
        ),
        // A gap is an ordering relationship, not a single subject's field.
        FactualAssertion::Gap { .. } => Entity::identity(),
        // Image facts feed the sibling image projection, not an entity field.
        FactualAssertion::Image { .. } => Entity::identity(),
    }
}

// ============================================================================
// Existence contributions
//
// The existence claim one factual assertion makes, as a singleton for the
// `lifespan` fold. The four bookend endpoints carry their own deny and affirm
// powers; an existence attestation and an interior event's date are pure
// witnesses. Every other assertion is silent about existence, so it folds as the
// identity — which the caller's own exhaustive match spells, so a new assertion
// cluster is a compile error there.
//
// Quantification is per *assertion*: two sources claiming different construction
// starts each deny their own past, and the later one wins the floor. That
// distinction is gone by the time the claims have merged into a bracket, which
// is why this rides the single fact.
//
// So a claim whose slot consensus came out empty still folds here, with full
// force — including the existence it guarantees, which can refute a demolition.
// Rivals are alternatives under this reading, not a joint claim to be
// reconciled: a disputed sighting is still a source saying the entity stood, and
// the render says so by contesting the instant rather than by deciding it.
// Dropping the losing side would be the fold quietly picking a winner.
// ============================================================================

/// A construction endpoint's existence claim.
fn construction_lifespan<R: IdScheme>(fact: &bookend::ConstructionFact<R>) -> Lifespan {
    match fact {
        bookend::ConstructionFact::Started { bound, .. } => {
            Lifespan::construction_started(bound.clone())
        }
        bookend::ConstructionFact::Completed { bound, .. } => {
            Lifespan::construction_completed(bound.clone())
        }
        // Kept as an exhaustive match rather than a trailing wildcard: a new
        // dated fact variant is then a compile error here, not a silent
        // fall-through to the identity.
        bookend::ConstructionFact::Location { .. } => Lifespan::identity(),
    }
}

/// A demolition endpoint's existence claim.
fn demolition_lifespan<R: IdScheme>(fact: &bookend::DemolitionFact<R>) -> Lifespan {
    match fact {
        bookend::DemolitionFact::Started { bound, .. } => {
            Lifespan::demolition_started(bound.clone())
        }
        bookend::DemolitionFact::Completed { bound, .. } => {
            Lifespan::demolition_completed(bound.clone())
        }
    }
}

/// An interior event's existence claim — its date is a witness, since an event
/// implies the entity was there for it.
fn event_lifespan<R: IdScheme>(fact: &event::Fact<R>) -> Lifespan {
    match fact {
        event::Fact::DurationalDate { bound, .. } | event::Fact::PointDate { bound, .. } => {
            Lifespan::witness(bound.clone())
        }
        event::Fact::HasEvent { .. }
        | event::Fact::MovedToLocation { .. }
        | event::Fact::DamageCause { .. }
        | event::Fact::MoveMethod { .. }
        | event::Fact::UsageChange { .. }
        | event::Fact::Designation { .. }
        | event::Fact::Description { .. } => Lifespan::identity(),
    }
}

/// A judgment's contribution to the entity. A `SameEntity` records one glue edge
/// keyed by its endpoint pair, its support tagging both `a` and `b` symmetrically
/// so a field either endpoint asserted into can find the edge by id and
/// `Citation::Judgment` stays live. A `Depiction` folds into `depictions`, keyed
/// by the depicted image, when this class is the depicted entity (its end of the
/// cross-id-space edge is a member). Other judgment kinds contribute the identity
/// entity.
fn inject_judgment<R: IdScheme, T>(
    fact_id: &FactId,
    assertion: &JudgmentAssertion<R>,
    stored: &StoredFact<R>,
    members: &BTreeSet<R::Entity>,
    provenance: &impl Fn(&FactId, &R::Entity, &StoredFact<R>) -> T,
) -> Entity<R::Entity, R::Event, R::Image, T>
where
    T: Semiring + Clone,
{
    match assertion {
        JudgmentAssertion::Identity {
            fact: identity::Fact::SameEntity { pair },
        } => {
            let support =
                provenance(fact_id, pair.a(), stored).plus(provenance(fact_id, pair.b(), stored));
            let mut entity = Entity::identity();
            entity
                .sameness
                .insert(pair.clone(), Cited { value: (), support });
            entity
        }
        JudgmentAssertion::Depiction { fact } => {
            let mut entity = Entity::identity();
            if let Some((image, entry)) = depiction_edge(
                fact_id,
                fact,
                &fact.entity,
                &fact.image,
                members,
                stored,
                provenance,
            ) {
                entity.depictions.insert(image.clone(), entry);
            }
            entity
        }
        _ => Entity::identity(),
    }
}

/// The far endpoint to key an outgoing edge under, when `near → far` crosses
/// the class boundary: kept when `near` is a member and `far` is not. An edge
/// with both ends in the class is a degenerate self-edge and folds to nothing.
fn same_space_target<'a, N: Ord>(near: &N, far: &'a N, members: &BTreeSet<N>) -> Option<&'a N> {
    (members.contains(near) && !members.contains(far)).then_some(far)
}

/// The far endpoint to key a cross-id-space edge under: kept when `near` is a
/// member. `far` lives in another id space, so it can never be a member — there
/// is no self-edge to exclude, unlike `same_space_target`.
fn cross_space_target<'a, N: Ord, F>(near: &N, far: &'a F, members: &BTreeSet<N>) -> Option<&'a F> {
    members.contains(near).then_some(far)
}

fn inject_attribute<R: IdScheme, T>(
    fact: &attribute::Fact<R>,
    members: &BTreeSet<R::Entity>,
    support: T,
    entity: &mut Entity<R::Entity, R::Event, R::Image, T>,
) where
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
            if let Some(target) = same_space_target(pair.from(), pair.to(), members) {
                let mut kinds = BTreeMap::new();
                kinds.insert(
                    *relation,
                    Cited {
                        value: (),
                        support: support.clone(),
                    },
                );
                entity.relations.insert(
                    target.clone(),
                    Cited {
                        value: kinds,
                        support,
                    },
                );
            }
        }
    }
}

fn inject_construction<R: IdScheme, T>(
    fact: &bookend::ConstructionFact<R>,
    support: T,
    bookend: &mut Bookend<T>,
) where
    T: Semiring + Clone,
{
    match fact {
        bookend::ConstructionFact::Started { bound, .. } => {
            bookend.started_at = Bracket::from((bound.clone(), support));
        }
        bookend::ConstructionFact::Completed { bound, .. } => {
            bookend.completed_at = Bracket::from((bound.clone(), support));
        }
        bookend::ConstructionFact::Location { location, .. } => {
            bookend.location = Bracket::from((location.clone(), support));
        }
    }
}

fn inject_demolition<R: IdScheme, T>(
    fact: &bookend::DemolitionFact<R>,
    support: T,
    bookend: &mut Bookend<T>,
) where
    T: Semiring + Clone,
{
    match fact {
        bookend::DemolitionFact::Started { bound, .. } => {
            bookend.started_at = Bracket::from((bound.clone(), support));
        }
        bookend::DemolitionFact::Completed { bound, .. } => {
            bookend.completed_at = Bracket::from((bound.clone(), support));
        }
    }
}

/// An interior event fact's contribution, tagged with the one entity whose
/// `HasEvent` owns the event. An event with no owner (no `HasEvent` in the bag)
/// contributes nothing.
///
/// `lifespan` is the event's existence witness, which rides that same ownership:
/// the event testifies for the entity whose lifetime holds it, so one gate
/// decides both.
fn inject_event_fact<R: IdScheme, Stored, T>(
    fact_id: &FactId,
    fact: &event::Fact<R>,
    stored: &Stored,
    reachers: &BTreeMap<R::Event, R::Entity>,
    provenance: &impl Fn(&FactId, &R::Entity, &Stored) -> T,
    lifespan: Lifespan,
) -> Entity<R::Entity, R::Event, R::Image, T>
where
    T: Semiring + Clone,
{
    let Some(owner) = reachers.get(fact.subject()) else {
        return Entity::identity();
    };
    let support = provenance(fact_id, owner, stored);
    let mut entity = Entity::identity();
    inject_event(fact, support, &mut entity);
    entity.lifespan = lifespan;
    entity
}

fn inject_event<R: IdScheme, T>(
    fact: &event::Fact<R>,
    support: T,
    entity: &mut Entity<R::Entity, R::Event, R::Image, T>,
) where
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
            record.designation = claimed_of(designation.as_str().to_owned(), support.clone());
        }
        event::Fact::Description { text, .. } => {
            record.descriptions.insert(
                text.as_str().to_owned(),
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

// ============================================================================
// Image projection
// ============================================================================

/// One depiction fact as a [`DepictionRecord`]: each present axis pins its
/// claimed value, an absent axis stays at the identity bracket. Shared by the
/// entity-side `depictions` and the image-side `depicts`, so both views pin the
/// same axes; each side's support cites its own subject.
pub(super) fn inject_depiction<R: IdScheme, T>(
    fact: &depiction::Fact<R>,
    support: &T,
) -> DepictionRecord<T>
where
    T: Semiring + Clone,
{
    let mut record = DepictionRecord::identity();
    match (&fact.localization, &fact.perspective) {
        (Some(geometry), Some(perspective)) => {
            record.localization = claimed_of(geometry.clone(), support.clone());
            record.perspective = claimed_of(*perspective, support.clone());
        }
        (Some(geometry), None) => {
            record.localization = claimed_of(geometry.clone(), support.clone());
        }
        (None, Some(perspective)) => record.perspective = claimed_of(*perspective, support.clone()),
        (None, None) => {}
    }
    record
}

/// One depiction edge, gated and supported once for both fold directions.
///
/// Yields the far endpoint to key the edge under and the [`Cited`]
/// [`DepictionRecord`], kept only when `near` — the depiction end the projected
/// class holds — is a member (see [`cross_space_target`]). The entity side
/// passes `near = entity`, `far = image`; the image side swaps them. Both run
/// through here so the gate, the support, and the pinned axes stay identical
/// across the two views.
fn depiction_edge<'f, R: IdScheme, N, F, Stored, T>(
    fact_id: &FactId,
    fact: &depiction::Fact<R>,
    near: &N,
    far: &'f F,
    members: &BTreeSet<N>,
    stored: &Stored,
    provenance: &impl Fn(&FactId, &N, &Stored) -> T,
) -> Option<(&'f F, Cited<DepictionRecord<T>, T>)>
where
    N: Ord,
    T: Semiring + Clone,
{
    let far = cross_space_target(near, far, members)?;
    let support = provenance(fact_id, near, stored);
    let record = inject_depiction(fact, &support);
    Some((
        far,
        Cited {
            value: record,
            support,
        },
    ))
}

/// Merge an image's facts into an [`Image`] — the generic fold over the product
/// of slots, mirroring [`project_facts`].
///
/// `members` is the projected `SameArtifact` class. Image-level facts tag their
/// support with the image they name; a composite `IsSubimageOf` edge routes by
/// which end the class holds (see [`inject_subimage`]); a `SameArtifact`
/// judgment lands one `sameness` glue edge — the realizations' merge provenance,
/// mirroring [`Entity`]'s `SameEntity` glue.
pub(crate) fn project_image_facts<R: IdScheme, T>(
    facts: &BTreeMap<FactId, StoredFact<R>>,
    members: &BTreeSet<R::Image>,
    provenance: impl Fn(&FactId, &R::Image, &StoredFact<R>) -> T,
) -> Image<R::Entity, R::Image, T>
where
    T: Semiring + Clone,
{
    facts
        .iter()
        .map(|(fact_id, fact)| inject_image(fact_id, fact, members, &provenance))
        .fold(
            <Image<R::Entity, R::Image, T> as CommutativeMonoid>::identity(),
            CommutativeMonoid::combine,
        )
}

/// One fact as a sparse, mostly-identity image. A factual image fact lands a
/// single leaf; a depiction, composite, or `SameArtifact` judgment lands one
/// edge; a meta fact backs nothing.
fn inject_image<R: IdScheme, T>(
    fact_id: &FactId,
    fact: &StoredFact<R>,
    members: &BTreeSet<R::Image>,
    provenance: &impl Fn(&FactId, &R::Image, &StoredFact<R>) -> T,
) -> Image<R::Entity, R::Image, T>
where
    T: Semiring + Clone,
{
    match fact {
        StoredFact::Factual(f) => inject_image_factual(fact_id, &f.assertion, fact, provenance),
        StoredFact::Judgment(j) => {
            inject_image_judgment(fact_id, &j.assertion, fact, members, provenance)
        }
        StoredFact::Meta(_) => Image::identity(),
    }
}

/// The image-cluster facts of a factual assertion. Other factual clusters carry
/// the entity projection's fields.
fn inject_image_factual<R: IdScheme, T>(
    fact_id: &FactId,
    assertion: &FactualAssertion<R>,
    stored: &StoredFact<R>,
    provenance: &impl Fn(&FactId, &R::Image, &StoredFact<R>) -> T,
) -> Image<R::Entity, R::Image, T>
where
    T: Semiring + Clone,
{
    match assertion {
        FactualAssertion::Image { fact } => inject_image_fact(fact_id, fact, stored, provenance),
        _ => Image::identity(),
    }
}

/// An image fact's contribution to the image-field projection.
fn inject_image_fact<R: IdScheme, Stored, T>(
    fact_id: &FactId,
    fact: &image::Fact<R>,
    stored: &Stored,
    provenance: &impl Fn(&FactId, &R::Image, &Stored) -> T,
) -> Image<R::Entity, R::Image, T>
where
    T: Semiring + Clone,
{
    let mut image = Image::identity();
    match fact {
        image::Fact::Source { image: id, url } => {
            let support = provenance(fact_id, id, stored);
            image.urls.insert(url.clone(), Cited { value: (), support });
        }
        image::Fact::Medium { image: id, medium } => {
            let support = provenance(fact_id, id, stored);
            image.medium = claimed_of(*medium, support);
        }
        image::Fact::SubjectDate { image: id, bound } => {
            let support = provenance(fact_id, id, stored);
            image.subject_date = Bracket::from((bound.clone(), support));
        }
        image::Fact::Author { .. }
        | image::Fact::CreatedDate { .. }
        | image::Fact::CapturedDate { .. }
        | image::Fact::CapturedLocation { .. } => {}
    }
    image
}

/// A judgment's contribution to the image. A `Depiction` folds into `depicts`,
/// keyed by the depicted entity, when this class is the depicting image (its end
/// of the cross-id-space edge is a member). A composite `IsSubimageOf` routes
/// through [`inject_subimage`]. A `SameArtifact` judgment records one `sameness`
/// glue edge.
fn inject_image_judgment<R: IdScheme, T>(
    fact_id: &FactId,
    assertion: &JudgmentAssertion<R>,
    stored: &StoredFact<R>,
    members: &BTreeSet<R::Image>,
    provenance: &impl Fn(&FactId, &R::Image, &StoredFact<R>) -> T,
) -> Image<R::Entity, R::Image, T>
where
    T: Semiring + Clone,
{
    match assertion {
        JudgmentAssertion::Depiction { fact } => {
            let mut image = Image::identity();
            if let Some((entity, entry)) = depiction_edge(
                fact_id,
                fact,
                &fact.image,
                &fact.entity,
                members,
                stored,
                provenance,
            ) {
                image.depicts.insert(entity.clone(), entry);
            }
            image
        }
        JudgmentAssertion::Composite {
            fact:
                composites::Fact::IsSubimageOf {
                    subimage,
                    parent,
                    region,
                },
        } => inject_subimage(
            fact_id, subimage, parent, region, members, stored, provenance,
        ),
        JudgmentAssertion::Identity {
            fact: identity::Fact::SameArtifact { pair },
        } => {
            let support =
                provenance(fact_id, pair.a(), stored).plus(provenance(fact_id, pair.b(), stored));
            let mut image = Image::identity();
            image
                .sameness
                .insert(pair.clone(), Cited { value: (), support });
            image
        }
        _ => Image::identity(),
    }
}

/// One `IsSubimageOf` edge, routed by which end the projected class holds: a
/// member-as-parent records the `subimage`, a member-as-subimage records the
/// `parent`. A class holding both ends keys neither — each routing's far
/// endpoint is itself a member, so [`same_space_target`] declines it.
fn inject_subimage<EntId, ImgId, Stored, T>(
    fact_id: &FactId,
    subimage: &ImgId,
    parent: &ImgId,
    region: &composites::SubimageRegion,
    members: &BTreeSet<ImgId>,
    stored: &Stored,
    provenance: &impl Fn(&FactId, &ImgId, &Stored) -> T,
) -> Image<EntId, ImgId, T>
where
    EntId: Ord,
    ImgId: Ord + Clone,
    T: Semiring + Clone,
{
    let region_entry = |support: T| Cited {
        value: RegionRecord {
            region: claimed_of(*region, support.clone()),
        },
        support,
    };
    let mut image = Image::identity();
    // member is the parent → the subimage is its child region
    if let Some(child) = same_space_target(parent, subimage, members) {
        image.subimages.insert(
            child.clone(),
            region_entry(provenance(fact_id, parent, stored)),
        );
    }
    // member is the subimage → the parent is its enclosing image
    if let Some(enclosing) = same_space_target(subimage, parent, members) {
        image.parent.insert(
            enclosing.clone(),
            region_entry(provenance(fact_id, subimage, stored)),
        );
    }
    image
}
