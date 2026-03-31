//! Ingestion-time index types.
//!
//! These are local indices into the vectors within an [`IngestionBundle`].
//! They provide type safety during ingestion — preventing accidental confusion
//! between entity, source, and link references.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Index into the entity map of an ingestion bundle.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct EntityIdx(usize);

impl EntityIdx {
    #[must_use]
    pub fn new(idx: usize) -> Self {
        Self(idx)
    }
}

/// Index into the image/source map of an ingestion bundle.
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

/// Index into the external-link map of an ingestion bundle.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct LinkIdx(usize);

impl LinkIdx {
    #[must_use]
    pub fn new(idx: usize) -> Self {
        Self(idx)
    }
}

/// Complete ingestion output with typed indices.
pub type IngestionOutput = chronoscope_core::IngestionBundle<EntityIdx, SourceIdx, LinkIdx>;

/// An entity relation using ingestion-time indices.
pub type IngestionRelation = chronoscope_core::EntityRelation<EntityIdx, SourceIdx>;
