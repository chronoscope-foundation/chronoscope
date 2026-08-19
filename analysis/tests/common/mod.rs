//! Shared helpers for the integration tests that load the real model: the
//! cause-chain printer and the model-shard resolver both test binaries use.

use std::{env, error, path::PathBuf};

use chronoscope_analysis::qwen3::MODEL_SHARD_ENV;

/// One line carrying an error's whole cause chain, since the boxed error a
/// failing test prints shows only the outermost message otherwise.
pub fn describe(error: &dyn error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    message
}

/// The model's first UQFF shard, from [`MODEL_SHARD_ENV`], or a named actionable
/// error if it is unset.
pub fn first_shard() -> Result<PathBuf, String> {
    env::var_os(MODEL_SHARD_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            format!(
                "{MODEL_SHARD_ENV} is unset. Set it to the model's `afq4-0.uqff` shard \
                 path (the `qwen-vlm-uqff` derivation's `firstShard`); the ignored test \
                 cannot find the weights any other way. `just model-test` wires it."
            )
        })
}
