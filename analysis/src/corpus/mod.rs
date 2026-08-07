//! Corpus image pipeline.
//!
//! Manifest parsing and rate-limited downloading for the `corpus-fetch` FOD
//! binary. The images are the development set the analysis pipeline runs
//! against; the assertions that once accompanied them were written against a
//! schema this crate replaces, and are recoverable from git history when the
//! pipeline does enough to make them meaningful again.

pub mod download;
pub mod manifest;

use std::path::PathBuf;

pub use download::ImageDownloader;
pub use manifest::CorpusManifest;

/// Resolve the corpus manifest path from the `CORPUS_MANIFEST` env var (set by Nix).
pub fn manifest_path() -> Result<PathBuf, CorpusError> {
    std::env::var("CORPUS_MANIFEST")
        .map(PathBuf::from)
        .map_err(|_| {
            CorpusError::Manifest(
                "CORPUS_MANIFEST env var not set — run from `nix develop` or `just`".into(),
            )
        })
}

/// Errors that can occur during corpus operations.
#[derive(Debug, thiserror::Error)]
pub enum CorpusError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("manifest error: {0}")]
    Manifest(String),
    #[error("download error: {0}")]
    Download(String),
    #[error("serialization error: {0}")]
    Serialization(String),
    /// Hashing shells out to `nix hash path` so the value matches what a
    /// recursive-mode FOD will compute, rather than reimplementing NAR hashing.
    #[error("nix invocation failed: {0}")]
    Nix(String),
}
