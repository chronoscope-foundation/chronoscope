//! Chronoscope database layer.
//!
//! This crate provides database access for the Chronoscope platform.
//! It is used by both the API server and background workers.

pub mod error;
pub mod ingestion;
pub mod media_store;
pub mod models;
pub mod queries;
pub mod queue;
pub(crate) mod row;
pub mod types;
pub mod url;
pub mod workers;

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use chrono::{NaiveDateTime, Utc};
use sqlx::Executor;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

use chronoscope_integrations::IntegrationRegistry;

pub use chronoscope_core::entity::EntityRelationType;
pub use chronoscope_core::links::LinkType;
pub use error::{DbError, DbResult, is_unique_violation};
pub use models::{
    Annotation, Coordinates, DateRange, Entity, EntityLink, EntityMedia, EntityThumbnail,
    FollowedUrl, Media, MediaData, MediaSlot, Page, PageData, ResearchUrl, ResearchUrlWithResolved,
    ResolvedContent, ResolvedTarget, User,
};
pub use queue::{ANALYSIS_QUEUE, Queue, QueueConfig, QueueItem, QueueQueries, url_queue_config};
pub use types::{
    AnalysisStatus, AnnotationId, AnnotationKindTag, Email, EntityId, EntityLinkId, ExternalIdType,
    MediaAnalysisState, MediaId, MediaType, PageId, ResearchUrlId, ResearchUrlStatus, SourceId,
    UserId,
};

pub use workers::MediaForAnalysis;

/// Get the current UTC timestamp as `NaiveDateTime` for database storage.
pub(crate) fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

/// Environment variable name for the regions database path.
///
/// The regions DB is a SpatiaLite database built by Nix containing administrative
/// boundaries from OpenStreetMap. The env var points directly to the `.sqlite` file.
/// Callers should resolve this once at startup and pass the path to `Database::new`.
pub const REGIONS_DB_ENV: &str = "REGIONS_DB";

/// Resolve `REGIONS_DB` from the environment.
///
/// Intended to be called once at program startup, not on every request.
///
/// # Errors
/// Returns `DbError::Config` if `REGIONS_DB` is not set.
pub fn resolve_regions_db() -> DbResult<std::path::PathBuf> {
    std::env::var(REGIONS_DB_ENV)
        .map(std::path::PathBuf::from)
        .map_err(|_| DbError::Config(format!("{REGIONS_DB_ENV} must be set")))
}

/// Split a bbox's longitude range for SQL binding.
/// Returns `(min_lon_a, max_lon_a, min_lon_b, max_lon_b)` — identical ranges
/// for non-crossing bboxes, split halves for antimeridian-crossing.
fn bbox_lon_ranges(bbox: &chronoscope_api_client::Bbox) -> (f64, f64, f64, f64) {
    if bbox.min_lon() > bbox.max_lon() {
        (bbox.min_lon(), 180.0, -180.0, bbox.max_lon())
    } else {
        (
            bbox.min_lon(),
            bbox.max_lon(),
            bbox.min_lon(),
            bbox.max_lon(),
        )
    }
}

/// Bind a Bbox's 6 parameters to a `query_as` in canonical order:
/// `min_lat`, `max_lat`, `min_lon_a`, `max_lon_a`, `min_lon_b`, `max_lon_b`.
///
/// SQL should use: `lat BETWEEN ?N AND ?N+1 AND (lon BETWEEN ?N+2 AND ?N+3 OR lon BETWEEN ?N+4 AND ?N+5)`.
fn bind_bbox<'q, T>(
    query: sqlx::query::QueryAs<'q, sqlx::Sqlite, T, sqlx::sqlite::SqliteArguments<'q>>,
    bbox: &chronoscope_api_client::Bbox,
) -> sqlx::query::QueryAs<'q, sqlx::Sqlite, T, sqlx::sqlite::SqliteArguments<'q>> {
    let (a_min, a_max, b_min, b_max) = bbox_lon_ranges(bbox);
    query
        .bind(bbox.min_lat())
        .bind(bbox.max_lat())
        .bind(a_min)
        .bind(a_max)
        .bind(b_min)
        .bind(b_max)
}

// ==================== Database ====================

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
    registry: IntegrationRegistry,

    // Type-erased for verification iteration
    all_queues: Vec<Arc<dyn QueueQueries>>,

    // Typed queue access for workers
    /// Queue for generic URLs (`worker_affinity IS NULL`).
    pub url_queue_generic: Arc<Queue<ResearchUrl>>,
    /// Queue for image analysis.
    pub analysis_queue: Arc<Queue<MediaForAnalysis>>,
}

impl Database {
    /// Get access to the underlying pool (for tests that need raw queries)
    #[cfg(test)]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Get access to the underlying pool (needed by API crate for its tests)
    #[must_use]
    pub fn pool_ref(&self) -> &SqlitePool {
        &self.pool
    }

    /// Get the integration registry for URL routing.
    #[must_use]
    pub fn registry(&self) -> &IntegrationRegistry {
        &self.registry
    }
}

/// Cap on cluster rows returned per `list_clusters_as_markers` call.
///
/// At low zoom the world has roughly ~250 countries; this is a generous
/// upper bound that prevents pathological queries from streaming the
/// entire `entity_regions` table back to a client.
const MAX_CLUSTERS_PER_QUERY: i64 = 500;

/// Typed row for the `LIST_CLUSTERS_IN_BBOX` query result.
///
/// Uses `sqlx::FromRow` instead of manual `.get("column_name")` calls.
#[derive(sqlx::FromRow)]
struct ClusterRow {
    osm_id: i64,
    region_name: String,
    centroid_lon: f64,
    centroid_lat: f64,
    bbox_min_lat: f64,
    bbox_max_lat: f64,
    bbox_min_lon: f64,
    bbox_max_lon: f64,
    representative_id: Option<String>,
}

impl Database {
    /// Create a new database connection pool, run migrations, and verify query plans.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if connection fails, `DbError::Migrate` if migrations fail,
    /// or `DbError::QueryPlan` if any query would cause a full table scan.
    pub async fn new(database_url: &str, regions_db_path: &Path) -> DbResult<Self> {
        let registry = chronoscope_integrations::create_registry(None)?;
        let pool = Self::create_pool(database_url, regions_db_path).await?;

        sqlx::migrate!("./migrations").run(&pool).await?;

        // Create queues
        let url_queue_generic = Arc::new(Queue::new(pool.clone(), url_queue_config(None)));
        let analysis_queue = Arc::new(Queue::new(pool.clone(), &ANALYSIS_QUEUE));

        // Collect for verification (same Arc, different view)
        let all_queues: Vec<Arc<dyn QueueQueries>> =
            vec![url_queue_generic.clone(), analysis_queue.clone()];

        let db = Self {
            pool,
            registry,
            all_queues,
            url_queue_generic,
            analysis_queue,
        };

        // Verify all query plans (static + queue-generated)
        db.verify_all_query_plans().await?;

        Ok(db)
    }

    /// Create a new database connection pool and run migrations, but skip query plan verification.
    ///
    /// This is used by tests that want to verify query plans themselves (to avoid circular dependency).
    pub async fn new_without_plan_verification(
        database_url: &str,
        regions_db_path: &Path,
    ) -> DbResult<Self> {
        let registry = chronoscope_integrations::create_registry(None)?;
        let pool = Self::create_pool(database_url, regions_db_path).await?;

        sqlx::migrate!("./migrations").run(&pool).await?;

        // Create queues
        let url_queue_generic = Arc::new(Queue::new(pool.clone(), url_queue_config(None)));
        let analysis_queue = Arc::new(Queue::new(pool.clone(), &ANALYSIS_QUEUE));

        // Collect for verification
        let all_queues: Vec<Arc<dyn QueueQueries>> =
            vec![url_queue_generic.clone(), analysis_queue.clone()];

        Ok(Self {
            pool,
            registry,
            all_queues,
            url_queue_generic,
            analysis_queue,
        })
    }

    /// Create the SQLite connection pool with SpatiaLite and attached regions DB.
    async fn create_pool(database_url: &str, regions_db_path: &Path) -> DbResult<SqlitePool> {
        let spatialite_dir = std::env::var("SPATIALITE_LIBRARY_PATH")
            .map_err(|_| DbError::Config("SPATIALITE_LIBRARY_PATH must be set".to_string()))?;

        // Avoid SqliteConnectOptions::from_str("sqlite::memory:") because it
        // sets in_memory(true), which adds SQLITE_OPEN_MEMORY. That flag
        // propagates to ATTACH DATABASE, causing file paths to be ignored
        // (attached DBs become empty in-memory instead of opening the file).
        //
        // For in-memory DBs, we replicate from_str's naming scheme (a unique
        // URI per pool) with ?mode=memory&cache=shared, which achieves shared
        // in-memory behavior through the URI parameter rather than the open
        // flag. For file-backed DBs, we use from_str normally.
        let options = if database_url == "sqlite::memory:" {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let seqno = SEQ.fetch_add(1, Ordering::Relaxed);
            SqliteConnectOptions::new().filename(format!(
                "file:chronoscope-mem-{seqno}?mode=memory&cache=shared"
            ))
        } else {
            SqliteConnectOptions::from_str(database_url)?
        }
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5))
        .extension(format!("{spatialite_dir}/mod_spatialite"));

        // ATTACH the regions DB on each new connection. Uses a file: URI with
        // immutable=1 since the DB lives in the read-only Nix store; without
        // this, SQLite tries to acquire file locks on a read-only mount and
        // can fail.
        //
        // Only `'` needs escaping (SQL-literal safety). Path separators must
        // NOT be percent-encoded — SQLite expects a literal filesystem path
        // after the `file:` prefix.
        let regions_path = regions_db_path.display().to_string();
        let escaped = regions_path.replace('\'', "''");
        let attach_sql = format!("ATTACH DATABASE 'file:{escaped}?immutable=1' AS regions_db");

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .after_connect(move |conn, _meta| {
                let sql = attach_sql.clone();
                Box::pin(async move {
                    conn.execute(sql.as_str()).await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await?;

        Ok(pool)
    }

    /// Verify all query plans (static queries + queue-generated queries).
    async fn verify_all_query_plans(&self) -> DbResult<()> {
        // Verify static queries
        queries::verify_all_query_plans(&self.pool).await?;

        // Verify queue-generated queries
        for queue in &self.all_queues {
            for (name, sql) in queue.queries() {
                queries::verify_query_plan_sql(&self.pool, name, sql).await?;
            }
        }

        Ok(())
    }

    // ==================== Users ====================

    /// Create a new user with username and email.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails (e.g., duplicate username/email).
    pub async fn create_user(&self, id: &UserId, username: &str, email: &Email) -> DbResult<()> {
        queries::CREATE_USER
            .query()
            .bind(id)
            .bind(username)
            .bind(email)
            .bind(now())
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Find a user by username or email (for login).
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn find_user_by_identifier(&self, identifier: &str) -> DbResult<Option<UserId>> {
        let row: Option<(UserId,)> = sqlx::query_as(queries::FIND_USER_BY_IDENTIFIER.sql)
            .bind(identifier)
            .bind(identifier)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(|(id,)| id))
    }

    /// Update a user's username.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the operation fails (e.g., duplicate username).
    pub async fn update_username(&self, user_id: &UserId, username: &str) -> DbResult<()> {
        let rows_affected = queries::UPDATE_USERNAME
            .query()
            .bind(username)
            .bind(user_id)
            .execute(&self.pool)
            .await?
            .rows_affected();

        if rows_affected == 0 {
            return Err(DbError::UserNotFound);
        }

        Ok(())
    }

    /// Update a user's email.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the operation fails (e.g., duplicate email).
    pub async fn update_email(&self, user_id: &UserId, email: &Email) -> DbResult<()> {
        let rows_affected = queries::UPDATE_EMAIL
            .query()
            .bind(email)
            .bind(user_id)
            .execute(&self.pool)
            .await?
            .rows_affected();

        if rows_affected == 0 {
            return Err(DbError::UserNotFound);
        }

        Ok(())
    }

    /// Get user info by ID.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn get_user(&self, user_id: &UserId) -> DbResult<Option<User>> {
        let row: Option<User> = sqlx::query_as(queries::GET_USER.sql)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row)
    }

    // ==================== Credentials ====================

    /// Store a new passkey credential as JSON.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the operation fails.
    pub async fn add_credential(
        &self,
        user_id: &UserId,
        credential_id: &str,
        passkey_json: &str,
    ) -> DbResult<()> {
        queries::ADD_CREDENTIAL
            .query()
            .bind(user_id)
            .bind(credential_id)
            .bind(passkey_json)
            .bind(now())
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Get all credentials for a user as JSON strings.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the operation fails.
    pub async fn get_credentials(&self, user_id: &UserId) -> DbResult<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(queries::GET_CREDENTIALS.sql)
            .bind(user_id)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(|(json,)| json).collect())
    }

    /// Update a credential after successful authentication.
    /// Persists the incremented sign counter for future clone detection
    /// (webauthn-rs rejects authentication if the counter doesn't increase).
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` or `DbError::CredentialNotFound`.
    pub async fn update_credential(
        &self,
        user_id: &UserId,
        credential_id: &str,
        passkey_json: &str,
    ) -> DbResult<()> {
        let rows_affected = queries::UPDATE_CREDENTIAL
            .query()
            .bind(passkey_json)
            .bind(user_id)
            .bind(credential_id)
            .execute(&self.pool)
            .await?
            .rows_affected();

        if rows_affected == 0 {
            return Err(DbError::CredentialNotFound);
        }

        Ok(())
    }

    // ==================== Research URLs ====================

    /// Submit a URL: creates the URL if new, and follows it for the user.
    /// Returns the URL id and whether it was newly created.
    ///
    /// The URL is normalized before storage to improve deduplication
    /// (e.g., removing tracking parameters, canonicalizing domains).
    ///
    /// The URL's domain is checked against the integration registry (provided at
    /// database construction) to compute `worker_affinity`. This determines which
    /// specialized worker should process the URL.
    ///
    /// # Errors
    /// Returns `DbError::InvalidArgument` if the URL cannot be parsed or normalized.
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn submit_url(
        &self,
        user_id: &UserId,
        url_str: &str,
    ) -> DbResult<(ResearchUrlId, bool)> {
        // Parse and normalize URL for deduplication
        let parsed_url = ::url::Url::parse(url_str)
            .map_err(|e| DbError::InvalidArgument(format!("invalid URL: {e}")))?;

        // Apply integration-specific normalization, then generic
        let integration_normalized = self.registry.normalize_url(&parsed_url);
        let normalized_url = url::normalize_url(&integration_normalized)
            .map_err(|e| DbError::InvalidArgument(format!("cannot normalize URL: {e}")))?;
        let normalized_str = normalized_url.to_string();

        // Compute worker affinity from the domain
        let worker_affinity: Option<String> = normalized_url
            .host_str()
            .and_then(|domain| self.registry.integration_name_for_domain(domain))
            .map(|name| name.to_string());

        // Try to insert the URL (ignored if already exists)
        let new_id = ResearchUrlId::generate();
        let result = queries::CREATE_URL_OR_IGNORE
            .query()
            .bind(&new_id)
            .bind(&normalized_str)
            .bind(ResearchUrlStatus::Pending)
            .bind(0_i32) // attempt_count
            .bind(&worker_affinity)
            .bind(now())
            .execute(&self.pool)
            .await?;

        // If we inserted, use our ID; otherwise fetch the existing one
        let (url_id, created) = if result.rows_affected() > 0 {
            (new_id, true)
        } else {
            let (id,): (ResearchUrlId,) = sqlx::query_as(queries::GET_URL_BY_URL.sql)
                .bind(&normalized_str)
                .fetch_one(&self.pool)
                .await?;
            (id, false)
        };

        // Follow the URL (ignore if already following)
        queries::CREATE_FOLLOW
            .query()
            .bind(user_id)
            .bind(&url_id)
            .bind(now())
            .execute(&self.pool)
            .await?;

        Ok((url_id, created))
    }

    /// Follow an existing URL by ID.
    /// Returns true if newly followed, false if already following.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn follow_url(&self, user_id: &UserId, url_id: &ResearchUrlId) -> DbResult<bool> {
        let rows_affected = queries::CREATE_FOLLOW
            .query()
            .bind(user_id)
            .bind(url_id)
            .bind(now())
            .execute(&self.pool)
            .await?
            .rows_affected();

        Ok(rows_affected > 0)
    }

    /// List URLs that a user follows, newest follow first (keyset pagination).
    ///
    /// Pass `None` for the first page, or `Some((followed_at, url_id))` of the last
    /// item from the previous page for subsequent pages.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn list_followed_urls(
        &self,
        user_id: &UserId,
        limit: i64,
        cursor: Option<(NaiveDateTime, &ResearchUrlId)>,
    ) -> DbResult<Vec<FollowedUrl>> {
        let rows: Vec<FollowedUrl> = match cursor {
            None => {
                sqlx::query_as(queries::LIST_FOLLOWED_URLS_FIRST.sql)
                    .bind(user_id)
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await?
            }
            Some((followed_at, url_id)) => {
                sqlx::query_as(queries::LIST_FOLLOWED_URLS_PAGE.sql)
                    .bind(user_id)
                    .bind(followed_at)
                    .bind(url_id)
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await?
            }
        };

        Ok(rows)
    }

    /// Get a single research URL by ID.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn get_url_by_id(&self, id: &ResearchUrlId) -> DbResult<Option<ResearchUrl>> {
        let row: Option<ResearchUrl> = sqlx::query_as(queries::GET_URL_BY_ID.sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row)
    }

    /// List all research URLs, newest first (keyset pagination).
    ///
    /// Pass `None` for the first page, or `Some((created_at, id))` of the last
    /// item from the previous page for subsequent pages.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn list_all_urls(
        &self,
        limit: i64,
        cursor: Option<(NaiveDateTime, &ResearchUrlId)>,
    ) -> DbResult<Vec<ResearchUrl>> {
        let rows: Vec<ResearchUrl> = match cursor {
            None => {
                sqlx::query_as(queries::LIST_ALL_URLS_FIRST.sql)
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await?
            }
            Some((created_at, id)) => {
                sqlx::query_as(queries::LIST_ALL_URLS_PAGE.sql)
                    .bind(created_at)
                    .bind(id)
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await?
            }
        };

        Ok(rows)
    }

    /// Get when a user started following a URL, if they are following it.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn get_follow_timestamp(
        &self,
        user_id: &UserId,
        url_id: &ResearchUrlId,
    ) -> DbResult<Option<NaiveDateTime>> {
        let row: Option<(NaiveDateTime,)> = sqlx::query_as(queries::GET_FOLLOW_TIMESTAMP.sql)
            .bind(user_id)
            .bind(url_id)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(|(ts,)| ts))
    }

    /// Unfollow a URL. Returns true if was following, false otherwise.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn unfollow_url(&self, user_id: &UserId, url_id: &ResearchUrlId) -> DbResult<bool> {
        let rows_affected = queries::DELETE_FOLLOW
            .query()
            .bind(user_id)
            .bind(url_id)
            .execute(&self.pool)
            .await?
            .rows_affected();

        Ok(rows_affected > 0)
    }

    // ==================== Entities ====================

    /// Find an entity by its database ID.
    pub async fn find_entity_by_id(&self, id: &EntityId) -> DbResult<Option<Entity>> {
        let row: Option<row::Entity> = sqlx::query_as(queries::FIND_ENTITY_BY_ID.sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| r.into_domain()).transpose()
    }

    /// Find entities by an external ID (e.g., Wikidata Q-ID).
    pub async fn find_entities_by_external_id(
        &self,
        id_type: &ExternalIdType,
        external_id: &str,
    ) -> DbResult<Vec<Entity>> {
        let rows: Vec<row::Entity> = sqlx::query_as(queries::FIND_ENTITIES_BY_EXTERNAL_ID.sql)
            .bind(id_type)
            .bind(external_id)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(|r| r.into_domain()).collect()
    }

    /// Get all links for an entity.
    pub async fn find_entity_links(&self, entity_id: &EntityId) -> DbResult<Vec<EntityLink>> {
        let rows: Vec<row::EntityLink> = sqlx::query_as(queries::FIND_ENTITY_LINKS.sql)
            .bind(entity_id)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(|r| r.into_domain()).collect()
    }

    /// Get all annotations for an entity.
    pub async fn find_annotations_by_entity(
        &self,
        entity_id: &EntityId,
    ) -> DbResult<Vec<Annotation>> {
        let rows: Vec<row::Annotation> = sqlx::query_as(queries::FIND_ANNOTATIONS_BY_ENTITY.sql)
            .bind(entity_id)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(|r| r.into_domain()).collect()
    }

    /// Get all resolved media for an entity (via annotations → `research_urls` → media).
    ///
    /// Returns media items that have been fully resolved (research URL → media).
    /// Unresolved annotations (where the URL hasn't been fetched yet) are excluded.
    pub async fn find_media_by_entity(&self, entity_id: &EntityId) -> DbResult<Vec<EntityMedia>> {
        let rows: Vec<row::EntityMedia> = sqlx::query_as(queries::FIND_MEDIA_BY_ENTITY.sql)
            .bind(entity_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|r| r.into_domain()).collect())
    }

    /// Get one representative thumbnail per entity for a batch of entity IDs.
    ///
    /// Entities without any resolved media are omitted from the result.
    /// The parameter is a JSON-serialized array of entity ID strings.
    pub async fn find_thumbnails_for_entities(
        &self,
        entity_ids_json: &str,
    ) -> DbResult<Vec<EntityThumbnail>> {
        let rows: Vec<row::EntityThumbnail> =
            sqlx::query_as(queries::FIND_THUMBNAILS_FOR_ENTITIES.sql)
                .bind(entity_ids_json)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(|r| r.into_domain()).collect())
    }

    /// List entities within a geographic bounding box (keyset pagination).
    ///
    /// Antimeridian-crossing viewports are handled in SQL by OR'ing two
    /// longitude ranges — no Rust-side splitting or dedup needed.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn list_entities_in_bbox(
        &self,
        bbox: &chronoscope_api_client::Bbox,
        limit: i64,
        cursor: Option<(NaiveDateTime, &EntityId)>,
    ) -> DbResult<Vec<Entity>> {
        let rows: Vec<row::Entity> = match cursor {
            None => {
                bind_bbox(
                    sqlx::query_as(queries::LIST_ENTITIES_IN_BBOX_FIRST.sql),
                    bbox,
                )
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            Some((updated_at, id)) => {
                bind_bbox(
                    sqlx::query_as(queries::LIST_ENTITIES_IN_BBOX_PAGE.sql),
                    bbox,
                )
                .bind(updated_at)
                .bind(id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.into_iter().map(|r| r.into_domain()).collect()
    }

    /// Count entities in a bounding box, up to a threshold limit.
    ///
    /// Returns at most `limit` — useful for deciding whether to show
    /// individual entities or clusters without fetching full rows.
    pub async fn count_entities_in_bbox(
        &self,
        bbox: &chronoscope_api_client::Bbox,
        limit: i64,
    ) -> DbResult<i64> {
        let (count,): (i64,) = bind_bbox(sqlx::query_as(queries::COUNT_ENTITIES_IN_BBOX.sql), bbox)
            .bind(limit)
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    /// Count clusters at a given zone type in a bounding box, up to a threshold limit.
    pub async fn count_clusters_in_bbox(
        &self,
        zone_type: &chronoscope_api_client::ZoneType,
        bbox: &chronoscope_api_client::Bbox,
        limit: i64,
    ) -> DbResult<i64> {
        let zone_type = zone_type.as_ref();
        let (count,): (i64,) = bind_bbox(
            sqlx::query_as::<_, (i64,)>(queries::COUNT_CLUSTERS_IN_BBOX.sql).bind(zone_type),
            bbox,
        )
        .bind(limit)
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    /// Convert entities from `list_entities_in_bbox` into `Vec<Marker>`,
    /// returning the markers plus a map of `marker_id` → `entity_id` for
    /// thumbnail resolution.
    pub async fn list_entities_as_markers(
        &self,
        bbox: &chronoscope_api_client::Bbox,
        limit: i64,
    ) -> DbResult<(
        Vec<chronoscope_api_client::Marker>,
        HashMap<chronoscope_api_client::MarkerId, EntityId>,
    )> {
        use chronoscope_api_client::{ClickAction, EntityPickerEntry, Marker, MarkerId};

        let entities = self.list_entities_in_bbox(bbox, limit, None).await?;

        // Group by coordinate to detect co-located entities.
        let mut coord_groups: HashMap<(u64, u64), Vec<chronoscope_api_client::EntitySummary>> =
            HashMap::new();
        for entity in entities {
            if let Some(summary) = entity.to_summary() {
                let key = (summary.latitude.to_bits(), summary.longitude.to_bits());
                coord_groups.entry(key).or_default().push(summary);
            }
        }

        let mut markers = Vec::with_capacity(coord_groups.len());
        let mut rep_map = HashMap::new();

        for group in coord_groups.into_values() {
            let first = &group[0];
            let marker_id = MarkerId::Entity(first.id.clone());
            // Track first entity for thumbnail resolution.
            rep_map.insert(marker_id.clone(), first.id.clone());

            if group.len() == 1 {
                markers.push(Marker {
                    id: marker_id,
                    latitude: first.latitude,
                    longitude: first.longitude,
                    label: first.name.clone(),
                    thumbnail_url: None,
                    click_action: ClickAction::Select {
                        entity_id: first.id.clone(),
                    },
                });
            } else {
                // Co-located: build disambiguation picker entries sorted
                // by earliest date (undated last) for temporal ordering.
                let mut sorted = group;
                sorted.sort_by_key(|e| (e.earliest_date.is_none(), e.earliest_date));
                let entries: Vec<EntityPickerEntry> = sorted
                    .iter()
                    .map(|e| EntityPickerEntry {
                        id: e.id.to_string(),
                        name: e.name.clone(),
                    })
                    .collect();
                markers.push(Marker {
                    id: marker_id,
                    latitude: sorted[0].latitude,
                    longitude: sorted[0].longitude,
                    label: sorted[0].name.clone(),
                    thumbnail_url: None,
                    click_action: ClickAction::Disambiguate { entries },
                });
            }
        }

        Ok((markers, rep_map))
    }

    /// List clusters, returning markers plus a map of `marker_id` → representative
    /// `entity_id` (so the API layer can resolve thumbnail URLs).
    pub async fn list_clusters_as_markers(
        &self,
        zone_type: &chronoscope_api_client::ZoneType,
        bbox: &chronoscope_api_client::Bbox,
    ) -> DbResult<(
        Vec<chronoscope_api_client::Marker>,
        HashMap<chronoscope_api_client::MarkerId, EntityId>,
    )> {
        let zone_type = zone_type.as_ref();

        let rows = self.query_clusters(zone_type, bbox).await?;

        // Collect representative IDs (may be NULL if a region has no entities
        // with coordinates, or if the correlated subquery found nothing).
        let representative_ids: Vec<Option<&str>> = rows
            .iter()
            .map(|row| row.representative_id.as_deref())
            .collect();

        // Batch-fetch representative entities in one round-trip.
        let non_null_ids: Vec<&str> = representative_ids.iter().filter_map(|id| *id).collect();
        let mut representatives: HashMap<String, chronoscope_api_client::EntitySummary> =
            if non_null_ids.is_empty() {
                HashMap::new()
            } else {
                let ids_json = serde_json::to_string(&non_null_ids)
                    .map_err(|e| DbError::InconsistentRow(format!("encode ids: {e}")))?;
                let entity_rows: Vec<row::Entity> =
                    sqlx::query_as(queries::FIND_ENTITIES_BY_IDS.sql)
                        .bind(&ids_json)
                        .fetch_all(&self.pool)
                        .await?;

                let mut map = HashMap::with_capacity(entity_rows.len());
                for entity_row in entity_rows {
                    match entity_row.into_domain() {
                        Ok(entity) => {
                            let id_str = entity.id.to_string();
                            if let Some(summary) = entity.to_summary() {
                                map.insert(id_str, summary);
                            }
                        }
                        Err(e) => {
                            // A bad representative row shouldn't take down the
                            // entire cluster response — log and skip.
                            eprintln!("warn: skipping bad representative entity: {e}");
                        }
                    }
                }
                map
            };

        let mut markers = Vec::with_capacity(rows.len());
        let mut rep_map: HashMap<chronoscope_api_client::MarkerId, EntityId> = HashMap::new();
        for (row, rep_id) in rows.iter().zip(representative_ids.iter()) {
            let representative = rep_id.and_then(|id| representatives.remove(id));

            let marker_id = chronoscope_api_client::MarkerId::Cluster(row.osm_id);

            // Track representative for thumbnail resolution by the API layer.
            if let Some(ref rep) = representative {
                rep_map.insert(marker_id.clone(), rep.id.clone());
            }

            markers.push(chronoscope_api_client::Marker {
                id: marker_id,
                latitude: row.centroid_lat,
                longitude: row.centroid_lon,
                label: Some(row.region_name.clone()),
                thumbnail_url: None, // resolved by the API layer using rep_map
                click_action: chronoscope_api_client::ClickAction::ZoomTo {
                    bbox: chronoscope_api_client::Bbox::new(
                        row.bbox_min_lat,
                        row.bbox_max_lat,
                        row.bbox_min_lon,
                        row.bbox_max_lon,
                    )
                    .map_err(|e| {
                        DbError::InconsistentRow(format!(
                            "invalid cluster bbox for osm_id {}: {e}",
                            row.osm_id
                        ))
                    })?,
                },
            });
        }

        Ok((markers, rep_map))
    }

    /// Run the cluster query for a bounding box (handles antimeridian in SQL).
    async fn query_clusters(
        &self,
        zone_type: &str,
        bbox: &chronoscope_api_client::Bbox,
    ) -> DbResult<Vec<ClusterRow>> {
        Ok(bind_bbox(
            sqlx::query_as::<_, ClusterRow>(queries::LIST_CLUSTERS_IN_BBOX.sql).bind(zone_type),
            bbox,
        )
        .bind(MAX_CLUSTERS_PER_QUERY)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Get all annotations for a research URL.
    pub async fn find_annotations_by_url(
        &self,
        url_id: &ResearchUrlId,
    ) -> DbResult<Vec<Annotation>> {
        let rows: Vec<row::Annotation> = sqlx::query_as(queries::FIND_ANNOTATIONS_BY_URL.sql)
            .bind(url_id)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(|r| r.into_domain()).collect()
    }

    // ==================== Dossier ====================

    /// Get a research URL with all resolved content (page or direct media).
    ///
    /// This fetches the research URL and, if resolved, the associated page or media.
    /// For pages, also fetches all referenced media items.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn get_research_dossier(
        &self,
        id: &ResearchUrlId,
    ) -> DbResult<Option<ResearchUrlWithResolved>> {
        // Get the research URL
        let research_url: Option<ResearchUrl> = sqlx::query_as(queries::GET_URL_BY_ID.sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

        let Some(research_url) = research_url else {
            return Ok(None);
        };

        // Build resolved content based on what type of resolution we have
        let resolved = match &research_url.target {
            ResolvedTarget::Unresolved => None,
            ResolvedTarget::Page(page_id) => {
                // Fetch page row
                let page_row: Option<row::Page> = sqlx::query_as(queries::GET_PAGE_BY_ID.sql)
                    .bind(page_id)
                    .fetch_optional(&self.pool)
                    .await?;

                // Fetch media slots for the page
                let rows: Vec<row::PageMedia> = sqlx::query_as(queries::GET_PAGE_MEDIA.sql)
                    .bind(page_id)
                    .fetch_all(&self.pool)
                    .await?;
                let media: Vec<MediaSlot> = rows
                    .into_iter()
                    .map(row::PageMedia::into_domain)
                    .collect::<DbResult<_>>()?;

                page_row
                    .map(|r| r.into_domain(media).map(ResolvedContent::Page))
                    .transpose()?
            }
            ResolvedTarget::Media(media_id) => {
                // Fetch direct media
                let row: Option<row::Media> = sqlx::query_as(queries::GET_MEDIA_BY_ID.sql)
                    .bind(media_id)
                    .fetch_optional(&self.pool)
                    .await?;
                row.map(|r| r.into_domain().map(ResolvedContent::Media))
                    .transpose()?
            }
        };

        Ok(Some(ResearchUrlWithResolved {
            research_url,
            resolved,
        }))
    }
}
