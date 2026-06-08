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
}
