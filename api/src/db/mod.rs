mod workers;

use chrono::{NaiveDateTime, Utc};
use dropshot::HttpError;
use sqlx::FromRow;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::str::FromStr;
use thiserror::Error;
use webauthn_rs::prelude::Passkey;

use crate::queries;
use crate::types::{
    Email, MediaId, MediaType, PageId, ResearchUrlId, ResearchUrlStatus, SourceType, UserId,
};

#[derive(Error, Debug)]
pub enum DbError {
    #[error("Database error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("Migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Query plan verification failed: {0}")]
    QueryPlan(#[from] queries::QueryPlanError),

    #[error("User not found")]
    UserNotFound,

    #[error("Credential not found")]
    CredentialNotFound,

    #[error("Invalid argument: {0}")]
    InvalidArgument(String),
}

pub type DbResult<T> = Result<T, DbError>;

// ==================== Data Types ====================

/// GPS location with latitude, longitude, and optional altitude.
/// Designed to match future PostGIS/Spatialite POINT type.
#[derive(Debug, Clone)]
pub struct GpsLocation {
    pub latitude: f64,
    pub longitude: f64,
    pub altitude: Option<f64>,
}

impl GpsLocation {
    /// Reconstruct GPS location from separate lat/lon/alt columns.
    /// Returns None if lat/lon are missing or only partially present.
    pub(crate) fn from_columns(
        lat: Option<f64>,
        lon: Option<f64>,
        alt: Option<f64>,
    ) -> Option<Self> {
        match (lat, lon) {
            (Some(latitude), Some(longitude)) => Some(GpsLocation {
                latitude,
                longitude,
                altitude: alt,
            }),
            _ => None,
        }
    }

    /// Decompose an optional GPS location into separate column values for database storage.
    pub(crate) fn to_columns(location: Option<&Self>) -> (Option<f64>, Option<f64>, Option<f64>) {
        match location {
            Some(loc) => (Some(loc.latitude), Some(loc.longitude), loc.altitude),
            None => (None, None, None),
        }
    }
}

/// A user account
#[derive(Debug, Clone, FromRow)]
pub struct User {
    pub id: UserId,
    pub username: String,
    pub email: Email,
    pub created_at: NaiveDateTime,
}

/// A research URL (canonical, deduplicated)
/// Note: page_id and media_id are mutually exclusive (enforced by DB constraint)
#[derive(Debug, Clone, FromRow)]
pub struct ResearchUrl {
    pub id: ResearchUrlId,
    pub url: String,
    pub page_id: Option<PageId>,
    pub media_id: Option<MediaId>,
    pub status: ResearchUrlStatus,
    pub created_at: NaiveDateTime,
}

/// A research URL that a user follows (includes follow timestamp)
#[derive(Debug, Clone, FromRow)]
pub struct FollowedUrl {
    #[sqlx(flatten)]
    pub research_url: ResearchUrl,
    pub followed_at: NaiveDateTime,
}

/// A media slot - a URL reference that may or may not be resolved to Media yet
#[derive(Debug, Clone)]
pub struct MediaSlot {
    pub url: String,
    pub resolved: Option<Media>,
}

impl MediaSlot {
    /// Create a slot for a URL that hasn't been fetched yet
    pub fn pending(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            resolved: None,
        }
    }
}

/// Core page data (used for both creation and reading)
#[derive(Debug, Clone)]
pub struct PageData {
    pub source_type: SourceType,
    pub title: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<NaiveDateTime>,
    pub content: Option<String>,
    pub fetched_at: NaiveDateTime,
    /// Media referenced by this page (in source order)
    pub media: Vec<MediaSlot>,
}

/// A page with database-generated fields
#[derive(Debug, Clone)]
pub struct Page {
    pub id: PageId,
    pub data: PageData,
    pub created_at: NaiveDateTime,
}

/// Internal row type for reading pages from DB (no media, fetched separately)
#[derive(Debug, FromRow)]
struct PageDbRow {
    id: PageId,
    source_type: SourceType,
    title: Option<String>,
    author: Option<String>,
    published_at: Option<NaiveDateTime>,
    content: Option<String>,
    fetched_at: NaiveDateTime,
    created_at: NaiveDateTime,
}

/// Core media data (used for both creation and reading)
#[derive(Debug, Clone)]
pub struct MediaData {
    pub exact_hash: Vec<u8>,
    pub perceptual_hash: Option<Vec<u8>>,
    pub storage_key: String,
    pub media_type: MediaType,
    pub width: i32,
    pub height: i32,
    pub duration_seconds: Option<f32>,
    pub captured_at: Option<NaiveDateTime>,
    pub location: Option<GpsLocation>,
    pub source_metadata: Option<String>, // JSON stored as text
    pub fetched_at: NaiveDateTime,
}

/// A media item with database-generated fields
#[derive(Debug, Clone)]
pub struct Media {
    pub id: MediaId,
    pub data: MediaData,
    pub created_at: NaiveDateTime,
}

/// Internal row type for sqlx (maps to flat DB columns)
#[derive(Debug, FromRow)]
pub(crate) struct MediaDbRow {
    id: MediaId,
    exact_hash: Vec<u8>,
    perceptual_hash: Option<Vec<u8>>,
    storage_key: String,
    media_type: MediaType,
    width: i32,
    height: i32,
    duration_seconds: Option<f32>,
    captured_at: Option<NaiveDateTime>,
    gps_latitude: Option<f64>,
    gps_longitude: Option<f64>,
    gps_altitude: Option<f64>,
    source_metadata: Option<String>,
    fetched_at: NaiveDateTime,
    created_at: NaiveDateTime,
}

impl MediaDbRow {
    pub(crate) fn into_media(self) -> Media {
        Media {
            id: self.id,
            data: MediaData {
                exact_hash: self.exact_hash,
                perceptual_hash: self.perceptual_hash,
                storage_key: self.storage_key,
                media_type: self.media_type,
                width: self.width,
                height: self.height,
                duration_seconds: self.duration_seconds,
                captured_at: self.captured_at,
                location: GpsLocation::from_columns(
                    self.gps_latitude,
                    self.gps_longitude,
                    self.gps_altitude,
                ),
                source_metadata: self.source_metadata,
                fetched_at: self.fetched_at,
            },
            created_at: self.created_at,
        }
    }
}

/// Resolved content - either a page with embedded media, or direct media
#[derive(Debug, Clone)]
pub enum ResolvedContent {
    Page(Page),
    Media(Media),
}

/// Full dossier data for a research URL
#[derive(Debug, Clone)]
pub struct ResearchUrlWithResolved {
    pub research_url: ResearchUrl,
    pub resolved: Option<ResolvedContent>,
}

/// Raw row from the page media query (internal use only)
#[derive(Debug, FromRow)]
struct PageMediaRow {
    source_url: String,
    // Media fields (all optional since LEFT JOIN)
    id: Option<MediaId>,
    exact_hash: Option<Vec<u8>>,
    perceptual_hash: Option<Vec<u8>>,
    storage_key: Option<String>,
    media_type: Option<MediaType>,
    width: Option<i32>,
    height: Option<i32>,
    duration_seconds: Option<f32>,
    captured_at: Option<NaiveDateTime>,
    gps_latitude: Option<f64>,
    gps_longitude: Option<f64>,
    gps_altitude: Option<f64>,
    source_metadata: Option<String>,
    fetched_at: Option<NaiveDateTime>,
    created_at: Option<NaiveDateTime>,
}

impl PageMediaRow {
    fn into_media_slot(self) -> MediaSlot {
        let resolved = match (
            self.id,
            self.exact_hash,
            self.storage_key,
            self.media_type,
            self.width,
            self.height,
            self.fetched_at,
            self.created_at,
        ) {
            (
                Some(id),
                Some(exact_hash),
                Some(storage_key),
                Some(media_type),
                Some(width),
                Some(height),
                Some(fetched_at),
                Some(created_at),
            ) => Some(Media {
                id,
                data: MediaData {
                    exact_hash,
                    perceptual_hash: self.perceptual_hash,
                    storage_key,
                    media_type,
                    width,
                    height,
                    duration_seconds: self.duration_seconds,
                    captured_at: self.captured_at,
                    location: GpsLocation::from_columns(
                        self.gps_latitude,
                        self.gps_longitude,
                        self.gps_altitude,
                    ),
                    source_metadata: self.source_metadata,
                    fetched_at,
                },
                created_at,
            }),
            _ => None,
        };

        MediaSlot {
            url: self.source_url,
            resolved,
        }
    }
}

// ==================== Database ====================

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
}

/// Get the current UTC timestamp as NaiveDateTime for database storage.
fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

impl Database {
    /// Get access to the underlying pool (for tests that need raw queries)
    #[cfg(test)]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

impl Database {
    /// Create a new database connection pool, run migrations, and verify query plans.
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` if connection fails, `DbError::Migrate` if migrations fail,
    /// or `DbError::QueryPlan` if any query would cause a full table scan.
    pub async fn new(database_url: &str) -> DbResult<Self> {
        let options = SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        sqlx::migrate!("./migrations").run(&pool).await?;

        queries::verify_all_query_plans(&pool).await?;

        Ok(Self { pool })
    }

    /// Create a new database connection pool and run migrations, but skip query plan verification.
    ///
    /// This is used by tests that want to verify query plans themselves (to avoid circular dependency).
    #[cfg(test)]
    pub async fn new_without_plan_verification(database_url: &str) -> DbResult<Self> {
        let options = SqliteConnectOptions::from_str(database_url)?
            .create_if_missing(true)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        sqlx::migrate!("./migrations").run(&pool).await?;

        Ok(Self { pool })
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

    /// Store a new passkey credential
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` or `DbError::Json` if the operation fails.
    pub async fn add_credential(&self, user_id: &UserId, passkey: &Passkey) -> DbResult<()> {
        let credential_id = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            passkey.cred_id().as_ref(),
        );
        let passkey_json = serde_json::to_string(passkey)?;

        queries::ADD_CREDENTIAL
            .query()
            .bind(user_id)
            .bind(&credential_id)
            .bind(&passkey_json)
            .bind(now())
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Get all credentials for a user
    ///
    /// # Errors
    /// Returns `DbError::Sqlx` or `DbError::Json` if the operation fails.
    pub async fn get_credentials(&self, user_id: &UserId) -> DbResult<Vec<Passkey>> {
        let rows: Vec<(String,)> = sqlx::query_as(queries::GET_CREDENTIALS.sql)
            .bind(user_id)
            .fetch_all(&self.pool)
            .await?;

        rows.into_iter()
            .map(|(json,)| serde_json::from_str(&json).map_err(DbError::from))
            .collect()
    }

    /// Update a credential after successful authentication.
    /// Persists the incremented sign counter for future clone detection
    /// (webauthn-rs rejects authentication if the counter doesn't increase).
    ///
    /// # Errors
    /// Returns `DbError::Sqlx`, `DbError::Json`, or `DbError::CredentialNotFound`.
    pub async fn update_credential(&self, user_id: &UserId, passkey: &Passkey) -> DbResult<()> {
        let passkey_json = serde_json::to_string(passkey)?;
        let credential_id = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            passkey.cred_id().as_ref(),
        );

        let rows_affected = queries::UPDATE_CREDENTIAL
            .query()
            .bind(&passkey_json)
            .bind(user_id)
            .bind(&credential_id)
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
    /// # Errors
    /// Returns `DbError::Sqlx` if the database operation fails.
    pub async fn submit_url(&self, user_id: &UserId, url: &str) -> DbResult<(ResearchUrlId, bool)> {
        // Try to insert the URL (ignored if already exists)
        let new_id = ResearchUrlId::generate();
        let result = queries::CREATE_URL_OR_IGNORE
            .query()
            .bind(&new_id)
            .bind(url)
            .bind(ResearchUrlStatus::Pending)
            .bind(0_i32) // attempt_count
            .bind(now())
            .execute(&self.pool)
            .await?;

        // If we inserted, use our ID; otherwise fetch the existing one
        let (url_id, created) = if result.rows_affected() > 0 {
            (new_id, true)
        } else {
            let (id,): (ResearchUrlId,) = sqlx::query_as(queries::GET_URL_BY_URL.sql)
                .bind(url)
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

impl From<DbError> for HttpError {
    fn from(e: DbError) -> Self {
        // Internal message logged by Dropshot; external message sent to client
        HttpError::for_internal_error(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_db_error_to_http_error_hides_details_but_logs_them() {
        // External message should be generic (sent to client)
        // Internal message should contain details (logged by Dropshot)
        let err = DbError::UserNotFound;
        let http_err: HttpError = err.into();

        assert!(http_err.status_code.is_server_error());
        assert_eq!(http_err.external_message, "Internal Server Error");
        assert_eq!(http_err.internal_message, "User not found");

        let err = DbError::CredentialNotFound;
        let http_err: HttpError = err.into();

        assert!(http_err.status_code.is_server_error());
        assert_eq!(http_err.external_message, "Internal Server Error");
        assert_eq!(http_err.internal_message, "Credential not found");
    }

    #[test]
    fn test_db_error_display() {
        assert_eq!(DbError::UserNotFound.to_string(), "User not found");
        assert_eq!(
            DbError::CredentialNotFound.to_string(),
            "Credential not found"
        );
    }
}
