//! Chronoscope image analysis.
//!
//! Currently only the corpus: the pinned development image set the pipeline
//! is built against, behind the `corpus` feature since the downloader runs
//! from Nix. The models it will run — SAM 3 and DINOv3 as ONNX graphs from
//! `nix/vision.nix`, a VLM through mistral.rs — are not here yet.

#![deny(clippy::unwrap_used)]
#![deny(unsafe_code)]

#[cfg(feature = "corpus")]
pub mod corpus;
