//! Qwen 3.6 through mistral.rs: an image and a prompt in, a value shaped like the
//! caller's type out.
//!
//! DINOv3 and SAM 3 turn an image into vectors or a mask; this turns one into
//! whatever Rust type the caller names. The type's schema constrains generation,
//! so the model can only emit tokens that keep the output a valid value of that
//! type, and the result deserializes straight into it: an ill-typed answer is
//! unrepresentable, not a parse that might fail afterward.
//!
//! `open` loads a prequantized AFQ4 UQFF. mistral.rs can't load Qwen 3.6's GGUF
//! export, so the weights are quantized ahead of time to the AFQ4
//! mixture-of-experts format (the `qwen-quantize` bin, built by the
//! `qwen-vlm-uqff` Nix derivation) and this reads that self-contained directory
//! back: no ISQ pass, just a load of the four-bit shards, leaving `ask` cheap
//! against the resident model.
//!
//! mistral.rs runs the Metal GPU backend on Apple hardware and CPU elsewhere.

use std::path::{Path, PathBuf};

use mistralrs::{Model, RequestBuilder, TextMessageRole, UqffMultimodalModelBuilder};
use thiserror::Error;

/// A loaded Qwen 3.6, quantized to AFQ4 and ready to answer prompts over images.
pub struct Qwen3 {
    model: Model,
}

impl Qwen3 {
    /// Loads the prequantized AFQ4 UQFF whose first shard is `first_shard`.
    ///
    /// `first_shard` is the path to the model's `afq4-0.uqff`, as the
    /// `qwen-vlm-uqff` derivation exposes it. mistral.rs takes the containing
    /// directory plus the shard's name and discovers the sibling shards, the
    /// residual, config, and tokenizer from that directory itself, so the loader
    /// owns no layout knowledge beyond the path it is handed.
    pub async fn open(first_shard: &Path) -> Result<Self, OpenError> {
        let dir = first_shard.parent().ok_or_else(|| OpenError::ShardPath {
            path: first_shard.to_path_buf(),
        })?;
        let name = first_shard
            .file_name()
            .ok_or_else(|| OpenError::ShardPath {
                path: first_shard.to_path_buf(),
            })?;
        let model =
            UqffMultimodalModelBuilder::new(dir.to_string_lossy(), vec![PathBuf::from(name)])
                .build()
                .await
                .map_err(|source| OpenError::Build {
                    path: first_shard.to_path_buf(),
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

/// Why the model could not be opened from its first UQFF shard.
#[derive(Debug, Error)]
pub enum OpenError {
    #[error("`{path}` is not a shard file with a parent directory")]
    ShardPath { path: PathBuf },

    #[error("mistral.rs could not load the prequantized AFQ4 UQFF from `{path}`")]
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
