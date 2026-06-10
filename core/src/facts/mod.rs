//! Fact-store wire model.
//!
//! Facts are the primitive: every claim the platform records is a fact, pairing
//! an assertion with a citation. Entities, images, and other domain concepts
//! are projections over the fact bag — they have no independent identity in the
//! wire model.
//!
//! # Module layout
//!
//! Wire grammar:
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
//! Storage + submit layer:
//!
//! - [`store`] — [`store::FactStore`] / [`store::FactView`] plus the
//!   per-subject view traits [`store::EntityView`] /
//!   [`store::EventView`] / [`store::ImageView`].
//! - [`schema`] — query types ([`schema::Bbox`], [`schema::TimeRange`],
//!   [`schema::FactPage`], [`schema::PageItem`], [`schema::EquivClass`],
//!   [`schema::EdgeSubgraph`]) and the per-subject stream enums
//!   ([`schema::EntityStream`] / [`schema::EventStream`] /
//!   [`schema::ImageStream`]). Each subject kind has a single canonical
//!   equivalence (and entities a single canonical edge relation), all
//!   implicit — there are no per-subject relation or grouping enums.
//! - [`submit`] — producer-side submission subsystem. The top-level
//!   module holds the producer-input shape ([`submit::Commit`],
//!   [`submit::Decl`], [`submit::SubmitFact`], and the bundle-local
//!   index newtypes). Submodules cover the rest of the pipeline:
//!   [`submit::result`] for post-submission result and storage types
//!   ([`submit::SubmitResult`], [`submit::Resolution`],
//!   [`submit::ResolutionOrigin`], [`submit::StoredFact`],
//!   [`submit::StoredCommit`], [`submit::CommitAuthor`],
//!   [`submit::FactLookup`]); [`submit::error`] for the
//!   [`submit::SubmitError`] enum; [`submit::pipeline`] for the
//!   free functions backends compose
//!   ([`submit::pipeline::substitute_facts_accumulating`], the matcher / validator
//!   entry points, and [`submit::commit_facts`]). Commit-id derivation
//!   lives directly on [`submit::Commit::id`].
//! - [`memory`] — [`memory::MemoryFactStore`] backend.
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
//! Every entity reference inside a fact is an [`ids::EntityId`] — there's no
//! description-shaped reference at the fact-store layer. A submission that
//! mentions an entity by name and disambiguators arrives at the submit layer as
//! an already-shaped bundle: the submission layer mints a fresh
//! [`ids::EntityId`] for every newly-mentioned entity and rewrites the
//! prose-style reference into a flat triple-set keyed by the fresh id
//! ([`attribute::Fact::Name`], [`attribute::Fact::Relationship`], disambiguator
//! facts on the same id).
//!
//! Cross-source identity is established later, in the matching layer, which
//! emits [`identity::Fact::SameEntity`] judgments to merge equivalent
//! freshly-minted ids. The projection collapses each `SameEntity` equivalence
//! class to one canonical entity and unifies the metadata on its members
//! (names, locations, lifecycle bookends). Submission and matching stay
//! separate: a submitter can't reach into a prior commit's ids, and the
//! matcher's judgments are reviewable, retractable facts like any other.
//!
//! # Image / picture / map layering
//!
//! [`ids::ImageId`] identifies an image — the bytes of a specific scan or
//! capture. "Picture" (a figurative depiction: photograph, painting, drawing)
//! and "map" (a cartographic representation) are role-claims layered on top of
//! images via [`picture::Fact::IsPicture`] and [`map::Fact::IsMap`]. An image
//! can carry either role, both (rare, a conflict), or neither. Picture-specific
//! attributes live in [`picture`]; map-specific attributes in [`map`];
//! byte-level provenance in [`image`].

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
pub mod memory;
pub mod observation;
pub mod picture;
pub mod schema;
pub mod spatial;
pub mod store;
pub mod submit;
#[cfg(test)]
mod wire_goldens;
