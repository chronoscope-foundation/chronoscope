//! The CLIP BPE tokenizer SAM 3's concept prompt needs.
//!
//! SAM 3 conditions its grounding decoder on the standard `OpenAI`/`open_clip`
//! CLIP BPE encoding of the prompt: `[bos] <bpe> [eos]`, lowercased, **padded to
//! 32** with pad id 0 — not eos. The graph derives its language mask as
//! `(tokens != 0)`, so the pad value is load-bearing: the HF tokenizer's own
//! padding pads with eos, which would leave no masked positions, so the runner
//! disables that padding and pads with 0 here. The `facebook/sam3`
//! `tokenizer.json` loads through the HF `tokenizers` crate byte-identically to
//! sam3's own `SimpleTokenizer`, so the crate's whole debt is this pad-and-widen.

use std::path::Path;

use thiserror::Error;
use tokenizers::Tokenizer;

/// The fixed token count the `language_encoder` graph takes (`tokens` int64
/// `[1, 32]`).
pub(crate) const CONTEXT_LEN: usize = 32;

/// SAM 3's concept-prompt tokenizer: the CLIP BPE, padded the way the graph reads.
pub(crate) struct ConceptTokenizer {
    tokenizer: Tokenizer,
}

impl ConceptTokenizer {
    /// Loads the CLIP BPE from a `tokenizer.json`, disabling its built-in padding
    /// and truncation so the runner owns the pad-to-32-with-0 the graph's mask
    /// reads.
    pub(crate) fn load(path: &Path) -> Result<Self, TokenizerError> {
        let mut tokenizer = Tokenizer::from_file(path).map_err(|error| TokenizerError::Load {
            detail: error.to_string(),
        })?;
        tokenizer.with_padding(None);
        tokenizer
            .with_truncation(None)
            .map_err(|error| TokenizerError::Load {
                detail: error.to_string(),
            })?;
        Ok(Self { tokenizer })
    }

    /// Encodes `prompt` to the `[32]` int64 ids the graph takes: the CLIP BPE ids
    /// with bos/eos (from `add_special_tokens`), zero-padded to 32.
    pub(crate) fn encode(&self, prompt: &str) -> Result<[i64; CONTEXT_LEN], TokenizerError> {
        let encoding =
            self.tokenizer
                .encode(prompt, true)
                .map_err(|error| TokenizerError::Encode {
                    prompt: prompt.to_owned(),
                    detail: error.to_string(),
                })?;
        pad_to_context(encoding.get_ids(), prompt)
    }
}

/// Zero-pads CLIP ids to [`CONTEXT_LEN`], rejecting a prompt that does not fit —
/// truncating past the eos would change what the model is asked to find. Concept
/// prompts are one or two words, so this is a guard, not a path.
fn pad_to_context(ids: &[u32], prompt: &str) -> Result<[i64; CONTEXT_LEN], TokenizerError> {
    if ids.len() > CONTEXT_LEN {
        return Err(TokenizerError::TooLong {
            prompt: prompt.to_owned(),
            len: ids.len(),
        });
    }
    let mut tokens = [0_i64; CONTEXT_LEN];
    for (slot, &id) in tokens.iter_mut().zip(ids) {
        *slot = i64::from(id);
    }
    Ok(tokens)
}

/// Why a concept prompt could not be tokenized.
#[derive(Debug, Error)]
pub enum TokenizerError {
    #[error("the CLIP tokenizer could not be loaded: {detail}")]
    Load { detail: String },

    #[error("the prompt `{prompt}` could not be tokenized: {detail}")]
    Encode { prompt: String, detail: String },

    #[error("the prompt `{prompt}` tokenized to {len} ids, over the {CONTEXT_LEN}-token context")]
    TooLong { prompt: String, len: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pads_ids_to_context_with_zeros() -> Result<(), TokenizerError> {
        // The eos (49407) already sits in the ids from `encode(_, true)`; padding
        // fills the rest with 0, which the graph reads as the masked-out tail.
        let padded = pad_to_context(&[49406, 2003, 49407], "building")?;
        assert_eq!(padded[0], 49406, "bos leads");
        assert_eq!(padded[1], 2003);
        assert_eq!(padded[2], 49407, "eos closes the content");
        assert!(padded[3..].iter().all(|&id| id == 0), "the rest is pad 0");
        Ok(())
    }

    #[test]
    fn rejects_an_over_long_prompt() {
        let ids = vec![1_u32; CONTEXT_LEN + 1];
        assert!(matches!(
            pad_to_context(&ids, "x"),
            Err(TokenizerError::TooLong { len, .. }) if len == CONTEXT_LEN + 1
        ));
    }
}
