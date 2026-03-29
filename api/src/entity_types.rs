//! Response types for entity API endpoints.

use chrono::NaiveDateTime;
use chronoscope_core::annotation::AnnotationKind;
use chronoscope_core::entity::{Entity, EntityType};
use chronoscope_core::links::{LinkTarget, LinkType};
use chronoscope_db::{Annotation, EntityDbId, EntityLink, EntityLinkDbId, StoredEntity};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ==================== Summary (for list/map endpoints) ====================

/// Lightweight entity summary for map markers and list views.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EntitySummary {
    pub id: EntityDbId,
    pub entity_type: EntityType,
    /// Best available name (English preferred, then first available).
    // TODO: Don't hardcode English preference — support user-preferred language.
    pub name: Option<String>,
    /// Latitude of the entity. Non-optional because this type is only returned
    /// by spatial (bounding box) queries, which inherently filter to entities
    /// with known coordinates.
    // TODO: A non-spatial entity listing endpoint would need Option<f64> here,
    // since the entity model doesn't require locations.
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

impl EntitySummary {
    /// Build a summary from a stored entity with coordinates.
    ///
    /// # Errors
    /// Returns an error if the entity has no location (should not happen for
    /// bbox-filtered queries, but we don't rely on that invariant).
    pub fn try_from_stored(entity: &StoredEntity) -> Result<Self, &'static str> {
        let coords = entity.location.ok_or("entity missing coordinates")?;
        Ok(Self {
            id: entity.id.clone(),
            entity_type: entity.entity_type(),
            name: pick_name(&entity.entity),
            latitude: coords.lat,
            longitude: coords.lon,
            earliest_date: entity.temporal_bounds.as_ref().map(|b| b.earliest),
            latest_date: entity.temporal_bounds.as_ref().map(|b| b.latest),
            updated_at: entity.updated_at,
        })
    }
}

/// Pick the best display name: prefer English, fall back to first available.
// TODO: Don't hardcode English preference — support user-preferred language.
fn pick_name(entity: &Entity) -> Option<String> {
    // Try English first
    let english = entity
        .names
        .iter()
        .find(|n| n.value.language.as_str().starts_with("en"));
    if let Some(name) = english {
        return Some(name.value.name.clone());
    }
    // Fall back to first name
    entity.names.first().map(|n| n.value.name.clone())
}

// ==================== Detail (for single entity view) ====================

/// Full entity detail including links and annotations.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EntityDetail {
    pub id: EntityDbId,
    pub entity: Entity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub earliest_date: Option<NaiveDateTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_date: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub links: Vec<EntityLinkSummary>,
    pub annotations: Vec<AnnotationSummary>,
}

/// An external link attached to an entity.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EntityLinkSummary {
    pub id: EntityLinkDbId,
    pub link_type: LinkType,
    pub target: LinkTarget,
}

impl From<EntityLink> for EntityLinkSummary {
    fn from(link: EntityLink) -> Self {
        Self {
            id: link.id,
            link_type: link.link_type,
            target: link.target,
        }
    }
}

/// An annotation linking an entity to a source image region.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AnnotationSummary {
    pub id: chronoscope_db::AnnotationDbId,
    pub kind: AnnotationKind,
    pub created_at: NaiveDateTime,
}

impl From<Annotation> for AnnotationSummary {
    fn from(ann: Annotation) -> Self {
        Self {
            id: ann.id,
            kind: ann.kind,
            created_at: ann.created_at,
        }
    }
}
