//! Database domain types.
//!
//! These are the public types returned by `Database` methods. Raw sqlx row
//! types live in the `row` module (pub(crate)) and are converted via
//! `into_domain()` before escaping the crate.

use chrono::NaiveDateTime;
use chronoscope_core::UncertainDate;
use sqlx::FromRow;

use chronoscope_integrations::IntegrationName;

use crate::types::{
    Email, MediaAnalysisState, MediaId, MediaType, PageId, ResearchUrlId, ResearchUrlStatus, UserId,
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

impl<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> for ResearchUrl {
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;

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
    pub source_type: IntegrationName,
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
    pub location: Option<chronoscope_core::UnresolvedLocation>,
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
