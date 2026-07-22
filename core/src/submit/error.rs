//! Errors from the submit pipeline.
//!
//! Covers index-out-of-range from substitution, post-resolution identity
//! self-equivalence from [`crate::grammar::identity::OrderedDistinctPair`]'s
//! constructor, and `Existing` decl id-not-found from the validator.
//! `#[non_exhaustive]` so rule-specific variants can be added without breaking
//! dependent matches.
//!
//! The id-carrying variants are typed by the three id kinds (`EntId`, `EvtId`,
//! `ImgId`) — the offending id rides through typed from
//! [`crate::grammar::identity::IdMapError`]. Keeping these direct type parameters
//! (rather than projecting through a bundled scheme) lets thiserror infer the
//! `Display` bound its `#[error]` messages need on each id kind.

use super::{EntityIdx, EventIdx, ImageIdx};
use crate::grammar::ids::{CommitId, FactId, SubjectKind};
use crate::grammar::lifecycle::LifetimeEventKind;

/// Which date a fact carries, named so a [`SubmitError::NonSingleIntervalDate`]
/// pinpoints the offending position across fact payloads and citations alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DateRole {
    /// `attribute::Fact::Name { valid_from }`.
    NameValidFrom,
    /// `attribute::Fact::Name { valid_to }`.
    NameValidTo,
    /// A construction/demolition bookend bound (`Started` / `Completed`).
    BookendBound,
    /// An existence witness's date (`existence::Fact { at }`).
    ExistenceWitness,
    /// A lifetime-event date (`PointDate` / `DurationalDate`).
    EventDate,
    /// `image::Fact::CreatedDate`.
    ImageCreated,
    /// `image::Fact::SubjectDate`.
    ImageSubject,
    /// `image::Fact::CapturedDate`.
    ImageCaptured,
    /// An `ExternalSource` publication/creation date carried by a citation
    /// (`Url`/`Book` published, `Archive` created).
    CitationDate,
}

impl std::fmt::Display for DateRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::NameValidFrom => "name valid-from",
            Self::NameValidTo => "name valid-to",
            Self::BookendBound => "bookend bound",
            Self::ExistenceWitness => "existence witness",
            Self::EventDate => "event date",
            Self::ImageCreated => "image created-date",
            Self::ImageSubject => "image subject-date",
            Self::ImageCaptured => "image captured-date",
            Self::CitationDate => "citation date",
        };
        f.write_str(label)
    }
}

/// Which location a fact carries, named so a [`SubmitError::EmptyLocation`]
/// pinpoints the offending position across the location-bearing facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LocationRole {
    /// A construction bookend's `Location`.
    BookendLocation,
    /// `event::Fact::MovedToLocation`.
    MovedToLocation,
    /// `image::Fact::CapturedLocation`.
    ImageCaptured,
}

impl std::fmt::Display for LocationRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::BookendLocation => "bookend location",
            Self::MovedToLocation => "move location",
            Self::ImageCaptured => "image captured-location",
        };
        f.write_str(label)
    }
}

/// Errors from `submit_commit`. `#[non_exhaustive]`, so dependent code handles
/// a default arm and rule-specific variants can be added without a breaking
/// change.
///
/// Parameterised directly by the three id kinds (`EntId` / `EvtId` / `ImgId`)
/// so the id-carrying variants carry the offending id typed rather than
/// stringified — and so thiserror infers the `Display` bound its `#[error]`
/// messages need on each.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SubmitError<EntId, EvtId, ImgId> {
    /// An [`EntityIdx`] inside a fact pointed
    /// past the end of the bundle's entity declarations.
    #[error("EntityIdx({idx}) out of range; bundle has {decl_count} entity declarations")]
    EntityIdxOutOfRange {
        /// The offending index value.
        idx: usize,
        /// The number of entity declarations the producer supplied in
        /// the commit bundle.
        decl_count: usize,
    },
    /// An [`EventIdx`] inside a fact pointed past
    /// the end of the bundle's event declarations.
    #[error("EventIdx({idx}) out of range; bundle has {decl_count} event declarations")]
    EventIdxOutOfRange {
        /// The offending index value.
        idx: usize,
        /// The number of event declarations the producer supplied in
        /// the commit bundle.
        decl_count: usize,
    },
    /// An [`ImageIdx`] inside a fact pointed past
    /// the end of the bundle's image declarations.
    #[error("ImageIdx({idx}) out of range; bundle has {decl_count} image declarations")]
    ImageIdxOutOfRange {
        /// The offending index value.
        idx: usize,
        /// The number of image declarations the producer supplied in
        /// the commit bundle.
        decl_count: usize,
    },
    /// A [`Decl::Existing`](super::Decl::Existing) entity id wasn't in the
    /// store at submit time — a typo'd or not-yet-minted id.
    ///
    /// `decl_position` is the index into the bundle's `entities` list (the
    /// [`EntityIdx`] naming this decl).
    #[error("Decl::Existing entity at decl position {decl_position:?} not found in store")]
    UnknownExistingEntity {
        /// Position of the offending decl in `Commit::entities`.
        decl_position: EntityIdx,
    },
    /// A [`Decl::Existing`](super::Decl::Existing) event id was
    /// not found in the store at submit time.
    #[error("Decl::Existing event at decl position {decl_position:?} not found in store")]
    UnknownExistingEvent {
        /// Position of the offending decl in `Commit::events`.
        decl_position: EventIdx,
    },
    /// A [`Decl::Existing`](super::Decl::Existing) image id was
    /// not found in the store at submit time.
    #[error("Decl::Existing image at decl position {decl_position:?} not found in store")]
    UnknownExistingImage {
        /// Position of the offending decl in `Commit::images`.
        decl_position: ImageIdx,
    },
    /// An `identity::Fact::SameEntity` resolved to a self-equivalence
    /// (`a == b`) after index substitution: two distinct indices resolved to
    /// one persistent id. The literal-self case is caught earlier by
    /// [`crate::grammar::identity::OrderedDistinctPair`]'s constructor.
    #[error("identity (entity) fact resolved to self-equivalence on id {id}")]
    IdentityEntitySelfEquivalence {
        /// The shared entity id both sides of the pair resolved to.
        id: EntId,
    },
    /// After bundle-local index substitution, an `identity::Fact::SameEvent`
    /// fact resolved to a self-equivalence (`a == b`). Symmetric to
    /// [`Self::IdentityEntitySelfEquivalence`], over event ids.
    #[error("identity (event) fact resolved to self-equivalence on id {id}")]
    IdentityEventSelfEquivalence {
        /// The shared event id both sides of the pair resolved to.
        id: EvtId,
    },
    /// After bundle-local index substitution, an `identity::Fact::SameArtifact`
    /// fact resolved to a self-equivalence (`a == b`). Symmetric to
    /// [`Self::IdentityEntitySelfEquivalence`], over image ids.
    #[error("identity (artifact) fact resolved to self-equivalence on id {id}")]
    IdentityArtifactSelfEquivalence {
        /// The shared image id both sides of the pair resolved to.
        id: ImgId,
    },
    /// An `identity::Fact::SameEvent` judgment. Reads answer singleton event
    /// classes — event equivalence is not resolved — so a stored `SameEvent`
    /// edge would be silently ignored. Rejecting it keeps the singleton
    /// answers honest.
    #[error("SameEvent on events {a} and {b} is rejected: reads do not resolve event equivalence")]
    SameEventUnresolvable {
        /// The canonically-smaller member of the rejected pair.
        a: EvtId,
        /// The canonically-larger member of the rejected pair.
        b: EvtId,
    },
    /// A retraction or supersession target fact was not found at submit
    /// time.
    #[error("fact {id} not found")]
    FactNotFound {
        /// The missing fact id.
        id: FactId,
    },
    /// A commit-retraction target commit was not found at submit time.
    #[error("commit {id} not found")]
    CommitNotFound {
        /// The missing commit id.
        id: CommitId,
    },
    /// A retraction or supersession targets a fact minted in the same commit.
    /// The corrected fact is submitted on its own, so a same-commit target is
    /// rejected.
    #[error(
        "a retraction or supersession targets a fact in the same commit: {target}; submit the corrected fact directly"
    )]
    MetaTargetInSameCommit {
        /// The same-commit target fact id.
        target: FactId,
    },
    /// A supersession names one fact as both its target and its replacement.
    #[error("supersession replacement equals its target: {target}")]
    SupersedeReplacementEqualsTarget {
        /// The fact named as both target and replacement.
        target: FactId,
    },
    /// An `attribute::Fact::Relationship` resolved to a self-loop after
    /// substitution — `from` and `to` mapped to one persistent id. The
    /// literal-self case is caught earlier by
    /// [`crate::grammar::identity::DistinctPair::new`].
    #[error("relationship resolved to self-reference on entity {id}")]
    SelfReferenceInRelationship {
        /// The shared entity id both sides resolved to.
        id: EntId,
    },
    /// An `observation::Fact::Spatial` resolved to a self-loop after id
    /// substitution. Symmetric to [`Self::SelfReferenceInRelationship`].
    #[error("spatial observation resolved to self-reference on entity {id}")]
    SelfReferenceInSpatial {
        /// The shared entity id both sides resolved to.
        id: EntId,
    },
    /// A declaration that no fact references. Rejected up-front so producers
    /// don't accumulate unused id mints.
    #[error("{kind} declaration at position {position} is never referenced by any fact")]
    UnusedDeclaration {
        /// Which subject id family the position refers to.
        kind: SubjectKind,
        /// Position of the unreferenced decl in its respective list.
        position: usize,
    },
    /// Two entity declarations resolved to one persistent id. A commit that
    /// names one entity twice is non-canonical — it breaks content-address
    /// dedup, and a self-pair would otherwise be the only thing caught.
    #[error("two entity declarations resolved to the same id {id}")]
    DuplicateEntityDecl {
        /// The id two entity declarations resolved to.
        id: EntId,
    },
    /// Two event declarations resolved to one persistent id. See
    /// [`Self::DuplicateEntityDecl`].
    #[error("two event declarations resolved to the same id {id}")]
    DuplicateEventDecl {
        /// The id two event declarations resolved to.
        id: EvtId,
    },
    /// Two image declarations resolved to one persistent id. See
    /// [`Self::DuplicateEntityDecl`].
    #[error("two image declarations resolved to the same id {id}")]
    DuplicateImageDecl {
        /// The id two image declarations resolved to.
        id: ImgId,
    },
    /// An event id carries no `HasEvent`. Every event has one subject entity
    /// and one declared kind, so each id needs exactly one `HasEvent` tying it
    /// to its entity; a minted event without one is typeless and can't be
    /// projected. The corrected commit adds the `HasEvent`.
    #[error("event {event} carries no HasEvent; every event must declare its subject and kind")]
    EventMissingHasEvent {
        /// The event with no `HasEvent`.
        event: EvtId,
    },
    /// An event id carries more than one distinct `HasEvent` — a different
    /// subject entity or a different declared kind on one id. An event has one
    /// subject and one kind; genuine cross-source disagreement is expressed as
    /// separate events (different ids), never two `HasEvent` on one id.
    /// (Re-asserting the identical `HasEvent` content-addresses to one fact,
    /// so it isn't a duplicate.)
    #[error(
        "event {event} carries conflicting HasEvent claims; disagreement belongs on separate events"
    )]
    EventMultipleHasEvent {
        /// The event with conflicting `HasEvent` claims.
        event: EvtId,
    },
    /// An interior event's `{entity, kind}` is pinned at its first-ever
    /// `HasEvent` and stays fixed for the life of that event id, across
    /// retraction. A `HasEvent` naming a different subject entity or kind than
    /// the pin — re-homing the event to another entity, re-typing its kind, or
    /// re-adopting a retracted event id under a new owner — is rejected; an
    /// identical re-assertion is accepted. Genuine disagreement belongs on a
    /// separate event id, not by mutating an existing one.
    #[error(
        "event {event}: HasEvent ownership is immutable — pinned to entity {pinned_entity} ({pinned_kind}) at its first-ever HasEvent, but this commit names entity {attempted_entity} ({attempted_kind})"
    )]
    EventOwnershipImmutable {
        /// The event whose ownership pin this commit tried to change.
        event: EvtId,
        /// The subject entity pinned at the event's earliest-ever `HasEvent`.
        pinned_entity: EntId,
        /// The kind pinned at the event's earliest-ever `HasEvent`.
        pinned_kind: LifetimeEventKind,
        /// The differing subject entity the event's active `HasEvent` carries.
        attempted_entity: EntId,
        /// The differing kind the event's active `HasEvent` carries.
        attempted_kind: LifetimeEventKind,
    },
    /// A payload or date fact on an event doesn't suit the kind the same
    /// commit's `HasEvent` declares — a damage cause on a non-`Damaged` event, a
    /// `DurationalDate` on a point event. The availability matrix
    /// ([`crate::grammar::event`] module docs) pins which fact suits which kind.
    #[error("event {event}: a {fact} fact does not match its declared kind {declared}")]
    EventFactKindMismatch {
        /// The event whose payload/date contradicts its declared kind.
        event: EvtId,
        /// The offending payload or date fact's variant name.
        fact: &'static str,
        /// The kind the event's `HasEvent` declares.
        declared: LifetimeEventKind,
    },
    /// A name's validity window closes before it opens: `valid_from`'s earliest
    /// possible date is after `valid_to`'s latest possible date.
    #[error("name validity window for {entity} is inverted: valid_from is after valid_to")]
    NameWindowInverted {
        /// The entity whose name window is inverted.
        entity: EntId,
    },
    /// A stored fact carries an [`UncertainDate`](crate::date::UncertainDate)
    /// that isn't a single non-empty interval — a disjunction or the empty
    /// union. A single source asserts one interval; a disjunction ("I can't
    /// decide") and ⊥ ("no possible date") are read-side projections, not
    /// storable claims.
    #[error("{role} carries a disjunction or empty date; a stored claim must be a single interval")]
    NonSingleIntervalDate {
        /// Which date position carried the non-single value.
        role: DateRole,
    },
    /// A stored fact carries the impossible location (⊥): the empty location
    /// itself, or one surviving inside a union/disjunction. A single source
    /// asserts a place, never the absence of one — the spatial parallel of the
    /// empty date interval.
    #[error("{role} carries the empty location; a stored claim must name a place")]
    EmptyLocation {
        /// Which location position carried the impossible value.
        role: LocationRole,
    },
    /// A stored location names more circles than the input-validation bound
    /// allows. A place is a handful of spots; hundreds signal machine-generated
    /// junk or an adversarial input, and the emptiness check is cubic in the
    /// circle count.
    #[error(
        "{role} names {circles} circles, over the {limit} limit; a place is a handful of spots"
    )]
    LocationTooComplex {
        /// Which location position carried the over-complex value.
        role: LocationRole,
        /// The circle count the location carries.
        circles: usize,
        /// The maximum circle count a stored location may carry.
        limit: usize,
    },
    /// An image-observation names an entity with no paired depiction tying that
    /// entity to the observed image. The observation describes something seen in
    /// the image, so the entity reference is meaningless without the depiction.
    #[error("image-observation references {entity} with no paired depiction on image {image}")]
    ObservationWithoutDepiction {
        /// The entity the observation names.
        entity: EntId,
        /// The image the observation was made against.
        image: ImgId,
    },
    /// A composite subimage equals its own parent.
    #[error("subimage equals its parent: {image}")]
    CompositeSelfParent {
        /// The image named as both subimage and parent.
        image: ImgId,
    },
    /// A subimage is placed under more than one parent.
    #[error("subimage {subimage} is placed under more than one parent")]
    CompositeMultipleParents {
        /// The subimage with multiple parents.
        subimage: ImgId,
    },
    /// An image is both a subimage and a parent; composites are one layer deep.
    #[error("image {image} is both a subimage and a parent; composites are flat")]
    CompositeChain {
        /// The image forming the chain.
        image: ImgId,
    },
}
