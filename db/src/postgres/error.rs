//! The Postgres fact-store backend error.
//!
//! Backend failures only — submit-pipeline domain errors flow through
//! [`SubmitError`](chronoscope_core::submit::SubmitError) inside
//! `SubmitCommitError::Submit`, and the backend-shared marshalling failures
//! through [`CodecError`](crate::common::error::CodecError). Every arm carries
//! the context its construction site knew, so a production failure names what
//! was being done, not just that SQL failed. Mirrors the SQLite backend's error
//! shape — the two share the codec arm and diverge only in the SQL-dialect
//! wording.

use chronoscope_core::geo::ViewportTilesError;

use crate::common::convert::IdConvertError;
use crate::common::error::CodecError;

/// Non-domain failures out of [`PostgresFactStore`](super::PostgresFactStore).
#[derive(Debug, thiserror::Error)]
pub enum PostgresFactStoreError {
    /// A SQL statement failed. `context` names the operation.
    #[error("postgres failure while {context}: {source}")]
    Sql {
        context: &'static str,
        #[source]
        source: sqlx::Error,
    },
    /// A recorded commit's claim updated no row: the fact id was never staged
    /// here, or another commit already claimed it.
    #[error("commit {commit_id} could not claim fact {fact_id}: no unclaimed staged row")]
    FactClaim { commit_id: String, fact_id: u64 },
    /// The pre-commit audit found a staged fact no recorded commit claimed; the
    /// whole transaction is refused.
    #[error(
        "transaction refused at commit: staged fact {fact_id} has no recorded commit standing \
         behind it"
    )]
    UnclaimedStaging { fact_id: i64 },
    /// A backend-shared codec (marshalling) step failed — id conversion, a JSON
    /// column, or commit-row assembly.
    #[error(transparent)]
    Codec(#[from] CodecError),
    /// A clustering read's viewport spanned too many tiles at the requested
    /// level, tripping the shared `viewport_tiles` guard or the clustering cap.
    /// Both mean the level is too fine for the viewport.
    #[error("clustering viewport spans too many tiles: {0}")]
    ClusterTiles(#[from] ViewportTilesError),
}

/// thiserror's `#[from] CodecError` won't chain an `IdConvertError` in one hop,
/// so the mint-id `u64_to_i64` / `i64_to_u64` sites keep their `?` through this
/// manual two-hop conversion.
impl From<IdConvertError> for PostgresFactStoreError {
    fn from(e: IdConvertError) -> Self {
        Self::Codec(e.into())
    }
}

/// Tag a `sqlx::Error` with the operation that hit it:
/// `.map_err(sql("staging fact"))`.
pub(super) fn sql(context: &'static str) -> impl FnOnce(sqlx::Error) -> PostgresFactStoreError {
    move |source| PostgresFactStoreError::Sql { context, source }
}
