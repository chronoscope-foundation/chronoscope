//! Chronoscope image analysis.
//!
//! DINOv3 and SAM 3 run here over the ONNX graphs `nix/analysis.nix` exports;
//! Qwen 3.6 runs through mistral.rs (`qwen3`), the pipeline's VLM.
//!
//! The corpus — the pinned development image set the pipeline is built
//! against — sits behind the `corpus` feature, since its downloader runs from
//! Nix.

#[cfg(feature = "corpus")]
pub mod corpus;
pub mod dinov3;
pub mod onnx;
mod preprocess;
pub mod qwen3;
pub mod sam3;
