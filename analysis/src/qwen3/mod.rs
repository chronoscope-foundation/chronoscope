//! Qwen 3.6 through mistral.rs: an image and a prompt in, a value shaped like the
//! caller's type out.
//!
//! DINOv3 and SAM 3 turn an image into vectors or a mask; this turns one into
//! whatever Rust type the caller names. The type's schema constrains generation,
//! so the model can only emit tokens that keep the output a valid value of that
//! type, and the result deserializes straight into it: an ill-typed answer is
//! unrepresentable, not a parse that might fail afterward.
//!
//! `open` is the expensive call. mistral.rs can't load Qwen 3.6's GGUF export, so
//! we load the full-precision safetensors and quantize them in place (ISQ) to
//! AFQ4, the mixture-of-experts kernel; that reads tens of gigabytes and rewrites
//! them to four bits once, leaving `ask` cheap against the resident model.
//!
//! mistral.rs runs the Metal GPU backend on Apple hardware and CPU elsewhere.

use std::path::{Path, PathBuf};

use mistralrs::{IsqType, Model, MultimodalModelBuilder, RequestBuilder, TextMessageRole};
use thiserror::Error;

/// A loaded Qwen 3.6, quantized to AFQ4 and ready to answer prompts over images.
pub struct Qwen3 {
    model: Model,
}

impl Qwen3 {
    /// Loads the base safetensors under `model_dir` and quantizes them to AFQ4.
    ///
    /// `model_dir` is a local directory of Hugging Face weights (config,
    /// tokenizer, and safetensors shards), not a repo id. The directory check
    /// keeps a missing path from reaching `MultimodalModelBuilder`, which would
    /// otherwise read a non-existent local path as a repo id and fail against the
    /// network instead of naming the real problem.
    pub async fn open(model_dir: &Path) -> Result<Self, OpenError> {
        if !model_dir.is_dir() {
            return Err(OpenError::NotADirectory {
                path: model_dir.to_path_buf(),
            });
        }
        let model = MultimodalModelBuilder::new(model_dir.to_string_lossy())
            .with_isq(IsqType::AFQ4)
            .build()
            .await
            .map_err(|source| OpenError::Build {
                path: model_dir.to_path_buf(),
                source: source.into(),
            })?;
        Ok(Self { model })
    }

    /// Answers `prompt` about `image`, decoding the reply into `T`.
    ///
    /// `T`'s schema is derived and handed to the sampler as a JSON-schema
    /// constraint, so the model can only emit tokens that keep the running output
    /// a valid `T`; the completed text is then deserialized into it.
    pub async fn ask<T>(&self, image: image::DynamicImage, prompt: &str) -> Result<T, AskError>
    where
        T: serde::de::DeserializeOwned + schemars::JsonSchema,
    {
        let request =
            RequestBuilder::new().add_image_message(TextMessageRole::User, prompt, vec![image]);
        self.model
            .generate_structured::<T>(request)
            .await
            .map_err(AskError::Generate)
    }
}

/// Why the model could not be opened from its weight directory.
#[derive(Debug, Error)]
pub enum OpenError {
    #[error("`{path}` is not a directory of Qwen 3.6 weights")]
    NotADirectory { path: PathBuf },

    #[error("mistral.rs could not load and quantize the model at `{path}`")]
    Build {
        path: PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Why a prompt could not be answered as the requested type.
#[derive(Debug, Error)]
pub enum AskError {
    #[error("mistral.rs could not generate a schema-constrained answer")]
    Generate(#[source] mistralrs::error::Error),
}
