//! Chronoscope analysis pipeline.
//!
//! Server-side SAM3 + VLM analysis via Triton Inference Server.
//!
//! # Architecture
//!
//! The analysis pipeline runs on a GPU server (Northflank) using Triton:
//!
//! ```text
//! ┌─────────────────┐                        ┌─────────────────────────────┐
//! │  Local Machine  │                        │  Northflank (GPU)           │
//! │                 │   northflank forward   │                             │
//! │  ┌───────────┐  │   (secure tunnel)      │  ┌───────────────────────┐  │
//! │  │ Rust CLI  │  │ ─────────────────────► │  │ Triton (private svc)  │  │
//! │  │ analyze   │  │   localhost:8080       │  │                       │  │
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
//! use chronoscope_analysis::{TritonClient, AnalysisResult};
//! use url::Url;
//!
//! let client = TritonClient::new(Url::parse("http://localhost:8080")?);
//!
//! // Check server health
//! if client.is_server_ready().await? {
//!     let image_bytes = std::fs::read("image.jpg")?;
//!     let result: AnalysisResult = client.analyze(&image_bytes).await?;
//!     println!("Relevant: {}", result.vlm.is_relevant);
//! }
//! ```

#![deny(clippy::unwrap_used)]
#![deny(unsafe_code)]

pub mod client;
pub mod error;
pub mod schema;

// Re-export main types
pub use client::TritonClient;
pub use error::AnalysisError;
pub use schema::{
    AnalysisRequest, AnalysisResult, AnalyzedMediaType, CompositeInfo, DetectedRegion,
    EntityType, ExtractedText, RegionAnalysis, RegionEntry, RegionRelationship, RelationType,
    RleMask, SceneType, VlmAnalysis, VlmOutput,
};
