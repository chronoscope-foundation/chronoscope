//! Triton Inference Server HTTP client.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use bytes::Bytes;
use chronoscope_integrations::{HttpClient, HttpRequest};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::AnalysisError;
use crate::schema::{AnalysisResult, VlmAnalysis};

/// Client for communicating with Triton Inference Server.
pub struct TritonClient {
    endpoint: Url,
    http: Arc<dyn HttpClient>,
}

/// Default timeout for inference request.
const DEFAULT_TIMEOUT: Duration = Duration::from_mins(5);

impl TritonClient {
    /// Create a new Triton client.
    ///
    /// # Arguments
    /// * `endpoint` - Base URL of the Triton server (e.g., `http://localhost:8080`)
    /// * `http` - HTTP client implementation (real or mock)
    #[must_use]
    pub fn new(endpoint: Url, http: Arc<dyn HttpClient>) -> Self {
        Self { endpoint, http }
    }

    /// Join a path to the endpoint URL.
    fn url(&self, path: &str) -> Result<Url, AnalysisError> {
        self.endpoint
            .join(path)
            .map_err(|e| AnalysisError::Connection(e.to_string()))
    }

    /// Check if the Triton server is ready.
    ///
    /// Returns `Ok(())` if ready, or an error describing why it's not ready.
    ///
    /// # Errors
    /// Returns `AnalysisError::Triton` if the server responds with a non-success status,
    /// or `AnalysisError::Connection` if the request fails entirely.
    pub async fn is_server_ready(&self) -> Result<(), AnalysisError> {
        let url = self.url("/v2/health/ready")?;

        let request = HttpRequest::get(url);
        let response = self
            .http
            .execute(request)
            .await
            .map_err(|e| AnalysisError::Connection(e.to_string()))?;

        if response.is_success() {
            Ok(())
        } else {
            Err(AnalysisError::Triton {
                status: response.status,
                message: String::from_utf8_lossy(&response.body).into_owned(),
            })
        }
    }

    /// Check if a specific model is ready.
    ///
    /// # Errors
    /// Returns an error if the model health check request fails.
    pub async fn is_model_ready(&self, model_name: &str) -> Result<bool, AnalysisError> {
        let url = self.url(&format!("/v2/models/{model_name}/ready"))?;

        let request = HttpRequest::get(url);
        let response = self
            .http
            .execute(request)
            .await
            .map_err(|e| AnalysisError::Connection(e.to_string()))?;

        Ok(response.is_success())
    }

    /// Analyze an image using the analysis pipeline.
    ///
    /// # Arguments
    /// * `image` - Raw image bytes (JPEG, PNG, etc.)
    ///
    /// # Errors
    /// Returns an error if the inference request fails, Triton returns an error,
    /// or the response cannot be parsed.
    pub async fn analyze(&self, image: &[u8]) -> Result<AnalysisResult, AnalysisError> {
        let url = self.url("/v2/models/analysis/infer")?;

        // Build the inference request
        let schema = schemars::schema_for!(VlmAnalysis);
        let schema_json = serde_json::to_string(&schema)
            .map_err(|e| AnalysisError::ResponseParsing(e.to_string()))?;

        // Note: shapes are [batch_size, dim] because model has max_batch_size=1
        let triton_request = TritonInferRequest {
            inputs: vec![
                TritonInputTensor {
                    name: "image".to_string(),
                    datatype: "BYTES".to_string(),
                    shape: vec![1, 1],
                    // Image bytes are base64-encoded for JSON transport
                    data: vec![base64::engine::general_purpose::STANDARD.encode(image)],
                },
                TritonInputTensor {
                    name: "schema".to_string(),
                    datatype: "BYTES".to_string(),
                    shape: vec![1, 1],
                    data: vec![schema_json],
                },
            ],
            outputs: vec![TritonOutputRequest {
                name: "result".to_string(),
            }],
        };

        let body_json = serde_json::to_vec(&triton_request)
            .map_err(|e| AnalysisError::ResponseParsing(e.to_string()))?;

        let request = HttpRequest::post(url)
            .json_body(Bytes::from(body_json))
            .timeout(DEFAULT_TIMEOUT);

        let response = self
            .http
            .execute(request)
            .await
            .map_err(|e| AnalysisError::Connection(e.to_string()))?;

        if !response.is_success() {
            return Err(AnalysisError::Triton {
                status: response.status,
                message: String::from_utf8_lossy(&response.body).into_owned(),
            });
        }

        let infer_response: TritonInferResponse = serde_json::from_slice(&response.body)
            .map_err(|e| AnalysisError::ResponseParsing(e.to_string()))?;

        // Extract the result from the response
        let result_output = infer_response
            .outputs
            .into_iter()
            .find(|o| o.name == "result")
            .ok_or_else(|| AnalysisError::ResponseParsing("missing 'result' output".to_string()))?;

        let result_str = result_output
            .data
            .first()
            .ok_or_else(|| AnalysisError::ResponseParsing("empty result data".to_string()))?;

        serde_json::from_str(result_str)
            .map_err(|e| AnalysisError::ResponseParsing(format!("invalid result JSON: {e}")))
    }
}

// ==================== Triton Protocol Types ====================

#[derive(Debug, Serialize)]
struct TritonInferRequest {
    inputs: Vec<TritonInputTensor>,
    outputs: Vec<TritonOutputRequest>,
}

#[derive(Debug, Serialize)]
struct TritonInputTensor {
    name: String,
    datatype: String,
    shape: Vec<i64>,
    data: Vec<String>,
}

#[derive(Debug, Serialize)]
struct TritonOutputRequest {
    name: String,
}

#[derive(Debug, Deserialize)]
struct TritonInferResponse {
    outputs: Vec<TritonOutputTensor>,
}

#[derive(Debug, Deserialize)]
struct TritonOutputTensor {
    name: String,
    data: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chronoscope_integrations::MockHttpClient;

    #[test]
    fn test_client_creation() -> Result<(), Box<dyn std::error::Error>> {
        let url = Url::parse("http://localhost:8080")?;
        let http = Arc::new(MockHttpClient::success(b"")?);
        let client = TritonClient::new(url.clone(), http);
        assert_eq!(client.endpoint, url);
        Ok(())
    }

    #[test]
    fn test_client_with_different_endpoints() -> Result<(), Box<dyn std::error::Error>> {
        // Test with path
        let url = Url::parse("http://localhost:8080/v2")?;
        let http = Arc::new(MockHttpClient::success(b"")?);
        let client = TritonClient::new(url.clone(), http);
        assert_eq!(client.endpoint, url);

        // Test with https
        let url = Url::parse("https://triton.example.com")?;
        let http = Arc::new(MockHttpClient::success(b"")?);
        let client = TritonClient::new(url.clone(), http);
        assert_eq!(client.endpoint, url);
        Ok(())
    }

    #[tokio::test]
    async fn test_is_server_ready_success() -> Result<(), Box<dyn std::error::Error>> {
        let url = Url::parse("http://localhost:8080")?;
        let http = Arc::new(MockHttpClient::success(b"")?);
        let client = TritonClient::new(url, http);

        // Should succeed without error
        client.is_server_ready().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_is_server_ready_not_ready() -> Result<(), Box<dyn std::error::Error>> {
        let url = Url::parse("http://localhost:8080")?;
        let http = Arc::new(MockHttpClient::status(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
        )?);
        let client = TritonClient::new(url, http);

        // 503 should return an error, not Ok(false)
        let result = client.is_server_ready().await;
        assert!(result.is_err(), "expected error for 503, got Ok");
        Ok(())
    }
}
