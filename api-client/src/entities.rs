//! Entity API response types.
//!
//! These are the wire-format types for entity endpoints. They're projections
//! and summaries of core domain types, not domain types themselves.

use chrono::NaiveDateTime;
use chronoscope_core::{AnnotationKind, Entity, EntityType, LinkTarget, LinkType};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{AnnotationId, EntityId, EntityLinkId, SourceId};

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
