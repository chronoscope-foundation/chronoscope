//! Geometry primitives referenced by depiction and feature facts.
//!
//! [`RleMask`] is a run-length-encoded binary mask in the COCO-compressed ASCII
//! format produced by `pycocotools` and SAM-family segmentation models.
//!
//! [`ProportionalRect`] is an axis-aligned rectangle in `0.0..=1.0`
//! proportional coordinates, shared between [`ImageRegion::BBox`] and
//! [`crate::facts::composites::SubimageRegion::Rect`] — same validation, same
//! wire shape.
//!
//! [`ImageRegion`] is the sum of pixel-precise masks and axis-aligned
//! proportional bounding boxes, anchored to the parent image. Both forms arrive
//! from real pipelines — SAM-family models emit RLE masks, VLMs that produce
//! normalized bbox output (Gemma 3, Florence-2) emit bboxes. The mask form's
//! pixel grid equals the parent image's; the bbox form uses `0.0..=1.0`
//! proportional coordinates, resolution-independent.
//!
//! [`SpatialGeometry`] is the sum of [`ImageRegion`] and
//! [`crate::geo::Polyline`], attached to map-bearing depiction facts where
//! either shape may show up depending on the medium.

use std::cmp::Ordering;

use chronoscope_macros::grammar_type;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geo::Polyline;

/// Run-length-encoded binary mask in the COCO compressed-string format.
///
/// `counts` holds compressed run lengths as ASCII bytes (modified LEB128
/// with a +48 offset). Runs alternate background and foreground, starting
/// with background. Consumers that need an expanded binary mask
/// round-trip via the upstream decoder.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RleMask {
    /// Compressed run-length bytes, COCO format.
    pub counts: String,
}

/// An axis-aligned proportional rectangle in `0.0..=1.0` coordinates.
///
/// The shared rect payload of [`ImageRegion::BBox`] and
/// [`crate::facts::composites::SubimageRegion::Rect`]. The smart constructor
/// [`ProportionalRect::new`] rejects non-finite coordinates, negative origins,
/// non-positive extents, and rectangles that extend past the unit square.
/// Deserialization routes through it so invalid shapes fail at the wire
/// boundary.
///
/// `Eq` is hand-implemented despite the `f32` fields: the constructor rejects
/// NaN/Inf and normalizes `-0.0` to `+0.0`, so the manual `Hash` (via
/// `f32::to_bits`) and `Ord` (via `f32::total_cmp`) stay consistent with the
/// derived `PartialEq` (under which `-0.0 == +0.0`). Every constructed value is
/// finite and free of negative zero, so equality is honest.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, JsonSchema)]
pub struct ProportionalRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl Eq for ProportionalRect {}

impl std::hash::Hash for ProportionalRect {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.x.to_bits().hash(state);
        self.y.to_bits().hash(state);
        self.width.to_bits().hash(state);
        self.height.to_bits().hash(state);
    }
}

// Manual Ord via `total_cmp` field-by-field. The smart constructor rejects
// NaN/Inf, so every constructed value compares cleanly. Field order mirrors the
// struct layout (`x`, `y`, `width`, `height`); it feeds content-address
// determinism, so changing it changes every CommitId.
impl PartialOrd for ProportionalRect {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for ProportionalRect {
    fn cmp(&self, other: &Self) -> Ordering {
        self.x
            .total_cmp(&other.x)
            .then_with(|| self.y.total_cmp(&other.y))
            .then_with(|| self.width.total_cmp(&other.width))
            .then_with(|| self.height.total_cmp(&other.height))
    }
}

impl ProportionalRect {
    /// Construct a proportional rect. Bounds must satisfy
    /// `0.0 <= x`, `0.0 <= y`, `width > 0`, `height > 0`,
    /// `x + width <= 1.0`, and `y + height <= 1.0`, with all four
    /// components finite.
    ///
    /// `-0.0` is normalized to `+0.0` so the manual `Hash`/`Ord`
    /// (bit-level / `total_cmp`) stay consistent with the derived
    /// `PartialEq`.
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Result<Self, ProportionalRectError> {
        if !(x.is_finite() && y.is_finite() && width.is_finite() && height.is_finite()) {
            return Err(ProportionalRectError::NotFinite);
        }
        if x < 0.0 || y < 0.0 {
            return Err(ProportionalRectError::NegativeOrigin { x, y });
        }
        if width <= 0.0 || height <= 0.0 {
            return Err(ProportionalRectError::NonPositiveExtent { width, height });
        }
        if x + width > 1.0 || y + height > 1.0 {
            return Err(ProportionalRectError::OutOfBounds {
                x,
                y,
                width,
                height,
            });
        }
        // Normalize `-0.0` to `+0.0`: `-0.0 + 0.0 == +0.0`, and adding
        // `0.0` is a no-op for every other finite value. Only `x`/`y` can
        // actually carry `-0.0` (extents <= 0.0 are already rejected), but
        // normalize all four uniformly.
        let x = x + 0.0;
        let y = y + 0.0;
        let width = width + 0.0;
        let height = height + 0.0;
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }

    /// Horizontal origin in `0.0..=1.0`.
    pub fn x(&self) -> f32 {
        self.x
    }

    /// Vertical origin in `0.0..=1.0`.
    pub fn y(&self) -> f32 {
        self.y
    }

    /// Width in proportional units; positive, with `x + width <= 1.0`.
    pub fn width(&self) -> f32 {
        self.width
    }

    /// Height in proportional units; positive, with `y + height <= 1.0`.
    pub fn height(&self) -> f32 {
        self.height
    }
}

impl<'de> Deserialize<'de> for ProportionalRect {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            x: f32,
            y: f32,
            width: f32,
            height: f32,
        }
        let w = Wire::deserialize(deserializer)?;
        Self::new(w.x, w.y, w.width, w.height).map_err(serde::de::Error::custom)
    }
}

/// A region within an image, anchored to its parent image.
///
/// Two forms map to real pipelines: pixel-precise masks (from segmentation
/// models) and axis-aligned proportional bounding boxes (from VLMs that output
/// normalized boxes). The mask form's pixel grid equals the parent image's
/// dimensions; the bbox form uses `0.0..=1.0` proportional coordinates,
/// resolution-independent.
///
/// Construct bbox via the smart constructor [`ImageRegion::bbox`];
/// deserialization routes through [`ProportionalRect`]'s wire boundary so
/// out-of-range bboxes fail at parse time.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ImageRegion {
    /// Pixel-precise binary mask whose grid matches the parent image's.
    Mask {
        /// The run-length-encoded binary mask.
        mask: RleMask,
    },
    /// Axis-aligned proportional bounding box in `0.0..=1.0`
    /// coordinates. Resolution-independent — survives rescans and
    /// downsamples of the parent image.
    #[serde(rename = "bbox")]
    BBox {
        /// The proportional bounding rectangle.
        rect: ProportionalRect,
    },
}

impl ImageRegion {
    /// Construct an axis-aligned proportional bbox. See
    /// [`ProportionalRect::new`] for the validation rules.
    pub fn bbox(x: f32, y: f32, width: f32, height: f32) -> Result<Self, ProportionalRectError> {
        Ok(Self::BBox {
            rect: ProportionalRect::new(x, y, width, height)?,
        })
    }
}

/// Errors from [`ProportionalRect::new`].
///
/// `PartialOrd` only — the error variants carry pre-validation `f32`
/// values that may be `NaN` or infinite, so total ordering would be
/// dishonest. Errors aren't part of the content-addressed-fact graph,
/// so missing `Ord` here doesn't constrain anything downstream.
#[derive(Debug, Clone, PartialEq)]
pub enum ProportionalRectError {
    /// One or more coordinates were not finite (`NaN` or infinite).
    NotFinite,
    /// The origin had a negative component; the origin must lie in
    /// `0.0..=1.0`.
    NegativeOrigin { x: f32, y: f32 },
    /// The width or height was zero or negative.
    NonPositiveExtent { width: f32, height: f32 },
    /// The rectangle extended beyond the unit square's `0.0..=1.0` range.
    OutOfBounds {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
}

impl std::fmt::Display for ProportionalRectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFinite => write!(f, "proportional rect coordinates must be finite"),
            Self::NegativeOrigin { x, y } => write!(
                f,
                "proportional rect origin must be non-negative, got (x={x}, y={y})"
            ),
            Self::NonPositiveExtent { width, height } => write!(
                f,
                "proportional rect extent must be positive, got width={width}, height={height}"
            ),
            Self::OutOfBounds {
                x,
                y,
                width,
                height,
            } => write!(
                f,
                "proportional rect exceeds unit-square bounds: x={x}, y={y}, width={width}, height={height}"
            ),
        }
    }
}

impl std::error::Error for ProportionalRectError {}

/// Spatial geometry attached to a map-bearing depiction fact.
///
/// Either an image region (mask or bbox) or a polyline trace.
/// Region is typical for areal features; polyline is typical for linear
/// features traced from the sheet.
///
/// Internal `"type"` tagging with the payload in a named field: the
/// `Region` payload is [`ImageRegion`], itself an internally-tagged enum
/// keyed on `type`. Nesting it under a named field
/// (`{"type":"region","region":{"type":"bbox",...}}`) keeps the inner
/// `type` key in a separate map, so the two tag keys never collide — the
/// duplicate-key failure a bare newtype payload would cause.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SpatialGeometry {
    /// Image region — mask or bbox.
    Region {
        /// The image region (mask or bbox).
        region: ImageRegion,
    },
    /// Polyline trace, for roads, paths, or boundaries.
    Polyline {
        /// The polyline trace.
        polyline: Polyline,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    // Origin coordinates that keep `-0.0` reachable: a plain `0.0..` float
    // strategy almost never samples exactly `-0.0`, so the bug this guards
    // against (negative-zero inconsistency between the derived `PartialEq` and
    // the manual `Hash`/`Ord`) would slip past. Mixing the two zero forms in
    // explicitly forces the law to exercise them.
    fn arb_origin() -> impl Strategy<Value = f32> {
        prop_oneof![
            4 => 0.0f32..0.5,
            1 => Just(-0.0f32),
            1 => Just(0.0f32),
        ]
    }

    fn arb_rect() -> impl Strategy<Value = ProportionalRect> {
        (arb_origin(), arb_origin(), 0.01f32..0.5, 0.01f32..0.5).prop_filter_map(
            "origin + extent must stay within the unit square",
            |(x, y, width, height)| ProportionalRect::new(x, y, width, height).ok(),
        )
    }

    fn hash_of(rect: &ProportionalRect) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        rect.hash(&mut hasher);
        hasher.finish()
    }

    proptest! {
        #[test]
        fn proportional_rect_eq_hash_ord_consistent_with_negative_zero(
            a in arb_rect(),
            b in arb_rect(),
        ) {
            // Whenever two rects compare equal, the manual `Hash` and `Ord`
            // must agree — the exact contract negative zero would otherwise
            // break (derived `PartialEq` folds `-0.0 == +0.0`, but
            // `to_bits`/`total_cmp` distinguish them).
            if a == b {
                prop_assert_eq!(hash_of(&a), hash_of(&b), "equal rects must hash equally");
                prop_assert_eq!(a.cmp(&b), Ordering::Equal, "equal rects must order Equal");
            }

            // The origin coordinates are the only place `-0.0` can enter
            // (extents <= 0.0 are rejected), so swapping `-0.0` for `+0.0`
            // there must be invisible across Eq, Hash, and Ord.
            let neg = ProportionalRect::new(-0.0, -0.0, a.width(), a.height())
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            let pos = ProportionalRect::new(0.0, 0.0, a.width(), a.height())
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            prop_assert_eq!(neg, pos, "negative and positive zero origins must be equal");
            prop_assert_eq!(hash_of(&neg), hash_of(&pos), "equal rects must hash equally");
            prop_assert_eq!(neg.cmp(&pos), Ordering::Equal, "equal rects must order Equal");
        }
    }

    #[test]
    fn image_region_bbox_rejects_out_of_bounds() {
        assert!(matches!(
            ImageRegion::bbox(0.6, 0.0, 0.5, 0.5),
            Err(ProportionalRectError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn image_region_bbox_rejects_zero_extent() {
        assert!(matches!(
            ImageRegion::bbox(0.0, 0.0, 0.0, 0.5),
            Err(ProportionalRectError::NonPositiveExtent { .. })
        ));
    }

    #[test]
    fn image_region_bbox_rejects_nan() {
        assert!(matches!(
            ImageRegion::bbox(f32::NAN, 0.0, 0.5, 0.5),
            Err(ProportionalRectError::NotFinite)
        ));
    }

    #[test]
    fn image_region_deserialize_validates_bbox() {
        // The bbox payload nests under the named `rect` field; the
        // out-of-bounds coordinates must still fail at the
        // `ProportionalRect` wire boundary.
        let result: Result<ImageRegion, _> = serde_json::from_str(
            r#"{"type":"bbox","rect":{"x":0.6,"y":0.0,"width":0.5,"height":0.5}}"#,
        );
        assert!(
            result.is_err(),
            "out-of-bounds bbox must fail at deserialize"
        );
    }

    #[test]
    fn image_region_bbox_round_trips_preserving_all_components() -> TestResult {
        // Catches a manual-Deserialize Raw → smart-ctor wiring bug that
        // dropped or reordered any of {x, y, width, height}.
        let region = ImageRegion::bbox(0.1, 0.2, 0.3, 0.4)?;
        let json = serde_json::to_string(&region)?;
        let parsed: ImageRegion = serde_json::from_str(&json)?;
        assert_eq!(parsed, region);
        Ok(())
    }
}
