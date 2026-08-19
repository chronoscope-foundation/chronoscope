//! The `sam3-onnx` export's `manifest.json`, parsed into the contract the runner
//! honours.
//!
//! `nix/scripts/verify-onnx.py` writes it only after every graph loads, so this
//! is the single file the Rust side reads. The export carries the whole SAM 3
//! model, so the manifest describes four graphs; the runner drives two of them —
//! the image encoder and the interactive single-object decoder — and validates
//! the parts that silently corrupt those if wrong. The grounding decoder and
//! language encoder ride along for concept/text prompting, unread here until a
//! caller wants them, so the wire names only the two driven graphs and serde
//! drops the rest.
//!
//! [`load`](SamManifest::load) is the validation, mirroring the DINOv3
//! manifest's decline-to-trust stance: a resize that letterboxes instead of
//! stretching (box prompts land off their objects), a BGR caller, an input shape
//! that is not the channel-first square, or a float input dtype (the caller
//! would owe a rescale the graph already bakes).

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use thiserror::Error;

use crate::model_manifest::Signature;

/// The encoder's one image input, named by the export.
const INPUT: &str = "image";

/// RGB planes, the only channel count the encoder takes and what `to_rgb8`
/// produces. Its position first in the input shape is what makes the shape
/// channel-first.
const CHANNELS: i64 = 3;

/// One `sam3-onnx` export's manifest, reduced to what the interactive runner
/// drives: the image encoder, the interactive decoder, and the numbers for
/// feeding and reading them, each held to its assertion at [`load`](Self::load).
#[derive(Debug)]
pub(crate) struct SamManifest {
    pub(crate) image_encoder: PathBuf,
    pub(crate) decoder: PathBuf,
    /// The square side the encoder takes, derived from its input shape; the
    /// caller stretches to it.
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
        let encoder = wire.image_encoder;

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

        // The channel-first square input carries the resolution: a [3, res, res]
        // shape both fixes the layout and names the side the caller stretches to.
        let resolution = match image.shape.as_slice() {
            &[CHANNELS, height, width] if height == width && height > 0 => height as usize,
            _ => {
                return Err(ManifestInvalid::InputShape {
                    shape: image.shape.clone(),
                });
            }
        };

        // The caller owes only the resize: a pure RGB stretch to the square. A
        // letterbox feeds padding the model never saw and lands box prompts off
        // their objects; BGR corrupts silently.
        if encoder.resize_mode != "stretch" {
            return Err(ManifestInvalid::ResizeMode {
                mode: encoder.resize_mode,
            });
        }
        if encoder.channel_order != "rgb" {
            return Err(ManifestInvalid::ChannelOrder {
                channel_order: encoder.channel_order,
            });
        }

        let decoder = wire.decoder_interactive;
        if decoder.low_res_mask_size == 0 {
            return Err(ManifestInvalid::LowResMaskSize);
        }
        if decoder.candidates == 0 {
            return Err(ManifestInvalid::Candidates);
        }

        Ok(Self {
            image_encoder: export.join(encoder.graph),
            decoder: export.join(decoder.graph),
            resolution,
            low_res_mask_size: decoder.low_res_mask_size,
            candidates: decoder.candidates,
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
    #[error("the encoder declares no `{name}` input")]
    MissingInput { name: &'static str },

    #[error("encoder input dtype `{dtype}` is not the `tensor(uint8)` the caller feeds raw")]
    InputDtype { dtype: String },

    #[error("encoder input shape {shape:?} is not a channel-first [3, N, N] square")]
    InputShape { shape: Vec<i64> },

    #[error("caller resize mode `{mode}` is not the `stretch` box prompts assume")]
    ResizeMode { mode: String },

    #[error("caller channel order `{channel_order}` is not the `rgb` the caller decodes")]
    ChannelOrder { channel_order: String },

    #[error("the interactive decoder declares a zero low-resolution mask size")]
    LowResMaskSize,

    #[error("the interactive decoder declares zero mask candidates")]
    Candidates,
}

/// The two graphs this crate drives. The catalog's other graphs (the grounding
/// decoder and language encoder) are dropped, since the wire does not
/// `deny_unknown_fields`.
#[derive(Deserialize)]
struct Wire {
    image_encoder: Encoder,
    decoder_interactive: Decoder,
}

/// The image encoder: its graph, its input signature, and the preprocessing the
/// caller performs that the tensor shape does not encode.
#[derive(Deserialize)]
struct Encoder {
    graph: PathBuf,
    inputs: Vec<Signature>,
    channel_order: String,
    resize_mode: String,
}

/// The interactive decoder: its graph and the two numbers the caller reads its
/// output with. Its own input signature stays catalog — an input drift is caught
/// at runtime by the session rejecting a wrong-named feed.
#[derive(Deserialize)]
struct Decoder {
    graph: PathBuf,
    low_res_mask_size: usize,
    candidates: usize,
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
            "image_encoder": {
                "graph": "image_encoder/sam3_image_encoder.onnx",
                "inputs": [{ "name": "image", "dtype": "tensor(uint8)", "shape": [3, 1008, 1008] }],
                "channel_order": "rgb",
                "resize_mode": "stretch"
            },
            "decoder_interactive": {
                "graph": "decoder_interactive/sam3_decoder_interactive.onnx",
                "low_res_mask_size": 288,
                "candidates": 3
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
        value["image_encoder"]["inputs"][0]["dtype"] = json!("tensor(float)");
        assert!(matches!(
            validate(value)?,
            Err(ManifestInvalid::InputDtype { .. })
        ));
        Ok(())
    }

    #[test]
    fn rejects_a_non_square_input_shape() -> TestResult {
        let mut value = valid();
        value["image_encoder"]["inputs"][0]["shape"] = json!([3, 1008, 512]);
        assert!(matches!(
            validate(value)?,
            Err(ManifestInvalid::InputShape { .. })
        ));
        Ok(())
    }

    #[test]
    fn rejects_a_letterbox_resize() -> TestResult {
        let mut value = valid();
        value["image_encoder"]["resize_mode"] = json!("letterbox");
        assert!(matches!(
            validate(value)?,
            Err(ManifestInvalid::ResizeMode { .. })
        ));
        Ok(())
    }

    #[test]
    fn rejects_a_bgr_caller() -> TestResult {
        let mut value = valid();
        value["image_encoder"]["channel_order"] = json!("bgr");
        assert!(matches!(
            validate(value)?,
            Err(ManifestInvalid::ChannelOrder { .. })
        ));
        Ok(())
    }

    #[test]
    fn rejects_a_zero_candidate_decoder() -> TestResult {
        let mut value = valid();
        value["decoder_interactive"]["candidates"] = json!(0);
        assert!(matches!(validate(value)?, Err(ManifestInvalid::Candidates)));
        Ok(())
    }
}
