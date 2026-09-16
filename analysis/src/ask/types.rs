//! The ask schemas: the shapes Qwen emits under constrained decode.
//!
//! Composite detection ([`CompositeOutcome`]) localizes a picture's panels as
//! boxes ([`Rect`] over [`Point`]) in the model's own 0-1000 coordinate
//! convention, which [`super::convert`] reads into core's proportional geometry.
//!
//! The per-subimage read is a small family of image-first calls. An image-level
//! gate ([`ImageOutcome`]) judges relevance and, when relevant, the image's
//! [`RelevantMedium`] — which carries the [`Perspective`] viewpoint only for a
//! photograph, since a map has none. With regions to read, a per-entity call
//! names each numbered Set-of-Mark region ([`EntityReading`]); with none, a
//! recall backstop ([`TriageOutcome`]) says whether the detector missed a real
//! structure. The leaf vocabulary is core's [`Text`] and [`Perspective`], reused
//! directly since the ask and core share the workspace schemars.

use chronoscope_core::grammar::depiction::Perspective;
use chronoscope_core::grammar::text::Text;
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

/// The image gate: whether a subimage belongs in the pipeline and, when it does,
/// how to read it. Always the first call over a subimage, on the raw image alone.
///
/// Relevance is a nuanced judgment, not "the segmenter found a region": a real
/// structure behind unsuitable content, or a scale model, is irrelevant despite a
/// region, so an `Irrelevant` verdict short-circuits the read regardless of region
/// count. `Relevant` carries the image's [`RelevantMedium`], which pins the
/// viewpoint onto a photograph and leaves the abstract media without one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "_type", rename_all = "snake_case")]
pub enum ImageOutcome {
    /// The image carries nothing the pipeline should read.
    Irrelevant {
        /// Why the image was set aside.
        reason: Text,
    },
    /// The image is worth reading, classified by its medium (which carries the
    /// viewpoint when it is a photograph).
    Relevant {
        /// What kind of image this is, and — for a photograph — its viewpoint.
        medium: RelevantMedium,
    },
}

/// The medium of a relevant image, carrying a viewpoint only where one applies.
///
/// A photograph is taken from a viewpoint (exterior or interior); a map or plan
/// is not, so `view` rides on [`Self::Picture`] alone and "an exterior map"
/// cannot be spelled. A pictorial map depicts buildings pictorially, so the
/// region flow reads it like a picture — but it still has no single viewpoint, so
/// it carries none. This is the ask-layer counterpart to core's flat
/// `ImageMedium` hint; the two differ deliberately (one gates a viewpoint, the
/// other does not), and the fact-emission mapping bridges them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "_type", rename_all = "snake_case")]
pub enum RelevantMedium {
    /// A photograph, taken from outside a structure or from within one.
    Picture {
        /// The image-level viewpoint: an exterior scene, or an interior subject.
        view: Perspective,
    },
    /// A pictorial map: buildings drawn pictorially, segmentable like a picture.
    PictorialMap,
    /// A cartographic map — an orthographic representation of terrain.
    Map,
    /// An orthographic plan of a structure's own layout.
    Plan,
}

/// One numbered mark's reading: what the marked object is, in the model's words.
///
/// The mark number is the loop index the pipeline drives, so it rides the trigger
/// rather than the schema; the reading carries only the description.
///
/// The field is `description_from_raw_image`, not `description`, on purpose: under
/// constrained JSON decode the model is forced to emit the key before its value,
/// so the key doubles as a just-in-time reminder — at the moment of generation — to
/// read the object's true appearance from the first (unmarked) image rather than
/// the marked overlay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntityReading {
    /// What the numbered object truly is, read from the unmarked image.
    pub description_from_raw_image: Text,
}

/// The recall backstop over a subimage the segmenter marked nothing in: whether a
/// real structure was there to mark after all.
///
/// Relevance, medium, and view are the gate's job, so triage only reports a missed
/// structure or its absence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "_type", rename_all = "snake_case")]
pub enum TriageOutcome {
    /// A real built structure the detector failed to mark.
    MissedStructures {
        /// What the detector missed.
        description: Text,
    },
    /// Nothing the detector should have marked.
    NothingHere,
}
