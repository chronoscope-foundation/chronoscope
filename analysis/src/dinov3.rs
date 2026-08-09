//! DINOv3 through `ort`: pixels in, `last_hidden_state` out.
//!
//! Mean/std normalization is baked into the graph by
//! `nix/scripts/export-dinov3.py`, since there is no `AutoImageProcessor` in
//! Rust and wrong constants would produce plausible embeddings that are
//! silently wrong. The split is where the checkpoint's own processor puts it:
//! it rescales to `[0, 1]` before resizing, so the resize runs in float and the
//! graph starts at normalize. [`Dinov3::embed`] therefore takes a decoded image
//! and owns the rescale, the resize to the model's fixed square resolution, RGB
//! channel order, and the float32 CHW layout the graph reads.

use std::path::Path;

use fast_image_resize::{
    FilterType, ResizeAlg, ResizeOptions, Resizer,
    images::{TypedImage, TypedImageRef},
    pixels::F32,
};
use image::{DynamicImage, RgbImage};
use ort::{session::Session, value::TensorRef};
use thiserror::Error;

use crate::{
    manifest::Dinov3Manifest,
    onnx::{self, SessionError},
};

pub use crate::manifest::{ManifestError, ManifestInvalid};

/// The graph's one input, named by the export.
const INPUT: &str = "image";

/// The graph's one output. A pooled output would collapse the patch grid that
/// masked pooling reads.
const OUTPUT: &str = "last_hidden_state";

/// RGB, the only channel count the model takes and what `to_rgb8` produces.
const CHANNELS: usize = 3;

/// Rescaled samples in channel-major order, three planes by construction.
struct ChwImage {
    height: usize,
    width: usize,
    samples: Vec<f32>,
}

impl ChwImage {
    /// Rescales an RGB frame into a planar float image, packing HWC to CHW.
    ///
    /// The frame carries its own dimensions, so the planes fill the extent they
    /// describe. Bytes are the only way in and the factor is validated at load,
    /// so every sample lands in `[0, 1]`.
    fn from_rgb(image: &RgbImage, factor: f32) -> Self {
        let (width, height) = image.dimensions();
        let interleaved = image.as_raw();
        let mut samples = Vec::with_capacity(interleaved.len());
        for channel in 0..CHANNELS {
            samples.extend(
                interleaved
                    .iter()
                    .skip(channel)
                    .step_by(CHANNELS)
                    .map(|&byte| f32::from(byte) * factor),
            );
        }
        Self {
            height: height as usize,
            width: width as usize,
            samples,
        }
    }

    /// Resamples to the square the model takes with an antialiased bilinear
    /// kernel, squashing the frame rather than letterboxing it: the processor is
    /// handed one `size` and no aspect to preserve.
    ///
    /// Each channel is resampled on its own, which is what a separable kernel
    /// over a planar image means and what torchvision does with the same input.
    fn resize(&self, resolution: usize) -> Result<Self, ResizeError> {
        // The resampler indexes pixels with `u32` and answers a zero-sized
        // request with an empty image rather than an error, so a degenerate
        // extent has to be caught here or it becomes a silently empty tensor.
        let extent = |value: usize| u32::try_from(value).ok().filter(|&value| value > 0);
        let (Some(width), Some(height), Some(side)) =
            (extent(self.width), extent(self.height), extent(resolution))
        else {
            return Err(ResizeError::Dimensions {
                height: self.height,
                width: self.width,
                resolution,
            });
        };

        // `Convolution` is the antialiased form: it widens kernel support by the
        // reduction factor, the way torchvision and Pillow both do, so every
        // downscale the corpus contains is filtered rather than point-sampled.
        let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear));
        let mut resizer = Resizer::new();
        let mut samples = Vec::with_capacity(CHANNELS * resolution * resolution);

        for plane in self.samples.chunks_exact(self.height * self.width) {
            let pixels: Vec<F32> = plane.iter().map(|&sample| F32::new(sample)).collect();
            let source = TypedImageRef::new(width, height, &pixels).map_err(|source| {
                ResizeError::Plane {
                    height: self.height,
                    width: self.width,
                    source,
                }
            })?;
            let mut resampled: TypedImage<F32> = TypedImage::new(side, side);
            resizer
                .resize_typed(&source, &mut resampled, &options)
                .map_err(|source| ResizeError::Resample { resolution, source })?;
            samples.extend(resampled.pixels().iter().map(|pixel| pixel.0));
        }

        Ok(Self {
            height: resolution,
            width: resolution,
            samples,
        })
    }
}

/// Why an image could not be resampled to the model's square.
#[derive(Debug, Error)]
pub enum ResizeError {
    #[error("cannot resample a {height}x{width} image to {resolution}x{resolution}")]
    Dimensions {
        height: usize,
        width: usize,
        resolution: usize,
    },

    #[error("the resampler rejected a {height}x{width} plane")]
    Plane {
        height: usize,
        width: usize,
        #[source]
        source: fast_image_resize::InvalidPixelsSize,
    },

    #[error("resampling a plane to {resolution}x{resolution} failed")]
    Resample {
        resolution: usize,
        #[source]
        source: fast_image_resize::ResizeError,
    },
}

/// `last_hidden_state` for one image: `sequence_length` rows of `hidden_size`,
/// prefix tokens first.
#[derive(Debug, Clone, PartialEq)]
pub struct Tokens {
    hidden_size: usize,
    values: Vec<f32>,
}

impl Tokens {
    pub fn hidden_size(&self) -> usize {
        self.hidden_size
    }

    pub fn sequence_length(&self) -> usize {
        self.values.len() / self.hidden_size
    }

    /// The `index`th token, `None` past the end of the sequence.
    pub fn token(&self, index: usize) -> Option<&[f32]> {
        let start = index.checked_mul(self.hidden_size)?;
        self.values.get(start..start.checked_add(self.hidden_size)?)
    }
}

/// Why a forward pass did not produce tokens.
#[derive(Debug, Error)]
pub enum ForwardError {
    #[error("could not build the input tensor")]
    Input(#[source] ort::Error),

    #[error("the model failed to run")]
    Run(#[source] ort::Error),

    #[error("the model produced no `{OUTPUT}`")]
    MissingOutput,

    #[error("`{OUTPUT}` is not an f32 tensor")]
    OutputType(#[source] ort::Error),

    #[error("`{OUTPUT}` has shape {shape:?}, expected one batch of tokens")]
    OutputShape { shape: Vec<i64> },
}

/// Why `embed` could not turn a decoded image into tokens.
#[derive(Debug, Error)]
pub enum EmbedError {
    #[error("could not resample the image to the model's input square")]
    Resize(#[source] ResizeError),

    #[error("the model did not run")]
    Forward(#[source] ForwardError),
}

/// Why a model could not be opened from its export directory.
#[derive(Debug, Error)]
pub enum OpenError {
    #[error("the export's manifest could not be loaded")]
    Manifest(#[source] ManifestError),

    #[error("the model could not be opened")]
    Session(#[source] SessionError),
}

/// One loaded DINOv3 model and the manifest that says how to feed it.
pub struct Dinov3 {
    session: Session,
    manifest: Dinov3Manifest,
}

impl Dinov3 {
    /// Loads the export's manifest and opens the model it names, ready to embed
    /// against its contract.
    pub fn open(export: &Path) -> Result<Self, OpenError> {
        let manifest = Dinov3Manifest::load(export).map_err(OpenError::Manifest)?;
        let session = onnx::session(&manifest.graph).map_err(OpenError::Session)?;
        Ok(Self { session, manifest })
    }

    /// Preprocesses a decoded image the way the checkpoint's processor does and
    /// runs the model, returning its `last_hidden_state`.
    ///
    /// `to_rgb8` fixes the channel count at three, broadcasting a grayscale
    /// frame to three planes the way the processor does when it converts to RGB.
    pub fn embed(&mut self, image: &DynamicImage) -> Result<Tokens, EmbedError> {
        let planar = ChwImage::from_rgb(&image.to_rgb8(), self.manifest.rescale_factor);
        let square = planar
            .resize(self.manifest.resolution)
            .map_err(EmbedError::Resize)?;
        self.forward(&square).map_err(EmbedError::Forward)
    }

    fn forward(&mut self, image: &ChwImage) -> Result<Tokens, ForwardError> {
        let shape = vec![CHANNELS as i64, image.height as i64, image.width as i64];
        let input = TensorRef::from_array_view((shape, image.samples.as_slice()))
            .map_err(ForwardError::Input)?;

        let outputs = self
            .session
            .run(ort::inputs![INPUT => input])
            .map_err(ForwardError::Run)?;
        let output = outputs.get(OUTPUT).ok_or(ForwardError::MissingOutput)?;
        let (shape, values) = output
            .try_extract_tensor::<f32>()
            .map_err(ForwardError::OutputType)?;

        let dimensions: &[i64] = shape;
        match *dimensions {
            [1, sequence_length, hidden_size] if sequence_length > 0 && hidden_size > 0 => {
                Ok(Tokens {
                    hidden_size: hidden_size as usize,
                    values: values.to_vec(),
                })
            }
            _ => Err(ForwardError::OutputShape {
                shape: dimensions.to_vec(),
            }),
        }
    }
}
