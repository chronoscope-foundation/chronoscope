//! Qwen 3.6 through mistral.rs: an image and a prompt in, a value shaped like the
//! caller's schema type out.
//!
//! DINOv3 and SAM 3 turn an image into vectors or a mask; this turns one into a
//! schema-constrained answer. [`crate::ask::constraint_value`] derives the JSON
//! schema from the caller's type and injects `x-guidance` (so the grammar forbids
//! indentation while keeping the separators the model expects); mistral.rs compiles that
//! into an llguidance grammar the sampler is held to. [`Qwen3::ask`] reads the
//! completion's finish reason so a token cap reads as an incomplete answer
//! rather than a parse bug, and returns an [`Outcome`].
//!
//! `open` loads a prequantized AFQ4 UQFF. mistral.rs can't load Qwen 3.6's GGUF
//! export, so the weights are quantized ahead of time to the AFQ4
//! mixture-of-experts format (the `qwen-quantize` bin, built by the
//! `qwen-vlm-uqff` Nix derivation) and this reads that self-contained directory
//! back: no ISQ pass, just a load of the four-bit shards. Prefix caching is
//! enabled at load, since the multimodal builder leaves it off by default.
//!
//! mistral.rs runs the Metal GPU backend on Apple hardware and CPU elsewhere.

use std::path::{Path, PathBuf};

use mistralrs::{Constraint, Model, RequestBuilder, TextMessageRole, UqffMultimodalModelBuilder};
use thiserror::Error;

use crate::ask::{self, Outcome};

/// Sequences held in the prefix cache. The multimodal builder disables prefix
/// caching by default (`prefix_cache_n: None`); a small window matches the text
/// builder's default and is enough for the two-pass, one-image reuse the
/// pipeline leans on.
const PREFIX_CACHE_SEQS: usize = 16;

/// Ceiling on an answer's generated tokens: the runaway guard for a constrained
/// decode that never reaches a stop, not a length target. Set well above a dense
/// multi-entity `RelevanceOutcome` (a summary plus many described entities), so a
/// legitimate answer finishes rather than tripping the cap and reading as
/// [`Outcome::Incomplete`].
const MAX_ANSWER_TOKENS: usize = 16_384;

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
    /// owns no layout knowledge beyond the path it is handed. Prefix caching is
    /// turned on via the plain builder, since the UQFF wrapper's by-value methods
    /// cannot chain and its `from_uqff` setting survives `into_inner`.
    pub async fn open(first_shard: &Path) -> Result<Self, OpenError> {
        // `Path::parent` yields `Some("")` for a bare filename, not `None`, so an
        // empty parent is rejected the same as a missing one.
        let dir = first_shard
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .ok_or_else(|| OpenError::ShardPath {
                path: first_shard.to_path_buf(),
            })?;
        let name = first_shard
            .file_name()
            .ok_or_else(|| OpenError::ShardPath {
                path: first_shard.to_path_buf(),
            })?;
        // mistral.rs reads a non-existent local path as a model id and reaches
        // the hub, so a mistyped shard fails opaquely over the network rather
        // than here. Naming the missing file keeps the diagnosis local.
        if !first_shard.is_file() {
            return Err(OpenError::ShardMissing {
                path: first_shard.to_path_buf(),
            });
        }
        let model =
            UqffMultimodalModelBuilder::new(dir.to_string_lossy(), vec![PathBuf::from(name)])
                .into_inner()
                .with_prefix_cache_n(Some(PREFIX_CACHE_SEQS))
                .build()
                .await
                .map_err(|source| OpenError::Build {
                    path: first_shard.to_path_buf(),
                    source: source.into(),
                })?;
        Ok(Self { model })
    }

    /// Answers `prompt` about `image`, constrained to `T`'s schema.
    ///
    /// `prompt` is the caller-assembled instruction; placing
    /// [`crate::ask::render_schema`]'s type text ahead of the decode to prime the
    /// model is the prompt-assembly step's job, not done here.
    ///
    /// The schema is derived from `T`, compiled into a grammar the sampler is
    /// held to, so the completion is a valid `T` when it finishes. The returned
    /// [`Outcome`] carries either the parsed value or the model stopping early;
    /// the finish reason is what separates a token-cap truncation (an invalid
    /// prefix) from a genuine parse failure.
    pub async fn ask<T>(
        &self,
        image: image::DynamicImage,
        prompt: &str,
    ) -> Result<Outcome<T>, AskError>
    where
        T: serde::de::DeserializeOwned + schemars::JsonSchema,
    {
        let schema = ask::constraint_value::<T>().map_err(AskError::Constraint)?;
        let request = RequestBuilder::new()
            .add_image_message(TextMessageRole::User, prompt, vec![image])
            .set_constraint(Constraint::JsonSchema(schema))
            .set_sampler_max_len(MAX_ANSWER_TOKENS);
        let response = self
            .model
            .send_chat_request(request)
            .await
            .map_err(AskError::Send)?;
        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or(AskError::NoChoices)?;
        ask::decode(&choice.finish_reason, choice.message.content).map_err(AskError::Decode)
    }
}

/// Why the model could not be opened from its first UQFF shard.
#[derive(Debug, Error)]
pub enum OpenError {
    #[error("`{path}` is not a shard file with a parent directory")]
    ShardPath { path: PathBuf },

    #[error(
        "no UQFF shard at `{path}`; set QWEN_MODEL_FIRST_SHARD to the model's \
         `afq4-0.uqff` (the `qwen-vlm-uqff` derivation's `firstShard`). A missing \
         local path would otherwise be taken for a model id and reach the network."
    )]
    ShardMissing { path: PathBuf },

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
    #[error("the schema constraint could not be built from the requested type")]
    Constraint(#[source] ask::ConstraintError),

    #[error("mistral.rs could not run the constrained request")]
    Send(#[source] mistralrs::error::Error),

    #[error("mistral.rs returned no choices for the request")]
    NoChoices,

    #[error("the model's answer could not be decoded")]
    Decode(#[source] ask::DecodeError),
}
