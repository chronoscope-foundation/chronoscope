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

use std::path::Path;

use chronoscope_core::grammar::geometry::{Dimensions, ProportionalRect, Region, RegionError};
use fast_image_resize::{
    FilterType, ResizeAlg, ResizeOptions, Resizer,
    images::{TypedImage, TypedImageRef},
    pixels::F32,
};
use image::{DynamicImage, GenericImageView};
use ort::{session::Session, value::TensorRef};
use thiserror::Error;

use crate::{
    onnx::{self, SessionError},
    preprocess::{CHANNELS, ChwImage, ResizeError, positive_extent},
};
use manifest::SamManifest;

pub use manifest::{ManifestError, ManifestInvalid};

/// The encoder's one input, named by the export.
const IMAGE_INPUT: &str = "image";

/// The interactive features the encoder emits (alongside the grounding pyramid
/// the concept head would read) and the decoder consumes.
const IMAGE_EMBED: &str = "image_embed";
const HIGH_RES_FEAT_0: &str = "high_res_feat_0";
const HIGH_RES_FEAT_1: &str = "high_res_feat_1";

/// One decoder-side feature map, held as the encoder produced it: a shape and
/// its row-major `f32` data.
struct FeatureMap {
    shape: Vec<i64>,
    data: Vec<f32>,
}

impl FeatureMap {
    fn tensor(&self) -> Result<TensorRef<'_, f32>, ort::Error> {
        TensorRef::from_array_view((self.shape.clone(), self.data.as_slice()))
    }
}

/// The image encoder's output, run once and reused across every prompt: the
/// SAM-style interactive features the decoder consumes and the original image's
/// grid, which the decoder's masks are scaled to.
pub struct EncodedImage {
    original: Dimensions,
    image_embed: FeatureMap,
    high_res_feat_0: FeatureMap,
    high_res_feat_1: FeatureMap,
}

/// One loaded SAM 3 model, driven through its interactive head: the two graphs,
/// the square to feed the encoder, and the mask-decode shape.
pub struct Sam3 {
    encoder: Session,
    decoder: Session,
    resolution: usize,
    low_res_mask_size: usize,
    candidates: usize,
}

impl Sam3 {
    /// Loads the export's manifest and opens the image encoder and the
    /// interactive decoder, ready to encode and prompt.
    pub fn open(export: &Path) -> Result<Self, OpenError> {
        let manifest = SamManifest::load(export).map_err(OpenError::Manifest)?;
        let encoder = onnx::session(&manifest.image_encoder).map_err(OpenError::Session)?;
        let decoder = onnx::session(&manifest.decoder).map_err(OpenError::Session)?;
        Ok(Self {
            encoder,
            decoder,
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
    pub fn encode(&mut self, image: &DynamicImage) -> Result<EncodedImage, EncodeError> {
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
        let input =
            TensorRef::from_array_view((shape, square.samples())).map_err(EncodeError::Input)?;

        let outputs = self
            .encoder
            .run(ort::inputs![IMAGE_INPUT => input])
            .map_err(EncodeError::Run)?;

        Ok(EncodedImage {
            original,
            image_embed: feature_map(&outputs, IMAGE_EMBED)?,
            high_res_feat_0: feature_map(&outputs, HIGH_RES_FEAT_0)?,
            high_res_feat_1: feature_map(&outputs, HIGH_RES_FEAT_1)?,
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
    pub fn segment_rect(
        &mut self,
        encoded: &EncodedImage,
        rect: &ProportionalRect,
    ) -> Result<Option<ScoredRegion>, SegmentError> {
        let side = self.resolution as f32;
        let point_coords = [
            rect.x() as f32 * side,
            rect.y() as f32 * side,
            (rect.x() + rect.width()) as f32 * side,
            (rect.y() + rect.height()) as f32 * side,
        ];
        let point_labels = [2.0_f32, 3.0];

        let outputs = self
            .decoder
            .run(ort::inputs![
                IMAGE_EMBED => encoded.image_embed.tensor().map_err(SegmentError::Input)?,
                HIGH_RES_FEAT_0 => encoded.high_res_feat_0.tensor().map_err(SegmentError::Input)?,
                HIGH_RES_FEAT_1 => encoded.high_res_feat_1.tensor().map_err(SegmentError::Input)?,
                "point_coords" => TensorRef::from_array_view((vec![1_i64, 2, 2], point_coords.as_slice())).map_err(SegmentError::Input)?,
                "point_labels" => TensorRef::from_array_view((vec![1_i64, 2], point_labels.as_slice())).map_err(SegmentError::Input)?,
            ])
            .map_err(SegmentError::Run)?;

        let ious = extract(&outputs, "iou_predictions")?;
        let masks = extract(&outputs, "low_res_masks")?;

        // The decoder emits `candidates` low-resolution mask logits and one IoU
        // each: `low_res_masks` is `[1, candidates, low, low]`, `iou_predictions`
        // `[1, candidates]`. Hold both to that so the best mask slice is in
        // bounds.
        let low = self.low_res_mask_size;
        if ious.len() != self.candidates || masks.len() != self.candidates * low * low {
            return Err(SegmentError::OutputShape {
                ious: ious.len(),
                masks: masks.len(),
            });
        }

        let best = ious
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(index, _)| index)
            .ok_or(SegmentError::NoCandidates)?;
        let logits = &masks[best * low * low..(best + 1) * low * low];
        let region = upsample_mask(logits, low, encoded.original).map_err(SegmentError::Mask)?;

        Ok((!region.is_empty()).then_some(ScoredRegion {
            region,
            score: ious[best],
        }))
    }
}

/// Extracts one named encoder output as an owned [`FeatureMap`], rejecting a
/// non-finite value that would poison the decoder.
fn feature_map(
    outputs: &ort::session::SessionOutputs,
    name: &'static str,
) -> Result<FeatureMap, EncodeError> {
    let output = outputs
        .get(name)
        .ok_or(EncodeError::MissingOutput { name })?;
    let (shape, data) = output
        .try_extract_tensor::<f32>()
        .map_err(|source| EncodeError::OutputType { name, source })?;
    if data.iter().any(|value| !value.is_finite()) {
        return Err(EncodeError::NonFinite { name });
    }
    Ok(FeatureMap {
        shape: shape.to_vec(),
        data: data.to_vec(),
    })
}

/// Extracts a named f32 decoder output as a slice, rejecting a non-finite value.
///
/// Symmetric with [`feature_map`] on the encoder side: a `NaN` `iou_predictions`
/// sorts as the maximum under `total_cmp`, so an unchecked `NaN` would be picked
/// as the best candidate and silently thresholded to an empty mask rather than
/// surfacing the corrupt decode.
fn extract<'a>(
    outputs: &'a ort::session::SessionOutputs,
    name: &'static str,
) -> Result<&'a [f32], SegmentError> {
    let output = outputs
        .get(name)
        .ok_or(SegmentError::MissingOutput { name })?;
    let (_, data) = output
        .try_extract_tensor::<f32>()
        .map_err(|source| SegmentError::OutputType { name, source })?;
    if data.iter().any(|value| !value.is_finite()) {
        return Err(SegmentError::NonFinite { name });
    }
    Ok(data)
}

/// Bilinearly upsamples the decoder's square low-resolution mask logits to the
/// original grid and thresholds at zero, the finish `Sam3Processor` applies. The
/// mask decode is resolution-independent, so the caller owns this step.
fn upsample_mask(logits: &[f32], low: usize, grid: Dimensions) -> Result<Region, MaskError> {
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
}

/// Why `encode` could not turn a decoded image into features.
#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("the image grid is not one the model can localize into")]
    Original(#[source] RegionError),

    #[error("could not resample the image to the encoder's square")]
    Resize(#[source] ResizeError),

    #[error("could not build the encoder input tensor")]
    Input(#[source] ort::Error),

    #[error("the image encoder failed to run")]
    Run(#[source] ort::Error),

    #[error("the encoder produced no `{name}` output")]
    MissingOutput { name: &'static str },

    #[error("encoder output `{name}` is not an f32 tensor")]
    OutputType {
        name: &'static str,
        #[source]
        source: ort::Error,
    },

    #[error("encoder output `{name}` holds a non-finite value")]
    NonFinite { name: &'static str },
}

/// Why `segment_rect` could not turn a box into a region.
#[derive(Debug, Error)]
pub enum SegmentError {
    #[error("could not build a decoder input tensor")]
    Input(#[source] ort::Error),

    #[error("the decoder failed to run")]
    Run(#[source] ort::Error),

    #[error("the decoder produced no `{name}` output")]
    MissingOutput { name: &'static str },

    #[error("decoder output `{name}` is not an f32 tensor")]
    OutputType {
        name: &'static str,
        #[source]
        source: ort::Error,
    },

    #[error("decoder output `{name}` holds a non-finite value")]
    NonFinite { name: &'static str },

    #[error("decoder returned {ious} scores and {masks} mask values, not the declared candidates")]
    OutputShape { ious: usize, masks: usize },

    #[error("the decoder returned no candidates to choose from")]
    NoCandidates,

    #[error("the decoder mask could not be resolved")]
    Mask(#[source] MaskError),
}
