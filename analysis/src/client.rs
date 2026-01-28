//! Triton Inference Server HTTP client.

use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::AnalysisError;
use crate::schema::{AnalysisResult, VlmAnalysis};

/// Client for communicating with Triton Inference Server.
pub struct TritonClient {
    endpoint: Url,
    http: reqwest::Client,
}

/// Default timeout for inference request.
const DEFAULT_TIMEOUT: Duration = Duration::from_mins(5);

impl TritonClient {
    /// Create a new Triton client.
    ///
    /// # Arguments
    /// * `endpoint` - Base URL of the Triton server (e.g., `http://localhost:8080`)
    ///
    /// # Errors
    /// Returns an error if the HTTP client cannot be initialized (e.g., TLS backend failure).
    pub fn new(endpoint: Url) -> Result<Self, AnalysisError> {
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .map_err(|e| AnalysisError::Connection(e.to_string()))?;
        Ok(Self { endpoint, http })
    }

    /// Check if the Triton server is ready.
    ///
    /// # Errors
    /// Returns an error if the health check request fails.
    pub async fn is_server_ready(&self) -> Result<bool, AnalysisError> {
        let url = self
            .endpoint
            .join("/v2/health/ready")
            .map_err(|e| AnalysisError::Connection(e.to_string()))?;

        let response = self.http.get(url).send().await?;
        Ok(response.status().is_success())
    }

    /// Check if a specific model is ready.
    ///
    /// # Errors
    /// Returns an error if the model health check request fails.
    pub async fn is_model_ready(&self, model_name: &str) -> Result<bool, AnalysisError> {
        let url = self
            .endpoint
            .join(&format!("/v2/models/{model_name}/ready"))
            .map_err(|e| AnalysisError::Connection(e.to_string()))?;

        let response = self.http.get(url).send().await?;
        Ok(response.status().is_success())
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
        let url = self
            .endpoint
            .join("/v2/models/analysis/infer")
            .map_err(|e| AnalysisError::Connection(e.to_string()))?;

        // Build the inference request
        let schema = schemars::schema_for!(VlmAnalysis);
        let schema_json = serde_json::to_string(&schema)
            .map_err(|e| AnalysisError::ResponseParsing(e.to_string()))?;

        // Note: shapes are [batch_size, dim] because model has max_batch_size=1
        let request = TritonInferRequest {
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

        let response = self.http.post(url).json(&request).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".to_string());
            return Err(AnalysisError::Triton(format!("{status}: {body}")));
        }

        let infer_response: TritonInferResponse = response
            .json()
            .await
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

    #[test]
    fn test_client_creation() -> Result<(), Box<dyn std::error::Error>> {
        let url = Url::parse("http://localhost:8080")?;
        let client = TritonClient::new(url.clone())?;
        assert_eq!(client.endpoint, url);
        Ok(())
    }

    #[test]
    fn test_client_with_different_endpoints() -> Result<(), Box<dyn std::error::Error>> {
        // Test with path
        let url = Url::parse("http://localhost:8080/v2")?;
        let client = TritonClient::new(url.clone())?;
        assert_eq!(client.endpoint, url);

        // Test with https
        let url = Url::parse("https://triton.example.com")?;
        let client = TritonClient::new(url.clone())?;
        assert_eq!(client.endpoint, url);
        Ok(())
    }
}
