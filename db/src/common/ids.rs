//! The SQL backends' id scheme: `i64` newtypes minted dense from the
//! `fact_counters` row — the `BIGINT`/`INTEGER` shape both SQL backends share,
//! so the ids bind and fetch without conversion. Minting from zero upward keeps
//! them non-negative; the known-id checks treat anything outside `0..counter`
//! (a wire-supplied negative included) as unminted.
//!
//! The wire and storage form (canonical decimal-string ids, bare-string
//! schema) comes from core's shared `subject_id_newtype!`, so this scheme
//! cannot drift from the other backends' wire behavior.

use schemars::JsonSchema;

use chronoscope_core::grammar::ids::IdScheme;
use chronoscope_core::subject_id_newtype;

subject_id_newtype!(
    SqlEntityId,
    i64,
    "entity",
    "SQL-backed entity id — an `i64` newtype minted from the store's entity counter."
);
subject_id_newtype!(
    SqlEventId,
    i64,
    "event",
    "SQL-backed lifetime-event id — an `i64` newtype minted from the store's event counter."
);
subject_id_newtype!(
    SqlImageId,
    i64,
    "image",
    "SQL-backed image id — an `i64` newtype minted from the store's image counter."
);

/// The SQL backends' id scheme: the three `i64`-newtype id kinds. Both
/// `BIGINT`-typed backends (SQLite, Postgres) bind and fetch these without
/// conversion, so the api id types stay backend-independent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, JsonSchema)]
pub struct SqlIds;

impl IdScheme for SqlIds {
    type Entity = SqlEntityId;
    type Event = SqlEventId;
    type Image = SqlImageId;
}
