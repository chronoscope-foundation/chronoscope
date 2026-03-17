//! Query definitions for Chronoscope.
//!
//! All database queries are defined here to enable:
//! - Centralized query management
//! - Startup verification that all queries use indexes (no full table scans)
//!
//! The startup check is critical because SQLite indexes don't guarantee
//! Postgres indexes exist - we need runtime validation in each environment.

use sqlx::{Sqlite, SqlitePool, query::Query, sqlite::SqliteArguments};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum QueryPlanError {
    #[error("Query '{name}' has full table scan: {detail}\nSQL: {sql}")]
    FullTableScan {
        name: String,
        sql: String,
        detail: String,
    },

    #[error("Failed to explain query '{name}': {source}")]
    ExplainFailed {
        name: String,
        #[source]
        source: sqlx::Error,
    },
}

/// A query definition that can be both executed and explained.
pub struct QueryDef {
    pub name: &'static str,
    pub sql: &'static str,
}

impl QueryDef {
    /// Create a sqlx Query from this definition, ready for binding parameters.
    pub fn query(&self) -> Query<'_, Sqlite, SqliteArguments<'_>> {
        sqlx::query(self.sql)
    }
}

/// Verify that all static queries use indexes (no full table scans).
///
/// This should be called on startup to catch missing indexes early.
/// SQLite and Postgres have different indexes, so this check must run
/// in each environment - the test suite alone isn't sufficient.
///
/// # Errors
/// Returns `QueryPlanError` if any query would cause a full table scan.
pub async fn verify_all_query_plans(pool: &SqlitePool) -> Result<(), QueryPlanError> {
    // TODO: When we add Postgres support, this will need to branch on DB type.
    // Postgres uses `EXPLAIN` with different output format.
    for query_def in ALL {
        verify_query_plan_sql(pool, query_def.name, query_def.sql).await?;
    }
    Ok(())
}

/// Verify a SQL query uses indexes (no full table scans).
///
/// Used both for static `QueryDef`s and dynamically-generated queue SQL.
///
/// # Errors
/// Returns `QueryPlanError` if the query would cause a full table scan.
pub async fn verify_query_plan_sql(
    pool: &SqlitePool,
    name: &str,
    sql: &str,
) -> Result<(), QueryPlanError> {
    let explain_sql = format!("EXPLAIN QUERY PLAN {sql}");

    let plan: Vec<(i32, i32, i32, String)> = sqlx::query_as(&explain_sql)
        .fetch_all(pool)
        .await
        .map_err(|e| QueryPlanError::ExplainFailed {
            name: name.to_string(),
            source: e,
        })?;

    for (_, _, _, detail) in plan {
        // SCAN without an index means full table scan
        // SCAN ... USING INDEX is fine (it's an index scan)
        // SEARCH is always fine (it's an index lookup)
        // SCAN (subquery-N) is fine (scanning materialized subquery result)
        // SCAN ... VIRTUAL TABLE is fine (table-valued function like json_each)
        if is_full_table_scan(&detail) {
            return Err(QueryPlanError::FullTableScan {
                name: name.to_string(),
                sql: sql.to_string(),
                detail,
            });
        }
    }

    Ok(())
}

/// Check if an EXPLAIN QUERY PLAN detail line indicates a full table scan.
fn is_full_table_scan(detail: &str) -> bool {
    detail.starts_with("SCAN ")
        && !detail.contains("USING")
        && !detail.contains("(subquery")
        && !detail.contains("VIRTUAL TABLE")
}

macro_rules! define_queries {
    ($($name:ident: $sql:literal),* $(,)?) => {
        $(pub const $name: QueryDef = QueryDef { name: stringify!($name), sql: $sql };)*

        /// All queries in the system. Used by tests to verify query plans.
        pub const ALL: &[&QueryDef] = &[$(&$name),*];
    };
}

define_queries! {
    // Users (created_at must be provided - no defaults)
    CREATE_USER: "INSERT INTO users (id, username, email, created_at) VALUES (?, ?, ?, ?)",
    FIND_USER_BY_IDENTIFIER: "SELECT id FROM users WHERE username = ? OR email = ?",
    UPDATE_USERNAME: "UPDATE users SET username = ? WHERE id = ?",
    UPDATE_EMAIL: "UPDATE users SET email = ? WHERE id = ?",
    GET_USER: "SELECT id, username, email, created_at FROM users WHERE id = ?",

    // Credentials (created_at must be provided - no defaults)
    ADD_CREDENTIAL: "INSERT INTO credentials (user_id, credential_id, passkey_json, created_at) VALUES (?, ?, ?, ?)",
    GET_CREDENTIALS: "SELECT passkey_json FROM credentials WHERE user_id = ?",
    UPDATE_CREDENTIAL: "UPDATE credentials SET passkey_json = ? WHERE user_id = ? AND credential_id = ?",

    // Research URLs (all required fields must be provided - no defaults)
    GET_URL_BY_URL: "SELECT id FROM research_urls WHERE url = ?",
    CREATE_URL: "INSERT INTO research_urls (id, url, status, attempt_count, worker_affinity, created_at) VALUES (?, ?, ?, ?, ?, ?)",
    // Used when adding media URLs from a page - inserts new URL or ignores if exists
    CREATE_URL_OR_IGNORE: "INSERT INTO research_urls (id, url, status, attempt_count, worker_affinity, created_at) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(url) DO NOTHING",
    // Batch insert URLs from JSON array of {id, url, affinity} objects. Params: ?1=created_at, ?2=JSON array
    // Uses INSERT OR IGNORE because SQLite's upsert clause (ON CONFLICT...DO) only works with VALUES, not SELECT.
    CREATE_URLS_BATCH: "
        INSERT OR IGNORE INTO research_urls (id, url, status, attempt_count, worker_affinity, created_at)
        SELECT json_extract(value, '$.id'), json_extract(value, '$.url'), 'pending', 0, json_extract(value, '$.affinity'), ?1
        FROM json_each(?2)
    ",
    GET_URL_BY_ID: "SELECT id, url, page_id, media_id, status, attempt_count, worker_affinity, created_at FROM research_urls WHERE id = ?",
    // Keyset pagination: first page (no cursor)
    LIST_ALL_URLS_FIRST: "SELECT id, url, page_id, media_id, status, attempt_count, worker_affinity, created_at FROM research_urls ORDER BY created_at DESC, id DESC LIMIT ?",
    // Keyset pagination: subsequent pages (cursor = created_at, id of last item)
    LIST_ALL_URLS_PAGE: "SELECT id, url, page_id, media_id, status, attempt_count, worker_affinity, created_at FROM research_urls WHERE (created_at, id) < (?, ?) ORDER BY created_at DESC, id DESC LIMIT ?",

    // Follows (created_at must be provided - no defaults)
    CREATE_FOLLOW: "INSERT INTO follows (user_id, url_id, created_at) VALUES (?, ?, ?) ON CONFLICT DO NOTHING",
    // Keyset pagination: first page (no cursor)
    LIST_FOLLOWED_URLS_FIRST: "SELECT r.id, r.url, r.page_id, r.media_id, r.status, r.attempt_count, r.worker_affinity, r.created_at, f.created_at as followed_at FROM research_urls r JOIN follows f ON f.url_id = r.id WHERE f.user_id = ? ORDER BY f.created_at DESC, r.id DESC LIMIT ?",
    // Keyset pagination: subsequent pages (cursor = followed_at, url_id of last item)
    LIST_FOLLOWED_URLS_PAGE: "SELECT r.id, r.url, r.page_id, r.media_id, r.status, r.attempt_count, r.worker_affinity, r.created_at, f.created_at as followed_at FROM research_urls r JOIN follows f ON f.url_id = r.id WHERE f.user_id = ? AND (f.created_at, r.id) < (?, ?) ORDER BY f.created_at DESC, r.id DESC LIMIT ?",
    GET_FOLLOW_TIMESTAMP: "SELECT created_at FROM follows WHERE user_id = ? AND url_id = ?",
    DELETE_FOLLOW: "DELETE FROM follows WHERE user_id = ? AND url_id = ?",

    // Pages
    GET_PAGE_BY_ID: "SELECT id, source_type, title, author, published_meta, content, fetched_at, created_at FROM pages WHERE id = ?",
    CREATE_PAGE: "INSERT INTO pages (id, source_type, title, author, published_earliest, published_latest, published_meta, content, fetched_at, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",

    // Media
    GET_MEDIA_BY_ID: "SELECT id, exact_hash, perceptual_hash, storage_key, media_type, width, height, duration_seconds, captured_meta, location_meta, source_metadata, fetched_at, created_at, analysis_status, analysis_result, analysis_error FROM media WHERE id = ?",
    // Uses ON CONFLICT DO UPDATE SET id = id to make RETURNING work even on conflict.
    // This is a no-op update that allows us to get the ID in a single atomic query.
    CREATE_MEDIA: "INSERT INTO media (id, exact_hash, perceptual_hash, storage_key, media_type, width, height, duration_seconds, captured_earliest, captured_latest, captured_meta, latitude, longitude, location_meta, source_metadata, fetched_at, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(exact_hash) DO UPDATE SET id = id RETURNING id",

    // Page media items (ordered by source_order, with optional resolved media)
    // Returns source_url from research_urls, plus media fields if resolved (NULL if pending)
    GET_PAGE_MEDIA: "SELECT r.url as source_url, m.id, m.exact_hash, m.perceptual_hash, m.storage_key, m.media_type, m.width, m.height, m.duration_seconds, m.captured_meta, m.location_meta, m.source_metadata, m.fetched_at, m.created_at, m.analysis_status, m.analysis_result, m.analysis_error FROM page_media pm JOIN research_urls r ON pm.url_id = r.id LEFT JOIN media m ON r.media_id = m.id WHERE pm.page_id = ? ORDER BY pm.source_order",
    // Batch insert page_media from JSON array of URLs. Resolves URLs to IDs via join.
    // Uses json_each key as source_order to preserve array ordering.
    // Params: ?1=page_id, ?2=JSON array of URL strings
    CREATE_PAGE_MEDIA_BATCH: "
        INSERT INTO page_media (page_id, url_id, source_order)
        SELECT ?1, ru.id, je.key
        FROM json_each(?2) je
        JOIN research_urls ru ON ru.url = je.value
    ",

    // Research URL status updates (for workers)
    // Note: Queue claim and mark_failed queries are now dynamically generated
    // by the Queue<T> abstraction in db/src/queue.rs
    UPDATE_URL_RESOLVED_PAGE: "UPDATE research_urls SET page_id = ?, status = 'complete', claimed_at = NULL, claimed_by = NULL WHERE id = ?",
    UPDATE_URL_RESOLVED_MEDIA: "UPDATE research_urls SET media_id = ?, status = 'complete', claimed_at = NULL, claimed_by = NULL WHERE id = ?",

    // Mark analysis complete with results
    UPDATE_ANALYSIS_COMPLETE: "
        UPDATE media
        SET analysis_status = 'complete',
            analysis_claimed_at = NULL,
            analysis_claimed_by = NULL,
            analysis_result = ?2
        WHERE id = ?1
    ",

    // ==================== Entities ====================

    INSERT_ENTITY: "
        INSERT INTO entities (id, entity_json, earliest_date, latest_date, latitude, longitude, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
    ",

    // External IDs
    INSERT_EXTERNAL_ID: "INSERT INTO entity_external_ids (id_type, external_id, entity_id) VALUES (?, ?, ?)",

    // Entity links
    INSERT_ENTITY_LINK: "INSERT OR IGNORE INTO entity_links (id, entity_id, link_type, target_url) VALUES (?, ?, ?, ?)",

    // Entity relations
    INSERT_ENTITY_RELATION: "INSERT INTO entity_relations (from_entity_id, to_entity_id, relation_type, evidence_json) VALUES (?, ?, ?, ?)",

    // Annotations
    INSERT_ANNOTATION: "INSERT OR IGNORE INTO annotations (id, entity_id, url_id, kind_json, created_at) VALUES (?, ?, ?, ?, ?)",

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    #[tokio::test]
    async fn test_all_queries_use_indexes() -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    {
        let db = Database::new_without_plan_verification("sqlite::memory:").await?;
        verify_all_query_plans(db.pool()).await?;
        Ok(())
    }
}
