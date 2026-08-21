//! Boilerplate shared by the cordoned reference comparisons. Only the pieces
//! that are byte-identical between them live here; each test keeps its own
//! fixture discovery, entry type, and numeric metric.

use std::{
    env, error,
    path::{Path, PathBuf},
};

use chronoscope_analysis::onnx::Accel;

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

/// The CoreML compiled-model cache root, from `COREML_CACHE`. Held by the caller
/// because [`Accel::CoreML`] borrows it; pair with [`accel`].
///
/// `just model-test` sets it, so the comparisons run on CoreML, the backend the
/// pipeline runs. It is `Option` only so the tests still run on a platform
/// without CoreML (no cache set, CPU fallback).
pub fn coreml_cache_root() -> Option<PathBuf> {
    env::var_os("COREML_CACHE").map(PathBuf::from)
}

/// The [`Accel`] a cache root selects: CoreML when set, CPU where CoreML is
/// unavailable.
pub fn accel(cache_root: Option<&Path>) -> Accel<'_> {
    cache_root.map_or(Accel::Cpu, |root| Accel::CoreML { cache_root: root })
}
