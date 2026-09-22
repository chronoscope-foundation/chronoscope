//! SAM 3 through `ort`: an image and a box, the one segmented object out.
//!
//! The export carries the whole SAM 3 model; the runner drives its interactive
//! head — box or point in, a single object's mask out — which is what "segment
//! the thing I pointed at" needs (its concept head instead detects every
//! instance of what the box exemplifies, a different task). The image encoder
//! runs once per image; the interactive decoder runs once per prompt over the
//! held features.
//!
//! Preprocessing is the caller's only debt: the graph bakes rescale and
//! normalize, so the caller decodes to RGB, packs planar uint8, and stretches to
//! the model's square. The resize is the shared `preprocess` resampler,
//! antialiased bilinear matching `Sam3Processor`'s `v2.Resize`, over the uint8
//! sample type — DINOv3 rescales to float first, and that dtype is the only
//! thing that differs between the two.

mod manifest;
mod tokenizer;

use std::path::Path;

use chronoscope_core::grammar::geometry::{Dimensions, ProportionalRect, Region, RegionError};
use fast_image_resize::{
    FilterType, ResizeAlg, ResizeOptions, Resizer,
    images::{TypedImage, TypedImageRef},
    pixels::F32,
};
use image::{DynamicImage, GenericImageView};
use ort::{
    session::{RunOptions, Session},
    value::Tensor,
};
use thiserror::Error;

use crate::{
    onnx::{self, Accel, SessionError},
    preprocess::{CHANNELS, ChwImage, ResizeError, positive_extent},
};
use manifest::SamManifest;
use tokenizer::{CONTEXT_LEN, ConceptTokenizer};

pub use manifest::{ManifestError, ManifestInvalid};
pub use tokenizer::TokenizerError;

/// The encoder's one input, named by the export.
const IMAGE_INPUT: &str = "image";

/// The interactive features the encoder emits (alongside the grounding pyramid
/// the concept head reads) and the interactive decoder consumes.
const IMAGE_EMBED: &str = "image_embed";
const HIGH_RES_FEAT_0: &str = "high_res_feat_0";
const HIGH_RES_FEAT_1: &str = "high_res_feat_1";

/// The grounding pyramid the same encoder pass emits and the concept decoder
/// reads. The export names six (`vision_pos_enc_0/1/2`, `backbone_fpn_0/1/2`),
/// but the traced grounding graph consumes only the coarsest positional encoding
/// and all three feature maps; `torch.onnx.export` const-folds the two unread
/// encodings away, so these four are the graph's real grounding inputs.
const VISION_POS_ENC_2: &str = "vision_pos_enc_2";
const BACKBONE_FPN_0: &str = "backbone_fpn_0";
const BACKBONE_FPN_1: &str = "backbone_fpn_1";
const BACKBONE_FPN_2: &str = "backbone_fpn_2";

/// The language encoder's one input and the two outputs the grounding decoder
/// conditions on. Its third output (`text_embeds`) is named at export but unread
/// by the traced graph, so the runner never extracts it.
const TOKENS: &str = "tokens";
const TEXT_ATTENTION_MASK: &str = "text_attention_mask";
const TEXT_MEMORY: &str = "text_memory";

/// The grounding decoder's inputs the interactive path does not share: the
/// original grid its masks are resized to, the language conditioning, and the
/// box-exemplar triple. A text-only prompt sends the export's "no real box"
/// convention — zero coords, label 1, and `box_masks = true` (true masks the
/// exemplar out).
const ORIGINAL_HEIGHT: &str = "original_height";
const ORIGINAL_WIDTH: &str = "original_width";
const LANGUAGE_MASK: &str = "language_mask";
const LANGUAGE_FEATURES: &str = "language_features";
const BOX_COORDS: &str = "box_coords";
const BOX_LABELS: &str = "box_labels";
const BOX_MASKS: &str = "box_masks";

/// The grounding decoder's outputs the runner reads: the per-instance
/// probabilities and the boolean masks. `boxes` is emitted too but the mask is
/// the region, so it goes unread.
const SCORES: &str = "scores";
const MASKS: &str = "masks";

/// The four graphs' names, shared by `session_for` and `GraphError` so a graph's
/// label — the one an error names — is defined once.
const IMAGE_ENCODER: &str = "image_encoder";
const DECODER: &str = "decoder";
const LANGUAGE_ENCODER: &str = "language_encoder";
const GROUNDING_DECODER: &str = "grounding_decoder";

/// Pins the decoder's `num_points` axis (the export left it symbolic) to the two
/// the box path always sends: its corners, labels 2 and 3.
///
/// This is the flag that puts the whole decoder on CoreML. Left symbolic, the
/// mask-decode tail has dynamic shapes CoreML's compiler cannot build an
/// execution plan for, so that partition falls back to CPU; pinning the axis
/// makes those shapes static and CoreML takes the graph. The other prompt kinds
/// SAM offers, point clicks and click-by-click refinement, are a variable number
/// of typed hints (`1`/`0` for include/exclude), not more box corners; serving
/// them means opening the decoder without this override, dynamic and on CPU. The
/// two here must match [`Sam3::segment_rect`]'s two prompt points.
const DECODER_DIMS: &[(&str, i64)] = &[("num_points", 2)];

/// One decoder-side feature map, lifted off the encoder's outputs into a tensor
/// ONNX Runtime owns.
///
/// Ownership is what lets a prompt be cancelled safely. A borrowed input view is
/// sound only while the inference future lives, and dropping that future ends the
/// borrow while a runtime thread may still be reading it. An owned value's
/// backing is held by the run itself, so passing `&tensor` to each prompt keeps
/// the encode's features reusable without copying them per prompt.
struct FeatureMap(Tensor<f32>);

impl FeatureMap {
    fn tensor(&self) -> &Tensor<f32> {
        &self.0
    }
}

/// The image encoder's output, run once and reused across every prompt: the two
/// feature families the one backbone pass emits — the SAM-style interactive
/// features [`segment_rect`](Sam3::segment_rect) consumes and the grounding
/// pyramid [`segment_concept`](Sam3::segment_concept) reads — plus the original
/// image's grid, which either decoder's masks are scaled to.
pub struct EncodedImage {
    original: Dimensions,
    image_embed: FeatureMap,
    high_res_feat_0: FeatureMap,
    high_res_feat_1: FeatureMap,
    vision_pos_enc_2: FeatureMap,
    backbone_fpn_0: FeatureMap,
    backbone_fpn_1: FeatureMap,
    backbone_fpn_2: FeatureMap,
}

/// One loaded SAM 3 model, driven through both its heads: the four graphs and
/// the CLIP tokenizer the concept prompt needs, the square to feed the encoder,
/// and the interactive mask-decode shape.
pub struct Sam3 {
    encoder: Session,
    decoder: Session,
    grounding_decoder: Session,
    language_encoder: Session,
    tokenizer: ConceptTokenizer,
    resolution: usize,
    low_res_mask_size: usize,
    candidates: usize,
}

impl Sam3 {
    /// Loads the export's manifest and opens all four graphs plus the concept
    /// tokenizer, ready to encode and prompt through either head.
    ///
    /// The grounding decoder is opened with no static dimensions to pin — its
    /// instance count and mask grid are both dynamic. That variable-sized output
    /// is one CoreML can't handle as of onnxruntime 1.29.0 (a no-match makes it
    /// zero-element, which trips a CoreML guard), so it runs on the CPU EP rather
    /// than `accel`: cheap, since the grounding decoder is small, and it prevents a
    /// crash on the CoreML backend. The image encode it reads still runs on `accel`.
    pub fn open(export: &Path, accel: Accel) -> Result<Self, OpenError> {
        let manifest = SamManifest::load(export).map_err(OpenError::Manifest)?;
        let encoder = onnx::session_for(&manifest.image_encoder, accel, IMAGE_ENCODER, &[])
            .map_err(OpenError::Session)?;
        let decoder = onnx::session_for(&manifest.decoder, accel, DECODER, DECODER_DIMS)
            .map_err(OpenError::Session)?;
        let grounding_decoder = onnx::session_for(
            &manifest.grounding_decoder,
            Accel::Cpu,
            GROUNDING_DECODER,
            &[],
        )
        .map_err(OpenError::Session)?;
        let language_encoder =
            onnx::session_for(&manifest.language_encoder, accel, LANGUAGE_ENCODER, &[])
                .map_err(OpenError::Session)?;
        let tokenizer =
            ConceptTokenizer::load(&manifest.tokenizer).map_err(OpenError::Tokenizer)?;
        Ok(Self {
            encoder,
            decoder,
            grounding_decoder,
            language_encoder,
            tokenizer,
            resolution: manifest.resolution,
            low_res_mask_size: manifest.low_res_mask_size,
            candidates: manifest.candidates,
        })
    }

    /// Preprocesses a decoded image the way `Sam3Processor` does and runs the
    /// image encoder once, holding the features a prompt reuses.
    ///
    /// `to_rgb8` fixes the channel count at three. The original grid is held so
    /// the decoder's masks scale back to it, so it is capped like any [`Region`]
    /// grid.
    pub async fn encode(&mut self, image: &DynamicImage) -> Result<EncodedImage, EncodeError> {
        let (width, height) = image.dimensions();
        let original = Dimensions::new(width, height).map_err(EncodeError::Original)?;

        let square = ChwImage::from_rgb(&image.to_rgb8(), |byte| byte)
            .resize(self.resolution)
            .map_err(EncodeError::Resize)?;
        let shape = vec![
            CHANNELS as i64,
            square.height() as i64,
            square.width() as i64,
        ];
        let tensor =
            Tensor::from_array((shape, square.into_samples())).map_err(input(IMAGE_ENCODER))?;

        let options = RunOptions::new().map_err(run(IMAGE_ENCODER))?;
        let outputs = self
            .encoder
            .run_async(ort::inputs![IMAGE_INPUT => tensor], &options)
            .map_err(run(IMAGE_ENCODER))?
            .await
            .map_err(run(IMAGE_ENCODER))?;

        Ok(EncodedImage {
            original,
            image_embed: feature_map(&outputs, IMAGE_ENCODER, IMAGE_EMBED)?,
            high_res_feat_0: feature_map(&outputs, IMAGE_ENCODER, HIGH_RES_FEAT_0)?,
            high_res_feat_1: feature_map(&outputs, IMAGE_ENCODER, HIGH_RES_FEAT_1)?,
            vision_pos_enc_2: feature_map(&outputs, IMAGE_ENCODER, VISION_POS_ENC_2)?,
            backbone_fpn_0: feature_map(&outputs, IMAGE_ENCODER, BACKBONE_FPN_0)?,
            backbone_fpn_1: feature_map(&outputs, IMAGE_ENCODER, BACKBONE_FPN_1)?,
            backbone_fpn_2: feature_map(&outputs, IMAGE_ENCODER, BACKBONE_FPN_2)?,
        })
    }

    /// Segments the one object the box points at, or `None` if the mask comes
    /// back empty.
    ///
    /// The box becomes its two corners as prompt points (labels 2 and 3) in the
    /// model's own pixel frame; the pure stretch makes that a plain scale of the
    /// proportional rect. The decoder returns ambiguity candidates with a
    /// predicted `IoU` each; the best is upsampled from the decoder's low
    /// resolution to the original grid and thresholded. The score is that
    /// predicted `IoU`.
    pub async fn segment_rect(
        &mut self,
        encoded: &EncodedImage,
        rect: &ProportionalRect,
    ) -> Result<Option<ScoredRegion>, RectError> {
        let side = self.resolution as f32;
        let point_coords = [
            rect.x() as f32 * side,
            rect.y() as f32 * side,
            (rect.x() + rect.width()) as f32 * side,
            (rect.y() + rect.height()) as f32 * side,
        ];
        let point_labels = [2.0_f32, 3.0];

        let options = RunOptions::new().map_err(run(DECODER))?;
        let outputs = self
            .decoder
            .run_async(ort::inputs![
                IMAGE_EMBED => encoded.image_embed.tensor(),
                HIGH_RES_FEAT_0 => encoded.high_res_feat_0.tensor(),
                HIGH_RES_FEAT_1 => encoded.high_res_feat_1.tensor(),
                "point_coords" => Tensor::from_array((vec![1_i64, 2, 2], point_coords.to_vec())).map_err(input(DECODER))?,
                "point_labels" => Tensor::from_array((vec![1_i64, 2], point_labels.to_vec())).map_err(input(DECODER))?,
            ], &options)
            .map_err(run(DECODER))?
            .await
            .map_err(run(DECODER))?;

        let ious = extract(&outputs, DECODER, "iou_predictions")?;
        let masks = extract(&outputs, DECODER, "low_res_masks")?;

        // The decoder emits `candidates` low-resolution mask logits and one IoU
        // each: `low_res_masks` is `[1, candidates, low, low]`, `iou_predictions`
        // `[1, candidates]`. Hold both to that so the best mask slice is in
        // bounds.
        let low = self.low_res_mask_size;
        if ious.len() != self.candidates || masks.len() != self.candidates * low * low {
            return Err(RectError::OutputShape {
                ious: ious.len(),
                masks: masks.len(),
            });
        }

        let best = ious
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(index, _)| index)
            .ok_or(RectError::NoCandidates)?;
        let logits = &masks[best * low * low..(best + 1) * low * low];
        let region = upsample_mask(logits, low, encoded.original).map_err(RectError::Mask)?;

        Ok(region.map(|region| ScoredRegion {
            region,
            score: ious[best],
        }))
    }

    /// Detects and segments every instance of a text concept — the concept
    /// head's task, where [`segment_rect`](Self::segment_rect) segments the one
    /// object a box points at.
    ///
    /// The prompt tokenizes to the CLIP context and conditions the grounding
    /// decoder through the language encoder; the box-exemplar inputs carry the
    /// export's "no real box" convention (`box_masks = true` masks the exemplar
    /// out) so the detection is text-only. The graph applies its own 0.5
    /// confidence filter and resizes each surviving mask to the original grid
    /// already thresholded, so the runner reads instances straight out — no
    /// candidate pick and no upsample. An empty result means the model found none
    /// of the concept.
    pub async fn segment_concept(
        &mut self,
        encoded: &EncodedImage,
        prompt: &str,
    ) -> Result<Vec<ScoredRegion>, ConceptError> {
        let tokens = self
            .tokenizer
            .encode(prompt)
            .map_err(ConceptError::Tokenize)?;
        let language_options = RunOptions::new().map_err(run(LANGUAGE_ENCODER))?;
        let encoded_text = self
            .language_encoder
            .run_async(ort::inputs![
                TOKENS => Tensor::from_array((vec![1_i64, CONTEXT_LEN as i64], tokens.to_vec())).map_err(input(LANGUAGE_ENCODER))?,
            ], &language_options)
            .map_err(run(LANGUAGE_ENCODER))?
            .await
            .map_err(run(LANGUAGE_ENCODER))?;

        // The grounding run reuses these, so both language outputs are lifted to
        // owned buffers and the language session's outputs dropped before it.
        let language_mask = extract_bool(&encoded_text, LANGUAGE_ENCODER, TEXT_ATTENTION_MASK)?;
        let language_features = feature_map(&encoded_text, LANGUAGE_ENCODER, TEXT_MEMORY)?;
        drop(encoded_text);

        let height = i64::from(encoded.original.height());
        let width = i64::from(encoded.original.width());
        // The export's text-only exemplar: zero coords, label 1, masked out.
        let box_coords = vec![0.0_f32; 4];
        let box_labels = vec![1_i64];
        let box_masks = vec![true];

        let grounding_options = RunOptions::new().map_err(run(GROUNDING_DECODER))?;
        let outputs = self
            .grounding_decoder
            .run_async(ort::inputs![
                ORIGINAL_HEIGHT => Tensor::from_array((Vec::<i64>::new(), vec![height])).map_err(input(GROUNDING_DECODER))?,
                ORIGINAL_WIDTH => Tensor::from_array((Vec::<i64>::new(), vec![width])).map_err(input(GROUNDING_DECODER))?,
                VISION_POS_ENC_2 => encoded.vision_pos_enc_2.tensor(),
                BACKBONE_FPN_0 => encoded.backbone_fpn_0.tensor(),
                BACKBONE_FPN_1 => encoded.backbone_fpn_1.tensor(),
                BACKBONE_FPN_2 => encoded.backbone_fpn_2.tensor(),
                LANGUAGE_MASK => &language_mask,
                LANGUAGE_FEATURES => language_features.tensor(),
                BOX_COORDS => Tensor::from_array((vec![1_i64, 1, 4], box_coords)).map_err(input(GROUNDING_DECODER))?,
                BOX_LABELS => Tensor::from_array((vec![1_i64, 1], box_labels)).map_err(input(GROUNDING_DECODER))?,
                BOX_MASKS => Tensor::from_array((vec![1_i64, 1], box_masks)).map_err(input(GROUNDING_DECODER))?,
            ], &grounding_options)
            .map_err(run(GROUNDING_DECODER))?
            .await
            .map_err(run(GROUNDING_DECODER))?;

        let scores = extract(&outputs, GROUNDING_DECODER, SCORES)?;
        let (_, masks) = outputs
            .get(MASKS)
            .ok_or(GraphError::MissingOutput {
                graph: GROUNDING_DECODER,
                name: MASKS,
            })?
            .try_extract_tensor::<bool>()
            .map_err(|source| GraphError::OutputType {
                graph: GROUNDING_DECODER,
                name: MASKS,
                source,
            })?;

        // The graph resizes every kept instance's mask to the original grid, so
        // each is one `original`-sized boolean plane; the channel axis it carries
        // is a singleton and folds into the per-instance stride.
        let pixels = (height as usize) * (width as usize);
        if masks.len() != scores.len() * pixels {
            return Err(ConceptError::OutputShape {
                scores: scores.len(),
                masks: masks.len(),
                pixels,
            });
        }

        // A kept instance whose thresholded mask is nonetheless empty carries no
        // pixels to segment: `from_dense` returns `None` and it is dropped the way
        // an empty box mask is.
        let mut regions = Vec::with_capacity(scores.len());
        for (index, &score) in scores.iter().enumerate() {
            let plane = &masks[index * pixels..(index + 1) * pixels];
            if let Some(region) =
                Region::from_dense(encoded.original, plane).map_err(ConceptError::Mask)?
            {
                regions.push(ScoredRegion { region, score });
            }
        }
        Ok(regions)
    }
}

/// Curries a graph label into a `GraphError::Input` / `::Run` constructor, for a
/// tidy `map_err` at the many tensor-build and graph-run call sites.
fn input(graph: &'static str) -> impl Fn(ort::Error) -> GraphError {
    move |source| GraphError::Input { graph, source }
}

fn run(graph: &'static str) -> impl Fn(ort::Error) -> GraphError {
    move |source| GraphError::Run { graph, source }
}

/// Extracts one named f32 output as an owned [`FeatureMap`], rejecting a
/// non-finite value that would poison a downstream graph. Owned because the
/// encoder's features outlive their `outputs`, and the language features re-feed
/// the grounding decoder after its own outputs drop.
fn feature_map(
    outputs: &ort::session::SessionOutputs,
    graph: &'static str,
    name: &'static str,
) -> Result<FeatureMap, GraphError> {
    let output = outputs
        .get(name)
        .ok_or(GraphError::MissingOutput { graph, name })?;
    let (shape, data) =
        output
            .try_extract_tensor::<f32>()
            .map_err(|source| GraphError::OutputType {
                graph,
                name,
                source,
            })?;
    if data.iter().any(|value| !value.is_finite()) {
        return Err(GraphError::NonFinite { graph, name });
    }
    Tensor::from_array((shape.to_vec(), data.to_vec()))
        .map(FeatureMap)
        .map_err(input(graph))
}

/// Extracts a named f32 output as a slice, rejecting a non-finite value.
///
/// A `NaN` `iou_predictions` sorts as the maximum under `total_cmp`, so an
/// unchecked `NaN` would be picked as the best candidate and silently thresholded
/// to an empty mask rather than surfacing the corrupt decode.
fn extract<'a>(
    outputs: &'a ort::session::SessionOutputs,
    graph: &'static str,
    name: &'static str,
) -> Result<&'a [f32], GraphError> {
    let output = outputs
        .get(name)
        .ok_or(GraphError::MissingOutput { graph, name })?;
    let (_, data) =
        output
            .try_extract_tensor::<f32>()
            .map_err(|source| GraphError::OutputType {
                graph,
                name,
                source,
            })?;
    if data.iter().any(|value| !value.is_finite()) {
        return Err(GraphError::NonFinite { graph, name });
    }
    Ok(data)
}

/// Lifts a named boolean output into a tensor ONNX Runtime owns, so the language
/// mask survives both its own session's outputs and a cancelled grounding run.
/// No finiteness check: a bool has no non-finite value to reject.
fn extract_bool(
    outputs: &ort::session::SessionOutputs,
    graph: &'static str,
    name: &'static str,
) -> Result<Tensor<bool>, GraphError> {
    let output = outputs
        .get(name)
        .ok_or(GraphError::MissingOutput { graph, name })?;
    let (shape, data) =
        output
            .try_extract_tensor::<bool>()
            .map_err(|source| GraphError::OutputType {
                graph,
                name,
                source,
            })?;
    Tensor::from_array((shape.to_vec(), data.to_vec())).map_err(input(graph))
}

/// Bilinearly upsamples the decoder's square low-resolution mask logits to the
/// original grid and thresholds at zero, the finish `Sam3Processor` applies. The
/// mask decode is resolution-independent, so the caller owns this step. `None`
/// when the thresholded mask is empty, since an empty mask is not a region.
fn upsample_mask(
    logits: &[f32],
    low: usize,
    grid: Dimensions,
) -> Result<Option<Region>, MaskError> {
    let (Some(low_side), Some(width), Some(height)) = (
        positive_extent(low),
        positive_extent(grid.width() as usize),
        positive_extent(grid.height() as usize),
    ) else {
        return Err(MaskError::Dimension {
            low,
            width: grid.width() as usize,
            height: grid.height() as usize,
        });
    };

    let pixels: Vec<F32> = logits.iter().map(|&value| F32::new(value)).collect();
    let source = TypedImageRef::new(low_side, low_side, &pixels)
        .map_err(|source| MaskError::Source { source })?;
    let mut resampled: TypedImage<F32> = TypedImage::new(width, height);
    let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear));
    Resizer::new()
        .resize_typed(&source, &mut resampled, &options)
        .map_err(|source| MaskError::Resample { source })?;

    let dense: Vec<bool> = resampled
        .pixels()
        .iter()
        .map(|pixel| pixel.0 > 0.0)
        .collect();
    Region::from_dense(grid, &dense).map_err(MaskError::Region)
}

/// One region SAM 3 segmented for a box, with the model's predicted mask `IoU`.
#[derive(Debug, Clone)]
pub struct ScoredRegion {
    pub region: Region,
    pub score: f32,
}

/// Why the decoder's mask could not be turned into a region.
#[derive(Debug, Error)]
pub enum MaskError {
    #[error("cannot upsample a {low}px mask onto a {width}x{height} grid")]
    Dimension {
        low: usize,
        width: usize,
        height: usize,
    },

    #[error("the resampler rejected the mask")]
    Source {
        #[source]
        source: fast_image_resize::InvalidPixelsSize,
    },

    #[error("upsampling the mask failed")]
    Resample {
        #[source]
        source: fast_image_resize::ResizeError,
    },

    #[error("the upsampled mask is not a valid region on the original grid")]
    Region(#[source] RegionError),
}

/// Why a model could not be opened from its export directory.
#[derive(Debug, Error)]
pub enum OpenError {
    #[error("the export's manifest could not be loaded")]
    Manifest(#[source] ManifestError),

    #[error("a graph could not be opened")]
    Session(#[source] SessionError),

    #[error("the concept tokenizer could not be loaded")]
    Tokenizer(#[source] TokenizerError),
}

/// The failure modes of feeding an ONNX graph and reading its tensors, shared by
/// every model op. Each names the graph it happened on, so one `Run` serves the
/// encoder, either decoder, and the language encoder while still saying which.
#[derive(Debug, Error)]
pub enum GraphError {
    #[error("could not build a `{graph}` input tensor")]
    Input {
        graph: &'static str,
        #[source]
        source: ort::Error,
    },

    #[error("the `{graph}` graph failed to run")]
    Run {
        graph: &'static str,
        #[source]
        source: ort::Error,
    },

    #[error("`{graph}` produced no `{name}` output")]
    MissingOutput {
        graph: &'static str,
        name: &'static str,
    },

    #[error("`{graph}` output `{name}` is not the expected tensor type")]
    OutputType {
        graph: &'static str,
        name: &'static str,
        #[source]
        source: ort::Error,
    },

    #[error("`{graph}` output `{name}` holds a non-finite value")]
    NonFinite {
        graph: &'static str,
        name: &'static str,
    },
}

/// Why `encode` could not turn a decoded image into features.
#[derive(Debug, Error)]
pub enum EncodeError {
    #[error(transparent)]
    Graph(#[from] GraphError),

    #[error("the image grid is not one the model can localize into")]
    Original(#[source] RegionError),

    #[error("could not resample the image to the encoder's square")]
    Resize(#[source] ResizeError),
}

/// Why `segment_rect` could not turn a box into a region.
#[derive(Debug, Error)]
pub enum RectError {
    #[error(transparent)]
    Graph(#[from] GraphError),

    #[error(
        "the decoder returned {ious} scores and {masks} mask values, not the declared candidates"
    )]
    OutputShape { ious: usize, masks: usize },

    #[error("the decoder returned no candidates to choose from")]
    NoCandidates,

    #[error("the decoder mask could not be resolved")]
    Mask(#[source] MaskError),
}

/// Why `segment_concept` could not turn a text prompt into regions.
#[derive(Debug, Error)]
pub enum ConceptError {
    #[error(transparent)]
    Graph(#[from] GraphError),

    #[error("the prompt could not be tokenized")]
    Tokenize(#[source] TokenizerError),

    #[error(
        "the grounding decoder returned {scores} scores but {masks} mask values, \
         not a whole number of {pixels}-pixel planes"
    )]
    OutputShape {
        scores: usize,
        masks: usize,
        pixels: usize,
    },

    #[error("a grounding mask could not be resolved into a region")]
    Mask(#[source] RegionError),
}
