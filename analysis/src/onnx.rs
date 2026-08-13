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

use ort::{
    ep::{CPU, CoreML, coreml::ModelFormat},
    session::Session,
};
use thiserror::Error;

/// Which execution backend a session runs on.
///
/// [`Cpu`](Backend::Cpu) is the portable path. [`CoreML`](Backend::CoreML)
/// offloads the graph to the ANE/GPU; `cache_dir` holds onnxruntime's compiled
/// `CoreML` models, so the minutes-long compile is paid once rather than on every
/// session. It reads fine from a read-only path (a Nix store path), but the cache
/// keys on the model path as given: the compile that populated it and the load
/// that reuses it must name the model identically, or the load silently recompiles.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Backend<'a> {
    Cpu,
    CoreML { cache_dir: &'a Path },
}

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

/// Builds a session over `graph` on the chosen [`Backend`].
///
/// The provider is selected explicitly rather than left to ORT's auto-selection,
/// so a session's backend is the caller's decision rather than whatever the
/// runtime happens to find available. `CoreML` lists a CPU provider after it so the
/// ops `CoreML` can't take (the windowed-attention reshuffles) fall back rather
/// than fail the whole graph.
pub(crate) fn session(graph: &Path, backend: Backend) -> Result<Session, SessionError> {
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

    let providers = match backend {
        Backend::Cpu => vec![CPU::default().build()],
        Backend::CoreML { cache_dir } => vec![
            CoreML::default()
                .with_model_format(ModelFormat::MLProgram)
                .with_model_cache_dir(cache_dir.to_string_lossy())
                .build(),
            CPU::default().build(),
        ],
    };

    let mut builder = Session::builder()
        .map_err(SessionError::Options)?
        .with_execution_providers(providers)
        .and_then(|builder| builder.with_intra_threads(INTRA_OP_THREADS))
        .map_err(|error| SessionError::Options(error.into()))?;

    builder
        .commit_from_file(graph)
        .map_err(|error| SessionError::Load {
            path: graph.to_path_buf(),
            source: error,
        })
}

/// Caller-facing backend choice for an export whose several models share one
/// acceleration decision. `CoreML` names a single cache root; each model within the
/// export caches under its own subdirectory of it, so a decoder's compiled models
/// never collide with the encoder's.
#[derive(Debug, Clone, Copy)]
pub enum Accel<'a> {
    Cpu,
    CoreML { cache_root: &'a Path },
}

/// Opens `graph` on `accel`. For `CoreML` the compiled-model cache lives at
/// `cache_root/name`, keeping this model's cache distinct from its siblings'.
pub(crate) fn session_for(graph: &Path, accel: Accel, name: &str) -> Result<Session, SessionError> {
    match accel {
        Accel::Cpu => session(graph, Backend::Cpu),
        Accel::CoreML { cache_root } => {
            let cache_dir = cache_root.join(name);
            session(
                graph,
                Backend::CoreML {
                    cache_dir: &cache_dir,
                },
            )
        }
    }
}
