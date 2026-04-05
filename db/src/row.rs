//! Raw sqlx row types — internal to the db crate.
//!
//! These structs mirror DB columns 1:1 for `sqlx::FromRow` derivation.
//! They never escape the crate; every `Database` method converts them
//! into domain types via `into_domain()` before returning.

use chrono::NaiveDateTime;
use chronoscope_core::UncertainDate;
use chronoscope_core::links::LinkType;
use sqlx::FromRow;

use crate::error::DbError;
use crate::models;
use chronoscope_integrations::IntegrationName;

use crate::types::{
    AnalysisStatus, AnnotationId, AnnotationKindTag, EntityId, EntityLinkId, MediaAnalysisState,
    MediaId, MediaType, PageId, ResearchUrlId, SourceId,
};

/// Construct analysis state from raw DB fields.
///
/// Returns an error if the status/data combination is invalid (e.g., `Complete`
/// without results), rather than silently falling back to a safe state.
fn build_analysis_state(
    status: AnalysisStatus,
    analysis_result: Option<String>,
    analysis_error: Option<String>,
) -> Result<MediaAnalysisState, DbError> {
    match status {
        AnalysisStatus::Pending => Ok(MediaAnalysisState::Pending),
        AnalysisStatus::Processing => Ok(MediaAnalysisState::Processing),
        AnalysisStatus::Complete => match analysis_result {
            Some(result) => Ok(MediaAnalysisState::Complete {
                analysis_result: result,
            }),
            None => Err(DbError::InconsistentRow(
                "analysis_status is 'complete' but analysis_result is NULL".to_string(),
            )),
        },
        AnalysisStatus::Failed => match analysis_error {
            Some(error) => Ok(MediaAnalysisState::Failed { error }),
            None => Err(DbError::InconsistentRow(
                "analysis_status is 'failed' but analysis_error is NULL".to_string(),
            )),
        },
    }
}

/// Parse a source type DB string into `IntegrationName`.
fn parse_source_type(s: &str) -> Result<IntegrationName, DbError> {
    s.parse()
        .map_err(|e: strum::ParseError| DbError::InconsistentRow(e.to_string()))
}

// ==================== Page ====================

#[derive(Debug, FromRow)]
pub struct Page {
    pub id: PageId,
    pub source_type: String,
    pub title: Option<String>,
    pub author: Option<String>,
    #[sqlx(json(nullable), rename = "published_meta")]
    pub published: Option<UncertainDate>,
    pub content: Option<String>,
    pub fetched_at: NaiveDateTime,
    pub created_at: NaiveDateTime,
}

impl Page {
    pub fn into_domain(self, media: Vec<models::MediaSlot>) -> Result<models::Page, DbError> {
        Ok(models::Page {
            id: self.id,
            data: models::PageData {
                source_type: parse_source_type(&self.source_type)?,
                title: self.title,
                author: self.author,
                published: self.published,
                content: self.content,
                fetched_at: self.fetched_at,
                media,
            },
            created_at: self.created_at,
        })
    }
}

// ==================== Media ====================

#[derive(Debug, FromRow)]
pub struct Media {
    pub id: MediaId,
    pub exact_hash: Vec<u8>,
    pub perceptual_hash: Option<Vec<u8>>,
    pub storage_key: String,
    pub media_type: MediaType,
    pub width: i32,
    pub height: i32,
    pub duration_seconds: Option<f32>,
    #[sqlx(json(nullable), rename = "captured_meta")]
    pub captured: Option<UncertainDate>,
    #[sqlx(json(nullable), rename = "location_meta")]
    pub location: Option<chronoscope_core::UncertainLocation<EntityId>>,
    pub source_metadata: Option<String>,
    pub fetched_at: NaiveDateTime,
    pub created_at: NaiveDateTime,
    pub analysis_status: AnalysisStatus,
    pub analysis_result: Option<String>,
    pub analysis_error: Option<String>,
}

impl Media {
    pub fn into_domain(self) -> Result<models::Media, DbError> {
        let analysis = build_analysis_state(
            self.analysis_status,
            self.analysis_result,
            self.analysis_error,
        )?;

        Ok(models::Media {
            id: self.id,
            data: models::MediaData {
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
        })
    }
}

// ==================== PageMedia (LEFT JOIN) ====================

#[derive(Debug, FromRow)]
pub struct PageMedia {
    pub source_url: String,
    // Media fields (all optional since LEFT JOIN)
    pub id: Option<MediaId>,
    pub exact_hash: Option<Vec<u8>>,
    pub perceptual_hash: Option<Vec<u8>>,
    pub storage_key: Option<String>,
    pub media_type: Option<MediaType>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub duration_seconds: Option<f32>,
    #[sqlx(json(nullable), rename = "captured_meta")]
    pub captured: Option<UncertainDate>,
    #[sqlx(json(nullable), rename = "location_meta")]
    pub location: Option<chronoscope_core::UncertainLocation<EntityId>>,
    pub source_metadata: Option<String>,
    pub fetched_at: Option<NaiveDateTime>,
    pub created_at: Option<NaiveDateTime>,
    pub analysis_status: Option<AnalysisStatus>,
    pub analysis_result: Option<String>,
    pub analysis_error: Option<String>,
}

impl PageMedia {
    pub fn into_domain(self) -> Result<models::MediaSlot, DbError> {
        let url = self.source_url.clone();
        let resolved = self.try_into_media()?.map(Media::into_domain).transpose()?;

        Ok(models::MediaSlot { url, resolved })
    }

    /// Assemble a [`Media`] row from LEFT JOIN fields.
    /// Returns `Ok(None)` if `id` is absent (unresolved slot).
    /// Returns `Err` if `id` is present but required fields are NULL.
    fn try_into_media(self) -> Result<Option<Media>, DbError> {
        let Some(id) = self.id else {
            return Ok(None);
        };

        let id_clone = id.clone();
        let missing = move |field: &str| {
            DbError::InconsistentRow(format!(
                "media {id_clone} has NULL {field} in page_media join"
            ))
        };

        Ok(Some(Media {
            id,
            exact_hash: self.exact_hash.ok_or_else(|| missing("exact_hash"))?,
            perceptual_hash: self.perceptual_hash,
            storage_key: self.storage_key.ok_or_else(|| missing("storage_key"))?,
            media_type: self.media_type.ok_or_else(|| missing("media_type"))?,
            width: self.width.ok_or_else(|| missing("width"))?,
            height: self.height.ok_or_else(|| missing("height"))?,
            duration_seconds: self.duration_seconds,
            captured: self.captured,
            location: self.location,
            source_metadata: self.source_metadata,
            fetched_at: self.fetched_at.ok_or_else(|| missing("fetched_at"))?,
            created_at: self.created_at.ok_or_else(|| missing("created_at"))?,
            analysis_status: self
                .analysis_status
                .ok_or_else(|| missing("analysis_status"))?,
            analysis_result: self.analysis_result,
            analysis_error: self.analysis_error,
        }))
    }
}

// ==================== EntityMedia (annotation → media join) ====================

#[derive(Debug, FromRow)]
pub struct EntityMedia {
    pub id: MediaId,
    pub storage_key: String,
    pub media_type: MediaType,
    pub width: i32,
    pub height: i32,
    #[sqlx(json(nullable), rename = "captured_meta")]
    pub captured: Option<UncertainDate>,
    #[sqlx(json, rename = "kind_json")]
    pub annotation_kind: chronoscope_core::annotation::AnnotationKind,
    pub source_url: String,
}

impl EntityMedia {
    pub fn into_domain(self) -> models::EntityMedia {
        models::EntityMedia {
            id: self.id,
            storage_key: self.storage_key,
            media_type: self.media_type,
            width: self.width,
            height: self.height,
            captured: self.captured,
            annotation_kind: self.annotation_kind,
            source_url: self.source_url,
        }
    }
}

// ==================== EntityThumbnail (batch thumbnail lookup) ====================

#[derive(Debug, FromRow)]
pub struct EntityThumbnail {
    pub entity_id: EntityId,
    pub storage_key: String,
    pub width: i32,
    pub height: i32,
}

impl EntityThumbnail {
    pub fn into_domain(self) -> models::EntityThumbnail {
        models::EntityThumbnail {
            entity_id: self.entity_id,
            storage_key: self.storage_key,
            width: self.width,
            height: self.height,
        }
    }
}

// ==================== Entity ====================

#[derive(Debug, FromRow)]
pub struct Entity {
    pub id: EntityId,
    pub entity_json: String,
    pub earliest_date: Option<NaiveDateTime>,
    pub latest_date: Option<NaiveDateTime>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

impl Entity {
    pub fn into_domain(self) -> Result<models::Entity, DbError> {
        let entity: chronoscope_core::entity::Entity<EntityId, SourceId> =
            serde_json::from_str(&self.entity_json)?;

        let id = self.id;

        let temporal_bounds = match (self.earliest_date, self.latest_date) {
            (Some(earliest), Some(latest)) => Some(models::DateRange { earliest, latest }),
            (None, None) => None,
            _ => {
                return Err(DbError::InconsistentRow(format!(
                    "entity {} has mismatched temporal shadow columns",
                    id
                )));
            }
        };

        let location = match (self.latitude, self.longitude) {
            (Some(lat), Some(lon)) => Some(models::Coordinates { lat, lon }),
            (None, None) => None,
            _ => {
                return Err(DbError::InconsistentRow(format!(
                    "entity {} has latitude without longitude or vice versa",
                    id
                )));
            }
        };

        Ok(models::Entity {
            id,
            entity,
            temporal_bounds,
            location,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

// ==================== EntityLink ====================

#[derive(Debug, Clone, FromRow)]
pub struct EntityLink {
    pub id: EntityLinkId,
    pub entity_id: EntityId,
    pub link_type: LinkType,
    pub target_json: String,
    /// Shadow column for dedup/display — domain type derives this from `target`.
    #[allow(dead_code)]
    pub target_url: String,
}

impl EntityLink {
    pub fn into_domain(self) -> Result<models::EntityLink, DbError> {
        let target = serde_json::from_str(&self.target_json)?;
        Ok(models::EntityLink {
            id: self.id,
            entity_id: self.entity_id,
            link_type: self.link_type,
            target,
        })
    }
}

// ==================== Annotation ====================

#[derive(Debug, Clone, FromRow)]
pub struct Annotation {
    pub id: AnnotationId,
    pub entity_id: EntityId,
    pub url_id: ResearchUrlId,
    /// Shadow column (generated from `kind_json`) — domain type has the parsed `AnnotationKind`.
    #[allow(dead_code)]
    pub kind: AnnotationKindTag,
    pub kind_json: String,
    pub created_at: NaiveDateTime,
}

impl Annotation {
    pub fn into_domain(self) -> Result<models::Annotation, DbError> {
        let kind = serde_json::from_str(&self.kind_json)?;
        Ok(models::Annotation {
            id: self.id,
            entity_id: self.entity_id,
            url_id: self.url_id,
            kind,
            created_at: self.created_at,
        })
    }
}
