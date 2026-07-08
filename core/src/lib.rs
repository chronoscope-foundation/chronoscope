//! Chronoscope Core Library
//!
//! Core types for the Chronoscope spatiotemporal knowledge platform. Core *is*
//! the fact store: facts are the primitive, and everything else is a layer over
//! them.
//!
//! Facts are the primitive: every claim the platform records is a fact, pairing
//! an assertion with a citation. Entities, images, and other domain concepts
//! are projections over the fact bag — they have no independent identity in the
//! wire model.
//!
//! ## Layers
//!
//! - [`grammar`] — the fact vocabulary: assertion sums, cluster `Fact` enums,
//!   citation and id types.
//! - [`store`] — persistence: the [`store::FactStore`] / [`store::FactView`]
//!   traits, the query vocabulary, and the in-memory backend.
//! - [`submit`], [`projection`], [`typed`], [`listing`], [`moment`] — the
//!   transformations over facts: write-path validation and matching, the
//!   read-side lattice fold, the display view, viewport enumeration, and
//!   display ordering.
//! - [`date`], [`location`], [`geo`], [`algebra`], [`nonempty`],
//!   [`external_ids`] — the shared value primitives and generic foundations.
//!
//! ## Entities and resolution
//!
//! Every entity reference inside a fact is an entity id — there's no
//! description-shaped reference at the fact-store layer. A submission that
//! mentions an entity by name and disambiguators arrives at the submit layer as
//! an already-shaped bundle: the submission layer mints a fresh entity id for
//! every newly-mentioned entity and rewrites the prose-style reference into a
//! flat triple-set keyed by the fresh id
//! ([`grammar::attribute::Fact::Name`],
//! [`grammar::attribute::Fact::Relationship`], disambiguator facts on the same
//! id).
//!
//! Cross-source identity is established later, in the matching layer, which
//! emits [`grammar::identity::Fact::SameEntity`] judgments to merge equivalent
//! freshly-minted ids. The projection collapses each `SameEntity` equivalence
//! class to one canonical entity and unifies the metadata on its members
//! (names, locations, lifecycle bookends). Submission and matching stay
//! separate: a submitter can't reach into a prior commit's ids, and the
//! matcher's judgments are reviewable, retractable facts like any other.
//!
//! ## One image, no roles
//!
//! An image id identifies an image — the bytes of a specific scan or capture.
//! "Picture" (a figurative depiction: photograph, painting, drawing) and "map"
//! (a cartographic representation) describe a medium, not a structural kind that
//! gates which facts an image may carry. The grammar carries one image type; the
//! medium rides on the non-gating, cited [`grammar::image::Fact::Medium`] field
//! ([`grammar::image::ImageMedium`]). Every former map / picture attribute is a
//! general [`grammar::image::Fact`].
//!
//! ## Target invariants
//!
//! Several call sites (notably the fact-store backend's `next_fact_id`
//! and fact-id minting in [`crate::store::memory`]) cast `usize` lengths
//! to `u64` with `as u64`. This is sound on every target the project
//! supports because `size_of::<usize>() <= size_of::<u64>()`; the const
//! block below asserts that statically so a hypothetical port to a
//! 128-bit-`usize` target wouldn't silently truncate.

const _: () = {
    assert!(
        std::mem::size_of::<usize>() <= std::mem::size_of::<u64>(),
        "this crate assumes usize fits in u64; the fact-id minting in \
         store::memory truncates otherwise"
    );
};

/// The build's version label: the git SHA injected via
/// `CHRONOSCOPE_BUILD_VERSION` at build time, or the crate version when the
/// variable is absent. Machine-authored fact-store commits carry it as their
/// [`grammar::ids`] `AnalyzerVersion`, so a stored judgment names the code that
/// produced it.
pub const BUILD_VERSION: &str = match option_env!("CHRONOSCOPE_BUILD_VERSION") {
    Some(sha) => sha,
    None => env!("CARGO_PKG_VERSION"),
};

const _: () = assert!(
    !BUILD_VERSION.is_empty(),
    "CHRONOSCOPE_BUILD_VERSION must not be set to an empty string"
);

pub mod algebra;
pub mod date;
pub mod external_ids;
pub mod geo;
pub mod grammar;
pub mod listing;
pub mod location;
pub mod moment;
pub mod nonempty;
pub mod projection;
pub mod store;
pub mod submit;
pub mod typed;

#[cfg(test)]
mod wire_goldens;

pub use algebra::lattice::{BoundedLattice, JoinSemilattice, MeetSemilattice};
pub use algebra::monoid::CommutativeMonoid;
pub use date::{DateBound, DateError, DatePrecision, UncertainDate};
pub use external_ids::{
    ExternalStringIdError, GeoNamesId, GettyTgnId, OhmId, OsmElementType, OsmId, TriggerEventId,
    WikidataEntityId, WikidataIdParseError, WikidataPropertyId,
};
pub use geo::{Bbox, BboxError, GeoPoint, GeoPointError, Meters};
pub use location::{
    ConflictStatus, Distance, Location, LocationError, LocationReference, UnresolvedLocation,
};
pub use moment::TransitionRole;
pub use projection::Claimed;
