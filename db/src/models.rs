//! Database models.

use chrono::NaiveDateTime;
use chronoscope_core::entity::{Entity, EntityRelationType, EntityTransition};
use chronoscope_core::links::LinkTarget;
use chronoscope_core::{UncertainDate, UncertainLocation};
use sqlx::FromRow;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::types::{
    AnalysisStatus, DbEntityType, Email, MediaAnalysisState, MediaId, MediaType, PageId,
    ResearchUrlId, ResearchUrlStatus, SourceType, UserId,
};

/// Construct analysis state from raw DB fields, enforcing invariants.
/// Invalid combinations (e.g., Complete without results) fall back to safe states.
///
fn build_analysis_state(
    status: AnalysisStatus,
    analysis_result: Option<String>,
    analysis_error: Option<String>,
) -> MediaAnalysisState {
    match status {
        AnalysisStatus::Pending => MediaAnalysisState::Pending,
        AnalysisStatus::Processing => MediaAnalysisState::Processing,
        AnalysisStatus::Complete => match analysis_result {
            Some(result) => MediaAnalysisState::Complete {
                analysis_result: result,
            },
            // DB corruption: Complete without results. Treat as pending.
            None => MediaAnalysisState::Pending,
        },
        AnalysisStatus::Failed => MediaAnalysisState::Failed {
            error: analysis_error.unwrap_or_else(|| "Unknown error".to_string()),
        },
    }
}

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

/// Internal row type for reading pages from DB (no media, fetched separately)
#[derive(Debug, FromRow)]
pub(crate) struct PageDbRow {
    pub(crate) id: PageId,
    pub(crate) source_type: SourceType,
    pub(crate) title: Option<String>,
    pub(crate) author: Option<String>,
    #[sqlx(json(nullable), rename = "published_meta")]
    pub(crate) published: Option<UncertainDate>,
    pub(crate) content: Option<String>,
    pub(crate) fetched_at: NaiveDateTime,
    pub(crate) created_at: NaiveDateTime,
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

/// Internal row type for sqlx (maps to flat DB columns)
#[derive(Debug, FromRow)]
pub(crate) struct MediaDbRow {
    pub(crate) id: MediaId,
    pub(crate) exact_hash: Vec<u8>,
    pub(crate) perceptual_hash: Option<Vec<u8>>,
    pub(crate) storage_key: String,
    pub(crate) media_type: MediaType,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) duration_seconds: Option<f32>,
    #[sqlx(json(nullable), rename = "captured_meta")]
    pub(crate) captured: Option<UncertainDate>,
    #[sqlx(json(nullable), rename = "location_meta")]
    pub(crate) location: Option<UncertainLocation>,
    pub(crate) source_metadata: Option<String>,
    pub(crate) fetched_at: NaiveDateTime,
    pub(crate) created_at: NaiveDateTime,
    pub(crate) analysis_status: AnalysisStatus,
    pub(crate) analysis_result: Option<String>,
    pub(crate) analysis_error: Option<String>,
}

impl MediaDbRow {
    pub(crate) fn into_media(self) -> Media {
        let analysis = build_analysis_state(
            self.analysis_status,
            self.analysis_result,
            self.analysis_error,
        );

        Media {
            id: self.id,
            data: MediaData {
                exact_hash: self.exact_hash,
                perceptual_hash: self.perceptual_hash,
                storage_key: self.storage_key,
                media_type: self.media_type,
                width: self.width,
                height: self.height,
                duration_seconds: self.duration_seconds,
                captured: self.captured,
                location: self.location,
                source_metadata: self.source_metadata,
                fetched_at: self.fetched_at,
            },
            created_at: self.created_at,
            analysis,
        }
    }
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

/// Raw row from the page media query (internal use only)
#[derive(Debug, FromRow)]
pub(crate) struct PageMediaRow {
    pub(crate) source_url: String,
    // Media fields (all optional since LEFT JOIN)
    pub(crate) id: Option<MediaId>,
    pub(crate) exact_hash: Option<Vec<u8>>,
    pub(crate) perceptual_hash: Option<Vec<u8>>,
    pub(crate) storage_key: Option<String>,
    pub(crate) media_type: Option<MediaType>,
    pub(crate) width: Option<i32>,
    pub(crate) height: Option<i32>,
    pub(crate) duration_seconds: Option<f32>,
    #[sqlx(json(nullable), rename = "captured_meta")]
    pub(crate) captured: Option<UncertainDate>,
    #[sqlx(json(nullable), rename = "location_meta")]
    pub(crate) location: Option<UncertainLocation>,
    pub(crate) source_metadata: Option<String>,
    pub(crate) fetched_at: Option<NaiveDateTime>,
    pub(crate) created_at: Option<NaiveDateTime>,
    pub(crate) analysis_status: Option<AnalysisStatus>,
    pub(crate) analysis_result: Option<String>,
    pub(crate) analysis_error: Option<String>,
}

impl PageMediaRow {
    pub(crate) fn into_media_slot(mut self) -> MediaSlot {
        let url = std::mem::take(&mut self.source_url);
        MediaSlot {
            url,
            resolved: self.into_media(),
        }
    }

    /// Build Media from LEFT JOIN fields. Returns None if id is absent.
    fn into_media(self) -> Option<Media> {
        let id = self.id?;
        let analysis = build_analysis_state(
            self.analysis_status?,
            self.analysis_result,
            self.analysis_error,
        );

        Some(Media {
            id,
            data: MediaData {
                exact_hash: self.exact_hash?,
                perceptual_hash: self.perceptual_hash,
                storage_key: self.storage_key?,
                media_type: self.media_type?,
                width: self.width?,
                height: self.height?,
                duration_seconds: self.duration_seconds,
                captured: self.captured,
                location: self.location,
                source_metadata: self.source_metadata,
                fetched_at: self.fetched_at?,
            },
            created_at: self.created_at?,
            analysis,
        })
    }
}

// ==================== Entities ====================

/// Row type for reading entities from DB.
#[derive(Debug, FromRow)]
pub struct EntityDbRow {
    pub id: crate::types::EntityDbId,
    pub entity_type: DbEntityType,
    pub entity_json: String,
    pub earliest_date: Option<NaiveDateTime>,
    pub latest_date: Option<NaiveDateTime>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

// ==================== Shadow Column Extraction ====================

/// Extract temporal bounds (earliest, latest) from entity transitions.
///
/// Scans all transitions for the earliest and latest dates across all
/// date fields (`started_at`, `completed_at`, `occurred_at`).
pub fn extract_temporal_bounds(entity: &Entity) -> (Option<String>, Option<String>) {
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

    (
        earliest.map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string()),
        latest.map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string()),
    )
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

// Manual match rather than serde because these are flat strings, not tagged JSON objects.

/// Serialize [`LinkType`] to its `snake_case` DB string.
pub fn link_type_to_db(lt: &chronoscope_core::links::LinkType) -> &'static str {
    match lt {
        chronoscope_core::links::LinkType::SameAs => "same_as",
        chronoscope_core::links::LinkType::Related => "related",
        chronoscope_core::links::LinkType::FurtherReading => "further_reading",
    }
}

/// Serialize [`EntityRelationType`] to its `snake_case` DB string.
pub fn relation_type_to_db(rt: &EntityRelationType) -> &'static str {
    match rt {
        EntityRelationType::Replaces => "replaces",
        EntityRelationType::Contains => "contains",
        EntityRelationType::MergedFrom => "merged_from",
        EntityRelationType::SplitFrom => "split_from",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chronoscope_core::entity::{EntityTransition, EntityType};

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
        assert!(earliest.is_some());
        let earliest_str = earliest.ok_or("no earliest")?;
        assert!(earliest_str.starts_with("1889"));
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

    #[test]
    fn link_type_to_db_values() {
        assert_eq!(
            link_type_to_db(&chronoscope_core::links::LinkType::SameAs),
            "same_as"
        );
        assert_eq!(
            link_type_to_db(&chronoscope_core::links::LinkType::Related),
            "related"
        );
        assert_eq!(
            link_type_to_db(&chronoscope_core::links::LinkType::FurtherReading),
            "further_reading"
        );
    }

    #[test]
    fn relation_type_to_db_values() {
        use chronoscope_core::entity::EntityRelationType;
        assert_eq!(
            relation_type_to_db(&EntityRelationType::Replaces),
            "replaces"
        );
        assert_eq!(
            relation_type_to_db(&EntityRelationType::Contains),
            "contains"
        );
    }
}
