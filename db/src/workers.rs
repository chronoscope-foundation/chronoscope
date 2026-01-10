//! Content creation functions for workers.
//!
//! These functions are used by background workers to create and update
//! pages, media, and research URL status.

use chrono::NaiveDateTime;
use serde::Serialize;

use crate::error::{DbError, DbResult};
use crate::models::{GpsLocation, MediaData, PageData, ResearchUrl};
use crate::types::{MediaId, PageId, ResearchUrlId};
use crate::{Database, now, queries};

/// Helper struct for batch URL insertion JSON payload.
#[derive(Serialize)]
struct UrlEntry {
    id: ResearchUrlId,
    url: String,
}

impl Database {
    /// Create a new page with its media slots.
    ///
    /// This is transactional: either everything succeeds or nothing is committed.
    /// Uses batch queries to minimize round trips:
    /// 1. Insert page row
    /// 2. Batch insert all media URLs (ON CONFLICT DO NOTHING)
    /// 3. Batch insert page_media links (resolves URLs to IDs via join)
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

        // 1. Insert page row
        sqlx::query(queries::CREATE_PAGE.sql)
            .bind(&page_id)
            .bind(data.source_type)
            .bind(&data.title)
            .bind(&data.author)
            .bind(data.published_at)
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
        let url_entries: Vec<UrlEntry> = data
            .media
            .iter()
            .map(|slot| UrlEntry {
                id: ResearchUrlId::generate(),
                url: slot.url.clone(),
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
    /// Media is deduplicated by exact_hash - if media with the same hash exists,
    /// the existing media ID is returned instead of creating a duplicate.
    ///
    /// Uses `ON CONFLICT DO UPDATE SET id = id RETURNING id` to atomically
    /// insert-or-get in a single query. The no-op update makes RETURNING fire
    /// even on conflict. This is fine because if the hash matches exactly,
    /// then (barring bugs or unlikely collisions) all metadata will match too.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn get_or_create_media(&self, data: &MediaData) -> DbResult<MediaId> {
        let id = MediaId::generate();
        let (gps_lat, gps_lon, gps_alt) = GpsLocation::to_columns(data.location.as_ref());

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
            .bind(data.captured_at)
            .bind(gps_lat)
            .bind(gps_lon)
            .bind(gps_alt)
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

    /// Mark a research URL as failed.
    ///
    /// Increments attempt_count and sets retry_after for backoff.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn mark_url_failed(
        &self,
        url_id: &ResearchUrlId,
        error_message: &str,
        retry_after: Option<NaiveDateTime>,
    ) -> DbResult<()> {
        queries::UPDATE_URL_FAILED
            .query()
            .bind(error_message)
            .bind(retry_after)
            .bind(url_id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Claim a batch of URLs for processing.
    ///
    /// Claims up to `batch_size` URLs that are either:
    /// - Pending and not claimed (or with a stale claim)
    /// - Failed but past their retry_after time
    ///
    /// Returns the claimed URLs, already marked as 'analyzing'.
    /// Uses optimistic locking - claims expire after `stale_threshold`.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn claim_urls(
        &self,
        worker_id: &str,
        batch_size: u32,
        stale_threshold: NaiveDateTime,
    ) -> DbResult<Vec<ResearchUrl>> {
        let now = now();

        let urls = sqlx::query_as(queries::CLAIM_URLS.sql)
            .bind(now) // ?1 = now (for claimed_at)
            .bind(worker_id) // ?2 = worker_id
            .bind(stale_threshold) // ?3 = stale_threshold
            .bind(now) // ?4 = now (for retry_after comparison)
            .bind(batch_size) // ?5 = batch_size
            .fetch_all(&self.pool)
            .await?;

        Ok(urls)
    }
}
