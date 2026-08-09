//! Chronoscope image analysis.
//!
//! DINOv3 runs here, over the ONNX graphs `nix/vision.nix` exports. SAM 3 and
//! a VLM through mistral.rs are not here yet.
//!
//! The corpus — the pinned development image set the pipeline is built
//! against — sits behind the `corpus` feature, since its downloader runs from
//! Nix.

#[cfg(feature = "corpus")]
pub mod corpus;
pub mod dinov3;
mod manifest;
pub mod onnx;
