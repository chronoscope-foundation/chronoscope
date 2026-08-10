//! The export's `manifest.json`, parsed into the preprocessing contract.
//!
//! `nix/scripts/export-dinov3.py` writes every number the caller owes into the
//! manifest so the ingest honours the checkpoint it loaded instead of a
//! transcription: the rescale factor, the resample kernel, and the square the
//! model takes. `nix/scripts/verify-onnx.py` embeds each sidecar under
//! `models.<name>.metadata` and writes the manifest only once every check has
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
    pub(crate) resolution: usize,
    /// Tokens the graph emits per image: the prefix then the patch grid.
    /// `forward` holds its output to this count, which is what makes slicing the
    /// patches out of it total.
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
        let model = wire.models.dinov3;
        let meta = model.metadata;
        let resize = meta.preprocessing.caller_resize;

        // `is_multiple_of` reports false for a zero patch size on a non-zero
        // resolution, so a zero patch size is rejected without a separate guard.
        if meta.resolution == 0 || !meta.resolution.is_multiple_of(meta.patch_size) {
            return Err(ManifestInvalid::Resolution {
                resolution: meta.resolution,
                patch_size: meta.patch_size,
            });
        }

        // Pooling indexes the grid the resolution lays out, so a grid that
        // disagrees with `resolution / patch_size` would pool over the wrong
        // shape. The division is exact by the check above.
        let [rows, columns] = meta.patch_grid;
        let side = meta.resolution / meta.patch_size;
        if rows != side || columns != side {
            return Err(ManifestInvalid::PatchGridShape {
                rows,
                columns,
                expected: side,
                resolution: meta.resolution,
                patch_size: meta.patch_size,
            });
        }

        if meta.prefix_tokens.checked_add(rows.saturating_mul(columns))
            != Some(meta.sequence_length)
        {
            return Err(ManifestInvalid::SequenceLength {
                sequence_length: meta.sequence_length,
                prefix_tokens: meta.prefix_tokens,
                rows,
                columns,
            });
        }

        // CLS lives at index 0 by DINOv3 convention, so the prefix cannot be
        // empty. Without this, a prefix-less export would validate and the top-
        // left patch would masquerade as the CLS token.
        if meta.prefix_tokens == 0 {
            return Err(ManifestInvalid::PrefixTokens);
        }

        if meta.hidden_size != EMBEDDING_DIM {
            return Err(ManifestInvalid::EmbeddingDim {
                embedding_dim: meta.hidden_size,
                expected: EMBEDDING_DIM,
            });
        }

        if meta.input_dtype != "float32" {
            return Err(ManifestInvalid::InputDtype {
                dtype: meta.input_dtype,
            });
        }

        if resize.target != [meta.resolution, meta.resolution] {
            return Err(ManifestInvalid::Target {
                target: resize.target,
                resolution: meta.resolution,
            });
        }

        // The caller decodes RGB into a CHW float plane, filters every downscale
        // with an antialiased bilinear kernel. A checkpoint declaring any other
        // order, layout, resample, or an unfiltered resize would be honoured on
        // paper and ignored in code, so it fails here rather than preprocessing
        // wrong.
        if resize.interpolation != "bilinear" {
            return Err(ManifestInvalid::Interpolation {
                value: resize.interpolation,
            });
        }

        if resize.channel_order != "rgb" {
            return Err(ManifestInvalid::ChannelOrder {
                channel_order: resize.channel_order,
            });
        }

        if resize.layout != "chw" {
            return Err(ManifestInvalid::Layout {
                layout: resize.layout,
            });
        }

        if !resize.antialias {
            return Err(ManifestInvalid::Antialias);
        }

        Ok(Self {
            graph: export.join(model.graph),
            resolution: meta.resolution,
            sequence_length: meta.sequence_length,
            prefix_tokens: meta.prefix_tokens,
            rescale_factor: parse_rescale_factor(resize.rescale_factor)?,
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
    #[error("resolution {resolution} is not a positive multiple of patch size {patch_size}")]
    Resolution {
        resolution: usize,
        patch_size: usize,
    },

    #[error(
        "patch grid {rows}x{columns} is not the {expected}x{expected} square a \
         {resolution}px input at patch size {patch_size} lays out"
    )]
    PatchGridShape {
        rows: usize,
        columns: usize,
        expected: usize,
        resolution: usize,
        patch_size: usize,
    },

    #[error(
        "sequence length {sequence_length} is not {prefix_tokens} prefix + \
         {rows}x{columns} patches"
    )]
    SequenceLength {
        sequence_length: usize,
        prefix_tokens: usize,
        rows: usize,
        columns: usize,
    },

    #[error("the export declares zero prefix tokens, so there is no CLS token at index 0")]
    PrefixTokens,

    #[error("embedding dimension {embedding_dim} is not the {expected} this crate is built for")]
    EmbeddingDim {
        embedding_dim: usize,
        expected: usize,
    },

    #[error("input dtype `{dtype}` is not the `float32` the model takes")]
    InputDtype { dtype: String },

    #[error("caller resize target {target:?} is not the {resolution}px square")]
    Target {
        target: [usize; 2],
        resolution: usize,
    },

    #[error("rescale factor {factor} would let a decoded byte leave `[0, 1]`")]
    RescaleFactor { factor: f64 },

    #[error("resample `{value}` is not a kernel this crate reproduces")]
    Interpolation { value: String },

    #[error("caller channel order `{channel_order}` is not the `rgb` the caller decodes")]
    ChannelOrder { channel_order: String },

    #[error("caller layout `{layout}` is not the `chw` the caller feeds the model")]
    Layout { layout: String },

    #[error("caller resize declares antialias off, but the caller filters every downscale")]
    Antialias,
}

/// The manifest fields this crate reads. Every other key the export writes
/// (graph signatures, baked normalization) is left for its own reader.
#[derive(Deserialize)]
struct Wire {
    models: WireModels,
}

#[derive(Deserialize)]
struct WireModels {
    dinov3: WireModel,
}

#[derive(Deserialize)]
struct WireModel {
    graph: PathBuf,
    metadata: WireMetadata,
}

#[derive(Deserialize)]
struct WireMetadata {
    resolution: usize,
    input_dtype: String,
    patch_size: usize,
    patch_grid: [usize; 2],
    prefix_tokens: usize,
    hidden_size: usize,
    sequence_length: usize,
    preprocessing: WirePreprocessing,
}

#[derive(Deserialize)]
struct WirePreprocessing {
    caller_resize: WireCallerResize,
}

#[derive(Deserialize)]
struct WireCallerResize {
    rescale_factor: f64,
    interpolation: String,
    target: [usize; 2],
    channel_order: String,
    layout: String,
    antialias: bool,
}
