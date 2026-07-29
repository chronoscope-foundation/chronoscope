//! Backend-shared, sqlx-free codec core for the fact-store backends.
//!
//! Everything here is Rust over core's grammar types — the `i64` ⇄ `u64`
//! fact-id boundary ([`convert`]), the shared `i64` id scheme ([`ids`]), the
//! row ⇄ domain JSON codecs ([`storage`]), the clustering read's rules over
//! fetched rows ([`cluster`]), the viewport read's membership decision
//! ([`spatial`]), and the codec-failure type ([`error`]). The SQLite and
//! Postgres backends both marshal and decide through this one core, so they can
//! never drift in how a fact serializes or how a read resolves. No `sqlx` type
//! appears in it: what needs a connection takes the fetch as a closure
//! ([`cluster::resolve_entities`]).

pub(crate) mod cluster;
pub(crate) mod convert;
pub(crate) mod error;
pub(crate) mod ids;
pub(crate) mod spatial;
pub(crate) mod storage;

pub use ids::{SqlEntityId, SqlEventId, SqlIds, SqlImageId};
