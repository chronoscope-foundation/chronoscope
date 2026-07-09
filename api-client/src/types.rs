//! Shared types that appear in API requests and responses.
//!
//! These are shared between server and client. When the `sqlx` feature is enabled,
//! enum types derive `sqlx::Type` so the database layer can use them directly.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ==================== Enums ====================

/// Status of a research URL in the processing pipeline.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    strum::Display,
    strum::AsRefStr,
)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(feature = "sqlx", sqlx(type_name = "TEXT", rename_all = "lowercase"))]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ResearchUrlStatus {
    /// Submitted, waiting to be fetched
    Pending,
    /// Currently being processed by a worker
    Processing,
    /// All automated processing complete
    Complete,
    /// Processing failed (see `error_message`)
    Failed,
}

/// Type of media content.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    strum::Display,
    strum::AsRefStr,
)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(feature = "sqlx", sqlx(type_name = "TEXT", rename_all = "snake_case"))]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum MediaType {
    Image,
    Video,
}

/// Administrative zone type for geographic clustering.
///
/// Ordered from coarsest to finest granularity. The DB column stores these
/// as `snake_case` TEXT; the Rust code uses this enum for type safety.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    strum::Display,
    strum::AsRefStr,
    strum::EnumString,
)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(feature = "sqlx", sqlx(type_name = "TEXT", rename_all = "snake_case"))]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ZoneType {
    Country,
    State,
    StateDistrict,
    City,
}
