//! Fact-store wire model.
//!
//! Facts are the primitive: every claim the platform records is a fact,
//! pairing an assertion with a citation. Entities, images, and other
//! domain concepts are projections over the fact bag — they don't have
//! independent identity in the wire model.
//!
//! # Module layout
//!
//! - [`ids`] — identifier newtypes ([`ids::FactId`], [`ids::CommitId`],
//!   plus the macro-generated string IDs).
//! - [`citations`] — citation flavors and source enums.
//! - [`lifecycle`] — lifetime-event kinds, durational roles, and the
//!   supporting domain enums (`DamageCause`, `MoveMethod`, `Usage`).
//! - [`geometry`] — image-mask, polyline, and spatial-geometry primitives
//!   referenced by depictions and features.
//! - [`features`] — image-grounded feature observation vocabulary
//!   ([`features::Feature`], `RoofShape`, `FacadeMaterial`, `Condition`,
//!   `SignText`).
//! - [`spatial`] — image-observation-grounded topological relation
//!   vocabulary ([`spatial::TopologicalRel`]).
//! - [`assertions`] — the three top-level assertion sums
//!   (`FactualAssertion`, `JudgmentAssertion`, `MetaAssertion`).
//!
//! ## Assertion clusters
//!
//! Each outer variant of `FactualAssertion` / `JudgmentAssertion` wraps a
//! cluster module's `Fact` enum. The clusters hold the bulk of the
//! variant code so the top-level assertion file stays small and reads as
//! an overview.
//!
//! Factual clusters:
//!
//! - [`attribute`] — entity-level attributes (names, external refs,
//!   relationships).
//! - [`bookend`] — construction / demolition flat per-entity facts
//!   (shared shape; the outer variant tag distinguishes the phase).
//! - [`event`] — interior-lifetime event facts plus cross-event gap
//!   primitives ([`event::OrderableEvent`], [`event::GapBounds`]).
//! - [`image`] — byte-level image facts (source URL).
//! - [`picture`] — pictorial role-claim and picture-specific
//!   attributes (capture date, capture location).
//! - [`map`] — map role-claim and map-specific attributes.
//!
//! Judgment clusters:
//!
//! - [`identity`] — same-entity / same-artifact / same-event equivalence.
//! - [`depiction`] — entity-in-picture / on-map depiction judgments plus
//!   the [`depiction::Perspective`] view enum.
//! - [`observation`] — image-grounded feature and spatial-relation
//!   observations.
//! - [`composites`] — composite-image sub-region structural facts plus
//!   [`composites::SubimageRegion`].
//!
//! # Entities and resolution
//!
//! Every entity reference inside a fact is an [`ids::EntityId`] — there is
//! no description-shaped reference at the fact-store layer. A submission
//! that mentions an entity by name and disambiguators arrives at the
//! submit layer as an already-shaped bundle of facts: the submission
//! layer mints a fresh [`ids::EntityId`] for every newly-mentioned entity
//! and rewrites the prose-style reference into a flat
//! ([`attribute::Fact::Name`], [`attribute::Fact::Relationship`],
//! disambiguator facts on the same id) triple-set keyed by the fresh id.
//!
//! Cross-source identity is established later, in the matching layer,
//! which emits [`identity::Fact::SameEntity`] judgments to merge
//! equivalent freshly-minted ids. The projection collapses each
//! `SameEntity` equivalence class to a single canonical entity and
//! unifies the metadata attached to its members (names, locations,
//! lifecycle bookends). Submission and matching are intentionally
//! separate concerns: a submitter cannot reach into a prior commit's
//! ids, and the matcher's judgments are reviewable, retractable facts
//! like any other.
//!
//! # Image / picture / map layering
//!
//! [`ids::ImageId`] identifies a *image* — the bytes that make up a
//! specific scan or capture. "Picture" (a figurative depiction:
//! photograph, painting, drawing) and "map" (a cartographic
//! representation) are role-claims layered on top of images via
//! [`picture::Fact::IsPicture`] and [`map::Fact::IsMap`]. A image can
//! carry either role, both (rare and considered a conflict), or
//! neither. Picture-specific attributes live in [`picture`];
//! map-specific attributes live in [`map`]; byte-level provenance
//! lives in [`image`].

pub mod assertions;
pub mod attribute;
pub mod bookend;
pub mod citations;
pub mod composites;
pub mod depiction;
pub mod event;
pub mod features;
pub mod geometry;
pub mod identity;
pub mod ids;
pub mod image;
pub mod lifecycle;
pub mod map;
pub mod observation;
pub mod picture;
pub mod spatial;
