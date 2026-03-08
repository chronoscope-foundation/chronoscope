//! ID types and newtypes for Chronoscope entities.
//!
//! These provide type-safe wrappers around identifiers to prevent mixing up different ID types.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Newtype wrapper for `OpenStreetMap` element IDs
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct OsmId(pub i64);

/// `OpenStreetMap` element types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OsmElementType {
    Node,
    Way,
    Relation,
}

/// Newtype wrapper for Entity IDs
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct EntityId(pub String);

/// Newtype wrapper for Image IDs
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ImageId(pub String);

/// Newtype wrapper for Map IDs
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct MapId(pub String);

/// Newtype wrapper for Document IDs
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct DocumentId(pub String);

/// Newtype wrapper for Source IDs (maps, photos, documents)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct SourceId(pub String);

/// Reference to an external event that triggered a transition
/// e.g., Wikidata event ID "Q362" for WWII, "Q7944" for 1906 SF earthquake
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct TriggerEventId(pub String);

/// Wikidata entity ID (e.g., "Q12345")
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct WikidataEntityId(pub String);

/// Wikidata property ID (e.g., "P571" for inception)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct WikidataPropertyId(pub String);

/// `GeoNames` geographical feature ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct GeoNamesId(pub i64);

/// Getty Thesaurus of Geographic Names entry ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct GettyTgnId(pub i64);

/// `OpenHistoricalMap` element ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct OhmId(pub i64);

/// Index into an ingestion entity list.
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

/// Index into an ingestion source list (images, maps, documents).
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

/// Index into an ingestion external link list.
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
