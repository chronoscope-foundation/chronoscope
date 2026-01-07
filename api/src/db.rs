use chrono::NaiveDateTime;
use dropshot::HttpError;
use sqlx::FromRow;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::str::FromStr;
use thiserror::Error;
use webauthn_rs::prelude::Passkey;

use crate::queries;
use crate::types::{Email, ResearchUrlId, UserId};

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
}

pub type DbResult<T> = Result<T, DbError>;

// ==================== Data Types ====================

/// A user account
#[derive(Debug, Clone, FromRow)]
pub struct User {
    pub id: UserId,
    pub username: String,
    pub email: Email,
    pub created_at: NaiveDateTime,
}

/// A research URL (canonical, deduplicated)
#[derive(Debug, Clone, FromRow)]
pub struct ResearchUrl {
    pub id: ResearchUrlId,
    pub url: String,
    pub created_at: NaiveDateTime,
}

/// A research URL that a user follows (includes follow timestamp)
#[derive(Debug, Clone, FromRow)]
pub struct FollowedUrl {
    pub id: ResearchUrlId,
    pub url: String,
    pub created_at: NaiveDateTime,
    pub followed_at: NaiveDateTime,
}

// ==================== Database ====================

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
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
        // Get or create the URL
        let existing: Option<(ResearchUrlId,)> = sqlx::query_as(queries::GET_URL_BY_URL.sql)
            .bind(url)
            .fetch_optional(&self.pool)
            .await?;

        let (url_id, created) = if let Some((id,)) = existing {
            (id, false)
        } else {
            let id = ResearchUrlId::generate();
            queries::CREATE_URL
                .query()
                .bind(&id)
                .bind(url)
                .execute(&self.pool)
                .await?;
            (id, true)
        };

        // Follow the URL (ignore if already following)
        queries::CREATE_FOLLOW
            .query()
            .bind(user_id)
            .bind(&url_id)
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
