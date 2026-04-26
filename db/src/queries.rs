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
    FIND_ENTITY_BY_ID: "
        SELECT id, entity_json, earliest_date, latest_date,
               latitude, longitude, created_at, updated_at
        FROM entities WHERE id = ?
    ",
    // Batch fetch entities by ID. Parameter ?1 is a JSON array of entity ID
    // strings. Used to load cluster representatives in one round-trip instead
    // of N+1 lookups.
    FIND_ENTITIES_BY_IDS: "
        SELECT id, entity_json, earliest_date, latest_date,
               latitude, longitude, created_at, updated_at
        FROM entities WHERE id IN (SELECT value FROM json_each(?1))
    ",
    FIND_ENTITIES_BY_EXTERNAL_ID: "
        SELECT e.id, e.entity_json, e.earliest_date, e.latest_date,
               e.latitude, e.longitude, e.created_at, e.updated_at
        FROM entities e
        JOIN entity_external_ids x ON x.entity_id = e.id
        WHERE x.id_type = ? AND x.external_id = ?
    ",

    // External IDs
    INSERT_EXTERNAL_ID: "INSERT INTO entity_external_ids (id_type, external_id, entity_id) VALUES (?, ?, ?)",

    // Entity links
    INSERT_ENTITY_LINK: "INSERT OR IGNORE INTO entity_links (id, entity_id, link_type, target_json, target_url) VALUES (?, ?, ?, ?, ?)",
    FIND_ENTITY_LINKS: "SELECT id, entity_id, link_type, target_json, target_url FROM entity_links WHERE entity_id = ?",

    // Entity relations
    INSERT_ENTITY_RELATION: "INSERT INTO entity_relations (from_entity_id, to_entity_id, relation_type, evidence_json) VALUES (?, ?, ?, ?)",

    // Annotations
    INSERT_ANNOTATION: "INSERT OR IGNORE INTO annotations (id, entity_id, url_id, kind_json, created_at) VALUES (?, ?, ?, ?, ?)",
    FIND_ANNOTATIONS_BY_ENTITY: "SELECT id, entity_id, url_id, kind, kind_json, created_at FROM annotations WHERE entity_id = ?",
    FIND_ANNOTATIONS_BY_URL: "SELECT id, entity_id, url_id, kind, kind_json, created_at FROM annotations WHERE url_id = ?",

    // Entity listing (bounding box + keyset pagination).
    // Ordered by (updated_at DESC, id DESC) so recently-modified entities
    // appear first; both columns are part of the keyset cursor.
    // Antimeridian-crossing viewports are handled by OR'ing two lon ranges
    // (both identical for non-crossing bboxes, the two halves for crossing).
    // Parameters: ?1=min_lat, ?2=max_lat, ?3=min_lon_a, ?4=max_lon_a, ?5=min_lon_b, ?6=max_lon_b, ?7=limit
    LIST_ENTITIES_IN_BBOX_FIRST: "
        SELECT id, entity_json, earliest_date, latest_date,
               latitude, longitude, created_at, updated_at
        FROM entities
        WHERE latitude BETWEEN ?1 AND ?2
          AND (longitude BETWEEN ?3 AND ?4 OR longitude BETWEEN ?5 AND ?6)
        ORDER BY updated_at DESC, id DESC
        LIMIT ?7
    ",
    // Parameters: ?1=min_lat, ?2=max_lat, ?3=min_lon_a, ?4=max_lon_a, ?5=min_lon_b, ?6=max_lon_b, ?7=cursor_updated_at, ?8=cursor_id, ?9=limit
    LIST_ENTITIES_IN_BBOX_PAGE: "
        SELECT id, entity_json, earliest_date, latest_date,
               latitude, longitude, created_at, updated_at
        FROM entities
        WHERE latitude BETWEEN ?1 AND ?2
          AND (longitude BETWEEN ?3 AND ?4 OR longitude BETWEEN ?5 AND ?6)
          AND (updated_at, id) < (?7, ?8)
        ORDER BY updated_at DESC, id DESC
        LIMIT ?9
    ",

    // All resolved media for a single entity (detail panel).
    // Joins annotations → research_urls → media to get the full media chain.
    // Ordered by annotation kind then media ID for stable display order.
    FIND_MEDIA_BY_ENTITY: "
        SELECT m.id, m.storage_key, m.media_type, m.width, m.height,
               m.captured_meta, a.kind_json, r.url as source_url
        FROM annotations a
        JOIN research_urls r ON a.url_id = r.id
        JOIN media m ON r.media_id = m.id
        WHERE a.entity_id = ?
        ORDER BY a.kind, m.id
    ",

    // First thumbnail per entity for a batch of entity IDs (map markers).
    // Uses MIN(m.id) in a correlated subquery to deterministically pick
    // one media per entity (the earliest-inserted media item).
    // Parameter ?1 is a JSON array of entity ID strings.
    FIND_THUMBNAILS_FOR_ENTITIES: "
        SELECT a.entity_id, m.storage_key, m.width, m.height
        FROM annotations a
        JOIN research_urls r ON a.url_id = r.id
        JOIN media m ON r.media_id = m.id
        WHERE a.entity_id IN (SELECT value FROM json_each(?1))
          AND m.id = (
            SELECT MIN(m2.id)
            FROM annotations a2
            JOIN research_urls r2 ON a2.url_id = r2.id
            JOIN media m2 ON r2.media_id = m2.id
            WHERE a2.entity_id = a.entity_id
          )
    ",

    // Threshold count of entities in a bounding box (capped at N+1 to avoid full scans).
    // Parameters: ?1=min_lat, ?2=max_lat, ?3=min_lon_a, ?4=max_lon_a, ?5=min_lon_b, ?6=max_lon_b, ?7=limit
    COUNT_ENTITIES_IN_BBOX: "
        SELECT COUNT(*) FROM (
            SELECT 1 FROM entities
            WHERE latitude BETWEEN ?1 AND ?2
              AND (longitude BETWEEN ?3 AND ?4 OR longitude BETWEEN ?5 AND ?6)
            LIMIT ?7
        )
    ",

    // Threshold count of clusters in a bounding box at a given zone type.
    // Uses a visible_regions CTE (same pattern as LIST_CLUSTERS_IN_BBOX) to
    // avoid a redundant double-join on the regions table.
    // Parameters: ?1=zone_type, ?2=min_lat, ?3=max_lat, ?4=min_lon_a, ?5=max_lon_a, ?6=min_lon_b, ?7=max_lon_b, ?8=limit
    COUNT_CLUSTERS_IN_BBOX: "
        WITH visible_regions AS (
            SELECT osm_id
            FROM regions_db.regions
            WHERE zone_type = ?1
              AND (
                  ROWID IN (
                      SELECT ROWID FROM regions_db.SpatialIndex
                      WHERE f_table_name = 'regions'
                        AND f_geometry_column = 'geometry'
                        AND search_frame = BuildMbr(?4, ?2, ?5, ?3, 4326)
                  )
                  OR ROWID IN (
                      SELECT ROWID FROM regions_db.SpatialIndex
                      WHERE f_table_name = 'regions'
                        AND f_geometry_column = 'geometry'
                        AND search_frame = BuildMbr(?6, ?2, ?7, ?3, 4326)
                  )
              )
        )
        SELECT COUNT(*) FROM (
            SELECT 1 FROM entity_regions er
            WHERE er.zone_type = ?1
              AND er.region_osm_id IN (SELECT osm_id FROM visible_regions)
            GROUP BY er.region_osm_id
            LIMIT ?8
        )
    ",

    // Entity region assignments (entity_regions table, local DB only).
    DELETE_ENTITY_REGIONS: "DELETE FROM entity_regions WHERE entity_id = ?",

    // ==================== Spatial (regions_db) ====================
    //
    // These reference the attached regions_db. They're in the same macro
    // (not a separate module) because the regions DB is always attached.

    // Assign all containing regions to an entity by point-in-polygon.
    // Uses ST_Intersects (not ST_Within) so points on boundaries still match.
    // TODO: when two polygons of the same zone_type cover a point (border
    // zones, enclaves), INSERT OR IGNORE picks an arbitrary winner.
    // TODO: rebuilding the regions DB can leave orphaned entity_regions rows.
    ASSIGN_ENTITY_REGIONS: "
        INSERT OR IGNORE INTO entity_regions (entity_id, region_osm_id, zone_type)
        SELECT ?3, r.osm_id, r.zone_type
        FROM regions_db.regions r
        WHERE r.ROWID IN (
            SELECT ROWID FROM regions_db.SpatialIndex
            WHERE f_table_name = 'regions'
              AND f_geometry_column = 'geometry'
              AND search_frame = MakePoint(?1, ?2, 4326)
        )
        AND ST_Intersects(MakePoint(?1, ?2, 4326), r.geometry)
    ",

    // List region clusters in a bounding box at a given zone type.
    //
    // `visible_regions` CTE: R-tree-filtered set of regions in the viewport.
    // Used by both `ranked_reps` (to scope the window function) and the main
    // SELECT (to filter results). The R-tree predicate appears exactly once.
    //
    // `ranked_reps` CTE: ranks entities per visible region by media presence,
    // proximity to centroid (Manhattan distance), and entity_id tiebreaker.
    // Only processes entities in viewport-visible regions (not all regions
    // of the zone_type globally).
    //
    // Antimeridian-crossing viewports are handled by OR'ing two BuildMbr
    // calls (both identical for non-crossing bboxes).
    //
    // Parameters: ?1=zone_type, ?2=min_lat, ?3=max_lat, ?4=min_lon_a, ?5=max_lon_a, ?6=min_lon_b, ?7=max_lon_b, ?8=limit
    LIST_CLUSTERS_IN_BBOX: "
        WITH visible_regions AS (
            SELECT osm_id
            FROM regions_db.regions
            WHERE zone_type = ?1
              AND (
                  ROWID IN (
                      SELECT ROWID FROM regions_db.SpatialIndex
                      WHERE f_table_name = 'regions'
                        AND f_geometry_column = 'geometry'
                        AND search_frame = BuildMbr(?4, ?2, ?5, ?3, 4326)
                  )
                  OR ROWID IN (
                      SELECT ROWID FROM regions_db.SpatialIndex
                      WHERE f_table_name = 'regions'
                        AND f_geometry_column = 'geometry'
                        AND search_frame = BuildMbr(?6, ?2, ?7, ?3, 4326)
                  )
              )
        ),
        region_centroids AS (
            SELECT osm_id, ST_X(ST_Centroid(geometry)) AS cx, ST_Y(ST_Centroid(geometry)) AS cy
            FROM regions_db.regions
            WHERE osm_id IN (SELECT osm_id FROM visible_regions)
        ),
        ranked_reps AS (
            SELECT
                er.region_osm_id,
                e.id AS entity_id,
                ROW_NUMBER() OVER (
                    PARTITION BY er.region_osm_id
                    ORDER BY
                        EXISTS(SELECT 1 FROM annotations a JOIN research_urls ru ON a.url_id = ru.id WHERE a.entity_id = e.id AND ru.media_id IS NOT NULL) DESC,
                        ABS(e.latitude - rc.cy) + ABS(e.longitude - rc.cx),
                        e.id
                ) AS rn
            FROM entity_regions er
            JOIN entities e ON e.id = er.entity_id
            JOIN region_centroids rc ON rc.osm_id = er.region_osm_id
            WHERE er.region_osm_id IN (SELECT osm_id FROM visible_regions)
        )
        SELECT
            r.osm_id,
            COALESCE(json_extract(r.international_names, '$.en'), r.name) AS region_name,
            rc.cx AS centroid_lon,
            rc.cy AS centroid_lat,
            MbrMinY(r.geometry) AS bbox_min_lat,
            MbrMaxY(r.geometry) AS bbox_max_lat,
            MbrMinX(r.geometry) AS bbox_min_lon,
            MbrMaxX(r.geometry) AS bbox_max_lon,
            rr.entity_id AS representative_id
        FROM entity_regions er
        JOIN regions_db.regions r ON r.osm_id = er.region_osm_id
        JOIN region_centroids rc ON rc.osm_id = r.osm_id
        LEFT JOIN ranked_reps rr ON rr.region_osm_id = r.osm_id AND rr.rn = 1
        WHERE er.zone_type = ?1
          AND r.osm_id IN (SELECT osm_id FROM visible_regions)
        GROUP BY r.osm_id
        LIMIT ?8
    ",
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    #[tokio::test]
    async fn test_all_queries_use_indexes() -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    {
        let db = Database::new_without_plan_verification(
            "sqlite::memory:",
            &crate::resolve_regions_db()?,
        )
        .await?;
        verify_all_query_plans(db.pool()).await?;
        Ok(())
    }
}
