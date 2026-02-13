//! Test harness for analysis worker tests.
//!
//! Provides mock Triton service for unit testing the worker flow.

#![allow(dead_code)] // Test infrastructure - used by tests in this module

use std::sync::Arc;

use bytes::Bytes;
use chrono::Utc;
use chronoscope_analysis::mock::MockTritonService;
use chronoscope_analysis::{ModelVersions, TritonService};
use chronoscope_db::media_store::{InMemoryMediaStore, MediaStore};
use chronoscope_db::workers::MediaForAnalysis;
use chronoscope_db::{Database, Email, MediaData, MediaId, MediaType, UserId};
use sha2::{Digest, Sha256};

use super::{AnalysisError, AnalysisWorker};
use crate::worker::{ItemResult, Worker};

/// Result type for tests.
pub type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Error for test harness failures.
#[derive(Debug)]
pub struct TestError(pub String);

impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for TestError {}

// ==================== Utilities ====================

/// Compute SHA-256 hash of bytes.
fn sha2_hash(data: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

// ==================== Test Fixtures ====================

/// Create a minimal valid `AnalysisResult` for testing.
pub fn minimal_analysis_result() -> chronoscope_analysis::AnalysisResult {
    use chronoscope_analysis::{
        AnalysisResult, AnalyzedMediaType, BoundingBox, PhotoColor, RleMask, SceneAnalysis,
        SceneType, Subimage, SubimageAnalysis, SubimageBounds,
    };

    AnalysisResult::Success {
        subimages: vec![Subimage {
            bounds: SubimageBounds {
                bbox: BoundingBox {
                    x: 0,
                    y: 0,
                    width: 100,
                    height: 100,
                },
                mask: RleMask {
                    counts: "01".to_string(),
                },
            },
            analysis: SubimageAnalysis::Analyzed {
                scene: SceneAnalysis {
                    media_type: AnalyzedMediaType::Photo {
                        color: PhotoColor::Color,
                    },
                    content_summary: "A test image".to_string(),
                    scene_type: SceneType::Outdoor,
                },
                embedding: None,
                regions: vec![],
            },
        }],
        versions: ModelVersions {
            vlm: "test-vlm".to_string(),
            sam3: "test-sam3".to_string(),
            dinov3: "test-dinov3".to_string(),
            git_sha: "test-sha".to_string(),
        },
    }
}

// ==================== Test Harness ====================

/// Test harness for analysis worker tests.
pub struct AnalysisTestHarness {
    db: Arc<Database>,
    media_store: Arc<InMemoryMediaStore>,
    user_id: UserId,
}

impl AnalysisTestHarness {
    /// Create a new test harness with an in-memory database.
    pub async fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let db = Arc::new(Database::new("sqlite::memory:").await?);
        let user_id = UserId::generate();
        db.create_user(&user_id, "testuser", &Email::new("test@example.com"))
            .await?;
        Ok(Self {
            db,
            media_store: Arc::new(InMemoryMediaStore::new()),
            user_id,
        })
    }

    /// Get a reference to the database.
    pub fn db(&self) -> &Arc<Database> {
        &self.db
    }

    /// Get a reference to the media store.
    pub fn media_store(&self) -> &Arc<InMemoryMediaStore> {
        &self.media_store
    }

    /// Store an image in the media store for testing.
    pub async fn store_image(
        &self,
        storage_key: &str,
        image_bytes: &[u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.media_store
            .put(
                storage_key,
                Bytes::copy_from_slice(image_bytes),
                "image/jpeg",
            )
            .await?;
        Ok(())
    }

    /// Create a media record in the database ready for analysis.
    ///
    /// Returns the media ID and storage key.
    pub async fn create_media_for_analysis(
        &self,
    ) -> Result<(MediaId, String), Box<dyn std::error::Error + Send + Sync>> {
        let storage_key = format!("test/{}.jpg", uuid::Uuid::new_v4());
        let image_bytes = b"fake jpeg data";

        // Store image bytes
        self.store_image(&storage_key, image_bytes).await?;

        // Create media record with test data
        let media_data = MediaData {
            exact_hash: sha2_hash(image_bytes),
            perceptual_hash: None,
            storage_key: storage_key.clone(),
            media_type: MediaType::Image,
            width: 100,
            height: 100,
            duration_seconds: None,
            captured_at: None,
            location: None,
            source_metadata: None,
            fetched_at: Utc::now().naive_utc(),
        };

        let media_id = self.db.get_or_create_media(&media_data).await?;
        Ok((media_id, storage_key))
    }

    /// Claim media for analysis and process with a mock Triton service.
    ///
    /// Returns the worker result for the first claimed item.
    pub async fn process_with_mock(
        &self,
        triton: Arc<dyn TritonService>,
    ) -> Result<
        (MediaForAnalysis, ItemResult<(), AnalysisError>),
        Box<dyn std::error::Error + Send + Sync>,
    > {
        // Claim media for analysis
        let stale_cutoff = Utc::now().naive_utc() - chrono::Duration::hours(1);
        let claimed = self
            .db
            .analysis_queue
            .claim("test-worker", 1, stale_cutoff)
            .await?;

        let media = claimed
            .into_iter()
            .next()
            .ok_or_else(|| TestError("no media claimed".into()))?;

        // Create worker with mock Triton service
        let worker = AnalysisWorker::new(triton, self.db.clone(), self.media_store.clone());

        // Process the batch
        let results = worker.process_batch(vec![media]).await;
        let (media, result) = results
            .into_iter()
            .next()
            .ok_or_else(|| TestError("no result returned".into()))?;

        Ok((media, result))
    }
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_successful_analysis() -> TestResult {
        let harness = AnalysisTestHarness::new().await?;
        harness.create_media_for_analysis().await?;

        let triton = Arc::new(MockTritonService::with_analysis_result(
            minimal_analysis_result(),
        ));

        let (_media, result) = harness.process_with_mock(triton).await?;

        assert!(
            matches!(result, ItemResult::Success { .. }),
            "expected Success, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_retriable_error_triton_unavailable() -> TestResult {
        let harness = AnalysisTestHarness::new().await?;
        harness.create_media_for_analysis().await?;

        let triton = Arc::new(MockTritonService::with_error(
            chronoscope_analysis::AnalysisError::Triton {
                retriable: true,
                message: "service temporarily unavailable".to_string(),
            },
        ));

        let (_media, result) = harness.process_with_mock(triton).await?;

        assert!(
            matches!(result, ItemResult::RetriableFailure { .. }),
            "expected RetriableFailure for retriable Triton error, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_retriable_error_triton_internal() -> TestResult {
        let harness = AnalysisTestHarness::new().await?;
        harness.create_media_for_analysis().await?;

        let triton = Arc::new(MockTritonService::with_error(
            chronoscope_analysis::AnalysisError::Triton {
                retriable: true,
                message: "internal server error".to_string(),
            },
        ));

        let (_media, result) = harness.process_with_mock(triton).await?;

        assert!(
            matches!(result, ItemResult::RetriableFailure { .. }),
            "expected RetriableFailure for retriable Triton error, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_permanent_error_triton_invalid_argument() -> TestResult {
        let harness = AnalysisTestHarness::new().await?;
        harness.create_media_for_analysis().await?;

        let triton = Arc::new(MockTritonService::with_error(
            chronoscope_analysis::AnalysisError::Triton {
                retriable: false,
                message: "invalid image format".to_string(),
            },
        ));

        let (_media, result) = harness.process_with_mock(triton).await?;

        assert!(
            matches!(result, ItemResult::PermanentFailure { .. }),
            "expected PermanentFailure for permanent Triton error, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_permanent_error_malformed_response() -> TestResult {
        let harness = AnalysisTestHarness::new().await?;
        harness.create_media_for_analysis().await?;

        let triton = Arc::new(MockTritonService::with_error(
            chronoscope_analysis::AnalysisError::ResponseParsing("malformed response".to_string()),
        ));

        let (_media, result) = harness.process_with_mock(triton).await?;

        assert!(
            matches!(result, ItemResult::PermanentFailure { .. }),
            "expected PermanentFailure for parsing error, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_retriable_error_storage_unavailable() -> TestResult {
        let harness = AnalysisTestHarness::new().await?;

        // Create media record but DON'T store the image bytes in the media store
        let storage_key = format!("test/{}.jpg", uuid::Uuid::new_v4());
        let fake_bytes = b"not stored";

        let media_data = MediaData {
            exact_hash: sha2_hash(fake_bytes),
            perceptual_hash: None,
            storage_key: storage_key.clone(),
            media_type: MediaType::Image,
            width: 100,
            height: 100,
            duration_seconds: None,
            captured_at: None,
            location: None,
            source_metadata: None,
            fetched_at: Utc::now().naive_utc(),
        };
        harness.db.get_or_create_media(&media_data).await?;

        // The triton client won't even be called - storage lookup fails first
        let triton = Arc::new(MockTritonService::with_analysis_result(
            minimal_analysis_result(),
        ));

        let (_media, result) = harness.process_with_mock(triton).await?;

        assert!(
            matches!(result, ItemResult::RetriableFailure { .. }),
            "expected RetriableFailure for storage unavailable, got {result:?}"
        );
        Ok(())
    }
}
