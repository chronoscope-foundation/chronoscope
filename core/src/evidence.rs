//! Evidence and citation types.
//!
//! Types for tracking the provenance of claims about entities.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::ids::{WikidataEntityId, WikidataPropertyId};

/// A value with its supporting evidence.
///
/// Used to pair claims (dates, locations, names) with the evidence that supports them.
///
/// # Usage patterns
/// - `None` = we don't know this value (no citation needed)
/// - `Some(Cited { value, evidence: [] })` = we claim X but have no evidence yet
/// - `Some(Cited { value, evidence: [...] })` = we claim X with supporting evidence
///
/// Generic over `S` (source reference type), following the same parametricity pattern
/// as `Annotation<S, E>` and `Entity<E, S>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(bound(deserialize = "T: serde::de::DeserializeOwned, S: serde::de::DeserializeOwned"))]
pub struct Cited<T, S> {
    pub value: T,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence<S>>,
}

impl<T, S> Cited<T, S> {
    #[must_use]
    pub fn new(value: T, evidence: Vec<Evidence<S>>) -> Self {
        Self { value, evidence }
    }

    /// Create a value without evidence (evidence can be added later).
    #[must_use]
    pub fn uncited(value: T) -> Self {
        Self {
            value,
            evidence: vec![],
        }
    }

    /// Map the inner value while preserving evidence.
    #[must_use]
    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> Cited<U, S> {
        Cited {
            value: f(self.value),
            evidence: self.evidence,
        }
    }
}

/// RLE-encoded binary mask (COCO compressed string format).
///
/// Run-length encoding alternates between background and foreground run lengths,
/// compressed into a compact ASCII string using modified LEB128 encoding.
/// This is the same format used by pycocotools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RleMask {
    /// Compressed run lengths as COCO string (modified LEB128, +48 ASCII offset).
    /// Alternates background/foreground; first run is always background.
    pub counts: String,
}

/// Evidence supporting a claim about an entity.
///
/// Generic over `S` (source reference type). The `Source` variant uses `S` to reference
/// an image, map, or document in our system. Other variants reference external systems
/// (web URLs, Wikidata, DBpedia) with their own identifiers.
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[serde(bound(deserialize = "S: serde::de::DeserializeOwned"))]
pub enum Evidence<S> {
    /// Evidence from a source in our system (image, map, or document).
    Source {
        source_id: S,
        #[serde(flatten)]
        detail: SourceDetail,
    },
    /// Evidence from a web page.
    #[serde(rename = "web_evidence")]
    Web {
        #[schemars(with = "String")]
        source_url: Url,
        excerpt: Option<String>,
    },
    /// Evidence from `DBpedia`.
    Dbpedia {
        /// `DBpedia` version (e.g., "2022.12.01")
        version: String,
        /// The `DBpedia` resource URI
        #[schemars(with = "String")]
        resource_uri: Url,
        /// Properties and their values that support this claim
        properties: std::collections::BTreeMap<String, String>,
    },
    /// Evidence from Wikidata.
    Wikidata {
        entity_id: WikidataEntityId,
        property_id: WikidataPropertyId,
        /// The property value as it appears in Wikidata
        property_value: String,
        /// Entity revision ID for permalink construction
        revision_id: u64,
    },
}

/// Detail for source evidence — what kind of source and per-type metadata.
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "source_type", rename_all = "snake_case")]
pub enum SourceDetail {
    /// Photographic image evidence.
    Image { region: Option<ImageRegion> },
    /// Historical map evidence.
    Map { region: Option<ImageRegion> },
    /// Document evidence (book, article, report, etc.).
    Document {
        excerpt: Option<String>,
        page_number: Option<i32>,
    },
}

/// Dimensions of a binary mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MaskDimensions {
    pub height: std::num::NonZeroU32,
    pub width: std::num::NonZeroU32,
}

/// Region within an image (for images and maps).
///
/// Uses COCO compressed RLE format (the same format used by SAM3/pycocotools).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImageRegion {
    /// Compressed RLE mask.
    pub mask: RleMask,
    /// Dimensions of the mask.
    pub size: MaskDimensions,
}

/// A polyline defined by a sequence of points.
///
/// Used for linear spatial features like roads, paths, or boundaries
/// that are better represented as lines than masks.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Polyline {
    /// Ordered sequence of `[x, y]` points (in pixel coordinates).
    /// Must contain at least 2 points.
    points: Vec<[f64; 2]>,
}

impl<'de> Deserialize<'de> for Polyline {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            points: Vec<[f64; 2]>,
        }
        let raw = Raw::deserialize(deserializer)?;
        Polyline::new(raw.points).map_err(serde::de::Error::custom)
    }
}

/// Errors from polyline construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolylineError {
    /// A polyline requires at least 2 points.
    TooFewPoints { count: usize },
}

impl fmt::Display for PolylineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooFewPoints { count } => {
                write!(f, "polyline requires at least 2 points, got {count}")
            }
        }
    }
}

impl std::error::Error for PolylineError {}

impl Polyline {
    /// Create a new polyline from a sequence of points.
    ///
    /// Returns `Err(PolylineError::TooFewPoints)` if fewer than 2 points are provided.
    pub fn new(points: Vec<[f64; 2]>) -> Result<Self, PolylineError> {
        if points.len() < 2 {
            return Err(PolylineError::TooFewPoints {
                count: points.len(),
            });
        }
        Ok(Self { points })
    }

    /// The points making up this polyline.
    #[must_use]
    pub fn points(&self) -> &[[f64; 2]] {
        &self.points
    }
}

/// Spatial geometry for annotations — either a mask or a polyline.
///
/// Serializes as internally tagged JSON: `{"type": "mask", ...fields}` or
/// `{"type": "polyline", ...fields}`. Inner struct fields are flattened alongside
/// the `type` discriminator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SpatialGeometry {
    /// Region mask (from SAM3 segmentation or manual annotation).
    Mask(ImageRegion),
    /// Polyline trace (for roads, paths, boundaries).
    Polyline(Polyline),
}
