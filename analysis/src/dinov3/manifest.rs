//! The export's `manifest.json`, parsed into the preprocessing contract.
//!
//! `nix/scripts/export-dinov3.py` writes every number the caller owes into the
//! manifest so the ingest honours the checkpoint it loaded instead of a
//! transcription: the rescale factor, the resample kernel, and the square the
//! model takes. `nix/scripts/verify-onnx.py` merges each graph's flat facts in
//! beside its input signature and writes the manifest only once every check has
//! passed, so this is the single file the Rust side reads.
//!
//! [`load`](Dinov3Manifest::load) is the validation: a [`Dinov3Manifest`] exists
//! only when every assertion held. A checkpoint that declared a resample this
//! crate cannot reproduce, a rescale that could push a byte outside `[0, 1]`, or
//! a shape that disagrees with itself fails there, at load, naming what it found.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use thiserror::Error;

use crate::model_manifest::Signature;

/// The graph's one image input, named by the export.
const INPUT: &str = "image";

/// RGB planes first, the layout the caller feeds and what a channel-first square
/// input shape declares.
const CHANNELS: i64 = 3;

/// The model's embedding width, the size `Embedding` is fixed to, and the sole
/// value tied to the DINOv3 variant: grid, prefix, and resolution all flow from
/// the manifest, so a different variant (ViT-B is 768, ViT-g 1536) changes this
/// constant and its assertion and nothing else. A checkpoint declaring a
/// different width fails at manifest load, and `forward` rechecks it against the
/// actual output tensor.
pub(crate) const EMBEDDING_DIM: usize = 1024;

/// One DINOv3 export's manifest: the graph and the numbers for feeding it, each
/// held to its assertion at [`load`](Self::load).
#[derive(Debug)]
pub(crate) struct Dinov3Manifest {
    pub(crate) graph: PathBuf,
    /// The square input side, derived from the graph's input shape.
    pub(crate) resolution: usize,
    /// Tokens the graph emits per image: the prefix then the patch grid, summed
    /// from `prefix_tokens` and the grid. `forward` holds its output to this
    /// count, which is what makes slicing the patches out of it total.
    pub(crate) sequence_length: usize,
    /// Leading tokens (CLS then registers) before the patch grid; patches begin
    /// at this index.
    pub(crate) prefix_tokens: usize,
    /// Per-byte scale, held so `255` times it stays in `[0, 1]`.
    pub(crate) rescale_factor: f32,
}

impl Dinov3Manifest {
    /// Reads and validates `<export>/manifest.json`.
    pub(crate) fn load(export: &Path) -> Result<Self, ManifestError> {
        let path = export.join("manifest.json");
        let bytes = fs::read(&path).map_err(|source| ManifestError::Read {
            path: path.clone(),
            source,
        })?;
        let wire: Wire = serde_json::from_slice(&bytes).map_err(|source| ManifestError::Parse {
            path: path.clone(),
            source,
        })?;
        Self::validate(export, wire).map_err(|source| ManifestError::Invalid { path, source })
    }

    fn validate(export: &Path, wire: Wire) -> Result<Self, ManifestInvalid> {
        let model = wire.dinov3;

        // The caller rescales to float and hands the graph the rescaled square,
        // so the input is `tensor(float)`; a `tensor(uint8)` here would mean the
        // graph still owes the rescale the caller already spent.
        let image = model
            .inputs
            .iter()
            .find(|input| input.name == INPUT)
            .ok_or(ManifestInvalid::MissingInput { name: INPUT })?;
        if image.dtype != "tensor(float)" {
            return Err(ManifestInvalid::InputDtype {
                dtype: image.dtype.clone(),
            });
        }

        // The channel-first square input carries the resolution: a [3, res, res]
        // shape both fixes the layout and names the side the caller resizes to.
        let resolution = match image.shape.as_slice() {
            &[CHANNELS, height, width] if height == width && height > 0 => height as usize,
            _ => {
                return Err(ManifestInvalid::InputShape {
                    shape: image.shape.clone(),
                });
            }
        };

        // Pooling indexes the grid row-major, so a non-square grid would pool
        // over the wrong shape and a zero grid would name no patch tokens at all.
        // Whether a well-formed grid matches `resolution / patch` is left to
        // `forward`'s token recount, since patch size is not on the wire.
        let [rows, columns] = model.patch_grid;
        if rows != columns || rows == 0 {
            return Err(ManifestInvalid::PatchGridShape { rows, columns });
        }

        // CLS lives at index 0 by DINOv3 convention, so the prefix cannot be
        // empty. Without this, a prefix-less export would validate and the top-
        // left patch would masquerade as the CLS token.
        if model.prefix_tokens == 0 {
            return Err(ManifestInvalid::PrefixTokens);
        }

        if model.hidden_size != EMBEDDING_DIM {
            return Err(ManifestInvalid::EmbeddingDim {
                embedding_dim: model.hidden_size,
                expected: EMBEDDING_DIM,
            });
        }

        // The caller decodes RGB into a CHW float plane and filters every
        // downscale with an antialiased bilinear kernel. A checkpoint declaring
        // any other order, resample, or an unfiltered resize would be honoured on
        // paper and ignored in code, so it fails here rather than preprocessing
        // wrong.
        if model.interpolation != "bilinear" {
            return Err(ManifestInvalid::Interpolation {
                value: model.interpolation,
            });
        }

        if model.channel_order != "rgb" {
            return Err(ManifestInvalid::ChannelOrder {
                channel_order: model.channel_order,
            });
        }

        if !model.antialias {
            return Err(ManifestInvalid::Antialias);
        }

        Ok(Self {
            graph: export.join(model.graph),
            resolution,
            sequence_length: model
                .prefix_tokens
                .saturating_add(rows.saturating_mul(columns)),
            prefix_tokens: model.prefix_tokens,
            rescale_factor: parse_rescale_factor(model.rescale_factor)?,
        })
    }
}

/// Narrows the manifest's rescale factor to `f32` and holds it to `[0, 1]`.
///
/// The bound is checked in `f32`, the arithmetic that spends it: `255` is the
/// largest byte, and the `f32` nearest to `1/255` multiplies with it to exactly
/// `1.0`, where the same product taken in `f64` rounds above `1.0` and would
/// reject the honest value.
fn parse_rescale_factor(factor: f64) -> Result<f32, ManifestInvalid> {
    let narrowed = factor as f32;
    if narrowed >= 0.0 && 255.0_f32 * narrowed <= 1.0 {
        Ok(narrowed)
    } else {
        Err(ManifestInvalid::RescaleFactor { factor })
    }
}

/// Why a manifest could not be read as the preprocessing contract.
#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("could not read the manifest at `{path}`")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("`{path}` is not shaped like a DINOv3 manifest")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("`{path}` describes a model this crate cannot honour")]
    Invalid {
        path: PathBuf,
        #[source]
        source: ManifestInvalid,
    },
}

/// The ways a manifest's own numbers fail their assertions, named for the day
/// a checkpoint change trips one.
#[derive(Debug, Error)]
pub enum ManifestInvalid {
    #[error("the graph declares no `{name}` input")]
    MissingInput { name: &'static str },

    #[error("input dtype `{dtype}` is not the `tensor(float)` the model takes")]
    InputDtype { dtype: String },

    #[error("input shape {shape:?} is not a channel-first [3, N, N] square")]
    InputShape { shape: Vec<i64> },

    #[error("patch grid {rows}x{columns} is not a positive square")]
    PatchGridShape { rows: usize, columns: usize },

    #[error("the export declares zero prefix tokens, so there is no CLS token at index 0")]
    PrefixTokens,

    #[error("embedding dimension {embedding_dim} is not the {expected} this crate is built for")]
    EmbeddingDim {
        embedding_dim: usize,
        expected: usize,
    },

    #[error("rescale factor {factor} would let a decoded byte leave `[0, 1]`")]
    RescaleFactor { factor: f64 },

    #[error("resample `{value}` is not a kernel this crate reproduces")]
    Interpolation { value: String },

    #[error("caller channel order `{channel_order}` is not the `rgb` the caller decodes")]
    ChannelOrder { channel_order: String },

    #[error("caller resize declares antialias off, but the caller filters every downscale")]
    Antialias,
}

/// The one graph this crate drives, flattened to the facts the caller feeds it
/// with. Its input signature carries the resolution and dtype; the rest is
/// preprocessing the tensor shape does not encode.
#[derive(Deserialize)]
struct Wire {
    dinov3: Graph,
}

#[derive(Deserialize)]
struct Graph {
    graph: PathBuf,
    inputs: Vec<Signature>,
    channel_order: String,
    interpolation: String,
    antialias: bool,
    rescale_factor: f64,
    prefix_tokens: usize,
    patch_grid: [usize; 2],
    hidden_size: usize,
}
