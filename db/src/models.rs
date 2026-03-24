//! Database domain types.
//!
//! These are the public types returned by `Database` methods. Raw sqlx row
//! types live in the `row` module (pub(crate)) and are converted via
//! `into_domain()` before escaping the crate.

use chrono::NaiveDateTime;
use chronoscope_core::annotation::AnnotationKind;
use chronoscope_core::entity::{Entity, EntityTransition, EntityType};
use chronoscope_core::links::{LinkTarget, LinkType};
use chronoscope_core::{UncertainDate, UncertainLocation};
use sqlx::FromRow;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::types::{
    AnnotationDbId, Email, EntityDbId, EntityLinkDbId, MediaAnalysisState, MediaId, MediaType,
    PageId, ResearchUrlId, ResearchUrlStatus, SourceType, UserId,
};

/// A user account
#[derive(Debug, Clone, FromRow)]
pub struct User {
    pub id: UserId,
    pub username: String,
    pub email: Email,
    pub created_at: NaiveDateTime,
}

/// What a research URL has resolved to after processing.
///
/// A URL starts as `Unresolved`, then a worker resolves it to either a page
/// (HTML content with embedded media references) or direct media (image/video).
/// The DB enforces mutual exclusion between `page_id` and `media_id` via a CHECK constraint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTarget {
    /// URL has not been resolved yet (both `page_id` and `media_id` are NULL).
    Unresolved,
    /// URL resolved to a page (HTML content).
    Page(PageId),
    /// URL resolved to direct media (image or video).
    Media(MediaId),
}

/// A research URL (canonical, deduplicated)
#[derive(Debug, Clone)]
pub struct ResearchUrl {
    pub id: ResearchUrlId,
    pub url: String,
    pub target: ResolvedTarget,
    pub status: ResearchUrlStatus,
    pub attempt_count: i32,
    /// Worker affinity: which specialized worker should process this URL.
    /// `None` means generic worker, `Some("reddit")`, etc. for specialized workers.
    pub worker_affinity: Option<String>,
    pub created_at: NaiveDateTime,
}

impl<'r> sqlx::FromRow<'r, SqliteRow> for ResearchUrl {
    fn from_row(row: &'r SqliteRow) -> Result<Self, sqlx::Error> {
        let page_id: Option<PageId> = row.try_get("page_id")?;
        let media_id: Option<MediaId> = row.try_get("media_id")?;

        let target = match (page_id, media_id) {
            (None, None) => ResolvedTarget::Unresolved,
            (Some(pid), None) => ResolvedTarget::Page(pid),
            (None, Some(mid)) => ResolvedTarget::Media(mid),
            (Some(_), Some(_)) => {
                return Err(sqlx::Error::ColumnDecode {
                    index: "page_id/media_id".to_string(),
                    source: Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "page_id and media_id are mutually exclusive",
                    )),
                });
            }
        };

        Ok(Self {
            id: row.try_get("id")?,
            url: row.try_get("url")?,
            target,
            status: row.try_get("status")?,
            attempt_count: row.try_get("attempt_count")?,
            worker_affinity: row.try_get("worker_affinity")?,
            created_at: row.try_get("created_at")?,
        })
    }
}

/// A research URL that a user follows (includes follow timestamp)
#[derive(Debug, Clone, FromRow)]
pub struct FollowedUrl {
    #[sqlx(flatten)]
    pub research_url: ResearchUrl,
    pub followed_at: NaiveDateTime,
}

/// A media slot - a URL reference that may or may not be resolved to Media yet
#[derive(Debug, Clone)]
pub struct MediaSlot {
    pub url: String,
    pub resolved: Option<Media>,
}

impl MediaSlot {
    /// Create a slot for a URL that hasn't been fetched yet
    pub fn pending(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            resolved: None,
        }
    }
}

/// Core page data (used for both creation and reading)
#[derive(Debug, Clone)]
pub struct PageData {
    pub source_type: SourceType,
    pub title: Option<String>,
    pub author: Option<String>,
    pub published: Option<UncertainDate>,
    pub content: Option<String>,
    pub fetched_at: NaiveDateTime,
    /// Media referenced by this page (in source order)
    pub media: Vec<MediaSlot>,
}

/// A page with database-generated fields
#[derive(Debug, Clone)]
pub struct Page {
    pub id: PageId,
    pub data: PageData,
    pub created_at: NaiveDateTime,
}

/// Core media data (used for both creation and reading)
#[derive(Debug, Clone)]
pub struct MediaData {
    pub exact_hash: Vec<u8>,
    pub perceptual_hash: Option<Vec<u8>>,
    pub storage_key: String,
    pub media_type: MediaType,
    pub width: i32,
    pub height: i32,
    pub duration_seconds: Option<f32>,
    pub captured: Option<UncertainDate>,
    pub location: Option<UncertainLocation>,
    pub source_metadata: Option<String>, // JSON stored as text
    pub fetched_at: NaiveDateTime,
}

/// A media item with database-generated fields
#[derive(Debug, Clone)]
pub struct Media {
    pub id: MediaId,
    pub data: MediaData,
    pub created_at: NaiveDateTime,
    /// Analysis state with associated data (enforces valid state combinations)
    pub analysis: MediaAnalysisState,
}

/// Resolved content - either a page with embedded media, or direct media
#[derive(Debug, Clone)]
pub enum ResolvedContent {
    Page(Page),
    Media(Media),
}

/// Full dossier data for a research URL
#[derive(Debug, Clone)]
pub struct ResearchUrlWithResolved {
    pub research_url: ResearchUrl,
    pub resolved: Option<ResolvedContent>,
}

// ==================== Entities ====================

/// Temporal bounds extracted from entity transitions.
///
/// Both fields are always present together — a single transition date
/// populates both earliest and latest with the same value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateRange {
    pub earliest: NaiveDateTime,
    pub latest: NaiveDateTime,
}

/// A WGS 84 coordinate pair.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coordinates {
    pub lat: f64,
    pub lon: f64,
}

/// An entity as stored in the database.
///
/// Wraps `chronoscope_core::entity::Entity` (the domain model) with
/// database metadata: ID, timestamps, and cached shadow columns.
#[derive(Debug, Clone)]
pub struct StoredEntity {
    pub id: EntityDbId,
    pub entity: Entity,
    pub temporal_bounds: Option<DateRange>,
    pub location: Option<Coordinates>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

impl StoredEntity {
    /// Entity type, derived from the stored entity.
    #[must_use]
    pub fn entity_type(&self) -> EntityType {
        self.entity.entity_type
    }
}

/// An external link attached to an entity, with the full structured target.
#[derive(Debug, Clone)]
pub struct EntityLink {
    pub id: EntityLinkDbId,
    pub entity_id: EntityDbId,
    pub link_type: LinkType,
    pub target: LinkTarget,
}

/// An annotation linking an entity to a source image region.
#[derive(Debug, Clone)]
pub struct Annotation {
    pub id: AnnotationDbId,
    pub entity_id: EntityDbId,
    pub url_id: ResearchUrlId,
    pub kind: AnnotationKind,
    pub created_at: NaiveDateTime,
}

// ==================== Shadow Column Extraction ====================

/// Extract temporal bounds (earliest, latest) from entity transitions.
///
/// Scans all transitions for the earliest and latest dates across all
/// date fields (`started_at`, `completed_at`, `occurred_at`).
pub fn extract_temporal_bounds(entity: &Entity) -> (Option<NaiveDateTime>, Option<NaiveDateTime>) {
    let mut earliest: Option<NaiveDateTime> = None;
    let mut latest: Option<NaiveDateTime> = None;

    for transition in &entity.transitions {
        let (start, end) = transition.date_range();
        for cited_date in [start, end].into_iter().flatten() {
            let e = cited_date.value.earliest();
            earliest = Some(earliest.map_or(e, |prev: NaiveDateTime| prev.min(e)));
            let l = cited_date.value.latest();
            latest = Some(latest.map_or(l, |prev: NaiveDateTime| prev.max(l)));
        }
    }

    (earliest, latest)
}

/// Extract location (lat, lon) from entity transitions.
///
/// Returns the last coordinate found scanning transitions, so that entities
/// which moved reflect their most recent location.
pub fn extract_location(entity: &Entity) -> Option<(f64, f64)> {
    let mut result = None;

    for transition in &entity.transitions {
        let loc = match transition {
            EntityTransition::Constructed { location, .. }
            | EntityTransition::Moved { location, .. } => location.as_ref(),
            EntityTransition::Modified { .. }
            | EntityTransition::Damaged { .. }
            | EntityTransition::Repaired { .. }
            | EntityTransition::Demolished { .. }
            | EntityTransition::UsageModified { .. }
            | EntityTransition::Designated { .. } => None,
        };

        if let Some(cited_loc) = loc
            && let UncertainLocation::Coordinates { lat, lon, .. } = &cited_loc.value
        {
            result = Some((*lat, *lon));
        }
    }
    result
}

/// Extract a canonical URL string from a [`LinkTarget`].
pub fn extract_target_url(target: &LinkTarget) -> String {
    target.to_url().to_string()
}

#[cfg(test)]
mod tests {
    use chrono::Datelike;

    use super::*;

    #[test]
    fn extract_temporal_bounds_empty() {
        let entity = Entity {
            entity_type: EntityType::Building,
            names: vec![],
            transitions: vec![],
        };
        let (earliest, latest) = extract_temporal_bounds(&entity);
        assert!(earliest.is_none());
        assert!(latest.is_none());
    }

    #[test]
    fn extract_temporal_bounds_from_constructed() -> Result<(), Box<dyn std::error::Error>> {
        use chronoscope_core::{Cited, DatePrecision, UncertainDate};
        let date = UncertainDate::with_precision(
            chrono::NaiveDate::from_ymd_opt(1889, 1, 1)
                .ok_or("bad date")?
                .and_hms_opt(0, 0, 0)
                .ok_or("bad time")?,
            DatePrecision::Year,
        )?;
        let entity = Entity {
            entity_type: EntityType::Building,
            names: vec![],
            transitions: vec![EntityTransition::Constructed {
                started_at: None,
                completed_at: Some(Cited::uncited(date)),
                location: None,
                trigger_event: None,
            }],
        };
        let (earliest, latest) = extract_temporal_bounds(&entity);
        // completed_at populates both earliest and latest
        let earliest_dt = earliest.ok_or("no earliest")?;
        assert_eq!(earliest_dt.and_utc().year(), 1889);
        assert!(latest.is_some());
        Ok(())
    }

    #[test]
    fn extract_location_from_constructed() -> Result<(), Box<dyn std::error::Error>> {
        use chronoscope_core::{Cited, UncertainLocation};
        let loc = UncertainLocation::coordinates(48.8584, 2.2945, None, None)?;
        let entity = Entity {
            entity_type: EntityType::Building,
            names: vec![],
            transitions: vec![EntityTransition::Constructed {
                started_at: None,
                completed_at: None,
                location: Some(Cited::uncited(loc)),
                trigger_event: None,
            }],
        };
        let (lat, lon) = extract_location(&entity).ok_or("no location")?;
        assert!((lat - 48.8584).abs() < f64::EPSILON);
        assert!((lon - 2.2945).abs() < f64::EPSILON);
        Ok(())
    }
}
