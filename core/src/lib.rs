//! Chronoscope Core Library
//!
//! Core types for the Chronoscope spatiotemporal knowledge platform.

pub mod annotation;
pub mod consistency;
pub mod date;
pub mod entity;
pub mod evidence;
#[cfg(test)]
pub mod fixtures;
pub mod ids;
pub mod ingestion;
pub mod links;
pub mod location;

pub use annotation::{Annotation, AnnotationKind, StoredAnnotation};
pub use consistency::ConsistencyWarning;
pub use date::{DateError, DatePrecision, PreciseDate, UncertainDate};
pub use entity::{
    DamageCause, Entity, EntityName, EntityRelation, EntityRelationType, EntityTransition,
    EntityType, IngestionRelation, MoveMethod, NameType, Usage,
};
pub use evidence::{
    Cited, Evidence, ImageRegion, MaskDimensions, Polyline, PolylineError, RleMask, SpatialGeometry,
};
pub use ids::{
    DocumentId, EntityId, EntityIdx, GeoNamesId, GettyTgnId, ImageId, LinkIdx, MapId, OhmId,
    OsmElementType, OsmId, SourceId, SourceIdx, TriggerEventId, WikidataEntityId,
    WikidataPropertyId,
};
pub use ingestion::{
    ImageSource, IngestionBundle, IngestionNotes, IngestionOutput, ReferenceError,
};
pub use links::{ExternalLink, LinkTarget, LinkType};
pub use location::{Distance, Elevation, LocationError, UncertainLocation};
