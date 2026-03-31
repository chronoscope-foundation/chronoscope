//! ID types for external system references.
//!
//! These provide type-safe wrappers around identifiers from external systems
//! (OpenStreetMap, Wikidata, GeoNames, etc.). They are domain concepts — references
//! to entities in other knowledge bases.
//!
//! Internal infrastructure IDs (`EntityId`, `SourceId`, etc.) live in the
//! `api-client` crate. Ingestion-time indices (`EntityIdx`, `SourceIdx`,
//! `LinkIdx`) live in the `ingestion` crate.

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
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(feature = "sqlx", sqlx(type_name = "TEXT", rename_all = "snake_case"))]
#[serde(rename_all = "snake_case")]
pub enum OsmElementType {
    Node,
    Way,
    Relation,
}

/// `OpenHistoricalMap` element ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct OhmId(pub i64);

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

/// Reference to an external event that triggered a transition.
///
/// Currently a simple string wrapper (e.g., Wikidata event ID "Q362" for WWII).
// TODO: parametrize over a proper event reference type when events are implemented
// as a first-class concept in the system.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct TriggerEventId(pub String);
