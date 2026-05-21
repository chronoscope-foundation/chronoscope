//! Geometry primitives referenced by depiction and feature facts.
//!
//! [`RleMask`] is a run-length-encoded binary mask in the COCO-compressed
//! ASCII format produced by `pycocotools` and SAM-family segmentation
//! models.
//!
//! [`ProportionalRect`] is an axis-aligned rectangle in `0.0..=1.0`
//! proportional coordinates. It's shared between [`ImageRegion::BBox`]
//! and [`crate::facts::composites::SubimageRegion::Rect`] — both apply
//! the same validation and the same wire shape.
//!
//! [`ImageRegion`] is the sum of pixel-precise masks and axis-aligned
//! proportional bounding boxes, anchored to the parent image. Both forms
//! arrive from real pipelines — SAM-family models emit RLE masks, while
//! VLMs that produce normalized bbox output (Gemma 3, Florence-2) emit
//! bboxes. The mask form's pixel grid equals the parent image's; the
//! bbox form uses `0.0..=1.0` proportional coordinates so it's
//! resolution-independent.
//!
//! [`GeoPoint`] is a validated 2D `(lat, lon)` point in WGS-84 degrees.
//!
//! [`Polyline`] is a sequence of `>=2` [`GeoPoint`]s, used for traced
//! linear features on maps (roads, paths, boundaries) where a region
//! shape isn't the natural fit.
//!
//! [`SpatialGeometry`] is the sum of [`ImageRegion`] and [`Polyline`],
//! attached to map-bearing depiction facts where either shape may show
//! up depending on the medium's nature.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Run-length-encoded binary mask in the COCO compressed-string format.
///
/// `counts` holds compressed run lengths as ASCII bytes (modified LEB128
/// with a +48 offset). Runs alternate background and foreground, starting
/// with background. Consumers that need an expanded binary mask
/// round-trip via the upstream decoder.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct RleMask {
    /// Compressed run-length bytes, COCO format.
    pub counts: String,
}

/// An axis-aligned proportional rectangle in `0.0..=1.0` coordinates.
///
/// Used as the shared rect payload by [`ImageRegion::BBox`] and
/// [`crate::facts::composites::SubimageRegion::Rect`]. The smart
/// constructor [`ProportionalRect::new`] rejects non-finite coordinates,
/// negative origins, non-positive extents, and rectangles that extend
/// past the unit square. Deserialization routes through it so invalid
/// shapes fail at the wire boundary.
///
/// `Eq` and `Hash` are derivable because the constructor refuses `NaN`
/// and infinity — every constructed value has well-behaved float
/// equality.
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

impl ProportionalRect {
    /// Construct a proportional rect. Bounds must satisfy
    /// `0.0 <= x`, `0.0 <= y`, `width > 0`, `height > 0`,
    /// `x + width <= 1.0`, and `y + height <= 1.0`, with all four
    /// components finite.
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
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }

    /// Horizontal origin in `0.0..=1.0`.
    #[must_use]
    pub fn x(&self) -> f32 {
        self.x
    }

    /// Vertical origin in `0.0..=1.0`.
    #[must_use]
    pub fn y(&self) -> f32 {
        self.y
    }

    /// Width in proportional units; positive, with `x + width <= 1.0`.
    #[must_use]
    pub fn width(&self) -> f32 {
        self.width
    }

    /// Height in proportional units; positive, with `y + height <= 1.0`.
    #[must_use]
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
/// Two forms map to real pipelines: pixel-precise masks (from
/// segmentation models) and axis-aligned proportional bounding boxes
/// (from VLMs that output normalized boxes). The mask form's pixel grid
/// equals the parent image's dimensions; the bbox form uses
/// `0.0..=1.0` proportional coordinates and is resolution-independent.
///
/// Construct bbox via the smart constructor [`ImageRegion::bbox`];
/// deserialization routes through [`ProportionalRect`]'s wire boundary
/// so out-of-range bboxes fail at parse time.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageRegion {
    /// Pixel-precise binary mask whose grid matches the parent image's.
    Mask(RleMask),
    /// Axis-aligned proportional bounding box in `0.0..=1.0`
    /// coordinates. Resolution-independent — survives rescans and
    /// downsamples of the parent image.
    #[serde(rename = "bbox")]
    BBox(ProportionalRect),
}

impl ImageRegion {
    /// Construct an axis-aligned proportional bbox. See
    /// [`ProportionalRect::new`] for the validation rules.
    pub fn bbox(x: f32, y: f32, width: f32, height: f32) -> Result<Self, ProportionalRectError> {
        Ok(Self::BBox(ProportionalRect::new(x, y, width, height)?))
    }
}

/// Errors from [`ProportionalRect::new`].
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

/// A 2D point in WGS-84 degrees.
///
/// The smart constructor [`GeoPoint::new`] rejects non-finite values and
/// coordinates outside the standard ranges (`lat` ∈ `[-90, 90]`,
/// `lon` ∈ `[-180, 180]`). Deserialization routes through it so wire-
/// invalid points fail at the boundary.
///
/// `Eq` is not derivable because the type carries `f64`, but the smart
/// constructor's NaN/Inf rejection means the constructed values do have
/// well-defined `PartialEq`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, JsonSchema)]
pub struct GeoPoint {
    lat: f64,
    lon: f64,
}

impl GeoPoint {
    /// Construct a geo-point. `lat` must lie in `[-90, 90]`, `lon` in
    /// `[-180, 180]`, and both must be finite.
    pub fn new(lat: f64, lon: f64) -> Result<Self, GeoPointError> {
        if !lat.is_finite() || !lon.is_finite() {
            return Err(GeoPointError::NotFinite { lat, lon });
        }
        if !(-90.0..=90.0).contains(&lat) {
            return Err(GeoPointError::LatitudeOutOfRange { lat });
        }
        if !(-180.0..=180.0).contains(&lon) {
            return Err(GeoPointError::LongitudeOutOfRange { lon });
        }
        Ok(Self { lat, lon })
    }

    /// Latitude in degrees, in `[-90, 90]`.
    #[must_use]
    pub fn lat(&self) -> f64 {
        self.lat
    }

    /// Longitude in degrees, in `[-180, 180]`.
    #[must_use]
    pub fn lon(&self) -> f64 {
        self.lon
    }
}

impl<'de> Deserialize<'de> for GeoPoint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            lat: f64,
            lon: f64,
        }
        let w = Wire::deserialize(deserializer)?;
        Self::new(w.lat, w.lon).map_err(serde::de::Error::custom)
    }
}

/// Errors from [`GeoPoint::new`].
#[derive(Debug, Clone, PartialEq)]
pub enum GeoPointError {
    /// One or more coordinates were not finite.
    NotFinite { lat: f64, lon: f64 },
    /// `lat` was outside `[-90, 90]`.
    LatitudeOutOfRange { lat: f64 },
    /// `lon` was outside `[-180, 180]`.
    LongitudeOutOfRange { lon: f64 },
}

impl std::fmt::Display for GeoPointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFinite { lat, lon } => {
                write!(
                    f,
                    "geo-point coordinates must be finite, got lat={lat}, lon={lon}"
                )
            }
            Self::LatitudeOutOfRange { lat } => {
                write!(f, "latitude must be in [-90, 90], got {lat}")
            }
            Self::LongitudeOutOfRange { lon } => {
                write!(f, "longitude must be in [-180, 180], got {lon}")
            }
        }
    }
}

impl std::error::Error for GeoPointError {}

/// A polyline defined by a sequence of `>=2` 2D geo-points.
///
/// Used for traced linear features on maps (roads, paths, parcel
/// boundaries) where a region mask is the wrong shape. The smart
/// constructor [`Polyline::new`] enforces the minimum-point invariant at
/// the parse boundary; the typed interior trusts the value.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Polyline {
    points: Vec<GeoPoint>,
}

impl Polyline {
    /// Construct a polyline from an ordered sequence of geo-points.
    /// Rejects sequences with fewer than two points.
    pub fn new(points: Vec<GeoPoint>) -> Result<Self, PolylineError> {
        if points.len() < 2 {
            return Err(PolylineError::TooFewPoints {
                count: points.len(),
            });
        }
        Ok(Self { points })
    }

    /// The points making up this polyline, in order.
    #[must_use]
    pub fn points(&self) -> &[GeoPoint] {
        &self.points
    }
}

impl<'de> Deserialize<'de> for Polyline {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            points: Vec<GeoPoint>,
        }
        let raw = Raw::deserialize(deserializer)?;
        Polyline::new(raw.points).map_err(serde::de::Error::custom)
    }
}

/// Errors from [`Polyline::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolylineError {
    /// Fewer than two points were supplied; a polyline requires at least
    /// two points to define a segment.
    TooFewPoints {
        /// The number of points actually supplied.
        count: usize,
    },
}

impl std::fmt::Display for PolylineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewPoints { count } => {
                write!(f, "polyline requires at least 2 points, got {count}")
            }
        }
    }
}

impl std::error::Error for PolylineError {}

/// Spatial geometry attached to a map-bearing depiction fact.
///
/// Either an image region (mask or bbox) or a polyline trace.
/// Region is typical for areal features; polyline is typical for linear
/// features traced from the sheet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SpatialGeometry {
    /// Image region — mask or bbox.
    Region(ImageRegion),
    /// Polyline trace, for roads, paths, or boundaries.
    Polyline(Polyline),
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn polyline_rejects_zero_points() {
        assert_eq!(
            Polyline::new(Vec::new()),
            Err(PolylineError::TooFewPoints { count: 0 })
        );
    }

    #[test]
    fn polyline_rejects_single_point() -> TestResult {
        let p = GeoPoint::new(0.0, 0.0)?;
        assert_eq!(
            Polyline::new(vec![p]),
            Err(PolylineError::TooFewPoints { count: 1 })
        );
        Ok(())
    }

    #[test]
    fn polyline_deserialize_rejects_short() {
        let result: Result<Polyline, _> = serde_json::from_str(r#"{"points":[{"lat":0,"lon":0}]}"#);
        assert!(
            result.is_err(),
            "single-point polyline must fail at deserialize"
        );
    }

    #[test]
    fn geo_point_rejects_out_of_range_lat() {
        assert!(matches!(
            GeoPoint::new(91.0, 0.0),
            Err(GeoPointError::LatitudeOutOfRange { .. })
        ));
    }

    #[test]
    fn geo_point_rejects_nan() {
        assert!(matches!(
            GeoPoint::new(f64::NAN, 0.0),
            Err(GeoPointError::NotFinite { .. })
        ));
    }

    #[test]
    fn geo_point_deserialize_validates() {
        let result: Result<GeoPoint, _> = serde_json::from_str(r#"{"lat":91.0,"lon":0.0}"#);
        assert!(result.is_err(), "out-of-range lat must fail at deserialize");
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
        let result: Result<ImageRegion, _> =
            serde_json::from_str(r#"{"type":"bbox","x":0.6,"y":0.0,"width":0.5,"height":0.5}"#);
        assert!(
            result.is_err(),
            "out-of-bounds bbox must fail at deserialize"
        );
    }

    #[test]
    fn polyline_round_trips_with_points_preserved() -> TestResult {
        let pl = Polyline::new(vec![
            GeoPoint::new(1.0, 2.0)?,
            GeoPoint::new(3.0, 4.0)?,
            GeoPoint::new(5.0, 6.0)?,
        ])?;
        let json = serde_json::to_string(&pl)?;
        let parsed: Polyline = serde_json::from_str(&json)?;
        assert_eq!(parsed.points(), pl.points());
        Ok(())
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
