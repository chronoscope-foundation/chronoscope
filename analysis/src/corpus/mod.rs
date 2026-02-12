//! Corpus regression test suite for the analysis pipeline.
//!
//! Validates analysis results against a known-good set of test images,
//! catching regressions when models or pipeline code change.
//!
//! Gated behind the `corpus-test` feature — never runs in `just check`.
