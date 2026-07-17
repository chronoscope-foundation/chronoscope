//! Geometry primitives referenced by depiction and feature facts.
//!
//! [`RleMask`] is a run-length-encoded binary mask in the COCO-compressed ASCII
//! format produced by `pycocotools` and SAM-family segmentation models.
//!
//! [`ProportionalCoord`] is one validated `0.0..=1.0` coordinate component; it
//! holds the finite-check, range-check, and negative-zero normalization that
//! [`ProportionalRect`] and [`ProportionalPoint`] share, so that machinery
//! lives in one place.
//!
//! [`ProportionalRect`] is an axis-aligned rectangle in `0.0..=1.0`
//! proportional coordinates, given as two corner [`ProportionalPoint`]s in
//! canonical order. Shared between [`ImageGeometry::BBox`] and
//! [`crate::grammar::composites::SubimageRegion::Rect`] — same validation, same
//! wire shape.
//!
//! [`ProportionalPolyline`] is a validated trace in `0.0..=1.0` proportional
//! coordinates, the vector path-marking shape — a road or boundary traced on a
//! sheet in the image's own frame.
//!
//! [`ImageGeometry`] is the image-space localization sum a depiction carries —
//! a pixel-precise [`RleMask`], an axis-aligned [`ProportionalRect`] bbox, or a
//! [`ProportionalPolyline`] trace. Each form arrives from a real pipeline:
//! SAM-family models emit RLE masks, VLMs that produce normalized bbox output
//! (Gemma 3, Florence-2) emit bboxes, and a path-marking pass emits polylines.
//! All are anchored to the image's own pixel / proportional frame; a geographic
//! reading is a property of the image's projection, derived downstream.

use chronoscope_macros::grammar_type;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::finite::Finite;

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

/// One coordinate component in `0.0..=1.0` proportional image space — the
/// shared building block of [`ProportionalRect`] and [`ProportionalPoint`].
///
/// The inner value is a [`Finite`], which rejects `NaN`/`±∞` and normalizes
/// `-0.0` to `+0.0`. That is what lets `Eq`, `Hash`, and `Ord` derive and stay
/// consistent with `PartialEq` (under which `-0.0 == +0.0`), so every type built
/// from coordinates derives its own `Eq`/`Hash`/`Ord` through this one. The
/// smart constructor [`ProportionalCoord::new`] adds the `0.0..=1.0` range check.
///
/// `#[serde(transparent)]` keeps the wire form a bare number: a coordinate is
/// indistinguishable from the `f64` it wraps. Deserialization routes that number
/// through [`ProportionalCoord::new`], so a non-finite or out-of-range
/// coordinate fails at the wire boundary, and every type built from coordinates
/// inherits that guard by deserializing its fields as `ProportionalCoord`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent)]
pub struct ProportionalCoord(Finite);

impl<'de> Deserialize<'de> for ProportionalCoord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl ProportionalCoord {
    /// Construct a proportional coordinate. The value must be finite and in
    /// `0.0..=1.0`; `-0.0` is normalized to `+0.0`.
    pub fn new(value: f64) -> Result<Self, ProportionalCoordError> {
        let finite = Finite::new(value).ok_or(ProportionalCoordError::NotFinite { value })?;
        if !(0.0..=1.0).contains(&value) {
            return Err(ProportionalCoordError::OutOfBounds { value });
        }
        Ok(Self(finite))
    }

    /// The coordinate value, in `0.0..=1.0`.
    pub fn get(self) -> f64 {
        self.0.get()
    }
}

/// Errors from [`ProportionalCoord::new`].
///
/// `PartialEq` only — the variants carry the pre-validation `f64`, which may be
/// `NaN`, so a total ordering would be dishonest, and errors aren't part of the
/// content-addressed fact graph.
#[derive(Debug, Clone, PartialEq)]
pub enum ProportionalCoordError {
    /// The coordinate was not finite (`NaN` or infinite).
    NotFinite { value: f64 },
    /// The coordinate lay outside the `0.0..=1.0` unit range.
    OutOfBounds { value: f64 },
}

impl std::fmt::Display for ProportionalCoordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFinite { value } => {
                write!(f, "proportional coordinate must be finite, got {value}")
            }
            Self::OutOfBounds { value } => {
                write!(
                    f,
                    "proportional coordinate must lie in 0.0..=1.0, got {value}"
                )
            }
        }
    }
}

impl std::error::Error for ProportionalCoordError {}

/// An axis-aligned proportional rectangle in `0.0..=1.0` coordinates, given by
/// two corner points in canonical order (`min.x <= max.x`, `min.y <= max.y`).
///
/// The shared rect payload of [`ImageGeometry::BBox`] and
/// [`crate::grammar::composites::SubimageRegion::Rect`].
/// [`ProportionalRect::from_corners`] takes two corners in any order and
/// normalizes them; coincident corners give a zero-area rect, the honest shape
/// for a VLM pointing at a small feature. Both corners already lie in `[0, 1]`,
/// so the rect is always in-bounds and assembly itself can't fail.
///
/// Both corners are [`ProportionalPoint`], so `Eq`/`Hash`/`Ord` derive through
/// their negative-zero-normalizing, NaN-free guarantee. Derived `Ord` compares
/// `min` then `max`; that order feeds content-address determinism, so changing
/// it changes every `CommitId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
pub struct ProportionalRect {
    min: ProportionalPoint,
    max: ProportionalPoint,
}

impl ProportionalRect {
    /// Assemble a rect from two corner points in any order, normalizing per
    /// axis to canonical order (`min.x <= max.x`, `min.y <= max.y`). Coincident
    /// corners yield a zero-area rect. Infallible — both corners are already
    /// valid in-range points.
    pub fn from_corners(a: ProportionalPoint, b: ProportionalPoint) -> Self {
        Self {
            min: ProportionalPoint {
                x: a.x.min(b.x),
                y: a.y.min(b.y),
            },
            max: ProportionalPoint {
                x: a.x.max(b.x),
                y: a.y.max(b.y),
            },
        }
    }

    /// Construct a rect from two corners given as raw floats, in any order.
    /// Each coordinate routes through [`ProportionalCoord::new`] for the
    /// finite + range check and the `-0.0` normalization, so an invalid
    /// coordinate is the only failure mode; corner assembly cannot fail.
    pub fn new(ax: f64, ay: f64, bx: f64, by: f64) -> Result<Self, ProportionalCoordError> {
        let a = ProportionalPoint::new(ax, ay)?;
        let b = ProportionalPoint::new(bx, by)?;
        Ok(Self::from_corners(a, b))
    }

    /// The corner with the smaller coordinates.
    pub fn min_corner(&self) -> ProportionalPoint {
        self.min
    }

    /// The corner with the larger coordinates.
    pub fn max_corner(&self) -> ProportionalPoint {
        self.max
    }

    /// The smaller x coordinate, in `0.0..=1.0`.
    pub fn x(&self) -> f64 {
        self.min.x()
    }

    /// The smaller y coordinate, in `0.0..=1.0`.
    pub fn y(&self) -> f64 {
        self.min.y()
    }

    /// Width in proportional units; zero for a degenerate rect.
    pub fn width(&self) -> f64 {
        self.max.x() - self.min.x()
    }

    /// Height in proportional units; zero for a degenerate rect.
    pub fn height(&self) -> f64 {
        self.max.y() - self.min.y()
    }
}

impl<'de> Deserialize<'de> for ProportionalRect {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Each corner validates through `ProportionalPoint`/`ProportionalCoord`
        // as it materializes; `from_corners` then normalizes the axis order.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            min: ProportionalPoint,
            max: ProportionalPoint,
        }
        let w = Wire::deserialize(deserializer)?;
        Ok(Self::from_corners(w.min, w.max))
    }
}

/// A point in `0.0..=1.0` proportional image coordinates — the vertex type of
/// [`ProportionalPolyline`].
///
/// Both components are [`ProportionalCoord`], so `Eq`/`Hash`/`Ord` derive
/// through its negative-zero-normalizing, NaN-free guarantee, and `Deserialize`
/// derives each field through the coordinate's wire-boundary guard.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct ProportionalPoint {
    x: ProportionalCoord,
    y: ProportionalCoord,
}

impl ProportionalPoint {
    /// Construct a proportional point. Both components must be finite and in
    /// `0.0..=1.0`; `-0.0` is normalized to `+0.0`.
    pub fn new(x: f64, y: f64) -> Result<Self, ProportionalCoordError> {
        Ok(Self {
            x: ProportionalCoord::new(x)?,
            y: ProportionalCoord::new(y)?,
        })
    }

    /// Horizontal coordinate in `0.0..=1.0`.
    pub fn x(&self) -> f64 {
        self.x.get()
    }

    /// Vertical coordinate in `0.0..=1.0`.
    pub fn y(&self) -> f64 {
        self.y.get()
    }
}

/// Upper bound on a [`ProportionalPolyline`]'s vertex count. Legitimate road
/// and boundary traces run to many vertices, so the cap is generous; it bounds a
/// stored trace's size and the cost of per-vertex validation, the same way
/// [`MAX_LOCATION_CIRCLES`] caps an already-deserialized location. Wire-size is
/// bounded upstream by the HTTP body limit, not by this cap.
///
/// [`MAX_LOCATION_CIRCLES`]: crate::submit::pipeline::MAX_LOCATION_CIRCLES
pub const MAX_POLYLINE_POINTS: usize = 10_000;

/// A polyline traced in `0.0..=1.0` proportional image coordinates — a path on
/// the image's own frame, such as a road or boundary traced across a map sheet.
///
/// The smart constructor [`ProportionalPolyline::new`] requires between two and
/// [`MAX_POLYLINE_POINTS`] points, each finite and within the unit square.
/// Deserialization validates each vertex through [`ProportionalCoord`] and
/// enforces the same count bounds, so a too-short, over-long, or out-of-range
/// trace fails at the wire boundary. `Eq`/`Hash`/`Ord` derive through
/// [`ProportionalPoint`], and so through [`ProportionalCoord`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
pub struct ProportionalPolyline {
    points: Vec<ProportionalPoint>,
}

impl ProportionalPolyline {
    /// Construct a proportional polyline from raw `(x, y)` pairs. Requires
    /// between two and [`MAX_POLYLINE_POINTS`] points, each finite and in
    /// `0.0..=1.0`.
    pub fn new(points: Vec<(f64, f64)>) -> Result<Self, ProportionalPolylineError> {
        if points.len() < 2 {
            return Err(ProportionalPolylineError::TooFewPoints {
                count: points.len(),
            });
        }
        if points.len() > MAX_POLYLINE_POINTS {
            return Err(ProportionalPolylineError::TooManyPoints {
                count: points.len(),
                limit: MAX_POLYLINE_POINTS,
            });
        }
        let points = points
            .into_iter()
            .map(|(x, y)| ProportionalPoint::new(x, y))
            .collect::<Result<Vec<_>, _>>()
            .map_err(ProportionalPolylineError::Coord)?;
        Ok(Self { points })
    }

    /// The polyline's vertices in order; always at least two.
    pub fn points(&self) -> &[ProportionalPoint] {
        &self.points
    }
}

impl<'de> Deserialize<'de> for ProportionalPolyline {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Each vertex validates through `ProportionalPoint`/`ProportionalCoord`
        // as the `Vec` materializes; only the count bounds remain to enforce.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            points: Vec<ProportionalPoint>,
        }
        let Wire { points } = Wire::deserialize(deserializer)?;
        let count = points.len();
        if count < 2 {
            return Err(serde::de::Error::custom(
                ProportionalPolylineError::TooFewPoints { count },
            ));
        }
        if count > MAX_POLYLINE_POINTS {
            return Err(serde::de::Error::custom(
                ProportionalPolylineError::TooManyPoints {
                    count,
                    limit: MAX_POLYLINE_POINTS,
                },
            ));
        }
        Ok(Self { points })
    }
}

/// Errors from [`ProportionalPolyline::new`].
///
/// `PartialEq` only — a wrapped [`ProportionalCoordError`] carries a
/// pre-validation `f64` that may be `NaN`, so a total ordering would be
/// dishonest, and errors aren't part of the content-addressed fact graph.
#[derive(Debug, Clone, PartialEq)]
pub enum ProportionalPolylineError {
    /// A vertex coordinate was invalid (non-finite or out of range).
    Coord(ProportionalCoordError),
    /// The trace named fewer than two points.
    TooFewPoints { count: usize },
    /// The trace named more than [`MAX_POLYLINE_POINTS`] points.
    TooManyPoints { count: usize, limit: usize },
}

impl std::fmt::Display for ProportionalPolylineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Coord(e) => write!(f, "proportional polyline vertex invalid: {e}"),
            Self::TooFewPoints { count } => write!(
                f,
                "proportional polyline needs at least two points, got {count}"
            ),
            Self::TooManyPoints { count, limit } => write!(
                f,
                "proportional polyline exceeds the {limit}-point limit, got {count}"
            ),
        }
    }
}

impl std::error::Error for ProportionalPolylineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Coord(e) => Some(e),
            Self::TooFewPoints { .. } | Self::TooManyPoints { .. } => None,
        }
    }
}

/// Image-space localization geometry for a depiction — where in the image's own
/// pixel / proportional frame an entity sits.
///
/// A pixel-precise [`RleMask`], an axis-aligned [`ProportionalRect`] bbox, or a
/// [`ProportionalPolyline`] trace. Masks and bboxes suit areal features, the
/// polyline a linear feature traced across the sheet. All are anchored to the
/// image's own frame; a geographic reading is a property of the image's
/// projection, derived downstream.
///
/// Construct a bbox via [`ImageGeometry::bbox`]; deserialization routes the
/// bbox and polyline payloads through their validating wire boundaries, so
/// out-of-range coordinates fail at parse time.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ImageGeometry {
    /// Pixel-precise binary mask whose grid matches the parent image's.
    Mask {
        /// The run-length-encoded binary mask.
        mask: RleMask,
    },
    /// Axis-aligned proportional bounding box in `0.0..=1.0` coordinates.
    /// Resolution-independent — survives rescans and downsamples of the image.
    #[serde(rename = "bbox")]
    BBox {
        /// The proportional bounding rectangle.
        rect: ProportionalRect,
    },
    /// Proportional polyline trace, for roads, paths, or boundaries.
    Polyline {
        /// The proportional polyline trace.
        polyline: ProportionalPolyline,
    },
}

impl ImageGeometry {
    /// Construct an axis-aligned proportional bbox from two corners given as
    /// raw floats, in any order. See [`ProportionalRect::new`] for the
    /// coordinate validation.
    pub fn bbox(ax: f64, ay: f64, bx: f64, by: f64) -> Result<Self, ProportionalCoordError> {
        Ok(Self::BBox {
            rect: ProportionalRect::new(ax, ay, bx, by)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::*;
    use proptest::prelude::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    // Coordinate values that keep `-0.0` reachable: a plain `0.0..` float
    // strategy almost never samples exactly `-0.0`, so the bug this guards
    // against (negative-zero inconsistency between `PartialEq` and `Hash`/`Ord`)
    // would slip past. Mixing the two zero forms in explicitly forces the laws
    // to exercise them.
    fn arb_coord() -> impl Strategy<Value = f64> {
        prop_oneof![
            4 => 0.0f64..0.5,
            1 => Just(-0.0f64),
            1 => Just(0.0f64),
        ]
    }

    fn arb_rect() -> impl Strategy<Value = ProportionalRect> {
        (arb_coord(), arb_coord(), arb_coord(), arb_coord()).prop_filter_map(
            "corner coordinates lie in the unit square",
            |(ax, ay, bx, by)| ProportionalRect::new(ax, ay, bx, by).ok(),
        )
    }

    fn hash_of<T: std::hash::Hash>(value: &T) -> u64 {
        use std::hash::Hasher;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    proptest! {
        /// `ProportionalCoord` folds `-0.0` into `+0.0` and rejects NaN/Inf, so
        /// its `Hash`/`Ord` must stay consistent with `PartialEq`. Every type
        /// built from coordinates derives through this, so the law protects
        /// `ProportionalRect`, `ProportionalPoint`, and `ProportionalPolyline`.
        #[test]
        fn proportional_coord_eq_hash_ord_consistent_with_negative_zero(
            a in arb_coord(),
            b in arb_coord(),
        ) {
            let ca = ProportionalCoord::new(a).map_err(|e| TestCaseError::fail(e.to_string()))?;
            let cb = ProportionalCoord::new(b).map_err(|e| TestCaseError::fail(e.to_string()))?;
            if ca == cb {
                prop_assert_eq!(hash_of(&ca), hash_of(&cb), "equal coords must hash equally");
                prop_assert_eq!(ca.cmp(&cb), Ordering::Equal, "equal coords must order Equal");
            }
            let neg = ProportionalCoord::new(-0.0).map_err(|e| TestCaseError::fail(e.to_string()))?;
            let pos = ProportionalCoord::new(0.0).map_err(|e| TestCaseError::fail(e.to_string()))?;
            prop_assert_eq!(neg, pos, "negative and positive zero must be equal");
            prop_assert_eq!(hash_of(&neg), hash_of(&pos), "equal coords must hash equally");
            prop_assert_eq!(neg.cmp(&pos), Ordering::Equal, "equal coords must order Equal");
        }

        #[test]
        fn proportional_rect_eq_hash_ord_consistent_with_negative_zero(
            a in arb_rect(),
            b in arb_rect(),
        ) {
            // Whenever two rects compare equal, `Hash` and `Ord` must agree —
            // the exact contract negative zero would otherwise break (`PartialEq`
            // folds `-0.0 == +0.0`, but `to_bits`/`total_cmp` distinguish them).
            if a == b {
                prop_assert_eq!(hash_of(&a), hash_of(&b), "equal rects must hash equally");
                prop_assert_eq!(a.cmp(&b), Ordering::Equal, "equal rects must order Equal");
            }

            // `-0.0` can enter any corner; swapping it for `+0.0` in the min
            // corner must be invisible across Eq, Hash, and Ord.
            let neg = ProportionalRect::new(-0.0, -0.0, a.max_corner().x(), a.max_corner().y())
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            let pos = ProportionalRect::new(0.0, 0.0, a.max_corner().x(), a.max_corner().y())
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            prop_assert_eq!(neg, pos, "negative and positive zero corners must be equal");
            prop_assert_eq!(hash_of(&neg), hash_of(&pos), "equal rects must hash equally");
            prop_assert_eq!(neg.cmp(&pos), Ordering::Equal, "equal rects must order Equal");
        }
    }

    #[test]
    fn proportional_rect_normalizes_corner_order() -> TestResult {
        // Corners in any order collapse to one canonical rect, so input order is
        // not a construction failure point and equal regions compare equal.
        let canonical = ProportionalRect::new(0.1, 0.2, 0.3, 0.4)?;
        let reversed = ProportionalRect::new(0.3, 0.4, 0.1, 0.2)?;
        let mixed = ProportionalRect::new(0.3, 0.2, 0.1, 0.4)?;
        assert_eq!(canonical, reversed);
        assert_eq!(canonical, mixed);
        assert_eq!(canonical.min_corner(), ProportionalPoint::new(0.1, 0.2)?);
        assert_eq!(canonical.max_corner(), ProportionalPoint::new(0.3, 0.4)?);
        Ok(())
    }

    #[test]
    fn proportional_rect_allows_zero_area() -> TestResult {
        // Coincident corners — a VLM pointing at a small feature — are a valid
        // degenerate rect, not an error.
        let rect = ProportionalRect::new(0.5, 0.5, 0.5, 0.5)?;
        assert_eq!(rect.min_corner(), rect.max_corner());
        Ok(())
    }

    #[test]
    fn proportional_rect_rejects_out_of_range_corner() {
        assert!(matches!(
            ProportionalRect::new(0.0, 0.0, 1.5, 0.5),
            Err(ProportionalCoordError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn image_geometry_bbox_rejects_nan() {
        assert!(matches!(
            ImageGeometry::bbox(f64::NAN, 0.0, 0.5, 0.5),
            Err(ProportionalCoordError::NotFinite { .. })
        ));
    }

    #[test]
    fn image_geometry_deserialize_validates_bbox() {
        // The out-of-range corner must fail at the `ProportionalRect` wire
        // boundary, nested under the `bbox` tag and the `rect` field.
        let result: Result<ImageGeometry, _> = serde_json::from_str(
            r#"{"type":"bbox","rect":{"min":{"x":0.6,"y":0.0},"max":{"x":1.5,"y":0.5}}}"#,
        );
        assert!(
            result.is_err(),
            "out-of-range bbox must fail at deserialize"
        );
    }

    #[test]
    fn image_geometry_bbox_round_trips_preserving_corners() -> TestResult {
        // Catches a manual-Deserialize Wire → from_corners wiring bug that
        // dropped or swapped a corner coordinate.
        let geometry = ImageGeometry::bbox(0.1, 0.2, 0.3, 0.4)?;
        let json = serde_json::to_string(&geometry)?;
        let parsed: ImageGeometry = serde_json::from_str(&json)?;
        assert_eq!(parsed, geometry);
        Ok(())
    }

    #[test]
    fn proportional_point_rejects_nan() {
        assert!(matches!(
            ProportionalPoint::new(f64::NAN, 0.5),
            Err(ProportionalCoordError::NotFinite { .. })
        ));
    }

    #[test]
    fn proportional_polyline_rejects_single_point() {
        assert!(matches!(
            ProportionalPolyline::new(vec![(0.1, 0.1)]),
            Err(ProportionalPolylineError::TooFewPoints { count: 1 })
        ));
    }

    #[test]
    fn proportional_polyline_rejects_too_many_points() {
        let points = vec![(0.5f64, 0.5f64); MAX_POLYLINE_POINTS + 1];
        assert!(matches!(
            ProportionalPolyline::new(points),
            Err(ProportionalPolylineError::TooManyPoints { count, limit })
                if count == MAX_POLYLINE_POINTS + 1 && limit == MAX_POLYLINE_POINTS
        ));
    }

    #[test]
    fn proportional_polyline_rejects_non_finite_vertex() {
        assert!(matches!(
            ProportionalPolyline::new(vec![(0.1, 0.1), (f64::INFINITY, 0.2)]),
            Err(ProportionalPolylineError::Coord(
                ProportionalCoordError::NotFinite { .. }
            ))
        ));
    }

    #[test]
    fn proportional_polyline_deserialize_validates_too_few_points() {
        let result: Result<ProportionalPolyline, _> =
            serde_json::from_str(r#"{"points":[{"x":0.1,"y":0.1}]}"#);
        assert!(
            result.is_err(),
            "single-point trace must fail at deserialize"
        );
    }

    #[test]
    fn proportional_polyline_deserialize_validates_out_of_range() {
        let result: Result<ProportionalPolyline, _> =
            serde_json::from_str(r#"{"points":[{"x":0.0,"y":0.0},{"x":1.5,"y":0.2}]}"#);
        assert!(
            result.is_err(),
            "out-of-range vertex must fail at the wire boundary"
        );
    }

    #[test]
    fn proportional_polyline_deserialize_validates_too_many_points() {
        let points = vec![r#"{"x":0.5,"y":0.5}"#; MAX_POLYLINE_POINTS + 1].join(",");
        let json = format!("{{\"points\":[{points}]}}");
        let result: Result<ProportionalPolyline, _> = serde_json::from_str(&json);
        assert!(
            result.is_err(),
            "over-long trace must fail at the wire boundary"
        );
    }

    #[test]
    fn proportional_polyline_round_trips_preserving_vertices() -> TestResult {
        // Catches a derived-Deserialize bug that dropped, reordered, or swapped
        // the x/y of any vertex.
        let trace = ProportionalPolyline::new(vec![(0.1, 0.2), (0.3, 0.4), (0.5, 0.6)])?;
        let json = serde_json::to_string(&trace)?;
        let parsed: ProportionalPolyline = serde_json::from_str(&json)?;
        assert_eq!(parsed, trace);
        Ok(())
    }
}
