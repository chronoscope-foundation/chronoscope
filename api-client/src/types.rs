//! Shared types that appear in API requests and responses.
//!
//! These are shared between server and client. When the `sqlx` feature is enabled,
//! enum types derive `sqlx::Type` so the database layer can use them directly.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ==================== Bbox ====================

/// Private deserialization target for bounding box parameters.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct RawBbox {
    min_lat: f64,
    max_lat: f64,
    min_lon: f64,
    max_lon: f64,
}

/// A bounding box that has been validated for geographic correctness.
///
/// Can only be constructed via deserialization (which validates automatically)
/// or via `Bbox::new()`.
///
/// Invariants:
/// - Latitudes are in `[-90, 90]` and longitudes in `[-180, 180]`
/// - `min_lat <= max_lat` (latitude is never inverted)
/// - `min_lon > max_lon` is allowed (antimeridian crossing)
#[derive(Debug, Clone, Serialize)]
pub struct Bbox {
    min_lat: f64,
    max_lat: f64,
    min_lon: f64,
    max_lon: f64,
}

impl TryFrom<RawBbox> for Bbox {
    type Error = String;

    fn try_from(raw: RawBbox) -> Result<Self, String> {
        if !(-90.0..=90.0).contains(&raw.min_lat) || !(-90.0..=90.0).contains(&raw.max_lat) {
            return Err(format!(
                "Latitudes must be in [-90, 90], got min_lat={} max_lat={}",
                raw.min_lat, raw.max_lat
            ));
        }
        if !(-180.0..=180.0).contains(&raw.min_lon) || !(-180.0..=180.0).contains(&raw.max_lon) {
            return Err(format!(
                "Longitudes must be in [-180, 180], got min_lon={} max_lon={}",
                raw.min_lon, raw.max_lon
            ));
        }
        if raw.min_lat > raw.max_lat {
            return Err(format!(
                "min_lat ({}) must be <= max_lat ({})",
                raw.min_lat, raw.max_lat
            ));
        }
        // min_lon > max_lon is valid — it means the bbox crosses the antimeridian
        Ok(Self {
            min_lat: raw.min_lat,
            max_lat: raw.max_lat,
            min_lon: raw.min_lon,
            max_lon: raw.max_lon,
        })
    }
}

impl<'de> Deserialize<'de> for Bbox {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawBbox::deserialize(deserializer)?;
        Self::try_from(raw).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Bbox {
    fn schema_name() -> String {
        "Bbox".to_string()
    }

    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        RawBbox::json_schema(generator)
    }
}

impl Bbox {
    /// Create a validated bounding box.
    ///
    /// # Errors
    /// Returns an error if coordinates are out of range or latitudes are inverted.
    pub fn new(min_lat: f64, max_lat: f64, min_lon: f64, max_lon: f64) -> Result<Self, String> {
        Self::try_from(RawBbox {
            min_lat,
            max_lat,
            min_lon,
            max_lon,
        })
    }

    pub fn min_lat(&self) -> f64 {
        self.min_lat
    }
    pub fn max_lat(&self) -> f64 {
        self.max_lat
    }
    pub fn min_lon(&self) -> f64 {
        self.min_lon
    }
    pub fn max_lon(&self) -> f64 {
        self.max_lon
    }
}

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
