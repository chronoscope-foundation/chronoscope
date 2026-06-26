//! Chronoscope Core Library
//!
//! Core types for the Chronoscope spatiotemporal knowledge platform.
//!
//! ## Target invariants
//!
//! Several call sites (notably the fact-store backend's `next_fact_id`
//! and fact-id minting in [`crate::facts::memory`]) cast `usize` lengths
//! to `u64` with `as u64`. This is sound on every target the project
//! supports because `size_of::<usize>() <= size_of::<u64>()`; the const
//! block below asserts that statically so a hypothetical port to a
//! 128-bit-`usize` target wouldn't silently truncate.

const _: () = {
    assert!(
        std::mem::size_of::<usize>() <= std::mem::size_of::<u64>(),
        "this crate assumes usize fits in u64; the fact-id minting in \
         facts::memory truncates otherwise"
    );
};

/// The build's version label: the git SHA injected via
/// `CHRONOSCOPE_BUILD_VERSION` at build time, or the crate version when the
/// variable is absent. Machine-authored fact-store commits carry it as their
/// [`facts::ids`] `AnalyzerVersion`, so a stored judgment names the code that
/// produced it.
pub const BUILD_VERSION: &str = match option_env!("CHRONOSCOPE_BUILD_VERSION") {
    Some(sha) => sha,
    None => env!("CARGO_PKG_VERSION"),
};

const _: () = assert!(
    !BUILD_VERSION.is_empty(),
    "CHRONOSCOPE_BUILD_VERSION must not be set to an empty string"
);

pub mod annotation;
pub mod claimed;
pub mod consistency;
pub mod date;
pub mod entity;
pub mod evidence;
pub mod facts;
pub mod geo;
pub mod ids;
pub mod ingestion;
pub mod lattice;
pub mod links;
pub mod location;
pub mod merge;
pub mod moment;
pub mod nonempty;

pub use annotation::{Annotation, AnnotationKind};
pub use claimed::Claimed;
pub use consistency::ConsistencyWarning;
pub use date::{DateBound, DateError, DatePrecision, UncertainDate};
pub use entity::{
    DamageCause, Entity, EntityName, EntityRelation, EntityRelationType, EntityTransition,
    MoveMethod, NameType, Usage,
};
pub use evidence::{
    Cited, Evidence, ImageRegion, MaskDimensions, Polyline, PolylineError, RleMask, SourceDetail,
    SpatialGeometry, WikidataField,
};
pub use geo::{Bbox, BboxError, GeoPoint, GeoPointError, Meters};
pub use ids::{
    ExternalStringIdError, GeoNamesId, GettyTgnId, OhmId, OsmElementType, OsmId, TriggerEventId,
    WikidataEntityId, WikidataIdParseError, WikidataPropertyId,
};
pub use ingestion::{ImageSource, IngestionBundle, IngestionNotes, ReferenceError};
pub use lattice::{BoundedLattice, JoinSemilattice, MeetSemilattice};
pub use links::{ExternalLink, LinkTarget, LinkType};
pub use location::{
    ConflictStatus, Distance, Location, LocationError, LocationReference, UnresolvedLocation,
};
pub use moment::{Moment, TransitionRole, decompose, structural_edges, topological_order};
