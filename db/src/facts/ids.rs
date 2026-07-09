//! The SQLite backend's id scheme: `i64` newtypes minted dense from the
//! `fact_counters` row — SQLite's own `INTEGER` shape, so the ids bind and
//! fetch without conversion. Minting from zero upward keeps them
//! non-negative; the known-id checks treat anything outside `0..counter`
//! (a wire-supplied negative included) as unminted.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use chronoscope_core::grammar::ids::IdScheme;

/// SQLite-backed entity id — an `i64` newtype minted from the store's entity
/// counter.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct SqliteEntityId(pub i64);

impl std::fmt::Display for SqliteEntityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "entity-{}", self.0)
    }
}

/// SQLite-backed lifetime-event id — an `i64` newtype minted from the
/// store's event counter.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct SqliteEventId(pub i64);

impl std::fmt::Display for SqliteEventId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "event-{}", self.0)
    }
}

/// SQLite-backed image id — an `i64` newtype minted from the store's image
/// counter.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct SqliteImageId(pub i64);

impl std::fmt::Display for SqliteImageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "image-{}", self.0)
    }
}

/// The SQLite backend's id scheme: the three `i64`-newtype id kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, JsonSchema)]
pub struct SqliteIds;

impl IdScheme for SqliteIds {
    type Entity = SqliteEntityId;
    type Event = SqliteEventId;
    type Image = SqliteImageId;
}
