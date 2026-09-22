//! Corpus image pipeline.
//!
//! The images are the development set the analysis pipeline runs against, and
//! the manifest carries what each one should read as: how many panels, and how
//! many regions within them. The orchestration test asserts against that, so
//! parsing and [`load_image`] compile in every build; [`download`], which the
//! `corpus-fetch` FOD binary drives, needs an HTTP stack and rides the `corpus`
//! feature.

#[cfg(feature = "corpus")]
pub mod download;
pub mod manifest;

use std::path::{Path, PathBuf};

use image::DynamicImage;

#[cfg(feature = "corpus")]
pub use download::ImageDownloader;
pub use manifest::CorpusManifest;

/// Loads the corpus image `id` from the link farm at `dir`.
///
/// The farm names each file by its bare entry id, so there is no extension to
/// read a decoder off: the format is sniffed from the bytes instead. Reading a
/// corpus image any other way decodes nothing.
pub fn load_image(dir: &Path, id: &str) -> Result<DynamicImage, CorpusError> {
    let path = dir.join(id);
    // Every arm names the file. A corpus read fails over a whole link farm of
    // bare ids, where "no such file" alone says nothing about which one.
    let read = |source| CorpusError::Read {
        path: path.clone(),
        source,
    };
    image::ImageReader::open(&path)
        .map_err(read)?
        .with_guessed_format()
        .map_err(read)?
        .decode()
        .map_err(|source| CorpusError::Decode {
            path,
            source: Box::new(source),
        })
}

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
    /// A corpus file could not be opened or read.
    #[error("could not read the corpus image at `{path}`")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A corpus file was read but held no image this crate can decode.
    #[error("could not decode the corpus image at `{path}`")]
    Decode {
        path: PathBuf,
        #[source]
        source: Box<image::ImageError>,
    },
    /// Hashing shells out to `nix hash path` so the value matches what a
    /// recursive-mode FOD will compute, rather than reimplementing NAR hashing.
    #[error("nix invocation failed: {0}")]
    Nix(String),
}
