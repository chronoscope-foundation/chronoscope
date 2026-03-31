//! Conversion functions from database models to API response types.
//!
//! The response types themselves live in `chronoscope_api_client::entities`.
//! This module provides the server-side logic for constructing them from
//! database rows (orphan rule prevents `impl From<DbType> for ClientType`).

pub use chronoscope_api_client::{
    AnnotationSummary, EntityLinkSummary, EntityResponse, EntitySummary,
};
use chronoscope_db::{Annotation, EntityLink};

use chronoscope_core::entity::Entity;
use chronoscope_db::{EntityId, SourceId};

/// Concrete core entity type parameterized with database ID types.
type CoreEntity = Entity<EntityId, SourceId>;

/// Build a summary from a stored entity with coordinates.
///
/// # Errors
/// Returns an error if the entity has no location (should not happen for
/// bbox-filtered queries, but we don't rely on that invariant).
pub fn entity_summary_from_stored(
    entity: &chronoscope_db::Entity,
) -> Result<EntitySummary, &'static str> {
    let coords = entity.location.ok_or("entity missing coordinates")?;
    Ok(EntitySummary {
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

/// Pick the best display name: prefer English, fall back to first available.
// TODO: Don't hardcode English preference — support user-preferred language.
fn pick_name(entity: &CoreEntity) -> Option<String> {
    entity.best_name("en").map(String::from)
}

/// Convert a database `EntityLink` to an API `EntityLinkSummary`.
pub fn entity_link_summary(link: EntityLink) -> EntityLinkSummary {
    EntityLinkSummary {
        id: link.id,
        link_type: link.link_type,
        target: link.target,
    }
}

/// Convert a database `Annotation` to an API `AnnotationSummary`.
pub fn annotation_summary(ann: Annotation) -> AnnotationSummary {
    AnnotationSummary {
        id: ann.id,
        kind: ann.kind,
        created_at: ann.created_at,
    }
}
