//! Chronoscope image analysis.
//!
//! DINOv3 and SAM 3 run here, over the ONNX graphs `nix/vision.nix` exports. A
//! VLM through mistral.rs is not here yet.
//!
//! The corpus — the pinned development image set the pipeline is built
//! against — sits behind the `corpus` feature, since its downloader runs from
//! Nix.

#[cfg(feature = "corpus")]
pub mod corpus;
pub mod dinov3;
pub mod onnx;
mod preprocess;
pub mod sam3;
