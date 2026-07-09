//! The SQLite fact-store backend error.
//!
//! Backend failures only — submit-pipeline domain errors flow through
//! [`SubmitError`](chronoscope_core::submit::SubmitError) inside
//! `SubmitCommitError::Submit`. Every arm carries the context its
//! construction site knew, so a production failure names what was being
//! done, not just that SQL failed.

use super::convert::IdConvertError;

/// Non-domain failures out of [`SqliteFactStore`](super::SqliteFactStore).
#[derive(Debug, thiserror::Error)]
pub enum SqliteFactStoreError {
    /// A SQL statement failed. `context` names the operation.
    #[error("sqlite failure while {context}: {source}")]
    Sql {
        context: &'static str,
        #[source]
        source: sqlx::Error,
    },
    /// A fact id crossed the `u64` ⇄ `i64` row boundary out of range.
    #[error(transparent)]
    IdConvert(#[from] IdConvertError),
    /// A stored JSON column failed to (de)serialize. `context` names the
    /// column and direction.
    #[error("JSON {context}: {source}")]
    Json {
        context: &'static str,
        #[source]
        source: serde_json::Error,
    },
    /// A recorded commit's claim updated no row: the fact id was never
    /// staged here, or another commit already claimed it.
    #[error("commit {commit_id} could not claim fact {fact_id}: no unclaimed staged row")]
    FactClaim { commit_id: String, fact_id: u64 },
    /// The pre-commit audit found a staged fact no recorded commit claimed;
    /// the whole transaction is refused.
    #[error(
        "transaction refused at commit: staged fact {fact_id} has no recorded commit standing \
         behind it"
    )]
    UnclaimedStaging { fact_id: i64 },
    /// A commit's stored form couldn't be assembled from its submit result
    /// — a driver bug, surfaced loudly.
    #[error("encoding commit row: {message}")]
    CommitRow { message: String },
}

/// Tag a `sqlx::Error` with the operation that hit it:
/// `.map_err(sql("staging fact"))`.
pub(super) fn sql(context: &'static str) -> impl FnOnce(sqlx::Error) -> SqliteFactStoreError {
    move |source| SqliteFactStoreError::Sql { context, source }
}

/// Tag a `serde_json::Error` with the column and direction that hit it.
pub(super) fn json(
    context: &'static str,
) -> impl FnOnce(serde_json::Error) -> SqliteFactStoreError {
    move |source| SqliteFactStoreError::Json { context, source }
}
