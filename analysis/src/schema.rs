//! Schema types for analysis results.
//!
//! Region-keyed output: SAM3 assigns each mask a `region_id` (1, 2, 3...),
//! painted on the annotated image. The VLM references regions by ID in its output,
//! enabling correlation between masks and descriptions.
//!
//! Region relationships form a constraint graph: when external knowledge identifies
//! one entity, constraints propagate along relationship edges to narrow down unknowns.
//! For example, if region 2 is identified as a building demolished in 1927, and region 1
//! is marked as "adjacent" to region 2, we learn that region 1 was near that location
//! and existed before 1927.

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ==================== Segmentation Types ====================

/// RLE-encoded binary mask (COCO compressed string format).
///
/// Run-length encoding alternates between background and foreground run lengths,
/// compressed into a compact ASCII string using modified LEB128 encoding.
/// This is the same format used by pycocotools.
///
/// Note: Mask dimensions are not stored per-mask. All masks in an `AnalysisResult`
/// share the same dimensions, stored in `AnalysisResult::image_size`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RleMask {
    /// Compressed run lengths as COCO string (modified LEB128, +48 ASCII offset).
    /// Alternates background/foreground; first run is always background.
    pub counts: String,
}

/// A detected region from SAM3 segmentation.
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

// ==================== VLM Analysis Types ====================
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

/// Composite image layout.
///
/// Some images are composites of multiple panels, often showing the same scene
/// at different times or from different angles. For single images, rows=1 and columns=1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompositeInfo {
    /// Number of rows in the grid
    #[schemars(
        description = "Number of rows of panels. 1 for a single image or side-by-side panels."
    )]
    pub rows: u8,

    /// Number of columns in the grid
    #[schemars(
        description = "Number of columns of panels. 1 for a single image or stacked panels."
    )]
    pub columns: u8,
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
        description = "Both regions show the same physical structure in different panels of a composite image (e.g., before/after views). For segmentation splits within a single view, use same_as in the region entry instead."
    )]
    SameStructure,
}

/// A relationship between two regions.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegionRelationship {
    /// The subject region ID (1-indexed, matching annotation labels).
    #[schemars(description = "Subject region number, e.g. 1")]
    pub subject: u32,

    /// The object region ID (1-indexed, matching annotation labels).
    #[schemars(description = "Object region number, e.g. 2")]
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
}

/// Analysis of a single numbered region.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

/// Entry for a numbered region - either full analysis or a reference to another region.
///
/// When segmentation incorrectly splits one structure into multiple regions, provide
/// full analysis for the lowest-numbered region and use `SameAs` for the others.
///
/// TODO: Handle the inverse case where the VLM identifies multiple distinct structures
/// within a single segmentation region. This might require a way to sub-divide regions
/// or report that a region contains multiple entities.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum RegionEntry {
    /// Full analysis of this region
    Analysis(RegionAnalysis),
    /// This region depicts the same structure as another (lower-numbered) region.
    /// Use when segmentation incorrectly split one structure into multiple regions.
    SameAs {
        #[schemars(
            description = "Region number this is the same structure as (must be lower than this region's number)"
        )]
        same_as: u32,
    },
}

/// VLM output - either successful analysis or an error.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum VlmOutput {
    /// Successful analysis
    Success(VlmAnalysis),
    /// Error during analysis (e.g., token limit exceeded, JSON parse failure)
    Error {
        /// Error message
        error: String,
        /// Raw VLM output if available (for debugging truncation issues)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_output: Option<String>,
    },
}

/// Full analysis output.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VlmAnalysis {
    /// Is this image relevant for historical building research?
    #[schemars(
        description = "True if image shows human-built structures: buildings, bridges, tunnels, railways, dams, monuments, infrastructure. False for: people without structures, nature, vehicles as subject, memes, food, animals, abstract art, porn, etc."
    )]
    pub is_relevant: bool,

    /// If not relevant, why?
    #[schemars(
        description = "Brief reason if rejected: e.g., 'portrait photo', 'natural landscape', 'vehicle photo', 'meme', 'food photo', 'animal photo'"
    )]
    pub rejection_reason: Option<String>,

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

    /// Composite image detection
    #[schemars(description = "Whether this is a multi-panel composite and how it's arranged")]
    pub composite: CompositeInfo,

    /// Analysis for each numbered region.
    #[schemars(
        description = "Entry for each numbered region visible in the annotated image. Key is region number as string. Every numbered region must have an entry. For regions that aren't built structures, omit them. If segmentation split one structure into multiple regions, provide full analysis for the lowest-numbered region and use same_as for the others."
    )]
    pub regions: HashMap<String, RegionEntry>,

    /// Relationships between regions.
    #[schemars(description = "Spatial and structural relationships between numbered regions.")]
    pub region_relationships: Vec<RegionRelationship>,

    /// Text elsewhere in image (not in any marked region)
    #[schemars(
        description = "Legible text not in marked regions: street signs, captions, watermarks, date stamps, photographer credits"
    )]
    pub extracted_text: Vec<ExtractedText>,

    /// Chain-of-thought reasoning from Qwen3-VL-Thinking model.
    /// Added by BLS model after VLM generation, not part of VLM's output schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    pub thinking: Option<String>,
}

/// Full analysis result combining segmentation and VLM stages.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnalysisResult {
    /// Source image dimensions as (height, width).
    /// All region masks share these dimensions.
    pub image_size: (u32, u32),

    /// Detected regions from SAM3 segmentation.
    /// Empty if no regions passed confidence threshold.
    pub segmentation: Vec<DetectedRegion>,

    /// Annotated image with region overlays (base64 JPEG).
    /// Useful for debugging to see what SAM3 segmented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotated_image: Option<String>,

    /// VLM analysis output (success or error).
    pub vlm: VlmOutput,
}

/// Request to analyze an image.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisRequest {
    /// Base64-encoded image data.
    pub image: String,

    /// JSON schema for constrained VLM output.
    pub schema: serde_json::Value,
}
