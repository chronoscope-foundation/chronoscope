//! Analysis worker.
//!
//! This module provides the `AnalysisWorker` which implements the `Worker` trait
//! to analyze images using Triton Inference Server.
//!
//! # Architecture
//!
//! The analysis worker sends images to the Triton BLS orchestrator, which runs
//! a multi-stage pipeline:
//! 1. **Subimage detection** (SAM3): Splits composite images into panels
//! 2. **Region segmentation** (SAM3): Segments structures within each subimage
//! 3. **VLM analysis**: Describes content, temporal cues, and region semantics
//! 4. **Embeddings** (DINOv3): Computes visual embeddings for similarity search
//!
//! Results are stored in the `media` table as a single JSON column.

mod error;
#[cfg(test)]
pub(crate) mod test_harness;

use std::sync::Arc;

use chronoscope_analysis::TritonService;
use chronoscope_db::Database;
use chronoscope_db::media_store::MediaStore;
use chronoscope_db::workers::MediaForAnalysis;
use tracing::{Instrument, info_span};

use crate::worker::{ItemResult, Worker};

pub use error::AnalysisError;

/// Worker that analyzes images using Triton Inference Server.
///
/// For each claimed media item:
/// 1. Fetches image bytes from media store
/// 2. Sends to the Triton BLS pipeline (subimage detection, SAM3, VLM, DINOv3)
/// 3. Stores results as JSON in the media table
pub struct AnalysisWorker {
    triton: Arc<dyn TritonService>,
    db: Arc<Database>,
    media_store: Arc<dyn MediaStore>,
}

impl AnalysisWorker {
    /// Create a new analysis worker.
    #[must_use]
    pub fn new(
        triton: Arc<dyn TritonService>,
        db: Arc<Database>,
        media_store: Arc<dyn MediaStore>,
    ) -> Self {
        Self {
            triton,
            db,
            media_store,
        }
    }

    /// Process a single media item.
    async fn process_media(&self, media: &MediaForAnalysis) -> Result<(), AnalysisError> {
        // 1. Fetch image bytes from storage
        let media_with_meta = self
            .media_store
            .get(&media.storage_key)
            .await
            .map_err(|e| AnalysisError::Storage(e.to_string()))?
            .ok_or_else(|| {
                AnalysisError::Storage(format!("media not found in storage: {}", media.storage_key))
            })?;

        // 2. Analyze with Triton
        let result = self.triton.analyze(&media_with_meta.data).await?;

        // 3. Store results in database
        let analysis_json = serde_json::to_string(&result)?;
        self.db
            .mark_analysis_complete(&media.id, &analysis_json)
            .await?;

        Ok(())
    }
}

#[async_trait::async_trait]
impl Worker for AnalysisWorker {
    type Item = MediaForAnalysis;
    type Discovered = (); // Analysis doesn't discover new items
    type Error = AnalysisError;

    async fn process_batch(
        &self,
        items: Vec<Self::Item>,
    ) -> Vec<(Self::Item, ItemResult<Self::Discovered, Self::Error>)> {
        let mut results = Vec::with_capacity(items.len());

        for item in items {
            let span = info_span!("analyze_media", media_id = %item.id);
            let result = async {
                match self.process_media(&item).await {
                    Ok(()) => ItemResult::Success { discovered: vec![] },
                    Err(e) if e.is_retriable() => ItemResult::RetriableFailure { error: e },
                    Err(e) => ItemResult::PermanentFailure { error: e },
                }
            }
            .instrument(span)
            .await;

            results.push((item, result));
        }

        results
    }
}
