//! Chronoscope image analysis.
//!
//! DINOv3 and SAM 3 run here over the ONNX graphs `nix/analysis.nix` exports;
//! Qwen 3.6 runs through mistral.rs (`qwen3`), the pipeline's VLM.
//!
//! The corpus is the pinned development image set the pipeline is built
//! against. Its manifest is the ground truth the orchestration test reads, so
//! it parses in every build; only the downloader, which the `corpus-fetch` FOD
//! binary runs from Nix, sits behind the `corpus` feature.

pub mod ask;
pub mod corpus;
pub mod dinov3;
mod geometry;
mod model_manifest;
pub mod onnx;
pub mod pipeline;
pub mod postprocess;
mod preprocess;
pub mod qwen3;
pub mod sam3;
pub mod scene;
pub mod setofmark;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
