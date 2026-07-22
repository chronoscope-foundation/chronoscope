//! Backend-shared, sqlx-free codec core for the fact-store backends.
//!
//! Everything here is pure Rust over core's grammar types — the `i64` ⇄ `u64`
//! fact-id boundary ([`convert`]), the shared `i64` id scheme ([`ids`]), the
//! row ⇄ domain JSON codecs ([`storage`]), and the codec-failure type
//! ([`error`]). Both SQL backends (SQLite today, Postgres next) marshal through
//! this same core; no `sqlx` type appears in it, so the two backends can never
//! drift in how a fact serializes.

pub(crate) mod convert;
pub(crate) mod error;
pub(crate) mod ids;
pub(crate) mod storage;

pub use ids::{SqlEntityId, SqlEventId, SqlIds, SqlImageId};
