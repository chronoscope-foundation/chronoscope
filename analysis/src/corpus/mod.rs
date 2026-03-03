//! Corpus image pipeline.
//!
//! Manifest parsing, rate-limited image downloading (for the `corpus-fetch`
//! FOD binary), per-image assertions, and pre-computed result loading.
//! Gated behind the `corpus-test` feature — never runs in `just check`.
//!
//! Environment variables (set by Nix):
//! - `CORPUS_MANIFEST` — path to the corpus manifest JSON (dev shell)
//! - `ANALYSIS_RESULTS` — path to the analysis results directory (`just corpus-test` builds on demand)

pub mod assertions;
pub mod download;
pub mod manifest;
pub mod runner;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use tracing::info;

use crate::schema::{AnalysisResult, Embedding};

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

/// Resolve the analysis results directory from the `ANALYSIS_RESULTS` env var (set by Nix).
fn analysis_results_dir() -> Result<PathBuf, CorpusError> {
    std::env::var("ANALYSIS_RESULTS")
        .map(PathBuf::from)
        .map_err(|_| {
            CorpusError::Pipeline(
                "ANALYSIS_RESULTS env var not set — run from `nix develop` or `just`".into(),
            )
        })
}

// ==================== Error ====================

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
    #[error("pipeline error: {0}")]
    Pipeline(String),
}

// ==================== Corpus Fixture ====================

/// Corpus fixture: manifest + pre-computed analysis results.
///
/// Reads results from the `ANALYSIS_RESULTS` directory (a Nix derivation
/// output containing `results.jsonl`). Nix content-addressing handles
/// cache invalidation — the store path changes when pipeline code changes.
pub struct CorpusFixture {
    manifest: CorpusManifest,
    results: BTreeMap<String, AnalysisResult>,
}

impl CorpusFixture {
    /// Reference to the underlying manifest.
    pub fn manifest(&self) -> &CorpusManifest {
        &self.manifest
    }

    /// Load corpus fixture from pre-computed analysis results.
    ///
    /// Reads `ANALYSIS_RESULTS/results.jsonl` produced by the Nix
    /// `analysis-results` derivation.
    pub fn load() -> Result<Self, CorpusError> {
        let manifest = CorpusManifest::load(&manifest_path()?)?;

        let results_dir = analysis_results_dir()?;
        let jsonl_path = results_dir.join("results.jsonl");
        info!(path = %jsonl_path.display(), "loading pre-computed analysis results");
        let results = runner::load_results_jsonl(&jsonl_path)?;

        Ok(Self { manifest, results })
    }

    /// Get analysis result by entry ID.
    pub fn result(&self, id: &str) -> Option<&AnalysisResult> {
        self.results.get(id)
    }

    /// Collect all addressable embeddings for ranking and clustering tests.
    ///
    /// Returns `(address, embedding)` pairs at both subimage and region levels:
    /// - `"schwerin-palace.0"` — single-subimage: the only subimage embedding
    /// - `"schwerin-palace.0/0"` — multi-subimage: subimage 0 embedding
    /// - `"schwerin-palace.0/1"` — multi-subimage: subimage 1 embedding
    /// - `"schwerin-palace.0[2]"` — single-subimage: region 2
    /// - `"schwerin-palace.0/1[3]"` — multi-subimage: region 3 in subimage 1
    ///
    /// Single-subimage images use bare IDs; multi-subimage images always
    /// include explicit `/N` indices (zero-based) to avoid ambiguity.
    pub fn all_addressable_embeddings(&self) -> HashMap<String, Embedding> {
        let mut out = HashMap::new();
        for (id, result) in &self.results {
            if let AnalysisResult::Success { subimages, .. } = result {
                let multi = subimages.len() > 1;
                for (si, sub) in subimages.iter().enumerate() {
                    let sub_addr = if multi {
                        format!("{id}/{si}")
                    } else {
                        id.clone()
                    };
                    if let Some(emb) = sub.analysis.embedding() {
                        out.insert(sub_addr.clone(), emb.clone());
                    }
                    if let Some(regions) = sub.analysis.regions() {
                        for (ri, region) in regions.iter().enumerate() {
                            out.insert(format!("{sub_addr}[{ri}]"), region.embedding.clone());
                        }
                    }
                }
            }
        }
        out
    }
}
