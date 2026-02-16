//! Schema types for analysis results.
//!
//! The pipeline outputs subimage-centric results: each source image is split into
//! one or more subimages (panels), and each subimage gets independent SAM3 segmentation,
//! optional VLM analysis, and DINOv3 embeddings.
//!
//! Region surroundings form a constraint graph: when external knowledge identifies
//! one entity, constraints propagate along relationship edges to narrow down unknowns.
//! For example, if region 2 is identified as a building demolished in 1927, and region 1
//! shares a wall with region 2, we learn that region 1 was near that location
//! and existed before 1927.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// DINOv3 CLS embedding dimensionality.
pub const EMBEDDING_DIM: usize = 1024;

// ==================== Segmentation Types ====================

/// RLE-encoded binary mask (COCO compressed string format).
///
/// Run-length encoding alternates between background and foreground run lengths,
/// compressed into a compact ASCII string using modified LEB128 encoding.
/// This is the same format used by pycocotools.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RleMask {
    /// Compressed run lengths as COCO string (modified LEB128, +48 ASCII offset).
    /// Alternates background/foreground; first run is always background.
    pub counts: String,
}

// ==================== Shared Analysis Types ====================
//
// NOTE: The schemars descriptions below are written as instructions for the model
// producing this output. They use imperative/neutral language rather than describing
// what "the VLM" does, since the model reading them IS the VLM.

/// Color characteristics of a photograph, used as a dating signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PhotoColor {
    /// Black and white / grayscale photograph.
    #[schemars(description = "Black and white or grayscale photograph")]
    Monochrome,
    /// Sepia-toned photograph (warm brown tint).
    #[schemars(description = "Sepia-toned photograph with warm brown tint")]
    Sepia,
    /// Hand-tinted or hand-colored photograph (selective color on B&W base).
    #[schemars(description = "Hand-tinted or hand-colored: selective color applied to a B&W base")]
    HandTinted,
    /// Full color photograph.
    #[schemars(description = "Full color photograph")]
    Color,
}

/// Type of media being analyzed.
///
/// Only `Photo` carries color information (a dating signal).
/// Other variants don't need it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnalyzedMediaType {
    /// A photograph, with color characteristics.
    Photo {
        /// Color characteristics of the photograph.
        color: PhotoColor,
    },
    /// A painting, drawing, sketch, or other hand-made illustration.
    Illustration,
    /// A 3D rendering or architectural visualization.
    Rendering,
    /// A photograph of a physical architectural model.
    PhotoOfModel,
    /// A map, site plan, or floor plan.
    Map,
}

/// Scene type classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SceneType {
    /// Exterior view - outside of buildings
    #[schemars(description = "Outside view of building exteriors, street scenes, skylines")]
    Outdoor,
    /// Interior view - inside a building
    #[schemars(
        description = "Inside a building: rooms, hallways, lobbies, interior architectural details"
    )]
    Indoor,
    /// Mixed or ambiguous - courtyards, covered markets, atriums, doorway shots
    #[schemars(
        description = "Ambiguous spaces: courtyards, covered passages, atriums, views through doorways"
    )]
    Mixed,
}

// ==================== Surroundings Types ====================

/// Type of non-entity context surrounding a region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SurroundingType {
    PavedRoad,
    UnpavedRoad,
    Sidewalk,
    Alley,
    EmptyLot,
    Parking,
    #[schemars(description = "Trees, grass, garden, or park")]
    Greenery,
    #[schemars(description = "River, canal, or harbor")]
    Water,
    Railroad,
    Fence,
    #[schemars(description = "Retaining wall or boundary wall")]
    Wall,
}

/// Type of spatial relationship between two regions.
///
/// These map to RCC-8 spatial relations for the constraint reasoning engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationType {
    /// RCC-8: EC.
    SharesWall,
    /// RCC-8: EC.
    Abutting,
    /// RCC-8: DC.
    NarrowSeparation,
    /// RCC-8: DC.
    AcrossStreet,
    /// RCC-8: DC.
    SameBlock,
    /// RCC-8: DC.
    VisibleNearby,
    /// RCC-8: NTPP.
    InsideCompound,
}

/// 0-based index into the subimage's `regions` array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct RegionIndex(pub u32);

/// A relationship from this region to another numbered region.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegionRelation {
    pub region_index: RegionIndex,
    pub relation: RelationType,
}

/// Surroundings of a region: non-entity context and relationships to other regions.
///
/// Each region describes both what non-entity things are around it AND its
/// relationships to other numbered regions. Relations reference only regions
/// with a lower index (the solver reconstructs the full symmetric graph).
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Surroundings {
    pub non_entity: Vec<SurroundingType>,

    #[schemars(
        description = "Other non-entity surroundings not covered above: 'cemetery', 'construction site'"
    )]
    pub other_non_entity: Vec<String>,

    #[schemars(
        description = "Spatial relationships to regions with LOWER index only (0-based). The solver reconstructs symmetric relations."
    )]
    pub related_regions: Vec<RegionRelation>,
}

/// Type of built structure in a region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    Building,
    Bridge,
    Tower,
    Monument,
    /// Walls, piers, dams, etc.
    Infrastructure,
    /// Not a built structure (trees, sky, streets, vehicles).
    NonStructure,
}

/// VLM analysis of a single numbered region.
///
/// Contains only what the VLM produces. The BLS orchestrator wraps this in a
/// [`Region`] which adds segmentation mask, confidence, and DINOv3 embedding.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegionAnalysis {
    pub entity_type: EntityType,

    #[schemars(description = "Brief factual description of what's visible")]
    pub description: String,

    pub surroundings: Surroundings,
}

// ==================== Analysis Pipeline Types ====================

/// Axis-aligned bounding box in pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoundingBox {
    /// X offset from left edge of the source image.
    pub x: u32,
    /// Y offset from top edge of the source image.
    pub y: u32,
    /// Width of the bounding box in pixels.
    pub width: u32,
    /// Height of the bounding box in pixels.
    pub height: u32,
}

/// Bounds of a subimage within the source image.
///
/// The `bbox` defines the coordinate rectangle in the source image.
/// The `mask` is in the **crop coordinate space** — its dimensions are
/// `(bbox.height, bbox.width)`. For rectangular crops the mask is all-ones
/// (RLE: `[0, H*W]`). For non-rectangular crops (e.g. diagonal panel borders)
/// the mask indicates which pixels within the bbox belong to this subimage.
///
/// All [`Region`] masks within a [`Subimage`] share these same dimensions.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SubimageBounds {
    /// Bounding box in source image coordinates.
    pub bbox: BoundingBox,
    /// Which pixels within the bbox belong to this subimage.
    /// Dimensions: `(bbox.height, bbox.width)`.
    pub mask: RleMask,
}

/// A unified region combining SAM3 segmentation, optional VLM analysis, and DINOv3 embedding.
///
/// Regions are ordered left-to-right by centroid; position in the array IS identity
/// (0-based index matches annotation labels).
///
/// Masks are in the subimage crop coordinate space — same dimensions as
/// the parent [`Subimage`]'s `bounds.bbox` (height x width).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Region {
    /// SAM3 segmentation confidence for this region.
    pub segmentation_confidence: f64,

    /// The SAM3 text prompt that detected this region (e.g. "building", "tower").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detected_as: String,

    /// Region mask, RLE-encoded in subimage crop coordinates.
    pub mask: RleMask,

    /// DINOv3 CLS embedding for this region crop (1024 dims, L2-normalized).
    /// `None` if embedding was not computed or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,

    /// VLM analysis of this region. `None` when VLM was skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis: Option<RegionAnalysis>,

    /// Sub-features detected within this region (e.g. towers within a building).
    /// One level of nesting only — features do not have their own features.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<Region>,
}

/// Scene-level analysis from the VLM.
///
/// Contains the scene-level fields that the VLM produces. Used directly in both
/// [`vlm_schema::AnalyzedOutput`] (for JSON schema generation) and
/// [`SubimageAnalysis::Analyzed`] (in assembled results), eliminating field duplication.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SceneAnalysis {
    /// Type of media
    #[schemars(description = "What type of visual media is this?")]
    pub media_type: AnalyzedMediaType,

    /// Brief factual summary
    #[schemars(description = "1-2 sentence factual description of what the image shows")]
    pub content_summary: String,

    /// Scene type
    #[schemars(description = "Is this an outdoor, indoor, or mixed/ambiguous scene?")]
    pub scene_type: SceneType,
}

/// Analysis result for a single subimage.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SubimageAnalysis {
    /// Subimage was analyzed successfully (VLM + SAM3 + DINOv3).
    Analyzed {
        /// VLM scene-level analysis (shared type with VLM schema).
        scene: SceneAnalysis,
        /// DINOv3 CLS embedding for the whole subimage crop (1024 dims, L2-normalized).
        /// `None` if embedding was not computed or failed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        embedding: Option<Vec<f32>>,
        /// Detected and analyzed regions within this subimage.
        regions: Vec<Region>,
    },
    /// Subimage was segmented but VLM was skipped (SAM3 + DINOv3 only).
    Segmented {
        /// DINOv3 CLS embedding for the whole subimage crop (1024 dims, L2-normalized).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        embedding: Option<Vec<f32>>,
        /// Detected regions (no VLM analysis, only segmentation + embeddings).
        regions: Vec<Region>,
    },
    /// Subimage was rejected (not relevant for historical building research).
    Rejected {
        /// Brief reason for rejection.
        reason: String,
    },
    /// An error occurred during analysis of this subimage.
    /// Distinct from Rejected: Rejected means the VLM intentionally
    /// determined the content isn't relevant. Error means something
    /// went wrong during processing.
    Error {
        /// Error message describing what went wrong.
        message: String,
    },
}

impl SubimageAnalysis {
    /// Extract the DINOv3 embedding, if present.
    pub fn embedding(&self) -> Option<&[f32]> {
        match self {
            Self::Analyzed { embedding, .. } | Self::Segmented { embedding, .. } => {
                embedding.as_deref()
            }
            _ => None,
        }
    }

    /// Extract the region list, if this is an analyzed or segmented variant.
    pub fn regions(&self) -> Option<&[Region]> {
        match self {
            Self::Analyzed { regions, .. } | Self::Segmented { regions, .. } => Some(regions),
            _ => None,
        }
    }
}

/// A subimage (panel) detected within the source image.
///
/// All masks within a subimage — both the bounds mask and per-region masks — share
/// dimensions `(bounds.bbox.height, bounds.bbox.width)`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Subimage {
    /// Bounds of this subimage within the source image.
    pub bounds: SubimageBounds,
    /// Analysis result (analyzed, segmented, rejected, or error).
    pub analysis: SubimageAnalysis,
}

/// Model versions used to produce an analysis result.
/// Enables reproducibility tracking: given the same image and these versions,
/// the same result should be produced.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelVersions {
    /// VLM model identifier (e.g., `"Qwen/Qwen3-VL-32B-Instruct"`).
    pub vlm: String,
    /// SAM3 model identifier.
    pub sam3: String,
    /// DINOv3 model identifier.
    pub dinov3: String,
    /// Git commit SHA of the analysis pipeline code, for reproducibility.
    pub git_sha: String,
}

/// Full analysis result from the analysis pipeline.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AnalysisResult {
    /// Analysis completed (one or more subimages processed).
    Success {
        /// Detected subimages (panels) with their analysis results.
        /// For single-image inputs, this contains exactly one entry covering the full image.
        subimages: Vec<Subimage>,
        /// Model versions used for this analysis run.
        versions: ModelVersions,
    },
    /// Image was rejected before analysis (e.g., too large, unsupported format).
    ImageRejected {
        /// Reason for rejection.
        reason: String,
    },
}

/// Types used exclusively for generating the JSON schema passed to the VLM
/// for constrained output. The VLM doesn't produce masks, embeddings, or
/// chain-of-thought in its structured JSON — those are added by the BLS
/// orchestrator, which merges VLM output with SAM3 and DINOv3 results to
/// produce the final [`SubimageAnalysis`].
///
/// These types must stay in sync with their assembled counterparts:
/// - [`SubimageOutput`] ↔ [`SubimageAnalysis`]
/// - [`AnalyzedOutput`] ↔ [`SubimageAnalysis::Analyzed`]
///
/// The shared leaf types ([`SceneAnalysis`], [`RegionAnalysis`], etc.) are
/// defined in the parent module and used by both.
pub mod vlm_schema {
    use super::*;

    /// VLM output for a single subimage — either analyzed or rejected.
    ///
    /// Used to generate the JSON schema passed to the VLM for constrained output.
    /// The BLS orchestrator merges this with SAM3 and DINOv3 results to produce
    /// [`SubimageAnalysis`].
    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    #[serde(tag = "status", rename_all = "snake_case")]
    pub enum SubimageOutput {
        /// Subimage contains relevant built structures.
        Analyzed(AnalyzedOutput),
        /// Subimage is not relevant for historical building research.
        Rejected {
            /// Brief reason for rejection.
            #[schemars(
                description = "Brief reason if rejected: e.g., 'portrait photo', 'natural landscape', 'vehicle photo', 'meme', 'food photo', 'animal photo'"
            )]
            reason: String,
        },
    }

    /// VLM analysis output for an analyzed subimage.
    ///
    /// The BLS orchestrator wraps this in [`SubimageAnalysis::Analyzed`], which adds
    /// DINOv3 embeddings and promotes `RegionAnalysis` to full [`Region`] with masks.
    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct AnalyzedOutput {
        /// Scene-level analysis.
        pub scene: SceneAnalysis,

        /// Analysis for each detected region, in the same order as the annotated labels (0-indexed).
        /// Every region must have an entry; use `non_structure` `entity_type` for non-structures.
        #[schemars(
            description = "Array of analyses, one per SAM3 region in order. Index = region number (0-based). Every region must have an entry; use non_structure entity_type for non-structures."
        )]
        pub regions: Vec<RegionAnalysis>,
    }
}

/// Request to analyze an image.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisRequest {
    /// Base64-encoded image data.
    pub image: String,

    /// JSON schema for constrained VLM output.
    pub schema: serde_json::Value,
}
