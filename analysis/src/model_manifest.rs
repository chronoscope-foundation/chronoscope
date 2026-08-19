//! The one manifest fact every exported graph shares: its input signature, as
//! `nix/scripts/verify-onnx.py` reads it off the real ONNX session.

use serde::Deserialize;

/// One graph input as the ONNX session reports it: its name, its element type in
/// ONNX Runtime's spelling (`tensor(uint8)`, `tensor(float)`), and its shape.
///
/// Only the two fully-static driven inputs (`image` on both models) are parsed
/// further, into the resolution and dtype a caller feeds. A catalog graph with a
/// symbolic axis (the interactive decoder's `point_coords`) carries that axis
/// here as a non-numeric dim and is never read past this struct.
#[derive(Debug, Deserialize)]
pub(crate) struct Signature {
    pub(crate) name: String,
    pub(crate) dtype: String,
    pub(crate) shape: Vec<i64>,
}
