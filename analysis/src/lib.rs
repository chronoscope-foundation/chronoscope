//! Chronoscope analysis pipeline.
//!
//! Server-side SAM3 + VLM analysis via Triton Inference Server.
//!
//! # Architecture
//!
//! The analysis pipeline runs on a GPU server using Triton, accessed via gRPC:
//!
//! ```text
//! ┌─────────────────┐                        ┌─────────────────────────────┐
//! │  Local Machine  │                        │  GPU Server                 │
//! │                 │   gRPC (HTTP/2)        │                             │
//! │  ┌───────────┐  │   (persistent conn)    │  ┌───────────────────────┐  │
//! │  │ Rust CLI  │  │ ─────────────────────► │  │ Triton (gRPC :8001)   │  │
//! │  │ analyze   │  │                        │  │                       │  │
//! │  └───────────┘  │                        │  │  analysis (BLS)       │  │
//! └─────────────────┘                        │  │    ├─► sam3 (Python)  │  │
//!                                            │  │    └─► vlm (vLLM)     │  │
//!                                            │  └───────────────────────┘  │
//!                                            └─────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use chronoscope_analysis::{GrpcTritonClient, TritonService, AnalysisResult};
//!
//! let client = GrpcTritonClient::connect("http://localhost:8001").await?;
//!
//! // Check server health
//! client.is_server_ready().await?;
//! let image_bytes = std::fs::read("image.jpg")?;
//! let result: AnalysisResult = client.analyze(&image_bytes).await?;
//! println!("Subimages: {}", result.subimages.len());
//! ```

#![deny(clippy::unwrap_used)]
#![deny(unsafe_code)]

pub mod error;
pub mod grpc;
#[cfg(any(test, feature = "testing"))]
pub mod mock;
pub mod schema;
pub mod service;

#[cfg(feature = "corpus-test")]
pub mod corpus;

/// Generated protobuf types for the Triton gRPC API.
#[allow(clippy::enum_variant_names)]
pub(crate) mod triton_proto {
    tonic::include_proto!("inference");
}

// Re-export main types
pub use error::AnalysisError;
pub use grpc::GrpcTritonClient;
pub use schema::{
    AnalysisRequest, AnalysisResult, AnalyzedMediaType, BoundingBox, ConditionIndicator,
    EntityType, ExtractedText, FacadeMaterial, ModelVersions, PhotoColor, Region, RegionAnalysis,
    RegionIndex, RegionObservations, RegionRelation, RelationType, RleMask, RoadSurface, RoofType,
    SceneAnalysis, SceneObservations, SceneType, StreetInfrastructure, StructuralElement, Subimage,
    SubimageAnalysis, SubimageBounds, SurroundingType, Surroundings, VehicleType, WindowShape,
};
pub use service::TritonService;
