//! Content creation functions for workers.
//!
//! These functions are used by background workers to create and update
//! pages, media, and research URL status.

use serde::Serialize;
use sqlx::FromRow;
use sqlx::types::Json;

use chronoscope_core::{UncertainDate, UncertainLocation};

use crate::error::{DbError, DbResult};
use crate::models::{MediaData, PageData};
use crate::types::{MediaId, PageId, ResearchUrlId};
use crate::{Database, now, queries};

/// Media item claimed for analysis.
#[derive(Debug, Clone, FromRow)]
pub struct MediaForAnalysis {
    /// Media ID
    pub id: MediaId,
    /// Storage key for retrieving the media bytes
    pub storage_key: String,
    /// Number of previous analysis attempts
    pub analysis_attempt_count: i32,
}

#[cfg(test)]
mod tests;

/// Helper struct for batch URL insertion JSON payload.
#[derive(Serialize)]
struct UrlEntry {
    id: ResearchUrlId,
    url: String,
    /// Worker affinity (null for generic URLs).
    affinity: Option<String>,
}

impl Database {
    /// Create a new page with its media slots.
    ///
    /// This always creates a new page - there's no deduplication. Each URL gets its
    /// own page record, even if the content is identical to another page. This is
    /// intentional: pages often have variable content (timestamps, ads, trackers)
    /// that makes content-based deduplication unreliable, and preserving the 1:1
    /// URL→page relationship maintains clear provenance.
    ///
    /// Compare with [`get_or_create_media`], which *does* deduplicate by content hash
    /// since identical images/videos are truly identical regardless of source URL.
    ///
    /// After creating a page, call [`mark_url_resolved_to_page`] to link the
    /// research URL to this page.
    ///
    /// This is transactional: either everything succeeds or nothing is committed.
    /// Uses batch queries to minimize round trips:
    /// 1. Insert page row
    /// 2. Batch insert all media URLs (ON CONFLICT DO NOTHING)
    /// 3. Batch insert `page_media` links (resolves URLs to IDs via join)
    ///
    /// # Errors
    /// Returns `DbError::InvalidArgument` if any media slots have pre-resolved media,
    /// or `DbError::Sqlx` if the database operation fails.
    pub async fn create_page(&self, data: &PageData) -> DbResult<PageId> {
        if !data.media.iter().all(|slot| slot.resolved.is_none()) {
            return Err(DbError::InvalidArgument(
                "create_page received pre-resolved media - this is a logic error".to_string(),
            ));
        }

        let page_id = PageId::generate();
        let timestamp = now();

        let mut tx = self.pool.begin().await?;

        // 1. Insert page row (shadow columns for indexing + JSON meta as source of truth)
        sqlx::query(queries::CREATE_PAGE.sql)
            .bind(&page_id)
            .bind(data.source_type)
            .bind(&data.title)
            .bind(&data.author)
            .bind(date_earliest(&data.published))
            .bind(date_latest(&data.published))
            .bind(data.published.as_ref().map(Json))
            .bind(&data.content)
            .bind(data.fetched_at)
            .bind(timestamp)
            .execute(&mut *tx)
            .await?;

        if data.media.is_empty() {
            tx.commit().await?;
            return Ok(page_id);
        }

        // 2. Batch insert all media URLs (generates IDs, ignores existing)
        // Media URLs discovered from pages get NULL affinity (generic worker)
        let url_entries: Vec<UrlEntry> = data
            .media
            .iter()
            .map(|slot| UrlEntry {
                id: ResearchUrlId::generate(),
                url: slot.url.clone(),
                affinity: None,
            })
            .collect();
        let urls_json = serde_json::to_string(&url_entries)?;

        sqlx::query(queries::CREATE_URLS_BATCH.sql)
            .bind(timestamp)
            .bind(&urls_json)
            .execute(&mut *tx)
            .await?;

        // 3. Batch insert page_media links (resolves URLs to IDs via join)
        let urls: Vec<&str> = data.media.iter().map(|s| s.url.as_str()).collect();
        let urls_array_json = serde_json::to_string(&urls)?;

        sqlx::query(queries::CREATE_PAGE_MEDIA_BATCH.sql)
            .bind(&page_id)
            .bind(&urls_array_json)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(page_id)
    }

    /// Create new media or return existing if hash matches.
    ///
    /// Unlike [`create_page`], this *does* deduplicate: if media with the same
    /// `exact_hash` already exists, the existing media ID is returned instead of
    /// creating a duplicate. This makes sense for media because identical bytes
    /// are truly identical regardless of which URL they came from - the same
    /// photo syndicated across 10 sites should be stored once.
    ///
    /// This means multiple research URLs can resolve to the same media record
    /// (N:1 relationship), whereas pages are always 1:1 with their source URL.
    ///
    /// After getting/creating media, call [`mark_url_resolved_to_media`] to link
    /// the research URL to this media.
    ///
    /// Uses `ON CONFLICT DO UPDATE SET id = id RETURNING id` to atomically
    /// insert-or-get in a single query. The no-op update makes RETURNING fire
    /// even on conflict.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn get_or_create_media(&self, data: &MediaData) -> DbResult<MediaId> {
        let id = MediaId::generate();

        let (lat, lon) = location_coords(&data.location);

        // Single atomic query: insert or return existing ID
        let (returned_id,): (MediaId,) = sqlx::query_as(queries::CREATE_MEDIA.sql)
            .bind(&id)
            .bind(&data.exact_hash)
            .bind(&data.perceptual_hash)
            .bind(&data.storage_key)
            .bind(data.media_type)
            .bind(data.width)
            .bind(data.height)
            .bind(data.duration_seconds)
            .bind(date_earliest(&data.captured))
            .bind(date_latest(&data.captured))
            .bind(data.captured.as_ref().map(Json))
            .bind(lat)
            .bind(lon)
            .bind(data.location.as_ref().map(Json))
            .bind(&data.source_metadata)
            .bind(data.fetched_at)
            .bind(now())
            .fetch_one(&self.pool)
            .await?;

        Ok(returned_id)
    }

    /// Mark a research URL as resolved to a page.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn mark_url_resolved_to_page(
        &self,
        url_id: &ResearchUrlId,
        page_id: &PageId,
    ) -> DbResult<()> {
        queries::UPDATE_URL_RESOLVED_PAGE
            .query()
            .bind(page_id)
            .bind(url_id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Mark a research URL as resolved to direct media.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn mark_url_resolved_to_media(
        &self,
        url_id: &ResearchUrlId,
        media_id: &MediaId,
    ) -> DbResult<()> {
        queries::UPDATE_URL_RESOLVED_MEDIA
            .query()
            .bind(media_id)
            .bind(url_id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Mark a media item's analysis as complete with results.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn mark_analysis_complete(
        &self,
        media_id: &MediaId,
        analysis_result: &str,
    ) -> DbResult<()> {
        queries::UPDATE_ANALYSIS_COMPLETE
            .query()
            .bind(media_id)
            .bind(analysis_result)
            .execute(&self.pool)
            .await?;

        Ok(())
    }
}

/// Format a date's earliest bound as an ISO 8601 TEXT shadow column value.
fn date_earliest(date: &Option<UncertainDate>) -> Option<String> {
    date.as_ref()
        .map(|d| d.earliest().format("%Y-%m-%dT%H:%M:%S").to_string())
}

/// Format a date's latest bound as an ISO 8601 TEXT shadow column value.
fn date_latest(date: &Option<UncertainDate>) -> Option<String> {
    date.as_ref()
        .map(|d| d.latest().format("%Y-%m-%dT%H:%M:%S").to_string())
}

/// Extract lat/lon shadow columns from an optional location.
fn location_coords(loc: &Option<UncertainLocation>) -> (Option<f64>, Option<f64>) {
    match loc {
        Some(UncertainLocation::Coordinates { lat, lon, .. }) => (Some(*lat), Some(*lon)),
        _ => (None, None), // TODO: geocode non-coordinate location variants
    }
}
