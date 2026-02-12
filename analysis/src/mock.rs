//! Mock Triton service for unit tests.

use async_trait::async_trait;

use crate::error::AnalysisError;
use crate::schema::AnalysisResult;
use crate::service::TritonService;

/// A mock Triton service that returns canned responses.
///
/// Supports independent configuration of `embed` and `analyze` results,
/// enabling tests that exercise one method failing while the other succeeds.
pub struct MockTritonService {
    analyze_result: Result<AnalysisResult, AnalysisError>,
    embed_result: Result<Vec<f32>, AnalysisError>,
}

impl MockTritonService {
    /// Create a mock that always returns the given analysis result.
    ///
    /// `embed` returns a default 1024-dimensional zero vector.
    #[must_use]
    pub fn with_analysis_result(result: AnalysisResult) -> Self {
        Self {
            analyze_result: Ok(result),
            embed_result: Ok(vec![0.0; 1024]),
        }
    }

    /// Create a mock that always returns the given error from both `embed` and `analyze`.
    #[must_use]
    pub fn with_error(error: AnalysisError) -> Self {
        Self {
            analyze_result: Err(error.clone()),
            embed_result: Err(error),
        }
    }

    /// Create a mock with independently configured embed and analyze results.
    #[must_use]
    pub fn new(
        embed_result: Result<Vec<f32>, AnalysisError>,
        analyze_result: Result<AnalysisResult, AnalysisError>,
    ) -> Self {
        Self {
            analyze_result,
            embed_result,
        }
    }
}

#[async_trait]
impl TritonService for MockTritonService {
    async fn is_server_ready(&self) -> Result<(), AnalysisError> {
        Ok(())
    }

    async fn is_model_ready(&self, _model_name: &str) -> Result<bool, AnalysisError> {
        Ok(true)
    }

    async fn embed(&self, _image: &[u8]) -> Result<Vec<f32>, AnalysisError> {
        self.embed_result.clone()
    }

    async fn analyze(&self, _image: &[u8]) -> Result<AnalysisResult, AnalysisError> {
        self.analyze_result.clone()
    }
}
