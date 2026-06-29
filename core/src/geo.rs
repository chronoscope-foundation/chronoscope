//! Geographic value types — general-purpose, not fact-store-specific.
//!
//! Lives at crate root because nothing here depends on facts/schemas/storage;
//! it's geo machinery any layer in the crate (or external consumers) can pick
//! up.
//!
//! - [`GeoPoint`] is a validated 2D `(lat, lon)` point in WGS-84 degrees.
//! - [`Bbox`] is an axis-aligned bounding box built from two [`GeoPoint`]s.
//! - [`Meters`] is a meter-valued scalar — a distance or radius on the sphere.
//! - [`SpherePoint`] / [`SphereCap`] are the compute-side spherical primitives:
//!   a point and a spherical cap (geodesic disk) on the unit sphere, with exact
//!   great-circle membership and cap-cap geometry. They are never serialized.

use std::cmp::Ordering;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Earth's mean radius in meters, the sphere [`SpherePoint::distance`]
/// measures on.
pub(crate) const EARTH_RADIUS_M: f64 = 6_371_000.0;

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

// ============================================================================
// Meters
// ============================================================================

/// A meter-valued scalar: a distance or radius on the sphere. Carrying the unit
/// in the type keeps `_m`-suffixed names off the call surface and stops a raw
/// `f64` being passed where a meter count is meant.
///
/// No smart constructor yet — callers that need a finite, non-negative value
/// (e.g. [`crate::location::Location::circle`]) validate `.0` themselves. No
/// `Ord`: equal-value comparison flows through `f64::total_cmp` on `.0` at the
/// few sites that order meter-bearing values.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent)]
pub struct Meters(pub f64);

// ============================================================================
// SpherePoint / SphereCap
// ============================================================================

/// A point on the unit sphere. Compute-side only — never serialized;
/// [`GeoPoint`] is the stored form. The only constructor is `From<GeoPoint>`,
/// so every `SpherePoint` is a genuine unit vector from a validated geo-point.
///
/// No `Eq`: the fields are `f64` from trigonometric arithmetic, with no
/// NaN-free smart-constructor guarantee.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SpherePoint {
    x: f64,
    y: f64,
    z: f64,
}

impl From<GeoPoint> for SpherePoint {
    fn from(p: GeoPoint) -> Self {
        let lat = p.lat().to_radians();
        let lon = p.lon().to_radians();
        let cos_lat = lat.cos();
        Self {
            x: cos_lat * lon.cos(),
            y: cos_lat * lon.sin(),
            z: lat.sin(),
        }
    }
}

impl SpherePoint {
    fn dot(&self, other: &SpherePoint) -> f64 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    /// The cross product, a vector normal to the plane of the two directions.
    /// Its magnitude is `sin θ` between unit vectors.
    fn cross(&self, other: &SpherePoint) -> (f64, f64, f64) {
        (
            self.y * other.z - self.z * other.y,
            self.z * other.x - self.x * other.z,
            self.x * other.y - self.y * other.x,
        )
    }

    /// Great-circle distance to another point, in meters on the
    /// [`EARTH_RADIUS_M`] sphere.
    ///
    /// The angle is `atan2(|u×v|, u·v)`, total over the whole range: the cross
    /// magnitude and dot are both finite, and `atan2` has no domain limit, so
    /// antipodal and coincident points yield `π` and `0` without an `acos`
    /// argument straying past `±1`.
    pub fn distance(&self, other: &SpherePoint) -> Meters {
        let (cx, cy, cz) = self.cross(other);
        let cross_mag = (cx * cx + cy * cy + cz * cz).sqrt();
        let angle = cross_mag.atan2(self.dot(other));
        Meters(EARTH_RADIUS_M * angle)
    }
}

/// A spherical cap: every point within an angular radius of a center direction,
/// the geodesic disk [`crate::location::Location::Circle`] denotes. Compute-side
/// only — never serialized.
///
/// Membership is exact at any radius up to a hemisphere, with no projection,
/// frame, or seam: [`covers`] compares great-circle distance to the radius
/// within a tolerance. The angular radius's cosine is precomputed for the
/// [`boundary_intersections`] solve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SphereCap {
    center: SpherePoint,
    radius: Meters,
    cos_radius: f64,
}

/// Relative meter tolerance for cap membership and containment — absorbs the
/// rounding of the trig that builds a [`SpherePoint`] from lat/lon, scaled by
/// the radius so a continental cap gets proportionally more slack.
const CAP_TOL_REL: f64 = 1e-9;
/// Absolute meter floor for the tolerance, so a zero-radius cap (a point) still
/// admits its own center under rounding.
const CAP_TOL_ABS_M: f64 = 1e-3;

impl SphereCap {
    /// A cap of the given meter radius about a center direction.
    pub fn new(center: SpherePoint, radius: Meters) -> Self {
        let cos_radius = (radius.0 / EARTH_RADIUS_M).cos();
        Self {
            center,
            radius,
            cos_radius,
        }
    }

    /// The cap's center direction — a candidate point for an emptiness witness.
    pub fn center(&self) -> SpherePoint {
        self.center
    }

    /// The cap's meter tolerance: a relative slop that grows with the radius,
    /// plus a fixed floor.
    fn tol(&self) -> Meters {
        Meters(self.radius.0 * CAP_TOL_REL + CAP_TOL_ABS_M)
    }

    /// Whether `p` lies in the cap, inclusive of the rim. The meter tolerance
    /// admits a boundary point that FP rounding pushes a hair outside.
    pub fn covers(&self, p: &SpherePoint) -> bool {
        self.center.distance(p).0 <= self.radius.0 + self.tol().0
    }

    /// Whether this cap contains `other` entirely: the centers' separation plus
    /// `other`'s radius fits within this radius, up to the tolerance.
    pub fn contains(&self, other: &SphereCap) -> bool {
        let between = self.center.distance(&other.center).0;
        between + other.radius.0 <= self.radius.0 + self.tol().0
    }

    /// The boundary intersection points of two caps' bounding circles: zero
    /// (disjoint, nested, or coincident/antipodal centers), one (tangent), or
    /// two. The two crossings are the corners of the lens-shaped (lune) overlap
    /// of the two caps.
    ///
    /// A boundary point sits on both rims, so for unit centers `u1, u2` with
    /// `g = u1·u2` it decomposes as `p = a·u1 + b·u2 + h·n̂`, where
    /// `n̂ = (u1×u2)/|u1×u2|` is the unit normal to their plane. Matching
    /// `p·u1 = cos ρ1` and `p·u2 = cos ρ2` gives the in-plane foot
    /// `m = a·u1 + b·u2`; unit length gives the out-of-plane offset
    /// `h² = 1 − |m|²`, and the crossings are `m ± h·n̂`. The solve is total:
    /// `1 − g²` vanishes only for coincident or antipodal centers (no shared
    /// rim — return none), and `h² ≤ 0` is tangency (`h = 0`, one point) or no
    /// crossing.
    ///
    /// Both small caps near each other and a near-hemisphere pair drive
    /// `1 − g²` toward zero, so `h²` is built in a form that factors the small
    /// `1 − g` out analytically rather than dividing by it twice: the dominant
    /// cancellation that a naive `(1 − |m|²)` carries is removed before the
    /// subtraction, keeping the sign of `h²` honest for the small circles real
    /// places actually use.
    pub fn boundary_intersections(&self, other: &SphereCap) -> Vec<SpherePoint> {
        let u1 = self.center;
        let u2 = other.center;
        let g = u1.dot(&u2);
        // Factor `1 − g²` so the near-degenerate `1 − g` is available exactly.
        let one_minus_g = 1.0 - g;
        let one_plus_g = 1.0 + g;
        let denom = one_minus_g * one_plus_g;
        // Coincident or antipodal centers share no determinate rim crossing.
        if denom <= f64::EPSILON {
            return Vec::new();
        }
        let cos_r1 = self.cos_radius;
        let cos_r2 = other.cos_radius;
        // `cos ρ1 − cos ρ2` as a product so two near-equal cosines don't cancel:
        // `cos x − cos y = −2·sin((x+y)/2)·sin((x−y)/2)`, with the angular radii
        // recovered from the stored meter radii.
        let r1 = self.radius.0 / EARTH_RADIUS_M;
        let r2 = other.radius.0 / EARTH_RADIUS_M;
        let cos_diff = -2.0 * ((r1 + r2) * 0.5).sin() * ((r1 - r2) * 0.5).sin();

        let a = (cos_r1 - g * cos_r2) / denom;
        let b = (cos_r2 - g * cos_r1) / denom;
        // `h² = 1 − |m|²` with `|m|² = (cos²ρ1 + cos²ρ2 − 2g·cosρ1·cosρ2)/denom`.
        // Splitting the numerator as `(cosρ1 − cosρ2)² + 2·cosρ1·cosρ2·(1 − g)`
        // lets the `(1 − g)` term cancel the matching factor in `denom`, leaving
        // one well-scaled subtraction instead of a tiny-over-tiny ratio.
        let h2 = (one_plus_g - 2.0 * cos_r1 * cos_r2) / one_plus_g - cos_diff * cos_diff / denom;
        let (nx, ny, nz) = u1.cross(&u2);
        let n_mag = denom.sqrt();

        let mut out = Vec::new();
        let mut push = |h: f64| {
            // `h` is the offset along the *unit* normal; scale the raw cross
            // product (length `n_mag`) to unit before stepping.
            let s = h / n_mag;
            out.push(SpherePoint {
                x: a * u1.x + b * u2.x + s * nx,
                y: a * u1.y + b * u2.y + s * ny,
                z: a * u1.z + b * u2.z + s * nz,
            });
        };
        // A tangency lands `h²` at zero, but rounding smears it to either side,
        // so a band around zero counts as the single contact point. Outside it, a
        // negative `h²` is a genuine gap (no crossing) and a positive one two
        // crossings. A real overlap's `h²` clears the band, so a near-miss never
        // masquerades as a crossing.
        let tol = 1e-12;
        if h2.abs() <= tol {
            push(0.0);
        } else if h2 > 0.0 {
            let h = h2.sqrt();
            push(h);
            push(-h);
        }
        out
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

    // --- SpherePoint / SphereCap tests ---
    //
    // These exercise the regimes a `Location` proptest can't reach: the
    // `MAX_UNCERTAINTY_RADIUS` cap bounds radii through `Location::circle`, but
    // `SphereCap::new` is uncapped, so continental and near-hemisphere caps,
    // high latitude, and antimeridian straddles all live here. Each would be
    // wrong under a planar projection — the seam wrap, the `cos(lat)` collapse,
    // and the flat-distance error at large radius are exactly what the sphere
    // removes.

    fn cap(lat: f64, lon: f64, radius_m: f64) -> Result<SphereCap, GeoPointError> {
        Ok(SphereCap::new(
            GeoPoint::new(lat, lon)?.into(),
            Meters(radius_m),
        ))
    }

    #[test]
    fn distance_matches_known_arc() -> TestResult {
        // One degree of latitude along a meridian is ~111 km.
        let a: SpherePoint = GeoPoint::new(0.0, 0.0)?.into();
        let b: SpherePoint = GeoPoint::new(1.0, 0.0)?.into();
        let d = a.distance(&b).0;
        assert!((d - 111_195.0).abs() < 100.0, "got {d}");
        Ok(())
    }

    #[test]
    fn distance_antipodal_is_half_circumference_no_nan() -> TestResult {
        // The `atan2` form must stay total at the antipode — an `acos` of a
        // rounded `-1.0001` would yield NaN.
        let a: SpherePoint = GeoPoint::new(0.0, 0.0)?.into();
        let b: SpherePoint = GeoPoint::new(0.0, 180.0)?.into();
        let d = a.distance(&b).0;
        assert!(d.is_finite(), "antipodal distance must be finite");
        assert!(
            (d - std::f64::consts::PI * EARTH_RADIUS_M).abs() < 1.0,
            "got {d}"
        );
        Ok(())
    }

    #[test]
    fn distance_coincident_is_zero() -> TestResult {
        let a: SpherePoint = GeoPoint::new(12.3, 45.6)?.into();
        assert!(a.distance(&a).0.abs() < 1e-6);
        Ok(())
    }

    #[test]
    fn cap_covers_antimeridian_straddle() -> TestResult {
        // A continental cap centered just west of the antimeridian covers a
        // point just east of it — the two are physically ~22 km apart. A planar
        // `lon − lon0` projection would place them ~40000 km apart and miss.
        let c = cap(0.0, 179.9, 50_000.0)?;
        let east: SpherePoint = GeoPoint::new(0.0, -179.9)?.into();
        assert!(c.covers(&east), "across-seam point must be inside");
        Ok(())
    }

    #[test]
    fn cap_covers_high_latitude() -> TestResult {
        // Two points near the north pole at opposite longitudes are ~2.2 km
        // apart on the sphere. An equirectangular frame's `cos(lat0)` factor
        // collapses to ~0 here, squashing all longitudes together; the sphere
        // keeps them honestly close, so a small cap covers across the pole.
        let c = cap(89.99, 0.0, 5_000.0)?;
        let across: SpherePoint = GeoPoint::new(89.99, 180.0)?.into();
        assert!(c.covers(&across), "across-pole point must be inside");
        Ok(())
    }

    #[test]
    fn cap_near_hemisphere_radius_distinguishes_sides() -> TestResult {
        // A cap covering nearly a full hemisphere (~9000 km) holds the far edge
        // of its own hemisphere but not a point past the antipode-ward rim. At
        // this radius a flat metric is meaningless; the great-circle test is
        // exact.
        let c = cap(0.0, 0.0, 9_000_000.0)?;
        let near_edge: SpherePoint = GeoPoint::new(0.0, 80.0)?.into();
        let past_rim: SpherePoint = GeoPoint::new(0.0, 100.0)?.into();
        assert!(c.covers(&near_edge), "point inside the ~81° rim must be in");
        assert!(!c.covers(&past_rim), "point past the rim must be out");
        Ok(())
    }

    #[test]
    fn cap_rim_point_is_inclusive() -> TestResult {
        // A point exactly one cap-radius away (constructed at the rim distance)
        // tests inside under the tolerance.
        let c = cap(40.0, -74.0, 100_000.0)?;
        // ~100 km north of the center: 100 km / 111_195 m-per-deg ≈ 0.899°.
        let rim: SpherePoint = GeoPoint::new(40.0 + 0.899, -74.0)?.into();
        assert!(c.covers(&rim), "rim point must count as inside");
        Ok(())
    }

    #[test]
    fn cap_contains_concentric_and_nested() -> TestResult {
        let big = cap(45.0, 10.0, 200_000.0)?;
        let small = cap(45.0, 10.0, 50_000.0)?; // concentric, smaller
        let offset = cap(45.5, 10.0, 20_000.0)?; // off-center but inside
        assert!(big.contains(&small), "concentric smaller cap is contained");
        assert!(big.contains(&offset), "nested off-center cap is contained");
        assert!(!small.contains(&big), "smaller cap can't contain bigger");
        Ok(())
    }

    #[test]
    fn cap_disjoint_has_no_intersection() -> TestResult {
        // Continent-apart caps: no shared rim point.
        let paris = cap(48.8566, 2.3522, 1_000.0)?;
        let tokyo = cap(35.6762, 139.6503, 1_000.0)?;
        assert!(paris.boundary_intersections(&tokyo).is_empty());
        Ok(())
    }

    #[test]
    fn cap_concentric_has_no_intersection() -> TestResult {
        // Coincident centers → `1 − g²` vanishes → no determinate crossing.
        let a = cap(0.0, 0.0, 100_000.0)?;
        let b = cap(0.0, 0.0, 50_000.0)?;
        assert!(a.boundary_intersections(&b).is_empty());
        Ok(())
    }

    #[test]
    fn cap_nested_has_no_intersection() -> TestResult {
        // One cap strictly inside the other (same center offset, big gap): rims
        // don't meet.
        let big = cap(0.0, 0.0, 300_000.0)?;
        let small = cap(0.1, 0.0, 1_000.0)?;
        assert!(big.boundary_intersections(&small).is_empty());
        Ok(())
    }

    #[test]
    fn cap_overlapping_meets_at_two_points_on_both_rims() -> TestResult {
        // Two caps ~100 km apart with 80 km radii cross at two points. Each lies
        // on both rims, so both caps just-cover it under the tolerance.
        let a = cap(40.0, -74.0, 80_000.0)?;
        let b = cap(40.9, -74.0, 80_000.0)?; // ~100 km north
        let pts = a.boundary_intersections(&b);
        assert_eq!(pts.len(), 2, "overlapping caps meet at two points");
        for p in &pts {
            assert!(a.covers(p) && b.covers(p), "crossing must lie on both rims");
        }
        Ok(())
    }

    #[test]
    fn cap_small_close_overlap_meets_at_two_points() -> TestResult {
        // Two 5 km caps ~9 km apart — the small-and-close regime real places
        // sit in. Here `1 − g²` is ~1e-6, so a naive `h²` that divides through it
        // twice cancels to noise and reports the rims as disjoint; the factored
        // solve keeps the two genuine crossings. Each lies on both rims.
        let a = cap(0.0, -0.0405, 5_000.0)?;
        let b = cap(0.0, 0.0405, 5_000.0)?; // ~9 km east, overlap margin ~1 km
        let pts = a.boundary_intersections(&b);
        assert_eq!(pts.len(), 2, "small overlapping caps meet at two points");
        for p in &pts {
            assert!(a.covers(p) && b.covers(p), "crossing must lie on both rims");
        }
        Ok(())
    }

    #[test]
    fn cap_tangent_meets_at_a_single_locus() -> TestResult {
        // Build exact external tangency: measure the center separation, then set
        // the second radius so the rims just touch (`r_a + r_b == d`). The locus
        // is a single point — one returned, or two coincident to a millimeter.
        let center_a: SpherePoint = GeoPoint::new(40.0, -74.0)?.into();
        let center_b: SpherePoint = GeoPoint::new(41.5, -74.0)?.into();
        let d = center_a.distance(&center_b).0;
        let r_a = 80_000.0;
        let a = SphereCap::new(center_a, Meters(r_a));
        let b = SphereCap::new(center_b, Meters(d - r_a));
        let pts = a.boundary_intersections(&b);
        assert!(!pts.is_empty(), "tangent caps must meet");
        if pts.len() == 2 {
            assert!(
                pts[0].distance(&pts[1]).0 < 1e-3,
                "the two crossings of tangent caps coincide"
            );
        }
        Ok(())
    }

    #[test]
    fn cap_strictly_disjoint_with_gap_has_no_intersection() -> TestResult {
        // Centres ~200 km apart, radii 80 km each: a clear gap, so `c²` is
        // strictly negative and nothing is returned.
        let a = cap(40.0, -74.0, 80_000.0)?;
        let b = cap(40.0 + 1.8, -74.0, 80_000.0)?; // ~200 km north
        assert!(a.boundary_intersections(&b).is_empty());
        Ok(())
    }
}
