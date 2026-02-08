//! Test harness for analysis worker tests.
//!
//! Provides mock HTTP responses for unit testing the worker flow.

#![allow(dead_code)] // Test infrastructure - used by tests in this module

use std::sync::Arc;

use bytes::Bytes;
use chrono::Utc;
use chronoscope_analysis::{ModelVersions, TritonClient};
use chronoscope_db::media_store::{InMemoryMediaStore, MediaStore};
use chronoscope_db::workers::MediaForAnalysis;
use chronoscope_db::{Database, Email, MediaData, MediaId, MediaType, UserId};
use chronoscope_integrations::{HttpClient, MockHttpClient};
use sha2::{Digest, Sha256};
use url::Url;

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

/// Build a valid Triton inference response body.
///
/// The response wraps an `AnalysisResult` in Triton's output format.
pub fn triton_success_response(
    analysis_result: &chronoscope_analysis::AnalysisResult,
) -> Result<Vec<u8>, serde_json::Error> {
    let result_json = serde_json::to_string(analysis_result)?;

    let response = serde_json::json!({
        "outputs": [{
            "name": "result",
            "data": [result_json]
        }]
    });

    serde_json::to_vec(&response)
}

/// Create a minimal valid `AnalysisResult` for testing.
pub fn minimal_analysis_result() -> chronoscope_analysis::AnalysisResult {
    use chronoscope_analysis::{
        AnalysisResult, AnalyzedMediaType, BoundingBox, RleMask, SceneType, Subimage,
        SubimageAnalysis, SubimageBounds,
    };

    AnalysisResult {
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
                media_type: AnalyzedMediaType::Photo,
                content_summary: "A test image".to_string(),
                scene_type: SceneType::Outdoor,
                temporal_cues: vec![],
                extracted_text: vec![],
                thinking: None,
                embedding: vec![],
                regions: vec![],
                region_relationships: vec![],
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

    /// Claim media for analysis and process with a mock HTTP client.
    ///
    /// Returns the worker result for the first claimed item.
    pub async fn process_with_mock(
        &self,
        http_client: Arc<dyn HttpClient>,
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

        // Create worker with mock HTTP client
        let triton_url = Url::parse("http://localhost:8000")?;
        let triton = TritonClient::new(triton_url, http_client);
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
    use reqwest::StatusCode;

    #[tokio::test]
    async fn test_successful_analysis() -> TestResult {
        let harness = AnalysisTestHarness::new().await?;
        harness.create_media_for_analysis().await?;

        // Mock a successful Triton response
        let response_body = triton_success_response(&minimal_analysis_result())?;
        let http = Arc::new(MockHttpClient::success(&response_body)?);

        let (_media, result) = harness.process_with_mock(http).await?;

        assert!(
            matches!(result, ItemResult::Success { .. }),
            "expected Success, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_retriable_error_triton_503() -> TestResult {
        let harness = AnalysisTestHarness::new().await?;
        harness.create_media_for_analysis().await?;

        // Mock a 503 Service Unavailable response
        let http = Arc::new(MockHttpClient::error_status(
            StatusCode::SERVICE_UNAVAILABLE,
            "service temporarily unavailable",
        )?);

        let (_media, result) = harness.process_with_mock(http).await?;

        assert!(
            matches!(result, ItemResult::RetriableFailure { .. }),
            "expected RetriableFailure for 503, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_retriable_error_triton_500() -> TestResult {
        let harness = AnalysisTestHarness::new().await?;
        harness.create_media_for_analysis().await?;

        // Mock a 500 Internal Server Error response
        let http = Arc::new(MockHttpClient::error_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal server error",
        )?);

        let (_media, result) = harness.process_with_mock(http).await?;

        assert!(
            matches!(result, ItemResult::RetriableFailure { .. }),
            "expected RetriableFailure for 500, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_retriable_error_triton_429() -> TestResult {
        let harness = AnalysisTestHarness::new().await?;
        harness.create_media_for_analysis().await?;

        // Mock a 429 Too Many Requests response
        let http = Arc::new(MockHttpClient::error_status(
            StatusCode::TOO_MANY_REQUESTS,
            "rate limited",
        )?);

        let (_media, result) = harness.process_with_mock(http).await?;

        assert!(
            matches!(result, ItemResult::RetriableFailure { .. }),
            "expected RetriableFailure for 429, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_permanent_error_triton_400() -> TestResult {
        let harness = AnalysisTestHarness::new().await?;
        harness.create_media_for_analysis().await?;

        // Mock a 400 Bad Request (e.g., invalid image format)
        let http = Arc::new(MockHttpClient::error_status(
            StatusCode::BAD_REQUEST,
            "invalid image format",
        )?);

        let (_media, result) = harness.process_with_mock(http).await?;

        assert!(
            matches!(result, ItemResult::PermanentFailure { .. }),
            "expected PermanentFailure for 400, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_permanent_error_image_too_large() -> TestResult {
        let harness = AnalysisTestHarness::new().await?;
        harness.create_media_for_analysis().await?;

        // Mock a 413 Payload Too Large response
        let http = Arc::new(MockHttpClient::error_status(
            StatusCode::PAYLOAD_TOO_LARGE,
            "image exceeds maximum size of 10MB",
        )?);

        let (_media, result) = harness.process_with_mock(http).await?;

        assert!(
            matches!(result, ItemResult::PermanentFailure { .. }),
            "expected PermanentFailure for 413, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_permanent_error_malformed_response() -> TestResult {
        let harness = AnalysisTestHarness::new().await?;
        harness.create_media_for_analysis().await?;

        // Mock a 200 OK with unparseable JSON body
        let http = Arc::new(MockHttpClient::success(b"not valid json {{{")?);

        let (_media, result) = harness.process_with_mock(http).await?;

        assert!(
            matches!(result, ItemResult::PermanentFailure { .. }),
            "expected PermanentFailure for malformed response, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_permanent_error_missing_result_output() -> TestResult {
        let harness = AnalysisTestHarness::new().await?;
        harness.create_media_for_analysis().await?;

        // Mock a 200 OK with valid JSON but missing 'result' output
        let response = serde_json::json!({
            "outputs": [{
                "name": "wrong_name",
                "data": ["{}"]
            }]
        });
        let http = Arc::new(MockHttpClient::success(&serde_json::to_vec(&response)?)?);

        let (_media, result) = harness.process_with_mock(http).await?;

        assert!(
            matches!(result, ItemResult::PermanentFailure { .. }),
            "expected PermanentFailure for missing result output, got {result:?}"
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

        // The HTTP client won't even be called - storage lookup fails first
        let response_body = triton_success_response(&minimal_analysis_result())?;
        let http = Arc::new(MockHttpClient::success(&response_body)?);

        let (_media, result) = harness.process_with_mock(http).await?;

        assert!(
            matches!(result, ItemResult::RetriableFailure { .. }),
            "expected RetriableFailure for storage unavailable, got {result:?}"
        );
        Ok(())
    }
}
