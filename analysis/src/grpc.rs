//! gRPC client for Triton Inference Server.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use prost::Message;
use sha2::{Digest, Sha256};

use crate::error::AnalysisError;
use crate::schema::AnalysisResult;
use crate::schema::vlm_schema::SubimageOutput;
use crate::service::TritonService;
use crate::triton_proto;
use crate::triton_proto::grpc_inference_service_client::GrpcInferenceServiceClient;

/// Default timeout for inference requests.
///
/// Set to 10 minutes to accommodate worst-case BLS pipeline execution:
/// SAM3 subimage detection (~2 min) + per-subimage entity segmentation (~2 min each)
/// + VLM analysis (~5 min concurrent) + DINOv3 embeddings (~1 min).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

/// Interval between HTTP/2 keep-alive pings.
///
/// Prevents intermediaries (load balancers, firewalls, cloud NAT) from silently
/// closing idle TCP connections between analysis requests. Must exceed Triton's
/// `min_recv_ping_interval` (default ~5 min) to avoid "Too many pings" rejection.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(300);

/// How long to wait for a keep-alive acknowledgment before considering the
/// connection dead.
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);

/// gRPC client for Triton Inference Server.
///
/// Supports three modes:
/// - **Live**: Connects to a real Triton server via gRPC.
/// - **Recording**: Live gRPC + writes response fixtures to disk.
/// - **Offline**: No gRPC connection; reads fixtures from disk.
///   Health checks (`is_server_ready`, `is_model_ready`) always succeed in
///   offline mode — the actual fixture lookup happens on `embed`/`analyze`.
pub struct GrpcTritonClient {
    /// The endpoint URL used to connect (empty for offline mode).
    endpoint: String,
    mode: ClientMode,
}

enum ClientMode {
    /// Live gRPC connection (no caching).
    Live {
        client: GrpcInferenceServiceClient<tonic::transport::Channel>,
    },
    /// Live gRPC + write fixtures to disk.
    Recording {
        client: GrpcInferenceServiceClient<tonic::transport::Channel>,
        cache_dir: PathBuf,
    },
    /// No gRPC connection; read fixtures from disk.
    Offline { cache_dir: PathBuf },
}

/// Serializable fixture format for cached responses.
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedResponse {
    request_hash: String,
    #[serde(with = "base64_bytes")]
    response_bytes: Vec<u8>,
}

/// Serde helper to encode/decode `Vec<u8>` as a base64 string in JSON.
mod base64_bytes {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        serializer.serialize_str(&encoded)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(&s)
            .map_err(serde::de::Error::custom)
    }
}

/// Create a connected gRPC client with keep-alive configured.
async fn connect_channel(
    endpoint: &str,
) -> Result<GrpcInferenceServiceClient<tonic::transport::Channel>, AnalysisError> {
    let channel = tonic::transport::Endpoint::from_shared(endpoint.to_string())
        .map_err(|e| AnalysisError::Transport(format!("invalid endpoint: {e}")))?
        .keep_alive_while_idle(true)
        .http2_keep_alive_interval(KEEPALIVE_INTERVAL)
        .keep_alive_timeout(KEEPALIVE_TIMEOUT)
        .connect()
        .await
        .map_err(|e| AnalysisError::Transport(format!("gRPC connect failed: {e}")))?;

    Ok(GrpcInferenceServiceClient::new(channel))
}

impl GrpcTritonClient {
    /// Connect to a live Triton server via gRPC. No caching.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC channel cannot be established.
    pub async fn connect(endpoint: &str) -> Result<Self, AnalysisError> {
        let client = connect_channel(endpoint).await?;
        Ok(Self {
            endpoint: endpoint.to_string(),
            mode: ClientMode::Live { client },
        })
    }

    /// Connect to a live Triton server and record response fixtures.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC channel cannot be established.
    pub async fn recording(endpoint: &str, cache_dir: PathBuf) -> Result<Self, AnalysisError> {
        let client = connect_channel(endpoint).await?;
        Ok(Self {
            endpoint: endpoint.to_string(),
            mode: ClientMode::Recording { client, cache_dir },
        })
    }

    /// Create an offline client that reads fixtures from disk.
    ///
    /// Health checks always succeed; actual fixture lookups happen on
    /// `embed`/`analyze` and return descriptive errors if a fixture is missing.
    #[must_use]
    pub fn offline(cache_dir: PathBuf) -> Self {
        Self {
            endpoint: String::new(),
            mode: ClientMode::Offline { cache_dir },
        }
    }

    /// The endpoint URL used to connect, or empty for offline mode.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Compute SHA-256 hash of a serialized protobuf request.
    ///
    /// **Determinism note:** `prost::Message::encode_to_vec` is deterministic for
    /// the current request shape (no populated `map` fields). If `parameters` maps
    /// are used in the future, protobuf map serialization order is *not* guaranteed,
    /// which would produce non-deterministic hashes. In that case, sort map entries
    /// before hashing or hash a canonical representation.
    fn request_hash(request: &triton_proto::ModelInferRequest) -> String {
        let bytes = request.encode_to_vec();
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        hex::encode(hasher.finalize())
    }

    /// Read a cached response from disk.
    fn read_cache(
        cache_dir: &Path,
        hash: &str,
    ) -> Result<triton_proto::ModelInferResponse, AnalysisError> {
        let path = cache_dir.join(format!("{hash}.json"));
        let data = std::fs::read_to_string(&path).map_err(|e| {
            // Missing fixture is a permanent error — retrying won't create the file.
            AnalysisError::Triton {
                retriable: false,
                message: format!("fixture not found: {} ({})", path.display(), e),
            }
        })?;
        let cached: CachedResponse = serde_json::from_str(&data)
            .map_err(|e| AnalysisError::ResponseParsing(format!("invalid fixture JSON: {e}")))?;
        triton_proto::ModelInferResponse::decode(cached.response_bytes.as_slice())
            .map_err(|e| AnalysisError::ResponseParsing(format!("invalid fixture protobuf: {e}")))
    }

    /// Write a response fixture to disk.
    fn write_cache(
        cache_dir: &Path,
        hash: &str,
        response: &triton_proto::ModelInferResponse,
    ) -> Result<(), AnalysisError> {
        std::fs::create_dir_all(cache_dir)
            .map_err(|e| AnalysisError::Transport(format!("failed to create cache dir: {e}")))?;

        let cached = CachedResponse {
            request_hash: hash.to_string(),
            response_bytes: response.encode_to_vec(),
        };

        let path = cache_dir.join(format!("{hash}.json"));
        let json = serde_json::to_string(&cached)
            .map_err(|e| AnalysisError::ResponseParsing(format!("fixture serialization: {e}")))?;
        std::fs::write(&path, json).map_err(|e| {
            AnalysisError::Transport(format!(
                "failed to write fixture: {} ({})",
                path.display(),
                e
            ))
        })?;
        Ok(())
    }

    /// Send a `ModelInfer` RPC to the connected Triton server.
    async fn call_model_infer(
        client: &GrpcInferenceServiceClient<tonic::transport::Channel>,
        request: triton_proto::ModelInferRequest,
    ) -> Result<triton_proto::ModelInferResponse, AnalysisError> {
        let mut client = client.clone();
        let mut grpc_request = tonic::Request::new(request);
        grpc_request.set_timeout(DEFAULT_TIMEOUT);
        let response = client
            .model_infer(grpc_request)
            .await
            .map_err(map_grpc_status)?;
        Ok(response.into_inner())
    }

    /// Execute a `ModelInfer` RPC, with optional caching.
    async fn model_infer(
        &self,
        request: triton_proto::ModelInferRequest,
    ) -> Result<triton_proto::ModelInferResponse, AnalysisError> {
        match &self.mode {
            ClientMode::Live { client } => Self::call_model_infer(client, request).await,
            ClientMode::Recording { client, cache_dir } => {
                let hash = Self::request_hash(&request);
                let inner = Self::call_model_infer(client, request).await?;
                Self::write_cache(cache_dir, &hash, &inner)?;
                Ok(inner)
            }
            ClientMode::Offline { cache_dir } => {
                let hash = Self::request_hash(&request);
                Self::read_cache(cache_dir, &hash)
            }
        }
    }

    /// Build a `ModelInferRequest` for a BYTES-typed input tensor.
    fn build_infer_request(
        model_name: &str,
        inputs: Vec<triton_proto::model_infer_request::InferInputTensor>,
        outputs: Vec<triton_proto::model_infer_request::InferRequestedOutputTensor>,
    ) -> triton_proto::ModelInferRequest {
        triton_proto::ModelInferRequest {
            model_name: model_name.to_string(),
            model_version: String::new(),
            id: String::new(),
            parameters: Default::default(),
            inputs,
            outputs,
            raw_input_contents: vec![],
        }
    }

    /// Create a BYTES input tensor with the given name and data.
    fn bytes_input(
        name: &str,
        data: Vec<u8>,
    ) -> triton_proto::model_infer_request::InferInputTensor {
        triton_proto::model_infer_request::InferInputTensor {
            name: name.to_string(),
            datatype: "BYTES".to_string(),
            shape: vec![1, 1],
            parameters: Default::default(),
            contents: Some(triton_proto::InferTensorContents {
                bytes_contents: vec![data],
                ..Default::default()
            }),
        }
    }

    /// Extract a named string output from the inference response.
    ///
    /// Triton can return BYTES data in two ways:
    /// 1. Inline in `output.contents.bytes_contents`
    /// 2. In `response.raw_output_contents[i]` (length-prefixed: 4-byte LE length + data)
    ///
    /// The 26.01+ Python backend typically uses `raw_output_contents`.
    fn extract_output(
        response: &triton_proto::ModelInferResponse,
        output_name: &str,
    ) -> Result<String, AnalysisError> {
        let (idx, output) = response
            .outputs
            .iter()
            .enumerate()
            .find(|(_, o)| o.name == output_name)
            .ok_or_else(|| {
                AnalysisError::ResponseParsing(format!("missing '{output_name}' output"))
            })?;

        // Try inline contents first.
        if let Some(contents) = output.contents.as_ref()
            && let Some(bytes) = contents.bytes_contents.first()
        {
            return std::str::from_utf8(bytes)
                .map(|s| s.to_string())
                .map_err(|e| {
                    AnalysisError::ResponseParsing(format!("invalid UTF-8 in output: {e}"))
                });
        }

        // Fall back to raw_output_contents (length-prefixed BYTES).
        let raw = response.raw_output_contents.get(idx).ok_or_else(|| {
            AnalysisError::ResponseParsing(format!(
                "'{output_name}' has no contents (inline or raw)"
            ))
        })?;

        if raw.len() < 4 {
            return Err(AnalysisError::ResponseParsing(format!(
                "'{output_name}' raw output too short ({} bytes)",
                raw.len()
            )));
        }

        let len = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
        let data = raw.get(4..4 + len).ok_or_else(|| {
            AnalysisError::ResponseParsing(format!(
                "'{output_name}' raw output truncated (expected {len} bytes, have {})",
                raw.len() - 4
            ))
        })?;

        std::str::from_utf8(data)
            .map(|s| s.to_string())
            .map_err(|e| AnalysisError::ResponseParsing(format!("invalid UTF-8 in output: {e}")))
    }
}

#[async_trait]
impl TritonService for GrpcTritonClient {
    async fn is_server_ready(&self) -> Result<(), AnalysisError> {
        match &self.mode {
            ClientMode::Live { client } | ClientMode::Recording { client, .. } => {
                let mut client = client.clone();
                let response = client
                    .server_ready(triton_proto::ServerReadyRequest {})
                    .await
                    .map_err(map_grpc_status)?;
                if response.into_inner().ready {
                    Ok(())
                } else {
                    Err(AnalysisError::Triton {
                        retriable: true,
                        message: "server not ready".to_string(),
                    })
                }
            }
            // Offline mode: always ready — fixture lookup happens on embed/analyze.
            ClientMode::Offline { .. } => Ok(()),
        }
    }

    async fn is_model_ready(&self, model_name: &str) -> Result<bool, AnalysisError> {
        match &self.mode {
            ClientMode::Live { client } | ClientMode::Recording { client, .. } => {
                let mut client = client.clone();
                let response = client
                    .model_ready(triton_proto::ModelReadyRequest {
                        name: model_name.to_string(),
                        version: String::new(),
                    })
                    .await
                    .map_err(map_grpc_status)?;
                Ok(response.into_inner().ready)
            }
            // Offline mode: always ready — fixture lookup happens on embed/analyze.
            ClientMode::Offline { .. } => Ok(true),
        }
    }

    async fn embed(&self, image: &[u8]) -> Result<Vec<f32>, AnalysisError> {
        let image_b64 = base64::engine::general_purpose::STANDARD.encode(image);

        let request = Self::build_infer_request(
            "dinov3",
            vec![Self::bytes_input("images", image_b64.into_bytes())],
            vec![
                triton_proto::model_infer_request::InferRequestedOutputTensor {
                    name: "embeddings".to_string(),
                    parameters: Default::default(),
                },
            ],
        );

        let response = self.model_infer(request).await?;
        let result_str = Self::extract_output(&response, "embeddings")?;

        serde_json::from_str(&result_str)
            .map_err(|e| AnalysisError::ResponseParsing(format!("invalid embedding JSON: {e}")))
    }

    async fn analyze(&self, image: &[u8]) -> Result<AnalysisResult, AnalysisError> {
        let schema = schemars::schema_for!(SubimageOutput);
        let schema_json = serde_json::to_string(&schema)
            .map_err(|e| AnalysisError::ResponseParsing(format!("schema serialization: {e}")))?;

        let image_b64 = base64::engine::general_purpose::STANDARD.encode(image);

        let request = Self::build_infer_request(
            "analysis",
            vec![
                Self::bytes_input("image", image_b64.into_bytes()),
                Self::bytes_input("schema", schema_json.into_bytes()),
            ],
            vec![
                triton_proto::model_infer_request::InferRequestedOutputTensor {
                    name: "result".to_string(),
                    parameters: Default::default(),
                },
            ],
        );

        let response = self.model_infer(request).await?;
        let result_str = Self::extract_output(&response, "result")?;

        serde_json::from_str(&result_str)
            .map_err(|e| AnalysisError::ResponseParsing(format!("invalid result JSON: {e}")))
    }
}

/// Map a gRPC status to an `AnalysisError`.
fn map_grpc_status(status: tonic::Status) -> AnalysisError {
    let retriable = matches!(
        status.code(),
        tonic::Code::Unavailable
            | tonic::Code::Internal
            | tonic::Code::ResourceExhausted
            | tonic::Code::DeadlineExceeded
            | tonic::Code::Aborted
            | tonic::Code::Unknown
    );

    AnalysisError::Triton {
        retriable,
        message: format!("{}: {}", status.code(), status.message()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triton_proto;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn test_request_hash_deterministic() {
        let request = triton_proto::ModelInferRequest {
            model_name: "test".to_string(),
            model_version: String::new(),
            id: String::new(),
            parameters: Default::default(),
            inputs: vec![],
            outputs: vec![],
            raw_input_contents: vec![],
        };

        let hash1 = GrpcTritonClient::request_hash(&request);
        let hash2 = GrpcTritonClient::request_hash(&request);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_request_hash_varies_with_model() {
        let request1 = triton_proto::ModelInferRequest {
            model_name: "model_a".to_string(),
            model_version: String::new(),
            id: String::new(),
            parameters: Default::default(),
            inputs: vec![],
            outputs: vec![],
            raw_input_contents: vec![],
        };
        let request2 = triton_proto::ModelInferRequest {
            model_name: "model_b".to_string(),
            model_version: String::new(),
            id: String::new(),
            parameters: Default::default(),
            inputs: vec![],
            outputs: vec![],
            raw_input_contents: vec![],
        };

        assert_ne!(
            GrpcTritonClient::request_hash(&request1),
            GrpcTritonClient::request_hash(&request2),
        );
    }

    #[test]
    fn test_request_hash_varies_with_input_data() {
        let request1 = GrpcTritonClient::build_infer_request(
            "same_model",
            vec![GrpcTritonClient::bytes_input("image", b"image_a".to_vec())],
            vec![],
        );
        let request2 = GrpcTritonClient::build_infer_request(
            "same_model",
            vec![GrpcTritonClient::bytes_input("image", b"image_b".to_vec())],
            vec![],
        );

        assert_ne!(
            GrpcTritonClient::request_hash(&request1),
            GrpcTritonClient::request_hash(&request2),
            "different input data should produce different hashes"
        );
    }

    #[test]
    fn test_bytes_input_construction() -> TestResult {
        let input = GrpcTritonClient::bytes_input("test_input", b"hello".to_vec());

        assert_eq!(input.name, "test_input");
        assert_eq!(input.datatype, "BYTES");
        assert_eq!(input.shape, vec![1, 1]);
        let contents = input.contents.as_ref().ok_or("should have contents")?;
        assert_eq!(contents.bytes_contents.len(), 1);
        assert_eq!(contents.bytes_contents[0], b"hello");
        Ok(())
    }

    #[test]
    fn test_extract_output_success() -> TestResult {
        let response = triton_proto::ModelInferResponse {
            model_name: "test".to_string(),
            model_version: "1".to_string(),
            id: String::new(),
            parameters: Default::default(),
            outputs: vec![triton_proto::model_infer_response::InferOutputTensor {
                name: "result".to_string(),
                datatype: "BYTES".to_string(),
                shape: vec![1, 1],
                parameters: Default::default(),
                contents: Some(triton_proto::InferTensorContents {
                    bytes_contents: vec![b"hello world".to_vec()],
                    ..Default::default()
                }),
            }],
            raw_output_contents: vec![],
        };

        let output = GrpcTritonClient::extract_output(&response, "result")?;
        assert_eq!(output, "hello world");
        Ok(())
    }

    #[test]
    fn test_extract_output_missing() {
        let response = triton_proto::ModelInferResponse {
            model_name: "test".to_string(),
            model_version: "1".to_string(),
            id: String::new(),
            parameters: Default::default(),
            outputs: vec![],
            raw_output_contents: vec![],
        };

        let result = GrpcTritonClient::extract_output(&response, "result");
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_output_empty_bytes_contents() -> TestResult {
        let response = triton_proto::ModelInferResponse {
            model_name: "test".to_string(),
            model_version: "1".to_string(),
            id: String::new(),
            parameters: Default::default(),
            outputs: vec![triton_proto::model_infer_response::InferOutputTensor {
                name: "result".to_string(),
                datatype: "BYTES".to_string(),
                shape: vec![1, 1],
                parameters: Default::default(),
                contents: Some(triton_proto::InferTensorContents {
                    bytes_contents: vec![],
                    ..Default::default()
                }),
            }],
            raw_output_contents: vec![],
        };

        let result = GrpcTritonClient::extract_output(&response, "result");
        let Err(err) = result else {
            return Err("empty bytes_contents should fail".into());
        };
        assert!(
            matches!(err, AnalysisError::ResponseParsing(ref msg) if msg.contains("no contents")),
            "error should mention no contents: {err}"
        );
        Ok(())
    }

    #[test]
    fn test_extract_output_invalid_utf8() -> TestResult {
        let response = triton_proto::ModelInferResponse {
            model_name: "test".to_string(),
            model_version: "1".to_string(),
            id: String::new(),
            parameters: Default::default(),
            outputs: vec![triton_proto::model_infer_response::InferOutputTensor {
                name: "result".to_string(),
                datatype: "BYTES".to_string(),
                shape: vec![1, 1],
                parameters: Default::default(),
                contents: Some(triton_proto::InferTensorContents {
                    bytes_contents: vec![vec![0xFF, 0xFE, 0xFD]],
                    ..Default::default()
                }),
            }],
            raw_output_contents: vec![],
        };

        let result = GrpcTritonClient::extract_output(&response, "result");
        let Err(err) = result else {
            return Err("non-UTF-8 bytes should fail".into());
        };
        assert!(
            matches!(err, AnalysisError::ResponseParsing(ref msg) if msg.contains("UTF-8")),
            "error should mention UTF-8: {err}"
        );
        Ok(())
    }

    #[test]
    fn test_grpc_status_mapping_retriable() {
        for code in [
            tonic::Code::Unavailable,
            tonic::Code::Internal,
            tonic::Code::ResourceExhausted,
            tonic::Code::DeadlineExceeded,
            tonic::Code::Aborted,
            tonic::Code::Unknown,
        ] {
            let status = tonic::Status::new(code, "test");
            let err = map_grpc_status(status);
            assert!(err.is_retriable(), "{code:?} should be retriable");
        }
    }

    #[test]
    fn test_grpc_status_mapping_permanent() {
        for code in [
            tonic::Code::InvalidArgument,
            tonic::Code::NotFound,
            tonic::Code::Unimplemented,
            tonic::Code::PermissionDenied,
        ] {
            let status = tonic::Status::new(code, "test");
            let err = map_grpc_status(status);
            assert!(!err.is_retriable(), "{code:?} should be permanent");
        }
    }

    #[test]
    fn test_offline_fixture_roundtrip() -> TestResult {
        let dir = tempfile::tempdir()?;
        let response = triton_proto::ModelInferResponse {
            model_name: "test_model".to_string(),
            model_version: "1".to_string(),
            id: String::new(),
            parameters: Default::default(),
            outputs: vec![triton_proto::model_infer_response::InferOutputTensor {
                name: "result".to_string(),
                datatype: "BYTES".to_string(),
                shape: vec![1, 1],
                parameters: Default::default(),
                contents: Some(triton_proto::InferTensorContents {
                    bytes_contents: vec![b"test data".to_vec()],
                    ..Default::default()
                }),
            }],
            raw_output_contents: vec![],
        };

        let hash = "abc123";
        GrpcTritonClient::write_cache(dir.path(), hash, &response)?;

        let loaded = GrpcTritonClient::read_cache(dir.path(), hash)?;

        assert_eq!(loaded.model_name, "test_model");
        assert_eq!(loaded.outputs.len(), 1);
        let contents = loaded.outputs[0]
            .contents
            .as_ref()
            .ok_or("should have contents")?;
        assert_eq!(contents.bytes_contents[0], b"test data");
        Ok(())
    }

    #[test]
    fn test_offline_fixture_missing() -> TestResult {
        let dir = tempfile::tempdir()?;
        let result = GrpcTritonClient::read_cache(dir.path(), "nonexistent");
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn test_write_cache_invalid_path() {
        let response = triton_proto::ModelInferResponse {
            model_name: "test".to_string(),
            model_version: "1".to_string(),
            id: String::new(),
            parameters: Default::default(),
            outputs: vec![],
            raw_output_contents: vec![],
        };

        // Writing to /dev/null/impossible should fail (can't create dirs under /dev/null)
        let result =
            GrpcTritonClient::write_cache(Path::new("/dev/null/impossible"), "hash", &response);
        assert!(result.is_err(), "writing to invalid path should fail");
    }

    #[tokio::test]
    async fn test_offline_health_checks() -> TestResult {
        let dir = tempfile::tempdir()?;
        let client = GrpcTritonClient::offline(dir.path().to_path_buf());

        client.is_server_ready().await?;
        assert!(client.is_model_ready("any_model").await?);
        Ok(())
    }
}
