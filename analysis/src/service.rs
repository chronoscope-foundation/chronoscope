//! Triton Inference Server service trait.

use async_trait::async_trait;

use crate::error::AnalysisError;
use crate::schema::AnalysisResult;

/// Abstraction over Triton Inference Server communication.
///
/// Implementations include:
/// - [`GrpcTritonClient`](crate::grpc::GrpcTritonClient): Live gRPC with optional fixture caching
/// - [`MockTritonService`](crate::mock::MockTritonService): In-memory mock for unit tests
#[async_trait]
pub trait TritonService: Send + Sync {
    /// Check if the Triton server is ready.
    async fn is_server_ready(&self) -> Result<(), AnalysisError>;

    /// Check if a specific model is ready.
    async fn is_model_ready(&self, model_name: &str) -> Result<bool, AnalysisError>;

    /// Compute a DINOv3 embedding for an image.
    ///
    /// Returns a 1024-dimensional L2-normalized CLS embedding.
    async fn embed(&self, image: &[u8]) -> Result<Vec<f32>, AnalysisError>;

    /// Analyze an image using the Triton BLS pipeline.
    ///
    /// Runs subimage detection (SAM3), region segmentation, VLM analysis,
    /// and DINOv3 embeddings.
    async fn analyze(&self, image: &[u8]) -> Result<AnalysisResult, AnalysisError>;
}
