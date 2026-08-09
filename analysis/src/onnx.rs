//! Session construction for the ONNX graphs `nix/vision.nix` exports.
//!
//! `ort` builds with `load-dynamic`, so ONNX Runtime is loaded dynamically at
//! first use instead of linked. `ort`'s own response to an unresolvable library
//! is a panic from deep inside its lazy initializer, which says nothing about
//! where the path was supposed to come from; the check here happens before any
//! `ort` call so the failure can name it.

use std::{
    env,
    path::{Path, PathBuf},
};

use ort::{ep::CPU, session::Session};
use thiserror::Error;

/// Where `load-dynamic` resolves ONNX Runtime from. The `analysis` dev shell
/// points it at the nixpkgs build.
const LIBRARY_VAR: &str = "ORT_DYLIB_PATH";

/// Thread count partitions GEMM reductions, making it the largest same-machine
/// source of drift between a recorded reference and a session reproducing it.
/// `nix/scripts/dinov3-fixture.py` pins the same number and writes it into the
/// reference, so the two agree by comparison rather than by convention.
const INTRA_OP_THREADS: usize = 1;

/// Why a session could not be built.
#[derive(Debug, Error)]
pub enum SessionError {
    #[error(
        "{LIBRARY_VAR} is unset, so ONNX Runtime cannot be located. \
         The `analysis` dev shell provides it: run `just model-test`, \
         or enter `nix develop .#analysis` first."
    )]
    LibraryUnset,

    #[error(
        "{LIBRARY_VAR} points at `{path}`, which does not exist. \
         Re-enter the `analysis` dev shell, or run `just model-test`."
    )]
    LibraryMissing { path: PathBuf },

    #[error("ONNX Runtime rejected the pinned session options")]
    Options(#[source] ort::Error),

    #[error("ONNX Runtime could not load the graph at `{path}`")]
    Load {
        path: PathBuf,
        #[source]
        source: ort::Error,
    },
}

/// Builds a session over `graph` with the provider and thread count that every
/// recorded reference was measured under.
///
/// The CPU provider is pinned rather than left to ORT's auto-selection so the
/// result is a property of the graph rather than of whatever the runtime
/// decides it has available.
pub(crate) fn session(graph: &Path) -> Result<Session, SessionError> {
    // An empty value carries no path to resolve, so it earns the unset guidance
    // over a missing-path error. The `DINOV3_FIXTURES` check in the reference
    // test reads its own empty value the same way.
    let library = env::var_os(LIBRARY_VAR)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or(SessionError::LibraryUnset)?;
    if !library.exists() {
        return Err(SessionError::LibraryMissing { path: library });
    }

    let mut builder = Session::builder()
        .map_err(SessionError::Options)?
        .with_execution_providers([CPU::default().build()])
        .and_then(|builder| builder.with_intra_threads(INTRA_OP_THREADS))
        .map_err(|error| SessionError::Options(error.into()))?;

    builder
        .commit_from_file(graph)
        .map_err(|error| SessionError::Load {
            path: graph.to_path_buf(),
            source: error,
        })
}
