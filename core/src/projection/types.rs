//! The projected value types — every field a [`Slot`], the whole entity a
//! product of them.

use std::collections::BTreeSet;

use url::Url;

use crate::algebra::semiring::Semiring;
use crate::date::UncertainDate;
use crate::grammar::attribute::{EntityRelationType, NameText, NameType};
use crate::grammar::citations::{ExternalReference, Language};
use crate::grammar::composites::SubimageRegion;
use crate::grammar::depiction::Perspective;
use crate::grammar::geometry::ImageGeometry;
use crate::grammar::identity::OrderedDistinctPair;
use crate::grammar::image::ImageMedium;
use crate::grammar::lifecycle::{DamageCause, LifetimeEventKind, MoveMethod, Usage};
use crate::location::UnresolvedLocation;
use crate::projection::Claimed;

use super::bracket::Bracket;
use super::provenance::MemberLineage;
use super::slot::{FactMap, FactSet, derive_slot};

/// The dedup key for a name claim: a name collapses only on an exact triple. Its
/// validity window rides as the value [`NameRecord`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NameKey {
    /// The name text, NFC-canonical.
    pub name: NameText,
    /// The name's language tag, canonical BCP-47.
    pub language: Language,
    /// What kind of name this is.
    pub name_type: NameType,
}

/// A name's validity window, the value slot under a [`NameKey`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameRecord<T> {
    /// When the name began applying.
    pub valid_from: Bracket<UncertainDate, T>,
    /// When the name stopped applying.
    pub valid_to: Bracket<UncertainDate, T>,
}

derive_slot!(NameRecord<T: Semiring>, { valid_from, valid_to });

/// One depiction's two annotation axes, each a restrictive bracket: where in the
/// image's frame the entity sits, and the view classification.
///
/// Read from either subject — the entity's [`Entity::depictions`] keys it by
/// image, the image's [`Image::depicts`] by entity. Both sides carry the same
/// axis values; their support is subject-scoped — the entity side cites the
/// entity, the image side the image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepictionRecord<T> {
    /// Where in the image the entity sits, when a source localized it.
    pub localization: Bracket<Claimed<ImageGeometry>, T>,
    /// The view classification, when a source supplied one.
    pub perspective: Bracket<Claimed<Perspective>, T>,
}

derive_slot!(DepictionRecord<T: Semiring>, { localization, perspective });

/// A subimage edge's region: the restrictive bracket pinning where a subimage
/// sits in its parent. Carried on both ends of the edge — the parent's
/// [`Image::subimages`] and the subimage's [`Image::parent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionRecord<T> {
    /// The proportional region of the parent the subimage occupies.
    pub region: Bracket<Claimed<SubimageRegion>, T>,
}

derive_slot!(RegionRecord<T: Semiring>, { region });

/// A bookend phase (construction or demolition): its endpoint dates and
/// location, each a restrictive bracket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookend<T> {
    /// When the phase started.
    pub started_at: Bracket<UncertainDate, T>,
    /// When the phase completed.
    pub completed_at: Bracket<UncertainDate, T>,
    /// Where the phase took place — populated only for the construction
    /// phase, since demolition facts carry no location.
    pub location: Bracket<UnresolvedLocation, T>,
}

derive_slot!(Bookend<T: Semiring>, { started_at, completed_at, location });

/// One interior lifetime event, keyed by its `SameEvent` class.
///
/// Flat at the merge layer: the discriminant (`kind`) is itself a merged,
/// possibly-uncertain bracket, so the honest shape is a product over an
/// uncertain discriminant rather than a sum chosen up front.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event<T> {
    /// The event's declared kind(s) — a single value once sources agree.
    pub kind: Bracket<Claimed<LifetimeEventKind>, T>,
    /// When a durational event started.
    pub started_at: Bracket<UncertainDate, T>,
    /// When a durational event completed.
    pub completed_at: Bracket<UncertainDate, T>,
    /// When a point event occurred.
    pub occurred_at: Bracket<UncertainDate, T>,
    /// Where a `Moved` event landed.
    pub location: Bracket<UnresolvedLocation, T>,
    /// A `Damaged` event's cause.
    pub cause: Bracket<Claimed<DamageCause>, T>,
    /// A `Moved` event's method.
    pub method: Bracket<Claimed<MoveMethod>, T>,
    /// A `UsageChanged` event's post-event usage set — the whole set is one
    /// value-mode atom, not a membership union.
    pub usages: Bracket<Claimed<BTreeSet<Usage>>, T>,
    /// A `Designated` event's designation text.
    pub designation: Bracket<Claimed<String>, T>,
    /// Free-form descriptions of the event.
    pub descriptions: FactSet<String, T>,
}

derive_slot!(Event<T: Semiring>, {
    kind, started_at, completed_at, occurred_at, location,
    cause, method, usages, designation, descriptions
});

/// The class's `SameEntity` glue, keyed by the endpoint pair each judgment
/// asserts. Re-asserting an edge from a second source unions its support under
/// one key. The root summary and the per-field connecting edges both derive from
/// this — it is the only sameness state the entity carries.
pub type Sameness<EntId, T> = FactSet<OrderedDistinctPair<EntId>, T>;

/// A single load-bearing glue edge surfaced for a field: the endpoint pair and
/// the support behind the judgment(s) asserting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlueEdge<'e, EntId: Ord, T> {
    /// The two entity ids this judgment unified, canonical-ordered.
    pub endpoints: &'e OrderedDistinctPair<EntId>,
    /// The accumulated support of the judgment(s) asserting this edge.
    pub support: &'e T,
}

/// The accumulated provenance of the class's `SameEntity` judgments — the derived
/// "why these ids are one entity" root, a `⊔` over the edge supports. No
/// production caller yet; the consumer is deferred.
pub fn sameness_summary<EntId, T>(sameness: &Sameness<EntId, T>) -> T
where
    EntId: Ord,
    T: Semiring + Clone,
{
    sameness
        .values()
        .fold(T::zero(), |acc, entry| acc.plus(entry.support.clone()))
}

/// Given a projected field's support, the `SameEntity` judgment(s) responsible
/// for that field being a cross-id merge — the read a consumer uses to explain a
/// merged value ("shown because we hold X = Y, per judgment J"). No production
/// caller yet; the explaining view is deferred.
///
/// A judgment is included exactly when *both* its endpoints are among the field's
/// contributing ids: a single-source field returns nothing; a field two merged
/// members both asserted returns the connecting judgment(s). Pollution-free.
/// Reads ids from the member-aware [`MemberLineage`].
pub fn connecting_glue<'e, EntId, ImgId>(
    sameness: &'e Sameness<EntId, MemberLineage<EntId, ImgId>>,
    support: &MemberLineage<EntId, ImgId>,
) -> Vec<GlueEdge<'e, EntId, MemberLineage<EntId, ImgId>>>
where
    EntId: Ord + Clone + 'e,
    ImgId: Ord + Clone + 'e,
{
    let ids: BTreeSet<&EntId> = support.iter().map(|(id, _)| id).collect();
    sameness
        .iter()
        .filter(|(pair, _)| ids.contains(pair.a()) && ids.contains(pair.b()))
        .map(|(endpoints, entry)| GlueEdge {
            endpoints,
            support: &entry.support,
        })
        .collect()
}

/// A pure projected entity: a product of slots over the entity's class.
///
/// The lifecycle mirrors the grammar — explicit `construction` / `demolition`
/// bookends plus interior events keyed by id — rather than one flat timeline.
///
/// Equality is structural. The date and location fields fold through `meet` /
/// `join` that are associative only up to denotation, so two projections that
/// denote the same thing can still differ in a retained `DateBound` precision or
/// a symbolic region shape. A single projection is deterministic — the fold runs
/// in `FactId` order — so this only matters to a consumer comparing projections
/// built from different fact orders: compare those fields by denotation, not `==`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entity<EntId: Ord, EvtId: Ord, ImgId: Ord, T> {
    /// Name claims, deduped by triple, each with its validity window.
    pub names: FactMap<NameKey, NameRecord<T>, T>,
    /// Directed relationships: target entity → the coexisting relation kinds.
    pub relations: FactMap<EntId, FactSet<EntityRelationType, T>, T>,
    /// External references — bare membership.
    pub refs: FactSet<ExternalReference, T>,
    /// The construction bookend.
    pub construction: Bookend<T>,
    /// The demolition bookend.
    pub demolition: Bookend<T>,
    /// Interior lifetime events, keyed by `SameEvent` class.
    pub events: FactMap<EvtId, Event<T>, T>,
    /// Images depicting this entity, keyed by image id, each with its
    /// localization and perspective.
    pub depictions: FactMap<ImgId, DepictionRecord<T>, T>,
    /// The class's `SameEntity` glue: each judgment's endpoint pair and support.
    /// The root summary and the per-field connecting edges derive from it.
    pub sameness: Sameness<EntId, T>,
}

derive_slot!(Entity<EntId: Ord, EvtId: Ord, ImgId: Ord, T: Semiring>, {
    names, relations, refs, construction, demolition, events, depictions, sameness
});

/// A pure projected image: a product of slots over the image's `SameArtifact`
/// class.
///
/// An image id is both the underlying artifact and a precise realization (scan)
/// of it; the model keeps them as one, so a `SameArtifact` class folds its
/// realizations into a single `Image`. That holds while the fields are
/// artifact-level (medium, depictions, structure), though a realization-level
/// attribute like a scan's capture date would want the two separated. Whether to
/// separate them is an open question.
///
/// The medium is restrictive (one settled value once sources agree); the rest
/// are additive memberships — source URLs, the entities depicted, the composite
/// edges to a parent or held subimages, and the `SameArtifact` glue tying the
/// realizations together (mirroring [`Entity`]'s `sameness`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image<EntId: Ord, ImgId: Ord, T> {
    /// The descriptive medium — a single value once sources agree.
    pub medium: Bracket<Claimed<ImageMedium>, T>,
    /// Source URLs the image's bytes were fetched from.
    pub urls: FactSet<Url, T>,
    /// Entities depicted in this image, keyed by entity id, each with its
    /// localization and perspective.
    pub depicts: FactMap<EntId, DepictionRecord<T>, T>,
    /// The composite this image is a panel of, keyed by parent id, with the
    /// region this image occupies.
    pub parent: FactMap<ImgId, RegionRecord<T>, T>,
    /// The panels this composite holds, keyed by subimage id, each with its
    /// region.
    pub subimages: FactMap<ImgId, RegionRecord<T>, T>,
    /// The class's `SameArtifact` glue: each judgment's endpoint pair and the
    /// support behind it — why these realizations are held to be one artifact.
    pub sameness: Sameness<ImgId, T>,
}

derive_slot!(Image<EntId: Ord, ImgId: Ord, T: Semiring>, {
    medium, urls, depicts, parent, subimages, sameness
});
