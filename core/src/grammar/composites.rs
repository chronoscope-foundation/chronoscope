//! Composite cluster — composite-image structural facts.
//!
//! Cluster module for the `Composite` variant of
//! [`crate::grammar::assertions::JudgmentAssertion`]. A composite image (a
//! stacked before-and-after, a multi-up grid, an inset thumbnail overlaid on a
//! wider view) is one image whose pixel area decomposes into sub-rectangles,
//! each conceptually a different photo with its own capture date, citations,
//! and depictions. The subimages are first-class images with their own image
//! ids; the link from a subimage to its parent travels through
//! [`Fact::IsSubimageOf`], which carries a [`SubimageRegion`].
//!
//! Whether an image is a composite is derivable rather than marker-flagged: an
//! image is a composite iff at least one `IsSubimageOf` fact names it as the
//! parent.
//!
//! # Error states (rejected at submit time)
//!
//! - **Chained subimages.** An image that is a subimage of another can't itself
//!   be the parent of a third — the structure is one layer deep by design. The
//!   submit layer rejects facts that would form a chain.
//! - **Multiple parents.** A subimage has at most one parent. Two `IsSubimageOf`
//!   facts with the same `subimage` and different `parent` are malformed, not a
//!   conflict.
//! - **Self-parent.** `subimage == parent` is malformed.
//!
//! # Conflicts (surfaced at projection time)
//!
//! - **Region disagreement.** Two `IsSubimageOf` facts agreeing on `subimage`
//!   and `parent` but disagreeing on `region` are a conflict the solver
//!   surfaces. Sources may disagree on the exact crop bounds even when they
//!   agree the sub-image exists.

use chronoscope_macros::grammar_type;

use crate::grammar::geometry::{ProportionalCoordError, ProportionalRect};

/// Composite-cluster fact.
///
/// Generic over the image reference type `ImgId`.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(bound(
    serialize = "ImgId: ::serde::Serialize",
    deserialize = "ImgId: ::serde::de::DeserializeOwned"
))]
#[schemars(bound = "ImgId: ::schemars::JsonSchema")]
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

impl<ImgId> Fact<ImgId> {
    /// Visit every image id this fact mentions.
    ///
    /// `IsSubimageOf` carries two image ids (`subimage` then `parent`); both
    /// go to the same closure, in field order. The no-self-parent invariant
    /// is a submit-layer rule, not a structural pair constraint.
    pub fn for_each_id(&self, fi: &mut impl FnMut(&ImgId)) {
        match self {
            Self::IsSubimageOf {
                subimage, parent, ..
            } => {
                fi(subimage);
                fi(parent);
            }
        }
    }

    /// Relabel every image id through the fallible closure, producing a
    /// `Fact<I2>`.
    pub fn try_map_ids<I2, Err>(
        &self,
        fi: &mut impl FnMut(&ImgId) -> Result<I2, Err>,
    ) -> Result<Fact<I2>, Err> {
        match self {
            Self::IsSubimageOf {
                subimage,
                parent,
                region,
            } => Ok(Fact::IsSubimageOf {
                subimage: fi(subimage)?,
                parent: fi(parent)?,
                region: *region,
            }),
        }
    }
}

/// The region of a parent image occupied by a subimage.
///
/// Coordinates are *proportional* (in `0.0..=1.0`) rather than pixel-based
/// so the same region applies across all perceptually-equivalent copies
/// of the parent — a higher-resolution rescan carries the same fractional
/// bounds. The smart constructor [`SubimageRegion::rect`] is the
/// sanctioned construction path; deserialization routes through
/// [`ProportionalRect`] so out-of-range coordinates fail at the wire boundary.
#[grammar_type]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SubimageRegion {
    /// An axis-aligned rectangle in proportional parent-image coordinates.
    Rect {
        /// The proportional rectangle bounding the subimage in its parent.
        rect: ProportionalRect,
    },
}

impl SubimageRegion {
    /// Construct a rectangular subimage region. See
    /// [`ProportionalRect::new`] for the validation rules.
    pub fn rect(ax: f32, ay: f32, bx: f32, by: f32) -> Result<Self, ProportionalCoordError> {
        Ok(Self::Rect {
            rect: ProportionalRect::new(ax, ay, bx, by)?,
        })
    }
}
