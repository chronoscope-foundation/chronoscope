//! DINOv3 through `ort`: pixels in, the CLS embedding and patch grid out.
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
    manifest::{Dinov3Manifest, EMBEDDING_DIM},
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

/// A raw `EMBEDDING_DIM`-d embedding in the model's output space, carried as the
/// graph produced it.
///
/// Normalization is the consumer's concern: cosine similarity normalizes at the
/// comparison, and region pooling takes a coverage-weighted mean of raw patch
/// embeddings before L2-normalizing the pooled result. Only the crate mints
/// these, from tokens `forward` has already held to finite.
#[derive(Debug, Clone)]
pub struct Embedding([f32; EMBEDDING_DIM]);

impl Embedding {
    pub(crate) fn new(components: [f32; EMBEDDING_DIM]) -> Self {
        Self(components)
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }
}

/// DINOv3's output for one image: the CLS embedding and the patch grid, both raw
/// model-space vectors the pipeline normalizes where it compares or pools them.
#[derive(Debug, Clone)]
pub struct Features {
    /// The whole-image embedding: DINOv3's global summary of the frame, for
    /// image-level similarity, distinct from any mean of the patches.
    pub cls: Embedding,
    /// One feature vector per 16x16-pixel image patch, row-major over the patch
    /// grid: `patches[i]` is grid cell `(i / cols, i % cols)`, left-to-right
    /// then top-to-bottom, with `cols = resolution / patch_size`. This is the
    /// ordering region pooling reads to map a patch back to an image location.
    pub patches: Vec<Embedding>,
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

    #[error("`{OUTPUT}` has shape {shape:?}, not the one batch of manifest-declared tokens")]
    OutputShape { shape: Vec<i64> },

    #[error("`{OUTPUT}` holds a non-finite token, which would poison the patch reduction")]
    NonFiniteToken,
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
    /// runs the model, returning its CLS embedding and patch grid.
    ///
    /// `to_rgb8` fixes the channel count at three, broadcasting a grayscale
    /// frame to three planes the way the processor does when it converts to RGB.
    pub fn embed(&mut self, image: &DynamicImage) -> Result<Features, EmbedError> {
        let planar = ChwImage::from_rgb(&image.to_rgb8(), self.manifest.rescale_factor);
        let square = planar
            .resize(self.manifest.resolution)
            .map_err(EmbedError::Resize)?;
        self.forward(&square).map_err(EmbedError::Forward)
    }

    fn forward(&mut self, image: &ChwImage) -> Result<Features, ForwardError> {
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

        // The boundary: hold the output to the manifest's exact shape and length
        // so every downstream slice is in-bounds, and reject a non-finite token
        // before it can poison the patch reduction.
        let expected = self.manifest.sequence_length;
        let dimensions: &[i64] = shape;
        match *dimensions {
            [1, sequence_length, dim]
                if sequence_length == expected as i64
                    && dim == EMBEDDING_DIM as i64
                    && values.len() == expected * EMBEDDING_DIM =>
            {
                let (tokens, _rest) = values.as_chunks::<EMBEDDING_DIM>();
                if tokens
                    .iter()
                    .any(|token| token.iter().any(|value| !value.is_finite()))
                {
                    return Err(ForwardError::NonFiniteToken);
                }

                // The guard forces `sequence_length == expected`, which manifest
                // validation keeps `>= 1`, so `tokens` is non-empty and this
                // always binds. The else is unreachable; a runtime length just
                // keeps the compiler from seeing it.
                let [cls, ..] = tokens else {
                    return Err(ForwardError::OutputShape {
                        shape: dimensions.to_vec(),
                    });
                };
                let patches = tokens
                    .iter()
                    .skip(self.manifest.prefix_tokens)
                    .map(|token| Embedding::new(*token))
                    .collect();

                Ok(Features {
                    cls: Embedding::new(*cls),
                    patches,
                })
            }
            _ => Err(ForwardError::OutputShape {
                shape: dimensions.to_vec(),
            }),
        }
    }
}
