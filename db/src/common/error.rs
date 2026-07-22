//! Backend-shared codec (marshalling) failures.
//!
//! The row ⇄ domain codecs in [`super::storage`] are pure Rust, free of any
//! backend's SQL type — a failure there is a marshalling failure, not a
//! database failure. Each SQLite/Postgres backend error `#[from]`-wraps this
//! one so those pure steps report uniformly while the backend keeps its own
//! SQL-error arm.

use super::convert::IdConvertError;

/// Backend-shared codec (marshalling) failures.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
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
    /// A commit's stored form couldn't be assembled from its submit result
    /// — a driver bug, surfaced loudly.
    #[error("encoding commit row: {message}")]
    CommitRow { message: String },
}

/// Tag a `serde_json::Error` with the column and direction that hit it.
pub(crate) fn json(context: &'static str) -> impl FnOnce(serde_json::Error) -> CodecError {
    move |source| CodecError::Json { context, source }
}
