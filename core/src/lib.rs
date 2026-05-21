//! Chronoscope Core Library
//!
//! Core types for the Chronoscope spatiotemporal knowledge platform.

pub mod annotation;
pub mod consistency;
pub mod date;
pub mod entity;
pub mod evidence;
pub mod facts;
pub mod ids;
pub mod ingestion;
pub mod links;
pub mod location;
pub mod merge;
pub mod moment;
pub mod nonempty;

pub use annotation::{Annotation, AnnotationKind};
pub use consistency::ConsistencyWarning;
pub use date::{DateBound, DateError, DatePrecision, UncertainDate};
pub use entity::{
    DamageCause, Entity, EntityName, EntityRelation, EntityRelationType, EntityTransition,
    MoveMethod, NameType, Usage,
};
pub use evidence::{
    Cited, Evidence, ImageRegion, MaskDimensions, Polyline, PolylineError, RleMask, SourceDetail,
    SpatialGeometry,
};
pub use ids::{
    GeoNamesId, GettyTgnId, OhmId, OsmElementType, OsmId, TriggerEventId, WikidataEntityId,
    WikidataPropertyId,
};
pub use ingestion::{ImageSource, IngestionBundle, IngestionNotes, ReferenceError};
pub use links::{ExternalLink, LinkTarget, LinkType};
pub use location::{Distance, Location, LocationError, LocationReference, UnresolvedLocation};
pub use moment::{Moment, TransitionRole, decompose, structural_edges, topological_order};
