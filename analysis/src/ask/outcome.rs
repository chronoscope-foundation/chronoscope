//! Turning one constrained completion into a typed outcome.
//!
//! The cases the pipeline treats differently, kept as distinct states rather than
//! a value-or-error pair:
//!
//! - the model finished and its output is a valid `T` ([`Outcome::Parsed`]);
//! - the model stopped early ([`Outcome::Incomplete`]): a token cap (`"length"`)
//!   or a cancel. Under an active grammar this leaves a syntactically invalid
//!   prefix, indistinguishable from a genuine parse failure on the bytes alone,
//!   so the finish reason is what separates them;
//! - something failed, surfaced as a [`DecodeError`]: a clean stop that produced
//!   empty or unparseable content (under a correct grammar, a grammar-versus-`T`
//!   mismatch), or a `"error"` finish, which is mistral.rs reporting a
//!   model-generation fault. The fault is kept distinct from a benign early stop
//!   so a backend failure is not read as the model merely stopping short.

use serde::de::DeserializeOwned;
use thiserror::Error;

/// The finish reason mistral.rs reports for a clean stop.
const FINISH_STOP: &str = "stop";

/// The finish reason mistral.rs sets when the model failed mid-generation
/// (`handle_seq_error`). A fault, not an early stop, so it decodes to an error.
const FINISH_ERROR: &str = "error";

/// A decoded answer, or the model stopping before it produced one.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome<T> {
    /// The model finished and its output parsed into `T`.
    Parsed(T),
    /// The model stopped before completing a value. `finish` is mistral.rs's
    /// reason (`"length"` for a token or context cap, `"canceled"`, …) and
    /// `raw_prefix` is whatever partial output it had emitted, for diagnosis.
    Incomplete {
        /// The finish reason mistral.rs reported.
        finish: String,
        /// The partial output emitted before stopping.
        raw_prefix: String,
    },
}

/// Why a clean-stop completion could not be decoded into the requested type.
#[derive(Debug, Error)]
pub enum DecodeError {
    /// The model finished cleanly but returned no content.
    #[error("the model finished cleanly but returned no content to parse")]
    EmptyStop,

    /// The model finished cleanly but its content did not deserialize as `T`. The
    /// raw content is carried for diagnosis, since under a correct grammar this
    /// is a grammar-versus-type mismatch, not a truncation.
    #[error("the model's answer did not parse as the requested type")]
    Parse {
        /// The content that failed to deserialize.
        raw: String,
        #[source]
        source: serde_json::Error,
    },

    /// mistral.rs reported a model-generation fault (finish reason `"error"`),
    /// not a clean stop or a cap. Surfaced as an error rather than an
    /// [`Outcome::Incomplete`] so a backend failure is not read as the model
    /// merely stopping short. The partial output is carried for diagnosis.
    #[error("the model failed during generation")]
    Generation {
        /// Whatever partial output was decoded before the fault.
        raw_prefix: String,
    },
}

/// Interprets a completion's `(finish_reason, content)` as an [`Outcome`].
///
/// A pure function of its inputs, so every branch is testable without a model: a
/// clean stop that deserializes is [`Outcome::Parsed`], a clean stop with empty
/// or unparseable content is a [`DecodeError`], an `"error"` finish is a
/// [`DecodeError::Generation`] fault, and any other non-stop finish is
/// [`Outcome::Incomplete`].
pub fn decode<T>(finish_reason: &str, content: Option<String>) -> Result<Outcome<T>, DecodeError>
where
    T: DeserializeOwned,
{
    match finish_reason {
        FINISH_STOP => {
            let content = content
                .filter(|body| !body.is_empty())
                .ok_or(DecodeError::EmptyStop)?;
            serde_json::from_str::<T>(&content)
                .map(Outcome::Parsed)
                .map_err(|source| DecodeError::Parse {
                    raw: content,
                    source,
                })
        }
        FINISH_ERROR => Err(DecodeError::Generation {
            raw_prefix: content.unwrap_or_default(),
        }),
        other => Ok(Outcome::Incomplete {
            finish: other.to_owned(),
            raw_prefix: content.unwrap_or_default(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Clone, PartialEq, Deserialize)]
    struct Answer {
        value: u32,
    }

    #[test]
    fn clean_stop_with_valid_content_parses() -> Result<(), DecodeError> {
        let outcome = decode::<Answer>("stop", Some(r#"{"value":7}"#.to_owned()))?;
        assert_eq!(outcome, Outcome::Parsed(Answer { value: 7 }));
        Ok(())
    }

    #[test]
    fn a_length_cap_is_incomplete_not_a_parse_error() -> Result<(), DecodeError> {
        // The whole reason the finish reason is threaded through: a truncated
        // grammar prefix is invalid JSON, and must read as "stopped early", not
        // as a decode bug.
        let outcome = decode::<Answer>("length", Some(r#"{"value":"#.to_owned()))?;
        assert_eq!(
            outcome,
            Outcome::Incomplete {
                finish: "length".to_owned(),
                raw_prefix: r#"{"value":"#.to_owned(),
            }
        );
        Ok(())
    }

    #[test]
    fn a_non_length_stop_reason_is_still_incomplete() -> Result<(), DecodeError> {
        let outcome = decode::<Answer>("canceled", None)?;
        assert_eq!(
            outcome,
            Outcome::Incomplete {
                finish: "canceled".to_owned(),
                raw_prefix: String::new(),
            }
        );
        Ok(())
    }

    #[test]
    fn a_generation_error_finish_is_a_fault_not_incomplete() {
        // mistral.rs sets finish "error" when the model failed mid-generation.
        // It must surface as an error, not fold into Incomplete where a caller
        // would read a backend fault as a benign early stop.
        assert!(matches!(
            decode::<Answer>("error", Some("partial".to_owned())),
            Err(DecodeError::Generation { .. })
        ));
    }

    #[test]
    fn a_clean_stop_with_no_content_is_a_decode_error() {
        assert!(matches!(
            decode::<Answer>("stop", None),
            Err(DecodeError::EmptyStop)
        ));
        assert!(matches!(
            decode::<Answer>("stop", Some(String::new())),
            Err(DecodeError::EmptyStop)
        ));
    }

    #[test]
    fn a_clean_stop_with_unparseable_content_is_a_decode_error() {
        // A correct grammar cannot produce this; when it happens it is a
        // grammar-versus-type mismatch, surfaced rather than silently dropped.
        let result = decode::<Answer>("stop", Some(r#"{"value":"not a number"}"#.to_owned()));
        assert!(matches!(result, Err(DecodeError::Parse { .. })));
    }
}
