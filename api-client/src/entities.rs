//! Entity API response types.
//!
//! These are the wire-format types for entity endpoints. They're projections
//! and summaries of core domain types, not domain types themselves.

use chrono::NaiveDateTime;
use chronoscope_core::{AnnotationKind, Entity, EntityType, LinkTarget, LinkType, UncertainDate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use std::fmt;

use crate::ids::{AnnotationId, EntityId, EntityLinkId, MediaId, SourceId};

/// Lightweight entity summary for map markers and list views.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EntitySummary {
    pub id: EntityId,
    pub entity_type: EntityType,
    /// Best available name (English preferred, then first available).
    pub name: Option<String>,
    /// Latitude of the entity. Non-optional because this type is only returned
    /// by spatial (bounding box) queries, which inherently filter to entities
    /// with known coordinates.
    pub latitude: f64,
    pub longitude: f64,
    /// Earliest known date across all transitions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub earliest_date: Option<NaiveDateTime>,
    /// Latest known date across all transitions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_date: Option<NaiveDateTime>,
    pub updated_at: NaiveDateTime,
}

/// Full entity response — the core `Entity` plus associated metadata.
///
/// This is a response envelope, not a domain type. The `entity` field contains
/// the actual domain data; the rest is infrastructure metadata.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EntityResponse {
    pub id: EntityId,
    pub entity: Entity<EntityId, SourceId>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub links: Vec<EntityLinkSummary>,
    pub annotations: Vec<AnnotationSummary>,
    /// Resolved media items associated with this entity (images, videos).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<MediaSummary>,
}

/// An external link attached to an entity.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EntityLinkSummary {
    pub id: EntityLinkId,
    pub link_type: LinkType,
    pub target: LinkTarget,
}

/// An annotation linking an entity to a source image region.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AnnotationSummary {
    pub id: AnnotationId,
    pub kind: AnnotationKind,
    pub created_at: NaiveDateTime,
}

/// A media item associated with an entity, for the detail panel image grid.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MediaSummary {
    pub id: MediaId,
    /// Ready-to-use URL for fetching the image (e.g., `/media/abc123.jpg` or CDN URL).
    pub url: String,
    /// Original upstream URL where this media was found.
    pub source_url: String,
    pub width: i32,
    pub height: i32,
    /// When the image was captured (may be uncertain / a range).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured: Option<UncertainDate>,
    pub annotation_kind: AnnotationKind,
}

/// Lightweight thumbnail info for map markers (one per entity).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ThumbnailInfo {
    /// Ready-to-use URL for fetching the thumbnail image.
    pub url: String,
    pub width: i32,
    pub height: i32,
}

// ==================== Unified Markers ====================

/// Typed marker identifier — either an entity UUID or a cluster OSM ID.
///
/// Serializes to/from a string: entity UUIDs serialize as-is, cluster IDs
/// as `"cluster-{osm_id}"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MarkerId {
    Entity(EntityId),
    Cluster(i64),
}

const CLUSTER_ID_PREFIX: &str = "cluster-";

impl fmt::Display for MarkerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Entity(id) => write!(f, "{id}"),
            Self::Cluster(osm_id) => write!(f, "{CLUSTER_ID_PREFIX}{osm_id}"),
        }
    }
}

impl Serialize for MarkerId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for MarkerId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        if let Some(osm_str) = s.strip_prefix(CLUSTER_ID_PREFIX) {
            let osm_id = osm_str.parse::<i64>().map_err(serde::de::Error::custom)?;
            Ok(Self::Cluster(osm_id))
        } else {
            Ok(Self::Entity(EntityId::new(s)))
        }
    }
}

impl JsonSchema for MarkerId {
    fn schema_name() -> String {
        "MarkerId".to_string()
    }

    fn json_schema(_generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        schemars::schema::SchemaObject {
            instance_type: Some(schemars::schema::InstanceType::String.into()),
            ..Default::default()
        }
        .into()
    }
}

/// A map marker — either an individual entity or a cluster of entities
/// in an administrative region. The server decides which to return based
/// on entity density in the requested bounding box.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Marker {
    /// Stable identifier. Entity UUID for individual entities,
    /// `"cluster-{osm_id}"` for region clusters.
    pub id: MarkerId,
    pub latitude: f64,
    pub longitude: f64,
    /// Display label (entity name or region name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Ready-to-use thumbnail URL, resolved server-side. `None` if the
    /// entity (or cluster representative) has no resolved media.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    /// What happens when the user clicks this marker.
    pub click_action: ClickAction,
}

/// What happens when a marker is clicked.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum ClickAction {
    /// Zoom the map to this bounding box (cluster markers).
    #[serde(rename = "zoom_to")]
    ZoomTo {
        #[serde(flatten)]
        bbox: crate::types::Bbox,
    },
    /// Open the entity detail panel (individual entity markers).
    #[serde(rename = "select")]
    Select {
        entity_id: EntityId,
        entity_type: EntityType,
    },
    /// Show a disambiguation picker (co-located entities at the same coordinates).
    #[serde(rename = "disambiguate")]
    Disambiguate { entries: Vec<EntityPickerEntry> },
}

/// One entry in a co-located entity disambiguation picker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EntityPickerEntry {
    pub id: String,
    pub name: Option<String>,
    pub entity_type: String,
}

/// Response for the unified markers endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MarkersResponse {
    pub markers: Vec<Marker>,
    /// True if results were truncated at the server limit.
    pub truncated: bool,
}
