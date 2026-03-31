//! Annotation types for connecting entities to sources.
//!
//! Annotations link entities to regions within images, maps, and documents.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::evidence::{ImageRegion, SpatialGeometry};

/// What kind of annotation this is, with per-variant geometry.
///
/// Each variant carries only the fields that make sense for that annotation type,
/// preventing nonsensical combinations (e.g., `extracted_text` on a spatial trace).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnnotationKind {
    /// Spatial outline traced on a map (building footprint, property boundary, road/path, etc.)
    SpatialTrace {
        /// Geometry of the trace (mask or polyline). `None` if not yet traced.
        #[serde(skip_serializing_if = "Option::is_none")]
        geometry: Option<SpatialGeometry>,
    },
    /// Entity exterior visible in photo/painting — used for photogrammetry.
    ExteriorView {
        /// Region where the entity exterior appears. `None` if not yet delineated.
        #[serde(skip_serializing_if = "Option::is_none")]
        region: Option<ImageRegion>,
    },
    /// Entity interior visible in photo.
    InteriorView {
        /// Region where the entity interior appears. `None` if not yet delineated.
        #[serde(skip_serializing_if = "Option::is_none")]
        region: Option<ImageRegion>,
    },
    /// Text region on map/photo containing information about an entity.
    TextualNote {
        /// Region containing the text. `None` if not yet delineated.
        #[serde(skip_serializing_if = "Option::is_none")]
        region: Option<ImageRegion>,
        /// Text extracted from the region.
        #[serde(skip_serializing_if = "Option::is_none")]
        extracted_text: Option<String>,
    },
}

/// An annotation connecting an entity to a region in a source.
///
/// Generic over reference types:
/// - For ingestion bundles: `Annotation<SourceIdx, EntityIdx>` (typed indices into vectors)
/// - For stored data: `Annotation<SourceId, EntityId>` (persistent IDs from api-client)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(bound(
    deserialize = "SourceRef: serde::de::DeserializeOwned, EntityRef: serde::de::DeserializeOwned"
))]
pub struct Annotation<SourceRef, EntityRef> {
    pub source: SourceRef,
    pub entity: EntityRef,
    pub kind: AnnotationKind,
}
