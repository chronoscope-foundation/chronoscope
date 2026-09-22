//! Session construction for the ONNX graphs `nix/vision.nix` exports.

use std::path::{Path, PathBuf};

use ort::{
    ep::{CPU, CoreML, coreml::ModelFormat},
    session::Session,
};
use thiserror::Error;

/// Which execution backend a session runs on.
///
/// [`Cpu`](Backend::Cpu) is the portable path. [CoreML](Backend::CoreML)
/// offloads the graph to the ANE/GPU; `cache_dir` holds onnxruntime's compiled
/// CoreML models, so the minutes-long compile is paid once rather than on every
/// session. It reads fine from a read-only path (a Nix store path), but the cache
/// keys on the model path as given: the compile that populated it and the load
/// that reuses it must name the model identically, or the load silently recompiles.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Backend<'a> {
    Cpu,
    CoreML { cache_dir: &'a Path },
}

/// Intra-op threads per session. `run_async` hands inference to this pool, so a
/// session that inferred with one thread has nowhere to run and errors.
///
/// Four rather than the machine's width: a model runs one inference at a time
/// (its mutex is held across the await), so the pool serves that one run, and
/// the machine's remaining cores are what the other models and the rest of the
/// pipeline run on.
///
/// The count does not move the numbers. Both reference comparisons were run at
/// 1 and at 8 threads, on CoreML and on the CPU provider, and every CLS drift,
/// worst-patch drift and mask `IoU` matched to four significant figures, while
/// the CPU run halved in wall-clock; the DINOv3 fixtures pin torch to one thread
/// on the recording side regardless.
const INTRA_OP_THREADS: usize = 4;

/// Why a session could not be built.
#[derive(Debug, Error)]
pub enum SessionError {
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
/// runtime happens to find available. CoreML lists a CPU provider after it so the
/// ops CoreML can't take (the windowed-attention reshuffles) fall back rather
/// than fail the whole graph.
///
/// `dims` pins named symbolic input dimensions to constants. The override lands
/// before provider partitioning, so a provider sees the shape as static: it is
/// how a caller hands CoreML a graph whose symbolic axis it could otherwise
/// not plan.
pub(crate) fn session(
    graph: &Path,
    backend: Backend,
    dims: &[(&str, i64)],
) -> Result<Session, SessionError> {
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
        .map_err(|error| SessionError::Options(error.into()))?;
    builder = builder
        .with_intra_threads(INTRA_OP_THREADS)
        .map_err(|error| SessionError::Options(error.into()))?;

    for &(name, size) in dims {
        builder = builder
            .with_dimension_override(name, size)
            .map_err(|error| SessionError::Options(error.into()))?;
    }

    builder
        .commit_from_file(graph)
        .map_err(|error| SessionError::Load {
            path: graph.to_path_buf(),
            source: error,
        })
}

/// Caller-facing backend choice for an export whose several models share one
/// acceleration decision. CoreML names a single cache root; each model within the
/// export caches under its own subdirectory of it, so a decoder's compiled models
/// never collide with the encoder's.
#[derive(Debug, Clone, Copy)]
pub enum Accel<'a> {
    Cpu,
    CoreML { cache_root: &'a Path },
}

/// Opens `graph` on `accel`, pinning the symbolic dimensions in `dims`. For
/// CoreML the compiled-model cache lives at `cache_root/name`, keeping this
/// model's cache distinct from its siblings'.
pub(crate) fn session_for(
    graph: &Path,
    accel: Accel,
    name: &str,
    dims: &[(&str, i64)],
) -> Result<Session, SessionError> {
    match accel {
        Accel::Cpu => session(graph, Backend::Cpu, dims),
        Accel::CoreML { cache_root } => {
            let cache_dir = cache_root.join(name);
            session(
                graph,
                Backend::CoreML {
                    cache_dir: &cache_dir,
                },
                dims,
            )
        }
    }
}
