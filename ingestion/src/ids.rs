//! Ingestion-time index types.
//!
//! Local indices into the per-item collections built during ingestion. They
//! provide type safety — preventing accidental confusion with other numeric
//! references.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Index into the image/source collection of an ingested item.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct SourceIdx(usize);

impl SourceIdx {
    #[must_use]
    pub fn new(idx: usize) -> Self {
        Self(idx)
    }
}
