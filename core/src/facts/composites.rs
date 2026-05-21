//! Composite cluster — composite-image structural facts.
//!
//! Cluster module for the `Composite` variant of
//! [`crate::facts::assertions::JudgmentAssertion`]. A composite image
//! (a stacked before-and-after, a multi-up grid, an inset thumbnail
//! overlaid on a wider view) is one image whose pixel area decomposes
//! into multiple sub-rectangles, each of which is conceptually a
//! different photo with its own capture date, citations, and
//! depictions. The subimages are first-class
//! [`crate::facts::ids::ImageId`]s; the link from a subimage to its
//! parent travels through [`Fact::IsSubimageOf`] which carries a
//! [`SubimageRegion`].
//!
//! The "is this image a composite?" question is derivable rather than
//! marker-flagged: an image is a composite iff at least one
//! `IsSubimageOf` fact names it as the parent.
//!
//! # Error states (rejected at submit time)
//!
//! - **Chained subimages.** An image that is a subimage of another
//!   cannot itself be the parent of a third — the structure is one
//!   layer deep by design. The submit layer rejects facts that would
//!   form a chain.
//! - **Multiple parents.** A subimage has at most one parent. Submitting
//!   two `IsSubimageOf` facts with the same `subimage` and different
//!   `parent` is a malformed claim, not a conflict.
//! - **Self-parent.** `subimage == parent` is malformed.
//!
//! # Conflicts (surfaced at projection time)
//!
//! - **Region disagreement.** Two `IsSubimageOf` facts agreeing on
//!   `subimage` and `parent` but disagreeing on `region` are a conflict
//!   the solver surfaces. Sources may disagree on the exact crop bounds
//!   even when they agree the sub-image exists.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::facts::geometry::{ProportionalRect, ProportionalRectError};

/// Composite-cluster fact.
///
/// Generic over the image reference type `ImgId`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(bound(
    serialize = "ImgId: Serialize",
    deserialize = "ImgId: serde::de::DeserializeOwned"
))]
#[schemars(bound = "ImgId: JsonSchema")]
pub enum Fact<ImgId> {
    /// One image is a sub-region of another (a composite's panel,
    /// inset, or grid cell). See the module docs for the submit-time
    /// invariants (no chains, one parent per subimage, no self-parent).
    IsSubimageOf {
        /// The smaller image — the panel or cell.
        subimage: ImgId,
        /// The parent image — the composite as a whole.
        parent: ImgId,
        /// The region of the parent occupied by the subimage.
        region: SubimageRegion,
    },
}

/// The region of a parent image occupied by a subimage.
///
/// Coordinates are *proportional* (in `0.0..=1.0`) rather than pixel-based
/// so the same region applies across all perceptually-equivalent copies
/// of the parent — a higher-resolution rescan carries the same fractional
/// bounds. The smart constructor [`SubimageRegion::rect`] is the
/// sanctioned construction path; deserialization routes through it via
/// [`ProportionalRect`]'s wire boundary so invalid bounds fail at the
/// wire boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SubimageRegion {
    /// An axis-aligned rectangle in proportional parent-image coordinates.
    Rect(ProportionalRect),
}

impl SubimageRegion {
    /// Construct a rectangular subimage region. See
    /// [`ProportionalRect::new`] for the validation rules.
    pub fn rect(x: f32, y: f32, width: f32, height: f32) -> Result<Self, ProportionalRectError> {
        Ok(Self::Rect(ProportionalRect::new(x, y, width, height)?))
    }
}
