//! The assembled request the VLM ask answers: fixed framing, the images, and the
//! trigger after them.
//!
//! The split is what lets the fixed framing lead as a system message while the
//! images sit in the user turn. `Qwen3::ask` renders the `preamble` as the system
//! prompt (Qwen's template puts a system message ahead of the image tokens) and
//! appends the schema text to it from the same type it constrains, so the framing
//! is a byte-stable prefix across a batch that shares it. The `postamble` is the
//! user-turn text after the images: the trigger to answer. That ordering holds
//! because mistral.rs's `add_image_message` emits the image parts and then the
//! text, so the postamble genuinely follows the images rather than being assumed
//! to.

use image::DynamicImage;

/// A VLM request: the system framing, the images, and the trigger after them.
pub struct Prompt {
    /// The fixed instruction and coordinate convention. `Qwen3::ask` renders this
    /// as the system message, with the schema text appended, ahead of the images.
    pub preamble: String,
    /// The images the request is about, carried in the user turn.
    pub images: Vec<DynamicImage>,
    /// The user-turn text after the images: the trigger to answer.
    pub postamble: String,
}
