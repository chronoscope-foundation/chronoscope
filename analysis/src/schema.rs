//! Schema types for analysis results.
//!
//! The pipeline outputs subimage-centric results: each source image is split into
//! one or more subimages (panels), and each subimage gets independent SAM3 segmentation,
//! VLM analysis, and DINOv3 embeddings.
//!
//! Region relationships form a constraint graph: when external knowledge identifies
//! one entity, constraints propagate along relationship edges to narrow down unknowns.
//! For example, if region 2 is identified as a building demolished in 1927, and region 1
//! is marked as "adjacent" to region 2, we learn that region 1 was near that location
//! and existed before 1927.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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

/// A detected region from SAM3 segmentation.
///
/// Used by legacy pipeline output. New pipeline uses [`Region`] which merges
/// segmentation, VLM analysis, and embeddings.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DetectedRegion {
    /// Region ID matching the label in the annotated image (1-indexed).
    pub region_id: u32,

    /// Confidence score from SAM3.
    pub confidence: f32,

    /// Full precision mask, RLE-encoded.
    pub mask: RleMask,
}

// ==================== Shared Analysis Types ====================
//
// NOTE: The schemars descriptions below are written as instructions for the model
// producing this output. They use imperative/neutral language rather than describing
// what "the VLM" does, since the model reading them IS the VLM.

/// Type of media being analyzed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AnalyzedMediaType {
    /// A photograph
    Photo,
    /// A painting, drawing, sketch, or other hand-made illustration
    Illustration,
    /// A 3D rendering or architectural visualization
    Rendering,
    /// A photograph of a physical architectural model
    PhotoOfModel,
    /// A map, site plan, or floor plan
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

/// Text extracted from the image.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ExtractedText {
    /// The text content
    #[schemars(description = "The text as it appears")]
    pub text: String,

    /// Where the text appears
    #[schemars(
        description = "Location: 'storefront sign', 'cornerstone', 'awning', 'bottom margin caption', 'watermark'"
    )]
    pub location: String,
}

// ==================== Region Relationship Types ====================

/// Type of relationship between two regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationType {
    /// Regions are immediately adjacent (share a boundary or are very close).
    /// Symmetric: if A is adjacent to B, then B is adjacent to A.
    #[schemars(
        description = "Structures are next to each other on the same side of a street, possibly sharing a wall"
    )]
    Adjacent,

    /// Regions are across a street, canal, or other linear open space from each other.
    /// Symmetric: if A is across from B, then B is across from A.
    #[schemars(
        description = "Structures face each other across a street, canal, or similar linear open space"
    )]
    AcrossFrom,

    /// Both regions depict the same physical structure across different panels.
    /// Symmetric: establishes identity between regions in composite images.
    #[schemars(
        description = "Both regions show the same physical structure in different panels of a composite image (e.g., before/after views)"
    )]
    SameStructure,
}

/// A relationship between two regions.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RegionRelationship {
    /// The subject region index (0-based, matching annotation labels).
    #[schemars(description = "Subject region index (0-based), e.g. 0")]
    pub subject: u32,

    /// The object region index (0-based, matching annotation labels).
    #[schemars(description = "Object region index (0-based), e.g. 1")]
    pub object: u32,

    /// Type of relationship between subject and object.
    #[schemars(description = "How subject relates to object")]
    pub relation: RelationType,
}

/// Type of built structure in a region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    /// A building - residential, commercial, institutional, etc.
    Building,
    /// A bridge spanning water, roads, or other obstacles.
    Bridge,
    /// A tower - clock tower, bell tower, water tower, etc.
    Tower,
    /// A monument, statue, or memorial structure.
    Monument,
    /// Other infrastructure - walls, piers, dams, etc.
    Infrastructure,
    /// Not a built structure (trees, sky, streets, vehicles).
    /// Produced when the VLM determines a segmented region doesn't contain
    /// a built structure.
    NonStructure,
}

/// Analysis of a single numbered region (VLM output only — no mask/embedding).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RegionAnalysis {
    /// Type of built structure in this region.
    pub entity_type: EntityType,

    /// Brief factual description of this region
    #[schemars(description = "Brief factual description of what's visible in this region")]
    pub description: String,

    /// Features that would help identify this specific structure
    #[schemars(
        description = "Distinguishing features: 'clock tower', 'distinctive dome', 'ornate cornice with lion heads'"
    )]
    pub identifiable_features: Vec<String>,

    /// Legible text visible in this region
    #[schemars(description = "Text visible in this region: signs, nameplates, cornerstone dates")]
    pub visible_text: Vec<ExtractedText>,

    /// Observable signs of damage or deterioration
    #[schemars(
        description = "Signs of damage if any: 'partially collapsed', 'fire damage', 'missing roof'. Empty if intact."
    )]
    pub damage_signs: Vec<String>,
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

/// A unified region combining SAM3 segmentation, VLM analysis, and DINOv3 embedding.
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

    /// Region mask, RLE-encoded in subimage crop coordinates.
    pub mask: RleMask,

    /// DINOv3 CLS embedding for this region crop (1024 dims, L2-normalized).
    /// Empty if embedding failed or was skipped.
    pub embedding: Vec<f32>,

    /// Type of built structure in this region.
    pub entity_type: EntityType,

    /// Brief factual description of what's visible in this region.
    pub description: String,

    /// Distinguishing features that would help identify this specific structure.
    pub identifiable_features: Vec<String>,

    /// Legible text visible in this region (signs, nameplates, cornerstone dates).
    pub visible_text: Vec<ExtractedText>,

    /// Observable signs of damage or deterioration, if any.
    pub damage_signs: Vec<String>,
}

/// Analysis result for a single subimage.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SubimageAnalysis {
    /// Subimage was analyzed successfully.
    Analyzed {
        /// Type of media.
        media_type: AnalyzedMediaType,
        /// 1-2 sentence factual description of what the subimage shows.
        content_summary: String,
        /// Indoor, outdoor, or mixed/ambiguous scene.
        scene_type: SceneType,
        /// Visual evidence suggesting time period.
        temporal_cues: Vec<String>,
        /// Legible text not in any marked region.
        extracted_text: Vec<ExtractedText>,
        /// Chain-of-thought reasoning from the VLM (not part of VLM schema).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thinking: Option<String>,
        /// DINOv3 CLS embedding for the whole subimage crop (1024 dims, L2-normalized).
        /// Empty if embedding failed.
        embedding: Vec<f32>,
        /// Detected and analyzed regions within this subimage.
        regions: Vec<Region>,
        /// Spatial and structural relationships between regions.
        region_relationships: Vec<RegionRelationship>,
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

/// A subimage (panel) detected within the source image.
///
/// All masks within a subimage — both the bounds mask and per-region masks — share
/// dimensions `(bounds.bbox.height, bounds.bbox.width)`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Subimage {
    /// Bounds of this subimage within the source image.
    pub bounds: SubimageBounds,
    /// Analysis result (analyzed or rejected).
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

/// Full analysis result — subimage-centric output from the analysis pipeline.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AnalysisResult {
    /// Detected subimages (panels) with their analysis results.
    /// For single-image inputs, this contains exactly one entry covering the full image.
    pub subimages: Vec<Subimage>,
    /// Model versions used for this analysis run.
    pub versions: ModelVersions,
}

// ==================== VLM Schema Types ====================
//
// Separate types for JSON Schema generation. The VLM doesn't produce masks or
// embeddings — those are merged in by the BLS orchestrator.

/// VLM output for a single subimage — either analyzed or rejected.
///
/// Used to generate the JSON schema passed to the VLM for constrained output.
/// The BLS orchestrator merges this with SAM3 and DINOv3 results to produce
/// [`SubimageAnalysis`].
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum VlmSubimageOutput {
    /// Subimage contains relevant built structures.
    Analyzed(VlmAnalyzedOutput),
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
/// This is the schema the VLM fills in. It does not include masks or embeddings —
/// those are added by the BLS orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VlmAnalyzedOutput {
    /// Type of media
    #[schemars(description = "What type of visual media is this?")]
    pub media_type: AnalyzedMediaType,

    /// Brief factual summary
    #[schemars(description = "1-2 sentence factual description of what the image shows")]
    pub content_summary: String,

    /// Scene type
    #[schemars(description = "Is this an outdoor, indoor, or mixed/ambiguous scene?")]
    pub scene_type: SceneType,

    /// Observable cues about time period
    #[schemars(
        description = "Visual evidence suggesting time period: 'black and white photo', 'horse-drawn carriages', '1950s automobiles', 'gas street lamps', 'modern glass curtain wall'"
    )]
    pub temporal_cues: Vec<String>,

    /// Analysis for each detected region, in the same order as the annotated labels (0-indexed).
    /// Every region must have an entry; use `non_structure` `entity_type` for non-structures.
    #[schemars(
        description = "Array of analyses, one per SAM3 region in order. Index = region number (0-based). Every region must have an entry; use non_structure entity_type for non-structures."
    )]
    pub regions: Vec<RegionAnalysis>,

    /// Relationships between regions.
    #[schemars(description = "Spatial and structural relationships between numbered regions.")]
    pub region_relationships: Vec<RegionRelationship>,

    /// Text elsewhere in image (not in any marked region)
    #[schemars(
        description = "Legible text not in marked regions: street signs, captions, watermarks, date stamps, photographer credits"
    )]
    pub extracted_text: Vec<ExtractedText>,
}

/// Request to analyze an image.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisRequest {
    /// Base64-encoded image data.
    pub image: String,

    /// JSON schema for constrained VLM output.
    pub schema: serde_json::Value,
}
