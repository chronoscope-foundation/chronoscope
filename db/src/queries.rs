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

    #[error("Failed to list schema relations while verifying query '{name}': {source}")]
    SchemaLookupFailed {
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
    verify_query_defs(pool, ALL).await
}

/// Verify a list of query definitions — the shared loop under every
/// `QueryDef` collection's verifier.
pub(crate) async fn verify_query_defs(
    pool: &SqlitePool,
    defs: &[&QueryDef],
) -> Result<(), QueryPlanError> {
    for query_def in defs {
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

    // CTE relations the plan itself declares (`MATERIALIZE name` /
    // `CO-ROUTINE name`). A later `SCAN name` of one of these is a bounded
    // subquery scan — the CTE's own steps were verified as plan lines of
    // their own — not a table scan.
    let mut cte_names: std::collections::HashSet<&str> = plan
        .iter()
        .filter_map(|(_, _, _, detail)| {
            detail
                .strip_prefix("MATERIALIZE ")
                .or_else(|| detail.strip_prefix("CO-ROUTINE "))
        })
        .collect();

    // A CTE named after a real relation gets no waiver: the plan renders a
    // full scan of the table and the bounded CTE scan as the same
    // `SCAN <name>` line (a subquery-scoped CTE leaves the outer name bound
    // to the table), so the waiver could accept a real table scan. Names
    // compare lowercased — SQLite resolves them case-insensitively.
    if !cte_names.is_empty() {
        let relations =
            schema_relation_names(pool)
                .await
                .map_err(|e| QueryPlanError::SchemaLookupFailed {
                    name: name.to_string(),
                    source: e,
                })?;
        cte_names.retain(|cte| !relations.contains(&cte.to_lowercase()));
    }

    // A `SCAN ... USING COVERING INDEX` line is judged by whether the index
    // is partial, so fetch the live partial-index names when one appears.
    let partial_indexes = if plan
        .iter()
        .any(|(_, _, _, detail)| detail.contains(" USING COVERING INDEX "))
    {
        partial_index_names(pool)
            .await
            .map_err(|e| QueryPlanError::SchemaLookupFailed {
                name: name.to_string(),
                source: e,
            })?
    } else {
        std::collections::HashSet::new()
    };

    for (_, _, _, detail) in &plan {
        // SCAN without an index means full table scan
        // SCAN ... USING INDEX is fine (it's an index scan)
        // SEARCH is always fine (it's an index lookup)
        // SCAN (subquery-N) is fine (scanning materialized subquery result)
        // SCAN ... VIRTUAL TABLE is fine (table-valued function like json_each)
        if is_full_table_scan(detail, &cte_names, &partial_indexes) {
            return Err(QueryPlanError::FullTableScan {
                name: name.to_string(),
                sql: sql.to_string(),
                detail: detail.clone(),
            });
        }
    }

    Ok(())
}

/// Check if an EXPLAIN QUERY PLAN detail line indicates a full table scan.
///
/// "USING" waives most `SCAN` lines even though an index scan still reads
/// every index row — the top-N-by-`ORDER BY` idiom depends on that (the scan
/// stops at the LIMIT). A covering-index scan gets no such benefit of the
/// doubt: nothing bounds it, so it counts as a full scan unless the index is
/// partial and therefore holds only the rows its predicate admits (the
/// unclaimed-staging audit probes one that is empty in committed state).
fn is_full_table_scan(
    detail: &str,
    cte_names: &std::collections::HashSet<&str>,
    partial_indexes: &std::collections::HashSet<String>,
) -> bool {
    let Some(scanned) = detail.strip_prefix("SCAN ") else {
        return false;
    };
    // A recursive CTE's initial `SELECT <constants>` step plans as a
    // constant-row scan — one synthesized row, no table behind it.
    if scanned == "CONSTANT ROW" {
        return false;
    }
    // Scanning a relation this same plan declared as a CTE is a bounded
    // subquery scan (fact-store recursive CTEs reference their tables
    // unaliased so the names line up).
    if cte_names.contains(scanned) {
        return false;
    }
    if let Some(index) = scanned.split(" USING COVERING INDEX ").nth(1) {
        return !partial_indexes.contains(&index.to_lowercase());
    }
    !detail.contains("USING")
        && !detail.contains("(subquery")
        && !detail.contains("VIRTUAL TABLE")
        // LEFT-JOIN scans are expected when joining a materialized CTE
        // (e.g., ranked_reps) — the CTE is a small temp table without
        // indexes, and scanning it is the correct plan.
        //
        // WARNING: This blanket exclusion could hide a real full-table LEFT-JOIN
        // scan on a large table (not a CTE). If new LEFT JOINs are added against
        // real tables, verify manually with EXPLAIN QUERY PLAN that the scan is
        // bounded. Currently the only LEFT-JOIN scans in practice are on
        // `ranked_reps` (small CTE) and `region_centroids` (small CTE).
        && !detail.contains("LEFT-JOIN")
}

/// Every table and view name in the live schema, across all attached
/// databases, lowercased.
async fn schema_relation_names(
    pool: &SqlitePool,
) -> Result<std::collections::HashSet<String>, sqlx::Error> {
    let schemas: Vec<(i64, String, Option<String>)> = sqlx::query_as("PRAGMA database_list")
        .fetch_all(pool)
        .await?;
    let mut names = std::collections::HashSet::new();
    for (_, schema, _) in schemas {
        let quoted = schema.replace('"', "\"\"");
        let rows: Vec<(String,)> = sqlx::query_as(&format!(
            "SELECT name FROM \"{quoted}\".sqlite_master WHERE type IN ('table', 'view')"
        ))
        .fetch_all(pool)
        .await?;
        names.extend(rows.into_iter().map(|(relation,)| relation.to_lowercase()));
    }
    Ok(names)
}

/// Every partial index name in the live schema, across all attached
/// databases, lowercased.
async fn partial_index_names(
    pool: &SqlitePool,
) -> Result<std::collections::HashSet<String>, sqlx::Error> {
    let schemas: Vec<(i64, String, Option<String>)> = sqlx::query_as("PRAGMA database_list")
        .fetch_all(pool)
        .await?;
    let mut names = std::collections::HashSet::new();
    for (_, schema, _) in schemas {
        let dquoted = schema.replace('"', "\"\"");
        let squoted = schema.replace('\'', "''");
        let rows: Vec<(String,)> = sqlx::query_as(&format!(
            "SELECT il.name FROM \"{dquoted}\".sqlite_master m \
             JOIN pragma_index_list(m.name, '{squoted}') il \
             WHERE m.type = 'table' AND il.\"partial\" = 1"
        ))
        .fetch_all(pool)
        .await?;
        names.extend(rows.into_iter().map(|(index,)| index.to_lowercase()));
    }
    Ok(names)
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

    /// A CTE named after a real table earns that name no scan waiver. The
    /// subquery-scoped CTE here leaves the outer `users` bound to the real
    /// table, whose full scan renders as `SCAN users` — identical to the
    /// CTE's own bounded scan line — so the verifier must refuse the query.
    #[tokio::test]
    async fn cte_named_after_real_table_does_not_waive_its_scan()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let db = Database::new_without_plan_verification("sqlite::memory:").await?;
        let sql = "
            SELECT *
            FROM (WITH users(id) AS MATERIALIZED (SELECT 1) SELECT * FROM users) sub, users
        ";
        let outcome = verify_query_plan_sql(db.pool(), "cte_shadows_real_table", sql).await;
        let Err(QueryPlanError::FullTableScan { detail, .. }) = outcome else {
            return Err(format!("expected a full-table-scan refusal, got {outcome:?}").into());
        };
        assert_eq!(detail, "SCAN users");
        Ok(())
    }

    /// A covering-index scan reads every index row — a table scan in index
    /// clothing — so the verifier refuses it end-to-end.
    #[tokio::test]
    async fn covering_index_scan_of_a_full_index_is_refused()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let db = Database::new_without_plan_verification("sqlite::memory:").await?;
        let sql = "SELECT kind, current_rep, fact_id FROM fact_subjects ORDER BY kind, current_rep";
        let outcome = verify_query_plan_sql(db.pool(), "covering_scan", sql).await;
        let Err(QueryPlanError::FullTableScan { detail, .. }) = outcome else {
            return Err(format!("expected a full-scan refusal, got {outcome:?}").into());
        };
        assert!(
            detail.starts_with("SCAN fact_subjects USING COVERING INDEX"),
            "refusal must name the covering scan, got {detail}"
        );
        Ok(())
    }

    /// The partial-index exemption: a covering scan of a partial index is
    /// bounded by the index predicate, so only the non-partial one refuses.
    #[test]
    fn covering_index_scan_is_waived_only_for_partial_indexes() {
        let no_ctes = std::collections::HashSet::new();
        let partials: std::collections::HashSet<String> =
            std::iter::once("idx_facts_unclaimed".to_owned()).collect();
        assert!(is_full_table_scan(
            "SCAN fact_subjects USING COVERING INDEX idx_subjects_rep",
            &no_ctes,
            &partials,
        ));
        assert!(!is_full_table_scan(
            "SCAN facts USING COVERING INDEX idx_facts_unclaimed",
            &no_ctes,
            &partials,
        ));
    }
}
