//! The SQLite backend's id scheme: `i64` newtypes minted dense from the
//! `fact_counters` row — SQLite's own `INTEGER` shape, so the ids bind and
//! fetch without conversion. Minting from zero upward keeps them
//! non-negative; the known-id checks treat anything outside `0..counter`
//! (a wire-supplied negative included) as unminted.
//!
//! The wire and storage form (canonical decimal-string ids, bare-string
//! schema) comes from core's shared `subject_id_newtype!`, so this scheme
//! cannot drift from the other backends' wire behavior.

use schemars::JsonSchema;

use chronoscope_core::grammar::ids::IdScheme;
use chronoscope_core::subject_id_newtype;

subject_id_newtype!(
    SqliteEntityId,
    i64,
    "entity",
    "SQLite-backed entity id — an `i64` newtype minted from the store's entity counter."
);
subject_id_newtype!(
    SqliteEventId,
    i64,
    "event",
    "SQLite-backed lifetime-event id — an `i64` newtype minted from the store's event counter."
);
subject_id_newtype!(
    SqliteImageId,
    i64,
    "image",
    "SQLite-backed image id — an `i64` newtype minted from the store's image counter."
);

/// The SQLite backend's id scheme: the three `i64`-newtype id kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, JsonSchema)]
pub struct SqliteIds;

impl IdScheme for SqliteIds {
    type Entity = SqliteEntityId;
    type Event = SqliteEventId;
    type Image = SqliteImageId;
}
