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

mod manifest;

use std::path::Path;

use image::DynamicImage;
use ort::{session::Session, value::TensorRef};
use thiserror::Error;

use crate::{
    onnx::{self, Accel, SessionError},
    preprocess::{CHANNELS, ChwImage},
};
use manifest::{Dinov3Manifest, EMBEDDING_DIM};

pub use crate::preprocess::ResizeError;
pub use manifest::{ManifestError, ManifestInvalid};

/// The graph's one input, named by the export.
const INPUT: &str = "image";

/// The graph's one output. A pooled output would collapse the patch grid that
/// masked pooling reads.
const OUTPUT: &str = "last_hidden_state";

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
    /// runs the model, returning its CLS embedding and patch grid.
    ///
    /// `to_rgb8` fixes the channel count at three, broadcasting a grayscale
    /// frame to three planes the way the processor does when it converts to RGB.
    pub fn embed(&mut self, image: &DynamicImage) -> Result<Features, EmbedError> {
        let factor = self.manifest.rescale_factor;
        let planar = ChwImage::from_rgb(&image.to_rgb8(), |byte| f32::from(byte) * factor);
        let square = planar
            .resize(self.manifest.resolution)
            .map_err(EmbedError::Resize)?;
        self.forward(&square).map_err(EmbedError::Forward)
    }

    fn forward(&mut self, image: &ChwImage<f32>) -> Result<Features, ForwardError> {
        let shape = vec![CHANNELS as i64, image.height() as i64, image.width() as i64];
        let input =
            TensorRef::from_array_view((shape, image.samples())).map_err(ForwardError::Input)?;

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
