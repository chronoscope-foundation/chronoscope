//! Errors from the submit pipeline.
//!
//! Covers index-out-of-range from substitution, post-resolution identity
//! self-equivalence from [`crate::facts::identity::OrderedDistinctPair`]'s
//! constructor, and `Existing` decl id-not-found from the validator.
//! `#[non_exhaustive]` so rule-specific variants can be added without breaking
//! dependent matches.
//!
//! The id-carrying variants are typed by the three id kinds (`E` entity, `V`
//! event, `I` image) — the offending id rides through typed from
//! [`crate::facts::identity::IdMapError`]. The `#[error]` messages format it;
//! thiserror infers the `Display` bounds.

use super::{EntityIdx, EventIdx, ImageIdx};
use crate::facts::ids::{CommitId, FactId, SubjectKind};

/// The role an image is used as or claimed to be. A picture carries capture
/// metadata and depicts entities in-frame; a map places entities by location.
/// Role coherence (one image, one role) is checked at submit time; the Display
/// form renders the word used in [`SubmitError::ImageRoleConflict`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ImageRole {
    /// A photographic image — captures a scene, depicts entities in-frame.
    Picture,
    /// A cartographic image — places entities by geographic location.
    Map,
}

impl std::fmt::Display for ImageRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Picture => write!(f, "picture"),
            Self::Map => write!(f, "map"),
        }
    }
}

/// Errors from `submit_commit`. `#[non_exhaustive]`, so dependent code handles
/// a default arm and rule-specific variants can be added without a breaking
/// change.
///
/// Generic over the three id kinds so the id-carrying variants carry the
/// offending id typed rather than stringified.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SubmitError<E, V, I> {
    /// An [`EntityIdx`](super::EntityIdx) inside a fact pointed
    /// past the end of the bundle's entity declarations.
    #[error("EntityIdx({idx}) out of range; bundle has {decl_count} entity declarations")]
    EntityIdxOutOfRange {
        /// The offending index value.
        idx: usize,
        /// The number of entity declarations the producer supplied in
        /// the commit bundle.
        decl_count: usize,
    },
    /// An [`EventIdx`](super::EventIdx) inside a fact pointed past
    /// the end of the bundle's event declarations.
    #[error("EventIdx({idx}) out of range; bundle has {decl_count} event declarations")]
    EventIdxOutOfRange {
        /// The offending index value.
        idx: usize,
        /// The number of event declarations the producer supplied in
        /// the commit bundle.
        decl_count: usize,
    },
    /// An [`ImageIdx`](super::ImageIdx) inside a fact pointed past
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
    /// [`EntityIdx`](super::EntityIdx) naming this decl).
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
    /// [`crate::facts::identity::OrderedDistinctPair`]'s constructor.
    #[error("identity (entity) fact resolved to self-equivalence on id {id}")]
    IdentityEntitySelfEquivalence {
        /// The shared entity id both sides of the pair resolved to.
        id: E,
    },
    /// After bundle-local index substitution, an `identity::Fact::SameEvent`
    /// fact resolved to a self-equivalence (`a == b`). Symmetric to
    /// [`Self::IdentityEntitySelfEquivalence`], over event ids.
    #[error("identity (event) fact resolved to self-equivalence on id {id}")]
    IdentityEventSelfEquivalence {
        /// The shared event id both sides of the pair resolved to.
        id: V,
    },
    /// After bundle-local index substitution, an `identity::Fact::SameArtifact`
    /// fact resolved to a self-equivalence (`a == b`). Symmetric to
    /// [`Self::IdentityEntitySelfEquivalence`], over image ids.
    #[error("identity (artifact) fact resolved to self-equivalence on id {id}")]
    IdentityArtifactSelfEquivalence {
        /// The shared image id both sides of the pair resolved to.
        id: I,
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
    /// [`crate::facts::identity::DistinctPair::new`].
    #[error("relationship resolved to self-reference on entity {id}")]
    SelfReferenceInRelationship {
        /// The shared entity id both sides resolved to.
        id: E,
    },
    /// An `observation::Fact::Spatial` resolved to a self-loop after id
    /// substitution. Symmetric to [`Self::SelfReferenceInRelationship`].
    #[error("spatial observation resolved to self-reference on entity {id}")]
    SelfReferenceInSpatial {
        /// The shared entity id both sides resolved to.
        id: E,
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
        id: E,
    },
    /// Two event declarations resolved to one persistent id. See
    /// [`Self::DuplicateEntityDecl`].
    #[error("two event declarations resolved to the same id {id}")]
    DuplicateEventDecl {
        /// The id two event declarations resolved to.
        id: V,
    },
    /// Two image declarations resolved to one persistent id. See
    /// [`Self::DuplicateEntityDecl`].
    #[error("two image declarations resolved to the same id {id}")]
    DuplicateImageDecl {
        /// The id two image declarations resolved to.
        id: I,
    },
    /// A `Demolition` bookend carries a location. Demolition location is
    /// derived from the entity's last known location, not separately asserted.
    #[error("demolition bookend carries a location for {entity}; demolition location is derived")]
    DemolitionLocation {
        /// The entity whose demolition bookend carried a location.
        entity: E,
    },
    /// A lifetime event carries facts whose kinds can't agree — e.g. a
    /// damage-cause and a move-method on one event.
    #[error("event {event} carries facts with incompatible lifetime-event kinds")]
    EventKindConflict {
        /// The event whose attached facts conflict on kind.
        event: V,
    },
    /// A name's validity window closes before it opens: `valid_from`'s earliest
    /// possible date is after `valid_to`'s latest possible date.
    #[error("name validity window for {entity} is inverted: valid_from is after valid_to")]
    NameWindowInverted {
        /// The entity whose name window is inverted.
        entity: E,
    },
    /// An image-observation names an entity with no paired depiction tying that
    /// entity to the observed image. The observation describes something seen in
    /// the image, so the entity reference is meaningless without the depiction.
    #[error("image-observation references {entity} with no paired depiction on image {image}")]
    ObservationWithoutDepiction {
        /// The entity the observation names.
        entity: E,
        /// The image the observation was made against.
        image: I,
    },
    /// A composite subimage equals its own parent.
    #[error("subimage equals its parent: {image}")]
    CompositeSelfParent {
        /// The image named as both subimage and parent.
        image: I,
    },
    /// A subimage is placed under more than one parent.
    #[error("subimage {subimage} is placed under more than one parent")]
    CompositeMultipleParents {
        /// The subimage with multiple parents.
        subimage: I,
    },
    /// An image is both a subimage and a parent; composites are one layer deep.
    #[error("image {image} is both a subimage and a parent; composites are flat")]
    CompositeChain {
        /// The image forming the chain.
        image: I,
    },
    /// A fact presupposes one role for an image (a picture-capture attribute or
    /// in-picture depiction needs a picture; an on-map depiction needs a map)
    /// while another fact about that image claims the opposite role. A
    /// claim-vs-claim disagreement (both `IsPicture` and `IsMap`) is deferred
    /// to projection — uncertainty is data — so only presupposition-vs-claim
    /// fires here.
    #[error("image {image} used as {used_as} but claimed as {claimed}")]
    ImageRoleConflict {
        /// The image whose role is contested.
        image: I,
        /// The role a fact presupposed for the image.
        used_as: ImageRole,
        /// The role another fact claimed the image to be.
        claimed: ImageRole,
    },
}
