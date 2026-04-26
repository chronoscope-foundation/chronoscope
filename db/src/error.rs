//! Database error types.

use thiserror::Error;

use crate::queries;

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

    #[error("Integration registry error: {0}")]
    Registry(#[from] chronoscope_integrations::RegistrationError),

    #[error("User not found")]
    UserNotFound,

    #[error("Credential not found")]
    CredentialNotFound,

    #[error("Invalid argument: {0}")]
    InvalidArgument(String),

    #[error("Bundle has invalid cross-references: {}", .0.join("; "))]
    InvalidBundle(Vec<String>),

    #[error("Inconsistent row data: {0}")]
    InconsistentRow(String),

    #[error("Configuration error: {0}")]
    Config(String),
}

pub type DbResult<T> = Result<T, DbError>;

/// Check if a database error is a uniqueness constraint violation.
#[must_use]
pub fn is_unique_violation(err: &DbError) -> bool {
    let DbError::Sqlx(sqlx::Error::Database(db_err)) = err else {
        return false;
    };
    db_err.kind() == sqlx::error::ErrorKind::UniqueViolation
}
