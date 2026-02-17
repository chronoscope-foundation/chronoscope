//! Corpus test suite for the analysis pipeline.
//!
//! Downloads a curated set of images, runs them through real SAM3 + DINOv3
//! models via Python subprocess, caches results on disk in content-addressed
//! run directories, and makes specific per-image assertions driven by metadata
//! in `corpus.json`.
//!
//! Gated behind the `corpus-test` feature — never runs in `just check`.

pub mod assertions;
pub mod download;
pub mod manifest;
pub mod runner;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::info;

use crate::schema::{AnalysisResult, Embedding};

use download::ImageDownloader;
use manifest::CorpusManifest;

/// Filename of the corpus manifest.
const CORPUS_JSON: &str = "corpus.json";

/// Directory for cached images (relative to analysis dir).
const CORPUS_IMAGE_DIR: &str = "corpus/images";

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

/// Eagerly-loaded corpus fixture with incremental caching.
///
/// Downloads images and runs pipeline on first access.
/// Results cached in content-addressed run directories, invalidated
/// when pipeline code changes.
pub struct CorpusFixture {
    manifest: CorpusManifest,
    results: BTreeMap<String, AnalysisResult>,
}

impl CorpusFixture {
    /// Build from pre-loaded manifest and results.
    ///
    /// Used by the viewer subcommand (loads cached results without running pipeline).
    pub fn from_parts(
        manifest: CorpusManifest,
        results: BTreeMap<String, AnalysisResult>,
    ) -> Result<Self, CorpusError> {
        manifest.validate()?;
        Ok(Self { manifest, results })
    }

    /// Reference to the underlying manifest.
    pub fn manifest(&self) -> &CorpusManifest {
        &self.manifest
    }

    /// Load corpus fixture, running the pipeline on any uncached images.
    ///
    /// Downloads all images (cached on disk) and runs the pipeline
    /// incrementally — only images not yet in the run cache are processed.
    pub fn load() -> Result<Self, CorpusError> {
        let analysis_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let manifest = CorpusManifest::load(&analysis_dir.join(CORPUS_JSON))?;

        let pipeline_hash = runner::compute_pipeline_hash(&analysis_dir)?;
        info!(%pipeline_hash, "corpus pipeline hash");

        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| CorpusError::Pipeline(format!("creating tokio runtime: {e}")))?;

        let image_paths = rt.block_on(download_all_inner(&analysis_dir, &manifest))?;
        let results = runner::run_pipeline(&analysis_dir, &pipeline_hash, &image_paths)?;

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

/// Download all corpus images (shared async helper).
async fn download_all_inner(
    analysis_dir: &Path,
    manifest: &CorpusManifest,
) -> Result<BTreeMap<String, PathBuf>, CorpusError> {
    let http: Arc<dyn chronoscope_integrations::HttpClient> = Arc::new(
        chronoscope_integrations::ReqwestClient::new()
            .map_err(|e| CorpusError::Download(format!("creating HTTP client: {e}")))?,
    );

    let image_dir = analysis_dir.join(CORPUS_IMAGE_DIR);
    let downloader = ImageDownloader::new(http, image_dir);
    downloader.download_all(manifest).await
}

/// Download all corpus images without running the pipeline.
pub async fn download_all(analysis_dir: &Path) -> Result<BTreeMap<String, PathBuf>, CorpusError> {
    let manifest = CorpusManifest::load(&analysis_dir.join(CORPUS_JSON))?;
    let image_paths = download_all_inner(analysis_dir, &manifest).await?;
    info!(count = image_paths.len(), "downloaded all corpus images");
    Ok(image_paths)
}
