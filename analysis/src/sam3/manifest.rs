//! The `sam3-onnx` export's `manifest.json`, parsed into the contract the runner
//! honours.
//!
//! `nix/scripts/verify-onnx.py` writes it only after every graph loads, so this
//! is the single file the Rust side reads. The export carries the whole SAM 3
//! model, so the manifest describes four graphs; the runner drives two of them —
//! the image encoder and the interactive single-object decoder — and validates
//! the parts that silently corrupt those if wrong. The grounding decoder and
//! language encoder ride along for concept/text prompting, unread here until a
//! caller wants them.
//!
//! [`load`](SamManifest::load) is the validation, mirroring the DINOv3
//! manifest's decline-to-trust stance: a resize that letterboxes instead of
//! stretching (box prompts land off their objects), a BGR or HWC caller, or a
//! float input dtype (the caller would owe a rescale the graph already bakes).

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use thiserror::Error;

/// The encoder's one image input, named by the export.
const INPUT: &str = "image";

/// RGB planes, the only channel count the encoder takes and what `to_rgb8`
/// produces.
const CHANNELS: i64 = 3;

/// One `sam3-onnx` export's manifest, reduced to what the interactive runner
/// drives: the image encoder, the interactive decoder, and the numbers for
/// feeding and reading them, each held to its assertion at [`load`](Self::load).
#[derive(Debug)]
pub(crate) struct SamManifest {
    pub(crate) image_encoder: PathBuf,
    pub(crate) decoder: PathBuf,
    /// The square side the encoder takes; the caller stretches to it.
    pub(crate) resolution: usize,
    /// The side of the square low-resolution masks the decoder emits, which the
    /// caller upsamples to the original grid.
    pub(crate) low_res_mask_size: usize,
    /// Ambiguity candidates the decoder returns per prompt; the caller keeps the
    /// highest-IoU one.
    pub(crate) candidates: usize,
}

impl SamManifest {
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
        let encoder = wire.models.image_encoder;
        let resize = encoder.metadata.preprocessing.caller_resize;
        let resolution = encoder.metadata.resolution;
        if resolution == 0 {
            return Err(ManifestInvalid::Resolution { resolution });
        }

        // The encoder takes one uint8 RGB square and the caller feeds it raw:
        // the graph owns rescale and normalize. The dtype is a preprocessing
        // fact, not just a shape, since a float input would mean the caller still
        // owes the rescale.
        let image = encoder
            .inputs
            .iter()
            .find(|input| input.name == INPUT)
            .ok_or(ManifestInvalid::MissingInput { name: INPUT })?;
        if image.dtype != "tensor(uint8)" {
            return Err(ManifestInvalid::InputDtype {
                dtype: image.dtype.clone(),
            });
        }
        if image.shape != [CHANNELS, resolution as i64, resolution as i64] {
            return Err(ManifestInvalid::InputShape {
                shape: image.shape.clone(),
                resolution,
            });
        }

        // The caller owes only the resize: a pure RGB CHW stretch to the square.
        // A letterbox feeds padding the model never saw and lands box prompts off
        // their objects; BGR or HWC corrupts silently.
        if resize.mode != "stretch" {
            return Err(ManifestInvalid::ResizeMode { mode: resize.mode });
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
        if resize.target != [resolution, resolution] {
            return Err(ManifestInvalid::Target {
                target: resize.target,
                resolution,
            });
        }

        let decoder = wire.models.decoder_interactive;
        if decoder.metadata.low_res_mask_size == 0 {
            return Err(ManifestInvalid::LowResMaskSize);
        }
        if decoder.metadata.num_candidates == 0 {
            return Err(ManifestInvalid::Candidates);
        }

        Ok(Self {
            image_encoder: export.join(encoder.graph),
            decoder: export.join(decoder.graph),
            resolution,
            low_res_mask_size: decoder.metadata.low_res_mask_size,
            candidates: decoder.metadata.num_candidates,
        })
    }
}

/// Why a manifest could not be read as the SAM 3 contract.
#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("could not read the manifest at `{path}`")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("`{path}` is not shaped like a SAM 3 manifest")]
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

/// The ways a manifest's own numbers fail their assertions, named for the day a
/// re-export trips one.
#[derive(Debug, Error)]
pub enum ManifestInvalid {
    #[error("resolution must be positive, got {resolution}")]
    Resolution { resolution: usize },

    #[error("the encoder declares no `{name}` input")]
    MissingInput { name: &'static str },

    #[error("encoder input dtype `{dtype}` is not the `tensor(uint8)` the caller feeds raw")]
    InputDtype { dtype: String },

    #[error("encoder input shape {shape:?} is not the [3, {resolution}, {resolution}] square")]
    InputShape { shape: Vec<i64>, resolution: usize },

    #[error("caller resize mode `{mode}` is not the `stretch` box prompts assume")]
    ResizeMode { mode: String },

    #[error("caller channel order `{channel_order}` is not the `rgb` the caller decodes")]
    ChannelOrder { channel_order: String },

    #[error("caller layout `{layout}` is not the `chw` the caller feeds the encoder")]
    Layout { layout: String },

    #[error("caller resize target {target:?} is not the {resolution}px square")]
    Target {
        target: [usize; 2],
        resolution: usize,
    },

    #[error("the interactive decoder declares a zero low-resolution mask size")]
    LowResMaskSize,

    #[error("the interactive decoder declares zero mask candidates")]
    Candidates,
}

/// The manifest fields this crate reads. Every other key the export writes (the
/// grounding decoder and language encoder signatures, the graph assertions) is
/// ignored, since serde drops unknown fields.
#[derive(Deserialize)]
struct Wire {
    models: WireModels,
}

#[derive(Deserialize)]
struct WireModels {
    image_encoder: WireEncoder,
    decoder_interactive: WireDecoder,
}

#[derive(Deserialize)]
struct WireEncoder {
    graph: PathBuf,
    inputs: Vec<WireInput>,
    metadata: WireEncoderMeta,
}

#[derive(Deserialize)]
struct WireInput {
    name: String,
    dtype: String,
    shape: Vec<i64>,
}

#[derive(Deserialize)]
struct WireEncoderMeta {
    resolution: usize,
    preprocessing: WirePreprocessing,
}

#[derive(Deserialize)]
struct WirePreprocessing {
    caller_resize: WireCallerResize,
}

#[derive(Deserialize)]
struct WireCallerResize {
    mode: String,
    channel_order: String,
    layout: String,
    target: [usize; 2],
}

#[derive(Deserialize)]
struct WireDecoder {
    graph: PathBuf,
    metadata: WireDecoderMeta,
}

#[derive(Deserialize)]
struct WireDecoderMeta {
    low_res_mask_size: usize,
    num_candidates: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A minimal manifest carrying exactly the fields the loader reads, with the
    /// real export's values. Tests mutate one field to exercise each assertion.
    fn valid() -> Value {
        json!({
            "models": {
                "image_encoder": {
                    "graph": "image_encoder/sam3_image_encoder.onnx",
                    "inputs": [{ "name": "image", "dtype": "tensor(uint8)", "shape": [3, 1008, 1008] }],
                    "metadata": {
                        "resolution": 1008,
                        "preprocessing": {
                            "caller_resize": {
                                "mode": "stretch",
                                "channel_order": "rgb",
                                "layout": "chw",
                                "target": [1008, 1008]
                            }
                        }
                    }
                },
                "decoder_interactive": {
                    "graph": "decoder_interactive/sam3_decoder_interactive.onnx",
                    "metadata": { "low_res_mask_size": 288, "num_candidates": 3 }
                }
            }
        })
    }

    fn validate(value: Value) -> Result<Result<SamManifest, ManifestInvalid>, serde_json::Error> {
        let wire: Wire = serde_json::from_value(value)?;
        Ok(SamManifest::validate(Path::new("/export"), wire))
    }

    #[test]
    fn accepts_the_real_contract() -> TestResult {
        let manifest = validate(valid())?.map_err(|e| e.to_string())?;
        assert_eq!(manifest.resolution, 1008);
        assert_eq!(manifest.low_res_mask_size, 288);
        assert_eq!(manifest.candidates, 3);
        assert_eq!(
            manifest.decoder,
            Path::new("/export/decoder_interactive/sam3_decoder_interactive.onnx")
        );
        Ok(())
    }

    #[test]
    fn rejects_a_non_uint8_input() -> TestResult {
        let mut value = valid();
        value["models"]["image_encoder"]["inputs"][0]["dtype"] = json!("tensor(float)");
        assert!(matches!(
            validate(value)?,
            Err(ManifestInvalid::InputDtype { .. })
        ));
        Ok(())
    }

    #[test]
    fn rejects_a_shape_that_disagrees_with_resolution() -> TestResult {
        let mut value = valid();
        value["models"]["image_encoder"]["inputs"][0]["shape"] = json!([3, 512, 512]);
        assert!(matches!(
            validate(value)?,
            Err(ManifestInvalid::InputShape { .. })
        ));
        Ok(())
    }

    #[test]
    fn rejects_a_letterbox_resize() -> TestResult {
        let mut value = valid();
        value["models"]["image_encoder"]["metadata"]["preprocessing"]["caller_resize"]["mode"] =
            json!("letterbox");
        assert!(matches!(
            validate(value)?,
            Err(ManifestInvalid::ResizeMode { .. })
        ));
        Ok(())
    }

    #[test]
    fn rejects_a_bgr_caller() -> TestResult {
        let mut value = valid();
        value["models"]["image_encoder"]["metadata"]["preprocessing"]["caller_resize"]["channel_order"] =
            json!("bgr");
        assert!(matches!(
            validate(value)?,
            Err(ManifestInvalid::ChannelOrder { .. })
        ));
        Ok(())
    }

    #[test]
    fn rejects_a_zero_candidate_decoder() -> TestResult {
        let mut value = valid();
        value["models"]["decoder_interactive"]["metadata"]["num_candidates"] = json!(0);
        assert!(matches!(validate(value)?, Err(ManifestInvalid::Candidates)));
        Ok(())
    }
}
