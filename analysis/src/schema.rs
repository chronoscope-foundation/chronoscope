//! Schema types for analysis results.
//!
//! The pipeline outputs subimage-centric results: each source image is split into
//! one or more subimages (panels), and each subimage gets independent SAM3 segmentation,
//! VLM analysis, and DINOv3 embeddings.
//!
//! Region surroundings form a constraint graph: when external knowledge identifies
//! one entity, constraints propagate along relationship edges to narrow down unknowns.
//! For example, if region 2 is identified as a building demolished in 1927, and region 1
//! shares a wall with region 2, we learn that region 1 was near that location
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

/// Text extracted from the image.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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

// ==================== Scene-level Observation Types ====================

/// Type of vehicle visible in the scene (dating signal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VehicleType {
    HorseDrawn,
    #[schemars(description = "Pre-~1930: exposed running boards, crank starts")]
    EarlyAutomobile,
    #[schemars(description = "~1930s-1970s: chrome, fins, rounded bodies")]
    MidcenturyAutomobile,
    #[schemars(description = "Post-~1980")]
    ModernAutomobile,
    Streetcar,
    Bicycle,
}

/// Street infrastructure elements visible in the scene (dating signal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StreetInfrastructure {
    GasLamp,
    ElectricPole,
    #[schemars(description = "Trolley, electric, or telegraph wires")]
    OverheadWires,
    TrolleyTracks,
    TrafficSignal,
    FireHydrant,
}

/// Road surface type (dating/location signal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoadSurface {
    Dirt,
    #[schemars(description = "Cobblestone or Belgian block")]
    Cobblestone,
    Asphalt,
    Concrete,
}

/// Scene-level observations visible across the entire subimage.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SceneObservations {
    pub vehicles: Vec<VehicleType>,
    pub street_infrastructure: Vec<StreetInfrastructure>,
    pub road_surface: Vec<RoadSurface>,

    #[schemars(
        description = "Other observations not covered above: e.g., 'elevated railway', 'horse trough', 'newsstand'"
    )]
    pub other_observations: Vec<String>,
}

// ==================== Region-level Observation Types ====================

/// Roof type visible on a structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoofType {
    Flat,
    /// Triangular roof.
    Gable,
    /// Slopes on all four sides.
    Hip,
    /// Double-sloped, steep lower section.
    Mansard,
    /// Barn-style, two slopes per side.
    Gambrel,
    Dome,
    /// Small dome or lantern on top of a roof.
    Cupola,
}

/// Facade material of a structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FacadeMaterial {
    Brick,
    Brownstone,
    Limestone,
    Marble,
    CastIron,
    GlassCurtainWall,
    Concrete,
    CorrugatedMetal,
    Stucco,
    Wood,
    TerraCotta,
    /// Generic or unspecified stone type.
    Stone,
}

/// Structural or architectural element visible on a structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StructuralElement {
    /// Projecting molding at roofline.
    Cornice,
    /// Triangular gable above entrance or window.
    Pediment,
    /// Flat column attached to wall.
    Pilaster,
    /// Corner stones.
    Quoin,
    /// Horizontal support above opening.
    Lintel,
    /// Wedge-shaped stone at arch apex.
    Keystone,
    /// Railing with balusters.
    Balustrade,
    BayWindow,
    /// Window projecting from roof.
    Dormer,
    FireEscape,
    WaterTower,
    Chimney,
    Tower,
    Spire,
    Porch,
    /// Covered entrance with columns.
    Portico,
    Columns,
    Balcony,
}

/// Window shape visible on a structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WindowShape {
    Rectangular,
    Arched,
    /// Oculus or porthole.
    Round,
}

/// Condition indicator for a structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConditionIndicator {
    Intact,
    #[schemars(description = "Charring, smoke stains, collapsed sections from fire")]
    FireDamage,
    PartialDemolition,
    UnderConstruction,
    Scaffolding,
    BoardedUp,
    Renovated,
    #[schemars(description = "Peeling paint, crumbling masonry, missing elements")]
    Deteriorating,
}

/// Region-level observations for a built structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegionObservations {
    pub roof_types: Vec<RoofType>,
    pub facade_materials: Vec<FacadeMaterial>,
    pub structural_elements: Vec<StructuralElement>,

    // TODO: Add ArchitecturalStyle enum when VLM reliability is validated
    // (classical, gothic_revival, beaux_arts, art_deco, victorian, etc.)
    pub window_shapes: Vec<WindowShape>,

    #[schemars(description = "Number of stories (floors) visible, if countable")]
    pub stories_visible: Option<u32>,

    #[schemars(
        description = "Other features not covered above: 'clock face', 'distinctive dome', 'ornate cornice with lion heads'"
    )]
    pub other_features: Vec<String>,
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
/// relationships to other numbered regions. Relationships may appear from both
/// sides (region 0 says `shares_wall` with region 1, region 1 says `shares_wall`
/// with region 0) — the solver can validate symmetry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Surroundings {
    pub non_entity: Vec<SurroundingType>,

    #[schemars(
        description = "Other non-entity surroundings not covered above: 'cemetery', 'construction site'"
    )]
    pub other_non_entity: Vec<String>,

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

    pub observations: RegionObservations,
    pub condition: Vec<ConditionIndicator>,

    #[schemars(description = "e.g., 'graffiti', 'ivy-covered'")]
    pub other_condition: Vec<String>,

    #[schemars(description = "Signs, nameplates, cornerstone dates")]
    pub visible_text: Vec<ExtractedText>,

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
    /// `None` if embedding was not computed or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,

    /// VLM analysis of this region.
    pub analysis: RegionAnalysis,
}

/// Scene-level analysis, shared between VLM schema and assembled results.
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

    /// Structured scene-level observations (vehicles, infrastructure, road surface).
    #[schemars(
        description = "Scene-level observations: vehicles, street infrastructure, road surfaces, and other temporal cues"
    )]
    pub scene_observations: SceneObservations,

    /// Text elsewhere in image (not in any marked region)
    #[schemars(
        description = "Legible text not in marked regions: street signs, captions, watermarks, date stamps, photographer credits"
    )]
    pub extracted_text: Vec<ExtractedText>,
}

/// Analysis result for a single subimage.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SubimageAnalysis {
    /// Subimage was analyzed successfully.
    Analyzed {
        /// VLM scene-level analysis (shared type with VLM schema).
        scene: SceneAnalysis,
        /// Chain-of-thought reasoning from the VLM (not part of VLM schema).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thinking: Option<String>,
        /// DINOv3 CLS embedding for the whole subimage crop (1024 dims, L2-normalized).
        /// `None` if embedding was not computed or failed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        embedding: Option<Vec<f32>>,
        /// Detected and analyzed regions within this subimage.
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
