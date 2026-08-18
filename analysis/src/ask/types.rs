//! The ask schema: the shape Qwen emits under constrained decode.
//!
//! The structural types (the outcome enums, [`Entity`]) are ask-local because a
//! VLM emits a flat box array in the model's own coordinate convention, not
//! core's nested proportional rect. The leaf vocabulary is core's: `media` is
//! [`ImageMedium`] and the free-text fields are [`Text`], reused directly since
//! the ask and core share the workspace schemars. [`super::convert`] bridges the
//! box back into core's geometry.

use chronoscope_core::grammar::{image::ImageMedium, text::Text};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One corner in the model's 0-1000 coordinate space, origin top-left, not core's
/// `[0, 1]` proportional space. [`super::convert`] bridges it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Point {
    /// Horizontal position: 0 at the left edge, 1000 at the right.
    pub x: f64,
    /// Vertical position: 0 at the top edge, 1000 at the bottom.
    pub y: f64,
}

/// A box as the model emits it: two labeled corners, so the schema states which
/// number is which rather than trusting the model to order an unlabeled array.
/// Coordinates are the model's 0-1000 convention; the reading into core geometry
/// lives in [`super::convert::bbox_to_proportional`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    /// The top-left corner.
    pub upper_left: Point,
    /// The bottom-right corner.
    pub lower_right: Point,
}

/// One entity the model localized: its box and a short description.
///
/// `box` is declared first so the model commits the box before any prose. `box`
/// is a Rust keyword, so the field is `bbox` renamed to the wire key `box`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Entity {
    /// Where the entity sits, in the model's coordinate convention.
    #[serde(rename = "box")]
    pub bbox: Rect,
    /// What the entity is, in the model's words.
    pub description: Text,
}

/// Pass 1: whether the image is a composite and, if so, its panels.
///
/// A tagged enum rather than `{ is_composite, panels }`, which would admit
/// `true` with no panels or `false` with panels. There is no `minItems` on
/// `panels`: once the model commits to `composite`, a minimum would force a
/// panel it may not see, so the ask stays permissive and
/// [`CompositeOutcome::panels`] collapses a sub-two-panel composite to a single
/// frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "_type", rename_all = "snake_case")]
pub enum CompositeOutcome {
    /// One frame; no subdivision.
    Single,
    /// Several panels, each a sub-rectangle of the frame in reading order.
    Composite {
        /// The panels, in the model's coordinate convention.
        panels: Vec<Rect>,
    },
}

impl CompositeOutcome {
    /// The panels to split the image into, or empty when it is a single frame.
    ///
    /// A `composite` with fewer than two panels is degenerate model output:
    /// splitting into one panel is the same as not splitting, so it collapses to
    /// single here rather than being rejected at the grammar.
    pub fn panels(&self) -> &[Rect] {
        match self {
            Self::Composite { panels } if panels.len() >= 2 => panels,
            Self::Single | Self::Composite { .. } => &[],
        }
    }
}

/// Pass 2: whether the (sub)image is relevant and, if so, its reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "_type", rename_all = "snake_case")]
pub enum RelevanceOutcome {
    /// The image is relevant: its medium, a summary, and the entities in it.
    Analyzed {
        /// The kind of image.
        media: ImageMedium,
        /// A short account of what the image shows.
        summary: Text,
        /// The entities the model localized.
        entities: Vec<Entity>,
    },
    /// The image is not relevant to the pipeline, with the model's reason.
    Irrelevant {
        /// Why the image was set aside.
        reason: Text,
    },
}
