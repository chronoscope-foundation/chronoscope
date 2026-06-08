//! Geographic value types — general-purpose, not fact-store-specific.
//!
//! Lives at crate root because nothing here depends on facts/schemas/storage;
//! it's geo machinery any layer in the crate (or external consumers) can pick
//! up.
//!
//! - [`GeoPoint`] is a validated 2D `(lat, lon)` point in WGS-84 degrees.
//! - [`Polyline`] is a sequence of `>=2` [`GeoPoint`]s, for traced linear
//!   features on maps (roads, paths, boundaries) where a region shape doesn't
//!   fit.
//! - [`Bbox`] is an axis-aligned bounding box built from two [`GeoPoint`]s.

use std::cmp::Ordering;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ============================================================================
// GeoPoint
// ============================================================================

/// A 2D point in WGS-84 degrees.
///
/// The smart constructor [`GeoPoint::new`] rejects non-finite values and
/// coordinates outside the standard ranges (`lat` ∈ `[-90, 90]`, `lon` ∈
/// `[-180, 180]`). Deserialization routes through it so wire-invalid points
/// fail at the boundary.
///
/// `Eq` / `Hash` / `Ord` are hand-implemented (the type carries `f64`, needed
/// for `BTreeSet<SubmitFact>` ordering): the constructor rejects NaN/Inf and
/// normalizes `-0.0` to `+0.0`, so the manual `Hash` (via `f64::to_bits`) and
/// `Ord` (via `f64::total_cmp`) stay consistent with the derived `PartialEq`
/// (under which `-0.0 == +0.0`). Every constructed `GeoPoint` is finite and
/// free of negative zero, so equality is honest.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, JsonSchema)]
pub struct GeoPoint {
    lat: f64,
    lon: f64,
}

impl Eq for GeoPoint {}

impl std::hash::Hash for GeoPoint {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.lat.to_bits().hash(state);
        self.lon.to_bits().hash(state);
    }
}

// Manual Ord via `total_cmp` field-by-field; the smart constructor
// rejects NaN/Inf so every constructed value compares cleanly.
impl PartialOrd for GeoPoint {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for GeoPoint {
    fn cmp(&self, other: &Self) -> Ordering {
        self.lat
            .total_cmp(&other.lat)
            .then_with(|| self.lon.total_cmp(&other.lon))
    }
}

impl GeoPoint {
    /// Construct a geo-point. `lat` must lie in `[-90, 90]`, `lon` in
    /// `[-180, 180]`, and both must be finite.
    ///
    /// `-0.0` is normalized to `+0.0` so the manual `Hash`/`Ord`
    /// (bit-level / `total_cmp`) stay consistent with the derived
    /// `PartialEq`.
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
        // Normalize `-0.0` to `+0.0`: `-0.0 + 0.0 == +0.0`, and adding
        // `0.0` is a no-op for every other finite value.
        let lat = lat + 0.0;
        let lon = lon + 0.0;
        Ok(Self { lat, lon })
    }

    /// Latitude in degrees, in `[-90, 90]`.
    pub fn lat(&self) -> f64 {
        self.lat
    }

    /// Longitude in degrees, in `[-180, 180]`.
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
        #[serde(deny_unknown_fields)]
        struct Wire {
            lat: f64,
            lon: f64,
        }
        let w = Wire::deserialize(deserializer)?;
        Self::new(w.lat, w.lon).map_err(serde::de::Error::custom)
    }
}

/// Errors from [`GeoPoint::new`].
///
/// `PartialOrd` only — the error variants carry pre-validation `f64`
/// values that may be `NaN` or infinite, so total ordering would be
/// dishonest. Errors aren't part of the content-addressed-fact graph,
/// so missing `Ord` here doesn't constrain anything downstream.
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

// ============================================================================
// Polyline
// ============================================================================

/// A polyline defined by a sequence of `>=2` 2D geo-points.
///
/// Used for traced linear features on maps (roads, paths, parcel
/// boundaries) where a region mask is the wrong shape. The smart
/// constructor [`Polyline::new`] enforces the minimum-point invariant at
/// the parse boundary; the typed interior trusts the value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
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
        #[serde(deny_unknown_fields)]
        struct Raw {
            points: Vec<GeoPoint>,
        }
        let raw = Raw::deserialize(deserializer)?;
        Polyline::new(raw.points).map_err(serde::de::Error::custom)
    }
}

/// Errors from [`Polyline::new`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

// ============================================================================
// Bbox
// ============================================================================

/// A bounding box for spatial queries, built from a southwest and a
/// northeast [`GeoPoint`]. [`Bbox::new`] enforces ordering
/// (`sw.lat() <= ne.lat()` and `sw.lon() <= ne.lon()`) so downstream
/// "is this point inside?" checks can't silently fail closed when callers
/// transposed the corners. Coordinate range and finiteness are already
/// enforced by [`GeoPoint::new`], so corner construction is the only
/// place those validations live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Bbox {
    /// Southwest corner (minimum latitude, minimum longitude).
    sw: GeoPoint,
    /// Northeast corner (maximum latitude, maximum longitude).
    ne: GeoPoint,
}

/// Errors from [`Bbox::new`].
#[derive(Debug, Clone, PartialEq)]
pub enum BboxError {
    /// `sw` is not actually southwest of `ne` — either its latitude is
    /// greater than `ne`'s, or its longitude is. Antimeridian-crossing
    /// boxes are not supported here; callers split into two.
    SwNotSouthwestOfNe {
        /// The southwest corner as supplied.
        sw: GeoPoint,
        /// The northeast corner as supplied.
        ne: GeoPoint,
    },
}

impl std::fmt::Display for BboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SwNotSouthwestOfNe { sw, ne } => write!(
                f,
                "bbox sw corner ({}, {}) must be south of and west of ne corner ({}, {}); \
                 antimeridian-crossing boxes must be split",
                sw.lat(),
                sw.lon(),
                ne.lat(),
                ne.lon(),
            ),
        }
    }
}

impl std::error::Error for BboxError {}

impl Bbox {
    /// Construct a bbox from southwest and northeast corners. Returns an
    /// error if `sw` is not actually southwest of `ne`
    /// (`sw.lat() > ne.lat()` or `sw.lon() > ne.lon()`). Coordinate range
    /// and finiteness validation lives on [`GeoPoint::new`] — both
    /// arguments are already-validated points.
    pub fn new(sw: GeoPoint, ne: GeoPoint) -> Result<Self, BboxError> {
        if sw.lat() > ne.lat() || sw.lon() > ne.lon() {
            return Err(BboxError::SwNotSouthwestOfNe { sw, ne });
        }
        Ok(Self { sw, ne })
    }

    /// Southwest corner — the minimum-latitude, minimum-longitude point.
    pub fn sw(&self) -> &GeoPoint {
        &self.sw
    }

    /// Northeast corner — the maximum-latitude, maximum-longitude point.
    pub fn ne(&self) -> &GeoPoint {
        &self.ne
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    // --- GeoPoint tests ---

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
    fn geo_point_negative_zero_is_consistent_across_eq_hash_ord() -> TestResult {
        use std::collections::HashSet;
        use std::hash::{Hash, Hasher};

        // `-0.0` in either coordinate must normalize to `+0.0`, so the two
        // forms are fully indistinguishable under Eq, Hash, and Ord — the
        // contract the manual Hash/Ord impls would otherwise violate.
        let neg = GeoPoint::new(-0.0, -0.0)?;
        let pos = GeoPoint::new(0.0, 0.0)?;

        assert_eq!(neg, pos, "negative and positive zero must be equal");

        let mut hasher_neg = std::collections::hash_map::DefaultHasher::new();
        let mut hasher_pos = std::collections::hash_map::DefaultHasher::new();
        neg.hash(&mut hasher_neg);
        pos.hash(&mut hasher_pos);
        assert_eq!(
            hasher_neg.finish(),
            hasher_pos.finish(),
            "equal points must hash equally"
        );

        let mut set = HashSet::new();
        set.insert(neg);
        set.insert(pos);
        assert_eq!(set.len(), 1, "the two zero-forms must dedup to one entry");

        assert_eq!(
            neg.cmp(&pos),
            std::cmp::Ordering::Equal,
            "equal points must order Equal"
        );
        Ok(())
    }

    // --- Polyline tests ---

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

    // --- Bbox tests ---

    #[test]
    fn bbox_accepts_valid() -> TestResult {
        let sw = GeoPoint::new(40.0, -74.0)?;
        let ne = GeoPoint::new(41.0, -73.0)?;
        let b = Bbox::new(sw, ne)?;
        assert_eq!(b.sw(), &sw);
        assert_eq!(b.ne(), &ne);
        Ok(())
    }

    #[test]
    fn bbox_rejects_sw_north_of_ne() -> TestResult {
        // sw.lat (40) > ne.lat (30) — sw is north of ne.
        let sw = GeoPoint::new(40.0, 0.0)?;
        let ne = GeoPoint::new(30.0, 1.0)?;
        assert!(matches!(
            Bbox::new(sw, ne),
            Err(BboxError::SwNotSouthwestOfNe { .. })
        ));
        Ok(())
    }

    #[test]
    fn bbox_rejects_sw_east_of_ne() -> TestResult {
        // sw.lon (5) > ne.lon (1) — sw is east of ne. (Antimeridian-
        // crossing boxes must be split into two, not folded here.)
        let sw = GeoPoint::new(0.0, 5.0)?;
        let ne = GeoPoint::new(1.0, 1.0)?;
        assert!(matches!(
            Bbox::new(sw, ne),
            Err(BboxError::SwNotSouthwestOfNe { .. })
        ));
        Ok(())
    }
}
