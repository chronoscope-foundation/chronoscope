//! Query definitions for the Chronoscope API.
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
        name: &'static str,
        sql: &'static str,
        detail: String,
    },

    #[error("Failed to explain query '{name}': {source}")]
    ExplainFailed {
        name: &'static str,
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

/// Verify that all queries use indexes (no full table scans).
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
        verify_query_plan(pool, query_def).await?;
    }
    Ok(())
}

async fn verify_query_plan(pool: &SqlitePool, query_def: &QueryDef) -> Result<(), QueryPlanError> {
    let explain_sql = format!("EXPLAIN QUERY PLAN {}", query_def.sql);

    let plan: Vec<(i32, i32, i32, String)> = sqlx::query_as(&explain_sql)
        .fetch_all(pool)
        .await
        .map_err(|e| QueryPlanError::ExplainFailed {
            name: query_def.name,
            source: e,
        })?;

    for (_, _, _, detail) in plan {
        // SCAN without an index means full table scan
        // SCAN ... USING INDEX is fine (it's an index scan)
        // SEARCH is always fine (it's an index lookup)
        let is_full_scan = detail.starts_with("SCAN") && !detail.contains("USING");

        if is_full_scan {
            return Err(QueryPlanError::FullTableScan {
                name: query_def.name,
                sql: query_def.sql,
                detail,
            });
        }
    }

    Ok(())
}

macro_rules! define_queries {
    ($($name:ident: $sql:literal),* $(,)?) => {
        $(pub const $name: QueryDef = QueryDef { name: stringify!($name), sql: $sql };)*

        /// All queries in the system. Used by tests to verify query plans.
        pub const ALL: &[&QueryDef] = &[$(&$name),*];
    };
}

define_queries! {
    // Users
    CREATE_USER: "INSERT INTO users (id, username, email) VALUES (?, ?, ?)",
    FIND_USER_BY_IDENTIFIER: "SELECT id FROM users WHERE username = ? OR email = ?",
    UPDATE_USERNAME: "UPDATE users SET username = ? WHERE id = ?",
    UPDATE_EMAIL: "UPDATE users SET email = ? WHERE id = ?",
    GET_USER: "SELECT id, username, email, created_at FROM users WHERE id = ?",

    // Credentials
    ADD_CREDENTIAL: "INSERT INTO credentials (user_id, credential_id, passkey_json) VALUES (?, ?, ?)",
    GET_CREDENTIALS: "SELECT passkey_json FROM credentials WHERE user_id = ?",
    UPDATE_CREDENTIAL: "UPDATE credentials SET passkey_json = ? WHERE user_id = ? AND credential_id = ?",

    // Research URLs
    GET_URL_BY_URL: "SELECT id FROM research_urls WHERE url = ?",
    CREATE_URL: "INSERT INTO research_urls (id, url) VALUES (?, ?)",
    GET_URL_BY_ID: "SELECT id, url, created_at FROM research_urls WHERE id = ?",
    // Keyset pagination: first page (no cursor)
    LIST_ALL_URLS_FIRST: "SELECT id, url, created_at FROM research_urls ORDER BY created_at DESC, id DESC LIMIT ?",
    // Keyset pagination: subsequent pages (cursor = created_at, id of last item)
    LIST_ALL_URLS_PAGE: "SELECT id, url, created_at FROM research_urls WHERE (created_at, id) < (?, ?) ORDER BY created_at DESC, id DESC LIMIT ?",

    // Follows
    CREATE_FOLLOW: "INSERT INTO follows (user_id, url_id) VALUES (?, ?) ON CONFLICT DO NOTHING",
    // Keyset pagination: first page (no cursor)
    LIST_FOLLOWED_URLS_FIRST: "SELECT r.id, r.url, r.created_at, f.created_at as followed_at FROM research_urls r JOIN follows f ON f.url_id = r.id WHERE f.user_id = ? ORDER BY f.created_at DESC, r.id DESC LIMIT ?",
    // Keyset pagination: subsequent pages (cursor = followed_at, url_id of last item)
    LIST_FOLLOWED_URLS_PAGE: "SELECT r.id, r.url, r.created_at, f.created_at as followed_at FROM research_urls r JOIN follows f ON f.url_id = r.id WHERE f.user_id = ? AND (f.created_at, r.id) < (?, ?) ORDER BY f.created_at DESC, r.id DESC LIMIT ?",
    GET_FOLLOW_TIMESTAMP: "SELECT created_at FROM follows WHERE user_id = ? AND url_id = ?",
    DELETE_FOLLOW: "DELETE FROM follows WHERE user_id = ? AND url_id = ?",
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[tokio::test]
    async fn test_all_queries_use_indexes() -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    {
        let db = Database::new_without_plan_verification("sqlite::memory:").await?;
        verify_all_query_plans(db.pool()).await?;
        Ok(())
    }
}
