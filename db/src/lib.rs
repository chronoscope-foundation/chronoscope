//! Chronoscope database layer.
//!
//! This crate provides database access for the Chronoscope platform.
//! It is used by both the API server and background workers.

pub mod error;
pub mod media_store;
pub mod models;
pub mod queries;
pub mod queue;
pub mod types;
pub mod url;
pub mod workers;

use std::str::FromStr;
use std::sync::Arc;

use chrono::{NaiveDateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

use chronoscope_integrations::IntegrationRegistry;

pub use error::{DbError, DbResult, is_unique_violation};
pub use models::{
    FollowedUrl, GpsLocation, Media, MediaData, MediaSlot, Page, PageData, ResearchUrl,
    ResearchUrlWithResolved, ResolvedContent, User,
};
pub use queue::{ANALYSIS_QUEUE, Queue, QueueConfig, QueueItem, QueueQueries, url_queue_config};
pub use types::{
    AnalysisStatus, Email, MediaAnalysisState, MediaId, MediaType, PageId, ResearchUrlId,
    ResearchUrlStatus, SourceType, UserId,
};

pub use workers::MediaForAnalysis;

use models::{MediaDbRow, PageDbRow, PageMediaRow};

/// Get the current UTC timestamp as `NaiveDateTime` for database storage.
pub(crate) fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
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

impl Database {
    /// Create a new database connection pool, run migrations, and verify query plans.
    ///
    /// Internally creates the default integration registry for URL normalization
    /// and worker affinity computation.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if connection fails, `DbError::Migrate` if migrations fail,
    /// or `DbError::QueryPlan` if any query would cause a full table scan.
    pub async fn new(database_url: &str) -> DbResult<Self> {
        let registry = chronoscope_integrations::create_registry(None)?;

        let options = SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

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
    pub async fn new_without_plan_verification(database_url: &str) -> DbResult<Self> {
        let registry = chronoscope_integrations::create_registry(None)?;

        let options = SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

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
            .map(|name| name.as_str().to_string());

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
        let resolved = if let Some(ref page_id) = research_url.page_id {
            // Fetch page row
            let page_row: Option<PageDbRow> = sqlx::query_as(queries::GET_PAGE_BY_ID.sql)
                .bind(page_id)
                .fetch_optional(&self.pool)
                .await?;

            // Fetch media slots for the page
            let rows: Vec<PageMediaRow> = sqlx::query_as(queries::GET_PAGE_MEDIA.sql)
                .bind(page_id)
                .fetch_all(&self.pool)
                .await?;
            let media: Vec<MediaSlot> = rows
                .into_iter()
                .map(PageMediaRow::into_media_slot)
                .collect();

            page_row.map(|row| {
                ResolvedContent::Page(Page {
                    id: row.id,
                    data: PageData {
                        source_type: row.source_type,
                        title: row.title,
                        author: row.author,
                        published_at: row.published_at,
                        content: row.content,
                        fetched_at: row.fetched_at,
                        media,
                    },
                    created_at: row.created_at,
                })
            })
        } else if let Some(ref media_id) = research_url.media_id {
            // Fetch direct media
            let row: Option<MediaDbRow> = sqlx::query_as(queries::GET_MEDIA_BY_ID.sql)
                .bind(media_id)
                .fetch_optional(&self.pool)
                .await?;
            row.map(|r| ResolvedContent::Media(r.into_media()))
        } else {
            None
        };

        Ok(Some(ResearchUrlWithResolved {
            research_url,
            resolved,
        }))
    }
}
