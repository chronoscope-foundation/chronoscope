//! Qwen 3.6 through mistral.rs: a prompt and its images in, a value shaped like
//! the caller's schema type out.
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

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use mistralrs::{Constraint, Model, RequestBuilder, TextMessageRole, UqffMultimodalModelBuilder};
use thiserror::Error;

use crate::ask::{self, Outcome, Prompt};

/// Sequences held in the prefix cache. The multimodal builder disables prefix
/// caching by default (`prefix_cache_n: None`). The image-first read issues
/// several calls per subimage over one shared image prefill (gate, then a
/// per-entity describe loop or a triage), and many subimages run concurrently, so
/// the window holds enough completed sequences that a subimage's prefixes stay
/// resident across its serial calls. Eviction is a count, not memory, and the KV
/// here is tiny, so this is generous headroom.
const PREFIX_CACHE_SEQS: usize = 64;

/// Ceiling on an answer's generated tokens: the runaway guard for a constrained
/// decode that never reaches a stop, not a length target. Every answer is now a
/// single small value — a gate judgment, one entity's description, or a triage —
/// so this sits well above the longest free-text description and a legitimate
/// answer finishes rather than tripping the cap and reading as
/// [`Outcome::Incomplete`].
const MAX_ANSWER_TOKENS: usize = 16_384;

/// The env var naming the model's first UQFF shard (its `afq4-0.uqff`), as the
/// `qwen-vlm-uqff` derivation's `firstShard` exposes it. The `analyze` binary and
/// the model tests read it; this is the single definition of that Nix-contract
/// name.
pub const MODEL_SHARD_ENV: &str = "QWEN_MODEL_FIRST_SHARD";

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

    /// Answers `prompt` about its images, constrained to `T`'s schema.
    ///
    /// The schema is derived from `T` once and used for both the sampling
    /// constraint and the type text placed in the prompt, so what the model is
    /// told and what it is held to are the one value and cannot desync. The read
    /// is image-first: the images lead the user turn and the framing, the rendered
    /// schema, and the trigger ([`Prompt::user_text`]) follow them, so a
    /// subimage's calls share a byte-identical `[images][framing + schema]` prefix
    /// the prefix cache reuses.
    ///
    /// The schema is compiled into a grammar the sampler is held to, so the
    /// completion is a valid `T` when it finishes. The returned [`Outcome`]
    /// carries either the parsed value or the model stopping early; the finish
    /// reason is what separates a token-cap truncation (an invalid prefix) from a
    /// genuine parse failure.
    pub async fn ask<T>(&self, prompt: Prompt) -> Result<Outcome<T>, AskError>
    where
        T: serde::de::DeserializeOwned + schemars::JsonSchema,
    {
        let completion = self.complete::<T>(prompt, 0).await?;
        ask::decode(&completion.finish_reason, completion.content).map_err(AskError::Decode)
    }

    /// The thinking twin of [`ask`](Self::ask): the model reasons in its native
    /// thinking region before the constrained JSON, so it works through what it sees
    /// before it commits to the answer. Unused in the pipeline today; a knob for a
    /// read where reasoning first earns its cost over a one-shot answer.
    ///
    /// mistral.rs renders the chat template with thinking on (its default), so the
    /// prompt already opens a `<think>` block; a Lark grammar (`think_grammar`) then
    /// caps the reasoning at `think_budget` tokens, forces the closing `</think>`,
    /// and holds the answer to the `%json` schema. Because the reasoning rides the
    /// model's trained thinking tokens, mistral.rs's reasoning parser splits it out:
    /// [`Answer::reasoning`] carries the transcript for traceability, and the decoded
    /// [`Outcome`] carries the answer.
    pub async fn ask_thinking<T>(
        &self,
        prompt: Prompt,
        think_budget: NonZeroUsize,
    ) -> Result<Answer<T>, AskError>
    where
        T: serde::de::DeserializeOwned + schemars::JsonSchema,
    {
        let completion = self.complete::<T>(prompt, think_budget.get()).await?;
        let outcome =
            ask::decode(&completion.finish_reason, completion.content).map_err(AskError::Decode)?;
        Ok(Answer {
            outcome,
            reasoning: completion.reasoning.unwrap_or_default(),
        })
    }

    /// The one completion choice for `prompt`, the shared body of [`ask`](Self::ask)
    /// and [`ask_thinking`](Self::ask_thinking): derive and render the schema, lead
    /// the user turn with the images, cap the length, and pull the single choice. A
    /// zero `think_budget` holds the model to the plain JSON schema with the chat
    /// template's thinking off; a positive one turns thinking on and wraps the answer
    /// in a budget-capped `think_grammar` reasoning region.
    async fn complete<T>(&self, prompt: Prompt, think_budget: usize) -> Result<Completion, AskError>
    where
        T: schemars::JsonSchema,
    {
        let schema = ask::constraint_value::<T>().map_err(AskError::Constraint)?;
        let rendered = ask::render_schema(&schema).map_err(AskError::Render)?;
        let content = prompt.user_text(&rendered);
        let constraint = if think_budget == 0 {
            Constraint::JsonSchema(schema)
        } else {
            Constraint::Lark(think_grammar(&schema, think_budget))
        };
        let request = RequestBuilder::new()
            // Greedy decode, so the same image answers the same way twice: the
            // reference comparisons and the eval's caching both rest on that.
            // mistral.rs builds requests this way today, and asserting it here
            // keeps the property ours rather than inherited. It comes first
            // because it resets the sampler, max length included.
            .set_deterministic_sampler()
            .add_image_message(TextMessageRole::User, content, prompt.images)
            .set_constraint(constraint)
            // The length cap covers reasoning and answer together, so add the budget
            // back: the answer keeps its full MAX_ANSWER_TOKENS after the reasoning
            // spends up to `think_budget` (which is 0 for a plain ask).
            .set_sampler_max_len(MAX_ANSWER_TOKENS + think_budget)
            // mistral.rs defaults enable_thinking to true, which opens a `<think>`
            // block in the prompt. A plain ask forces the JSON answer at once, so it
            // must not be primed to reason; only a thinking call opens the block.
            .enable_thinking(think_budget > 0);
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
        Ok(Completion {
            finish_reason: choice.finish_reason,
            content: choice.message.content,
            reasoning: choice.message.reasoning_content,
        })
    }
}

/// The one completion choice's raw pieces before decoding: the finish reason, the
/// answer content, and the reasoning mistral.rs's parser split off a thinking call.
struct Completion {
    finish_reason: String,
    content: Option<String>,
    reasoning: Option<String>,
}

/// A thinking answer: the decoded [`Outcome`], plus the reasoning the model
/// produced before it. The reasoning is kept for traceability and is empty when
/// the model answered without reasoning first.
#[derive(Debug, Clone)]
pub struct Answer<T> {
    /// The decoded answer, or the model stopping short.
    pub outcome: Outcome<T>,
    /// The model's reasoning transcript, empty when it reasoned nothing.
    pub reasoning: String,
}

/// Builds the Lark grammar for [`Qwen3::ask_thinking`]: free reasoning capped at
/// `budget` tokens, then the model's native `</think>` token, then the `%json`
/// schema. mistral.rs renders the chat template with thinking on (its default), so
/// the prompt already ends with an open `<think>`; the generation picks up inside
/// that block, which is what actually primes the model to reason. The grammar must
/// not re-open `<think>`, which produces a double block the model closes empty; it
/// generates the reasoning, then forces the closing `</think>` (an unquoted
/// special-token terminal, mandatory, not a stop a budget cap could skip), then the
/// answer. The special token sits outside the regex's byte-space, so the reasoning
/// needs no exclusions. The `%json` schema is embedded inline; llguidance honors its
/// `x-guidance` overrides the same as the plain [`ask::constraint_value`] path, so a
/// thinking call and a plain call hold the model to the identical answer shape.
fn think_grammar(schema: &serde_json::Value, budget: usize) -> String {
    // `Value`'s Display serializes it as compact JSON and cannot fail, unlike the
    // general `serde_json::to_string`.
    let schema_json = schema.to_string();
    format!(
        "start: reasoning </think> body\n\
         reasoning[max_tokens={budget}]: /[\\s\\S]*/\n\
         body: %json {schema_json}"
    )
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

    #[error("the schema could not be rendered into the prompt")]
    Render(#[source] ask::RenderError),

    #[error("mistral.rs could not run the constrained request")]
    Send(#[source] mistralrs::error::Error),

    #[error("mistral.rs returned no choices for the request")]
    NoChoices,

    #[error("the model's answer could not be decoded")]
    Decode(#[source] ask::DecodeError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ask::EntityReading;

    #[test]
    fn think_grammar_wraps_a_native_think_region_then_the_schema()
    -> Result<(), Box<dyn std::error::Error>> {
        let schema = ask::constraint_value::<EntityReading>()?;
        let grammar = think_grammar(&schema, 50);
        // The start rule is the reasoning (picked up inside the prompt's already-open
        // <think>), then the native </think> special token (unquoted, resolved against
        // the tokenizer; a required terminal, not a stop a budget cap could skip),
        // then the JSON body.
        assert!(grammar.starts_with("start: reasoning </think> body\n"));
        // The reasoning is free text hard-capped at the budget, with no exclusions
        // since the special tokens sit outside the regex's byte-space.
        assert!(grammar.contains(r"reasoning[max_tokens=50]: /[\s\S]*/"));
        // The body embeds exactly the schema value the plain constraint compiles,
        // so a thinking call and a plain call hold the model to the identical shape.
        let embedded = serde_json::to_string(&schema)?;
        assert!(grammar.ends_with(&format!("body: %json {embedded}")));
        Ok(())
    }
}
