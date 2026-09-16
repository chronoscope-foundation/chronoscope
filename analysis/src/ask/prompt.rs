//! The assembled request the VLM ask answers: the images, then the fixed framing,
//! the rendered schema, and the trigger after them.
//!
//! The read is image-first: everything up to and including the images — and the
//! fixed instruction plus the rendered schema — is byte-identical across a
//! subimage's calls, and only the trailing trigger varies. That shared leading
//! prefix is what mistral.rs's prefix cache keys on (leading-token match plus
//! image content-hash), so a later call reuses the earlier call's image prefill
//! and the vision encoder is skipped. `Qwen3::ask` places the images first in the
//! user turn and [`Prompt::user_text`] after them, because `add_image_message`
//! emits the image parts and then the text; the framing therefore genuinely
//! follows the images rather than leading as a system message.

use image::DynamicImage;

/// A VLM request: the images, the fixed framing shown after them, and the trigger.
pub struct Prompt {
    /// The fixed instruction and coordinate convention, shown after the images
    /// with the rendered schema appended. Identical across a subimage's calls, so
    /// it sits inside the byte-stable prefix the calls share.
    pub preamble: String,
    /// The images the request is about, submitted first in the user turn.
    pub images: Vec<DynamicImage>,
    /// The user-turn text after the framing: the trigger to answer. The one part
    /// that varies across a subimage's calls (e.g. which numbered entity to read).
    pub postamble: String,
}

impl Prompt {
    /// The user-turn text placed after the images: the fixed framing, the rendered
    /// schema, then the trigger, in that order.
    ///
    /// A newline closes the schema so the shared `[framing + schema]` region ends
    /// at a token boundary and tokenizes the same whatever trigger trails it,
    /// keeping the prefix the cache matches byte-identical across a subimage's
    /// calls. This is the seam the assembly test inspects; `Qwen3::ask` submits
    /// exactly this string after the images.
    pub fn user_text(&self, rendered_schema: &str) -> String {
        format!(
            "{}\n\n{}\n{}",
            self.preamble, rendered_schema, self.postamble
        )
    }
}
