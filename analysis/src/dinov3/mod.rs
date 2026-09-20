//! DINOv3 through `ort`: pixels in, the CLS token and patch grid out.
//!
//! Mean/std normalization is baked into the graph by
//! `nix/scripts/export-dinov3.py`, since there is no `AutoImageProcessor` in
//! Rust and wrong constants would produce plausible embeddings that are
//! silently wrong. The split is where the checkpoint's own processor puts it:
//! it rescales to `[0, 1]` before resizing, so the resize runs in float and the
//! graph starts at normalize. [`Dinov3::embed`] therefore takes a decoded image
//! and owns the rescale, the resize to the model's fixed square resolution, RGB
//! channel order, and the float32 CHW layout the graph reads.

mod embedding;
mod manifest;
mod pool;
#[cfg(test)]
mod reference;

use std::{num::NonZeroUsize, path::Path};

use chronoscope_core::grammar::geometry::Region;
use image::DynamicImage;
use ort::{
    session::{RunOptions, Session},
    value::Tensor,
};
use thiserror::Error;

use crate::{
    onnx::{self, Accel, SessionError},
    preprocess::{CHANNELS, ChwImage},
};
use embedding::normalize;
use manifest::{Dinov3Manifest, EMBEDDING_DIM};

pub use crate::preprocess::ResizeError;
pub use embedding::{DegenerateEmbedding, Embedding};
pub use manifest::{ManifestError, ManifestInvalid};

/// The graph's one input, named by the export.
const INPUT: &str = "image";

/// The graph's one output. A pooled output would collapse the patch grid that
/// masked pooling reads.
const OUTPUT: &str = "last_hidden_state";

/// One token as the graph produced it: a raw vector in the model's output
/// space, finite because `forward` rejects any other.
type Token = [f32; EMBEDDING_DIM];

/// DINOv3's output for one image, read as unit-length [`Embedding`]s through
/// [`image`](Self::image) for the whole frame and, within the crate, a pooled
/// region; [`scene::Features`](crate::scene::Features) is the branded way to the
/// latter.
///
/// The tokens stay raw because a region's embedding is the normalized mean of
/// raw patches, the masked-average-pooling recipe DINO region descriptors use;
/// normalizing each patch first would discard the magnitudes that mean weighs.
#[derive(Debug, Clone)]
pub struct Features {
    /// DINOv3's global summary of the frame, for image-level similarity,
    /// distinct from any mean of the patches.
    cls: Token,
    /// One token per 16x16-pixel patch of the model's input square, row-major:
    /// `patches[i]` is grid cell `(i / cols, i % cols)`. `forward` holds the
    /// count to `cols * cols`, which region pooling indexes by.
    patches: Vec<Token>,
    /// Patches per side of the square grid.
    cols: NonZeroUsize,
}

impl Features {
    /// The whole-image embedding: the CLS token, normalized.
    pub fn image(&self) -> Result<Embedding, DegenerateEmbedding> {
        Embedding::from_raw(&self.cls)
    }

    /// The embedding of the part of the image `region` covers: the mean of the
    /// patch grid, read as a bilinear field, over the mask, normalized.
    ///
    /// `region` must be a mask over the image these features embed. It is read
    /// in proportional coordinates, so any mask resolution of that frame works:
    /// `embed` squashes the whole frame into the model's square, so a fraction of
    /// the mask's width is the same fraction of the patch grid's.
    ///
    /// Crate-visible because that precondition is unstated here and unenforceable:
    /// [`scene::Features::region`](crate::scene::Features::region) is the way in,
    /// and its brand carries the proof.
    ///
    /// Errors only when the covered patches cancel, leaving no direction.
    pub(crate) fn region(&self, region: &Region) -> Result<Embedding, DegenerateEmbedding> {
        let weights = pool::patch_weights(region, self.cols);
        let mut sum = [0.0_f64; EMBEDDING_DIM];
        let mut mass = 0.0;
        for (patch, &weight) in self.patches.iter().zip(&weights) {
            if weight > 0.0 {
                for (total, &value) in sum.iter_mut().zip(patch) {
                    *total += weight * f64::from(value);
                }
                mass += weight * magnitude(patch);
            }
        }
        normalize(&sum, mass)
    }

    /// The raw patch grid, row-major as `patches` is, for the reference
    /// comparison that holds each patch against the checkpoint's own output.
    /// The pipeline reads patches through [`region`](Self::region), so that
    /// comparison is the only caller.
    #[cfg(test)]
    pub(crate) fn raw_patches(&self) -> &[Token] {
        &self.patches
    }
}

/// A token's L2 norm, in the f64 the pooled sums accumulate in.
fn magnitude(token: &Token) -> f64 {
    token
        .iter()
        .map(|&value| f64::from(value) * f64::from(value))
        .sum::<f64>()
        .sqrt()
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
    pub fn open(export: &Path, accel: Accel) -> Result<Self, OpenError> {
        let manifest = Dinov3Manifest::load(export).map_err(OpenError::Manifest)?;
        let session =
            onnx::session_for(&manifest.graph, accel, "dinov3", &[]).map_err(OpenError::Session)?;
        Ok(Self { session, manifest })
    }

    /// The square input side the model takes, for a caller that wants to name
    /// the resolution in its own diagnostics.
    pub fn resolution(&self) -> usize {
        self.manifest.resolution
    }

    /// Preprocesses a decoded image the way the checkpoint's processor does and
    /// runs the model, returning its CLS token and patch grid.
    ///
    /// `to_rgb8` fixes the channel count at three, broadcasting a grayscale
    /// frame to three planes the way the processor does when it converts to RGB.
    pub async fn embed(&mut self, image: &DynamicImage) -> Result<Features, EmbedError> {
        let factor = self.manifest.rescale_factor;
        let planar = ChwImage::from_rgb(&image.to_rgb8(), |byte| f32::from(byte) * factor);
        let square = planar
            .resize(self.manifest.resolution)
            .map_err(EmbedError::Resize)?;
        self.forward(square).await.map_err(EmbedError::Forward)
    }

    /// The input tensor owns its samples rather than viewing them. A borrowed
    /// view is sound only while the inference future lives, and dropping that
    /// future (a timeout, a `select!`) ends the borrow while a runtime thread
    /// may still be reading it; an owned value's backing is held by the run
    /// itself.
    async fn forward(&mut self, image: ChwImage<f32>) -> Result<Features, ForwardError> {
        let shape = vec![CHANNELS as i64, image.height() as i64, image.width() as i64];
        let input =
            Tensor::from_array((shape, image.into_samples())).map_err(ForwardError::Input)?;

        let options = RunOptions::new().map_err(ForwardError::Run)?;
        let outputs = self
            .session
            .run_async(ort::inputs![INPUT => input], &options)
            .map_err(ForwardError::Run)?
            .await
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
                // `expected` is the prefix plus the grid side squared, so the
                // tokens past the prefix are exactly the grid `Features` indexes.
                let patches = tokens
                    .iter()
                    .skip(self.manifest.prefix_tokens)
                    .copied()
                    .collect();

                Ok(Features {
                    cls: *cls,
                    patches,
                    cols: self.manifest.patch_grid_side,
                })
            }
            _ => Err(ForwardError::OutputShape {
                shape: dimensions.to_vec(),
            }),
        }
    }
}
