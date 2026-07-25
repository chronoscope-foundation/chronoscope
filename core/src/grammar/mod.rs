//! Fact vocabulary — the grammar of assertions and citations.
//!
//! Facts are the platform's primitive: every claim pairs an assertion with a
//! citation. This module holds the vocabulary those facts are written in — the
//! assertion sums, their per-cluster `Fact` enums, and the citation and id
//! types they reference. Entities, images, and events have no independent
//! identity here; they are projections over the fact bag.
//!
//! ## Wire grammar
//!
//! - [`ids`] — identifier newtypes ([`ids::FactId`], [`ids::CommitId`],
//!   plus the macro-generated string IDs).
//! - [`text`] — [`text::Text`], the shared free-text value.
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
//! cluster module's `Fact` enum. The clusters hold the bulk of the variant
//! code so the top-level assertion file stays small.
//!
//! Factual clusters:
//!
//! - [`attribute`] — entity-level attributes (names, external refs,
//!   relationships).
//! - [`bookend`] — construction / demolition flat per-entity facts
//!   (shared shape; the outer variant tag distinguishes the phase).
//! - [`existence`] — a flat per-entity existence witness (the entity
//!   provably existed at a date).
//! - [`event`] — interior-lifetime event facts plus cross-event gap
//!   primitives ([`event::OrderableEvent`], [`event::GapBounds`]).
//! - [`image`] — image-level facts: source URL, author, created date,
//!   capture date / location, and the descriptive [`image::ImageMedium`].
//!
//! Judgment clusters:
//!
//! - [`identity`] — same-entity / same-artifact / same-event equivalence.
//! - [`depiction`] — entity-in-image depiction judgments plus the
//!   [`depiction::Perspective`] view enum.
//! - [`observation`] — image-grounded feature and spatial-relation
//!   observations.
//! - [`composites`] — composite-image sub-region structural facts plus
//!   [`composites::SubimageRegion`].

pub mod assertions;
pub mod attribute;
pub mod bookend;
pub mod citations;
pub mod composites;
pub mod depiction;
pub mod event;
pub mod existence;
pub mod features;
pub mod geometry;
pub mod identity;
pub mod ids;
pub mod image;
pub mod lifecycle;
pub mod observation;
pub mod spatial;
pub mod text;
