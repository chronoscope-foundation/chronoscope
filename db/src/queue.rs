//! Unified work queue abstraction.
//!
//! This module provides a generic `Queue<T>` that generates SQL at construction time
//! based on configuration. It handles the common queue operations (`claim`, `mark_failed`)
//! while allowing different item types and table structures.
//!
//! # Design
//!
//! - `Queue<T>` - Generic struct owning generated SQL and pool reference
//! - `QueueItem` trait - Implemented by item types to extract ID and attempt count
//! - `QueueQueries` trait - Type-erased trait for EXPLAIN verification
//! - `QueueConfig` - Configuration for SQL generation

use std::marker::PhantomData;
use std::sync::LazyLock;

use chrono::NaiveDateTime;
use sqlx::{FromRow, SqlitePool};

use chronoscope_integrations::IntegrationName;

use crate::error::DbError;

/// Trait for queue items - extracts ID and attempt count.
///
/// Implemented by types that can be claimed from a work queue.
pub trait QueueItem: for<'r> FromRow<'r, sqlx::sqlite::SqliteRow> + Send + Unpin {
    /// The type of item identifiers.
    type Id: std::fmt::Display
        + Send
        + Sync
        + Clone
        + for<'q> sqlx::Encode<'q, sqlx::Sqlite>
        + sqlx::Type<sqlx::Sqlite>;

    /// Get the item's ID.
    fn id(&self) -> Self::Id;

    /// Get the attempt count for retry logic.
    fn attempt_count(&self) -> i32;
}

/// Trait for type-erased query access (for EXPLAIN verification).
///
/// This allows iterating over all queues at startup to verify their
/// generated SQL uses indexes properly.
pub trait QueueQueries: Send + Sync {
    /// Get all queries for verification as (name, sql) pairs.
    fn queries(&self) -> Vec<(&str, &str)>;
}

/// Configuration for a work queue.
///
/// Used to generate SQL at construction time. Column names are derived
/// from the `col_prefix` (e.g., prefix `analysis_` gives `analysis_status`).
pub struct QueueConfig {
    /// Queue name for logging and verification messages.
    pub name: &'static str,
    /// Database table name.
    pub table: &'static str,
    /// Column name prefix (e.g., "" or "analysis_").
    pub col_prefix: &'static str,
    /// Additional WHERE filter (e.g., `worker_affinity IS NULL`, `media_type = 'image'`).
    pub extra_filter: &'static str,
    /// Columns to return from RETURNING clause.
    pub returning_cols: &'static str,
}

/// A typed work queue with generated SQL.
///
/// The SQL is generated at construction time based on the config, allowing
/// different table structures and filters while sharing the same claim/fail logic.
pub struct Queue<T> {
    name: String,
    pool: SqlitePool,
    claim_sql: String,
    mark_failed_sql: String,
    /// In-process worker-progress sender, mirrored from the owning `Database`
    /// so `mark_failed` can tick alongside the success-side `mark_url_resolved_*`
    /// methods. Gated behind `test-support` (see `Database::worker_progress`
    /// for the multi-instance caveat).
    #[cfg(any(test, feature = "test-support"))]
    worker_progress_tx: tokio::sync::watch::Sender<u64>,
    _phantom: PhantomData<T>,
}

impl<T> Queue<T> {
    /// Create a new queue with generated SQL. Under `test-support`, the
    /// caller passes the owning `Database`'s worker-progress sender so
    /// `mark_failed` can tick alongside the success-side notifications.
    ///
    /// `pub(crate)` so external callers must go through `Database::new` /
    /// `Database::make_url_queue` — that's the only path that wires the pool
    /// and worker-progress sender consistently.
    #[must_use]
    pub(crate) fn new(
        pool: SqlitePool,
        config: &QueueConfig,
        #[cfg(any(test, feature = "test-support"))] worker_progress_tx: tokio::sync::watch::Sender<
            u64,
        >,
    ) -> Self {
        let claim_sql = Self::generate_claim_sql(config);
        let mark_failed_sql = Self::generate_mark_failed_sql(config);

        Self {
            name: config.name.to_string(),
            pool,
            claim_sql,
            mark_failed_sql,
            #[cfg(any(test, feature = "test-support"))]
            worker_progress_tx,
            _phantom: PhantomData,
        }
    }

    /// Get the queue name (for logging).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    fn generate_claim_sql(config: &QueueConfig) -> String {
        let p = config.col_prefix;
        format!(
            r#"
            UPDATE {table}
            SET {p}status = 'processing', {p}claimed_at = ?1, {p}claimed_by = ?2
            WHERE id IN (
                SELECT id FROM (
                    SELECT id, {p}retry_after, created_at FROM {table}
                    WHERE {p}status = 'pending' AND {extra_filter} AND ({p}claimed_at IS NULL OR {p}claimed_at < ?3)
                    UNION ALL
                    SELECT id, {p}retry_after, created_at FROM {table}
                    WHERE {p}status = 'processing' AND {extra_filter} AND {p}claimed_at < ?3
                    UNION ALL
                    SELECT id, {p}retry_after, created_at FROM {table}
                    WHERE {p}status = 'failed' AND {extra_filter} AND {p}retry_after IS NOT NULL AND {p}retry_after <= ?4
                )
                ORDER BY {p}retry_after NULLS FIRST, created_at
                LIMIT ?5
            )
            RETURNING {returning_cols}
            "#,
            table = config.table,
            p = p,
            extra_filter = config.extra_filter,
            returning_cols = config.returning_cols,
        )
    }

    fn generate_mark_failed_sql(config: &QueueConfig) -> String {
        let p = config.col_prefix;
        format!(
            r#"
            UPDATE {table}
            SET {p}status = 'failed',
                {p}error = ?2,
                {p}attempt_count = {p}attempt_count + 1,
                {p}retry_after = ?3,
                {p}claimed_at = NULL,
                {p}claimed_by = NULL
            WHERE id = ?1
            "#,
            table = config.table,
            p = p,
        )
    }
}

impl<T: QueueItem> Queue<T> {
    /// Claim a batch of items for processing.
    ///
    /// Claims up to `batch_size` items that are either:
    /// - Pending and not claimed (or with a stale claim)
    /// - Processing but claim is stale (worker died)
    /// - Failed but past their `retry_after` time
    ///
    /// Returns the claimed items, already marked as 'processing'.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn claim(
        &self,
        worker_id: &str,
        batch_size: u32,
        stale_cutoff: NaiveDateTime,
    ) -> Result<Vec<T>, DbError> {
        let now = crate::now();
        let items = sqlx::query_as(&self.claim_sql)
            .bind(now) // ?1 claimed_at
            .bind(worker_id) // ?2 claimed_by
            .bind(stale_cutoff) // ?3 stale cutoff
            .bind(now) // ?4 retry_after comparison
            .bind(batch_size) // ?5 limit
            .fetch_all(&self.pool)
            .await?;
        Ok(items)
    }

    /// Mark an item as failed with optional retry.
    ///
    /// Increments `attempt_count` and sets `retry_after` for backoff.
    /// Pass `None` for `retry_after` for permanent failure (no retry).
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn mark_failed(
        &self,
        item_id: &T::Id,
        error: &str,
        retry_after: Option<NaiveDateTime>,
    ) -> Result<(), DbError> {
        sqlx::query(&self.mark_failed_sql)
            .bind(item_id) // ?1
            .bind(error) // ?2
            .bind(retry_after) // ?3
            .execute(&self.pool)
            .await?;
        self.notify_worker_progress();
        Ok(())
    }

    /// Bump the in-process worker-progress counter. No-op without `test-support`.
    #[cfg_attr(
        not(any(test, feature = "test-support")),
        expect(
            clippy::unused_self,
            reason = "self carries worker_progress_tx, used under test/test-support; &self keeps one uniform signature across cfgs (callers do self.notify_worker_progress())"
        )
    )]
    fn notify_worker_progress(&self) {
        #[cfg(any(test, feature = "test-support"))]
        // wrapping: subscribers only care about changedness, not the value;
        // u64 won't realistically wrap in a test process anyway.
        self.worker_progress_tx
            .send_modify(|n| *n = n.wrapping_add(1));
    }
}

impl<T: Send + Sync> QueueQueries for Queue<T> {
    fn queries(&self) -> Vec<(&str, &str)> {
        vec![
            ("claim", &self.claim_sql),
            ("mark_failed", &self.mark_failed_sql),
        ]
    }
}

// ==================== Queue Configurations ====================

/// Queue config for media analysis (images only).
pub const ANALYSIS_QUEUE: QueueConfig = QueueConfig {
    name: "analysis",
    table: "media",
    col_prefix: "analysis_",
    extra_filter: "media_type = 'image'",
    returning_cols: "id, storage_key, analysis_attempt_count",
};

// ==================== QueueItem Implementations ====================

use crate::models::ResearchUrl;
use crate::types::{MediaId, ResearchUrlId};
use crate::workers::MediaForAnalysis;

impl QueueItem for ResearchUrl {
    type Id = ResearchUrlId;

    fn id(&self) -> Self::Id {
        self.id.clone()
    }

    fn attempt_count(&self) -> i32 {
        self.attempt_count
    }
}

impl QueueItem for MediaForAnalysis {
    type Id = MediaId;

    fn id(&self) -> Self::Id {
        self.id.clone()
    }

    fn attempt_count(&self) -> i32 {
        self.analysis_attempt_count
    }
}

// ==================== URL Queue Configurations ====================

/// Create a URL queue config. Leaks strings for dynamic parts to get static references.
fn make_url_config(affinity: Option<IntegrationName>) -> QueueConfig {
    let (name_static, filter_static): (&'static str, &'static str) = match affinity {
        None => ("url_generic", "worker_affinity IS NULL"),
        Some(integration) => {
            let affinity_str: &str = integration.as_ref();
            let filter = format!("worker_affinity = '{affinity_str}'");
            let name = format!("url_{affinity_str}");
            (
                Box::leak(name.into_boxed_str()),
                Box::leak(filter.into_boxed_str()),
            )
        }
    };

    QueueConfig {
        name: name_static,
        table: "research_urls",
        col_prefix: "",
        extra_filter: filter_static,
        returning_cols: "id, url, page_id, media_id, status, attempt_count, worker_affinity, created_at",
    }
}

/// Cached URL queue configs. Each is initialized lazily on first access.
static GENERIC_URL_CONFIG: LazyLock<QueueConfig> = LazyLock::new(|| make_url_config(None));
static REDDIT_URL_CONFIG: LazyLock<QueueConfig> =
    LazyLock::new(|| make_url_config(Some(IntegrationName::Reddit)));
static INSTAGRAM_URL_CONFIG: LazyLock<QueueConfig> =
    LazyLock::new(|| make_url_config(Some(IntegrationName::Instagram)));

/// Get the URL queue config for a worker affinity.
///
/// - `None` returns the generic queue config (`worker_affinity IS NULL`)
/// - `Some(integration)` returns the config for that specific integration
///
/// Returns a reference to a cached config. Configs are created once
/// at first access and reused for the lifetime of the program.
#[must_use]
pub fn url_queue_config(affinity: Option<IntegrationName>) -> &'static QueueConfig {
    match affinity {
        None | Some(IntegrationName::Generic) => &GENERIC_URL_CONFIG,
        Some(IntegrationName::Reddit) => &REDDIT_URL_CONFIG,
        Some(IntegrationName::Instagram) => &INSTAGRAM_URL_CONFIG,
    }
}
