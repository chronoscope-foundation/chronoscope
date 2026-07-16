//! Geographic value types — general-purpose, not fact-store-specific.
//!
//! Lives at crate root because nothing here depends on facts/schemas/storage;
//! it's geo machinery any layer in the crate (or external consumers) can pick
//! up.
//!
//! - [`GeoPoint`] is a validated 2D `(lat, lon)` point in WGS-84 degrees.
//! - [`Viewport`] is a map viewport built from two [`GeoPoint`] corners, with
//!   a wrap convention for antimeridian-crossing spans; [`IndexRect`] is its
//!   non-wrapping half, the shape spatial-index rows store.
//! - [`Meters`] is a meter-valued scalar — a WGS84 geodesic distance or radius.
//! - [`Circle`] is the compute-side primitive: a cap (disk) about a
//!   [`GeoPoint`] center. Distance, membership, and the circle-circle boundary
//!   crossing all solve on WGS84 via [`geographiclib_rs`]
//!   ([`Circle::boundary_intersections`]). It is never serialized.

use std::cmp::Ordering;

use geographiclib_rs::{DirectGeodesic, Geodesic, InverseGeodesic};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A nominal Earth radius in meters. Distances and membership are WGS84 (see
/// [`Circle`]); this sphere is only the scaffold for the
/// conservative longitude half-width in [`cap_bounding_rects`], whose
/// `asin(sin r / cos φ)` over-covers the wider-radius ellipsoid. (≈ the WGS84
/// mean radius `(2a+b)/3`; as a scaffold its exact value is immaterial.)
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

    /// The WGS84 geodesic distance to another point (Karney's inverse solution).
    fn distance(&self, other: &GeoPoint) -> Meters {
        Meters(Geodesic::wgs84().inverse(self.lat, self.lon, other.lat, other.lon))
    }

    /// A point minted from a WGS84 geodesic solve (`direct`/`inverse` outputs),
    /// which geographiclib already returns in-range; the clamp absorbs last-ulp
    /// drift so this is total without a fallible check in the solver hot paths.
    fn from_wgs84(lat: f64, lon: f64) -> Self {
        debug_assert!(
            lat.is_finite() && lon.is_finite(),
            "geodesic output must be finite"
        );
        Self {
            lat: lat.clamp(-90.0, 90.0) + 0.0,
            lon: lon.clamp(-180.0, 180.0) + 0.0,
        }
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
// Viewport
// ============================================================================

/// A map viewport for spatial queries, built from a southwest and a
/// northeast [`GeoPoint`].
///
/// Latitude is a plain interval `[sw.lat(), ne.lat()]`. Longitude follows the
/// viewport wrap convention: when `sw.lon() <= ne.lon()` the box spans the
/// interval `[sw.lon(), ne.lon()]`; when `sw.lon() > ne.lon()` the box wraps
/// across the ±180° antimeridian, covering `[sw.lon(), 180]` together with
/// `[-180, ne.lon()]`. [`Viewport::halves`] is the single interpretation of
/// the wrap, so spatial code reads the non-wrapping halves rather than the
/// raw corners.
///
/// [`Viewport::new`] enforces latitude ordering so a transposed corner pair is
/// caught at construction. Coordinate range and finiteness are already
/// enforced by [`GeoPoint::new`], so corner construction is the only place
/// those validations live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Viewport {
    /// Southwest corner: the minimum latitude and the western longitude edge
    /// (numerically the greater value on an antimeridian-wrapping box).
    sw: GeoPoint,
    /// Northeast corner: the maximum latitude and the eastern longitude edge.
    ne: GeoPoint,
}

/// Errors from [`Viewport::new`].
///
/// Only latitude can be checked for corner transposition. Latitude has no wrap,
/// so `sw.lat() > ne.lat()` is a *provable* transposed corner pair. Longitude's
/// `sw.lon() > ne.lon()` is instead the deliberate antimeridian-wrap convention
/// (see [`Viewport`]), so it can't be canonicalized: reordering the corners would
/// turn a transposition into a bogus wrap box, silently corrupting the lon axis.
/// The lat guard is the one transposition symptom we can prove, so we reject it
/// rather than construct a wrong box.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewportError {
    /// `sw`'s latitude exceeds `ne`'s — the box is inverted north-to-south.
    /// Longitude ordering is free: `sw.lon() > ne.lon()` denotes an
    /// antimeridian-wrapping box (see [`Viewport`]).
    LatitudeInverted {
        /// The southwest corner as supplied.
        sw: GeoPoint,
        /// The northeast corner as supplied.
        ne: GeoPoint,
    },
}

impl std::fmt::Display for ViewportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LatitudeInverted { sw, ne } => write!(
                f,
                "viewport sw latitude ({}) must be at or below ne latitude ({})",
                sw.lat(),
                ne.lat(),
            ),
        }
    }
}

impl std::error::Error for ViewportError {}

/// Errors from [`Viewport::from_coords`]: a corner coordinate out of range or
/// non-finite (from [`GeoPoint::new`]), or a latitude-inverted corner pair
/// (from [`Viewport::new`]).
#[derive(Debug, Clone, PartialEq)]
pub enum ViewportCoordsError {
    /// The southwest corner's latitude or longitude failed [`GeoPoint::new`].
    Southwest(GeoPointError),
    /// The northeast corner's latitude or longitude failed [`GeoPoint::new`].
    Northeast(GeoPointError),
    /// The corner pair was latitude-inverted (see [`ViewportError`]).
    Viewport(ViewportError),
}

impl std::fmt::Display for ViewportCoordsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Southwest(e) => write!(f, "southwest viewport corner: {e}"),
            Self::Northeast(e) => write!(f, "northeast viewport corner: {e}"),
            Self::Viewport(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for ViewportCoordsError {}

impl From<ViewportError> for ViewportCoordsError {
    fn from(e: ViewportError) -> Self {
        Self::Viewport(e)
    }
}

impl Viewport {
    /// Construct a viewport from southwest and northeast corners. Rejects an
    /// inverted latitude span (`sw.lat() > ne.lat()`); a westward longitude
    /// span (`sw.lon() > ne.lon()`) is accepted as an antimeridian wrap (see
    /// [`Viewport`]). Coordinate range and finiteness validation lives on
    /// [`GeoPoint::new`] — both arguments are already-validated points.
    pub fn new(sw: GeoPoint, ne: GeoPoint) -> Result<Self, ViewportError> {
        if sw.lat() > ne.lat() {
            return Err(ViewportError::LatitudeInverted { sw, ne });
        }
        Ok(Self { sw, ne })
    }

    /// Construct a viewport from flat `(min_lat, max_lat, min_lon, max_lon)`
    /// coordinates — the shape a map-viewport query arrives in. The `min_*`
    /// pair becomes the southwest corner, `max_*` the northeast; a westward
    /// longitude span (`min_lon > max_lon`) is the antimeridian-wrap
    /// convention (see [`Viewport`]). Per-corner range/finiteness comes from
    /// [`GeoPoint::new`], latitude ordering from [`Viewport::new`].
    pub fn from_coords(
        min_lat: f64,
        max_lat: f64,
        min_lon: f64,
        max_lon: f64,
    ) -> Result<Self, ViewportCoordsError> {
        let sw = GeoPoint::new(min_lat, min_lon).map_err(ViewportCoordsError::Southwest)?;
        let ne = GeoPoint::new(max_lat, max_lon).map_err(ViewportCoordsError::Northeast)?;
        Ok(Self::new(sw, ne)?)
    }

    /// Southwest corner — the minimum-latitude, minimum-longitude point.
    pub fn sw(&self) -> &GeoPoint {
        &self.sw
    }

    /// Northeast corner — the maximum-latitude, maximum-longitude point.
    pub fn ne(&self) -> &GeoPoint {
        &self.ne
    }

    /// The box's minimum latitude (southwest corner).
    pub fn min_lat(&self) -> f64 {
        self.sw.lat()
    }

    /// The box's maximum latitude (northeast corner).
    pub fn max_lat(&self) -> f64 {
        self.ne.lat()
    }

    /// The western longitude edge. On an antimeridian-wrapping box this is
    /// numerically greater than [`max_lon`](Self::max_lon).
    pub fn min_lon(&self) -> f64 {
        self.sw.lon()
    }

    /// The eastern longitude edge.
    pub fn max_lon(&self) -> f64 {
        self.ne.lon()
    }

    /// The one or two non-wrapping [`IndexRect`]s this viewport covers —
    /// the single place the wrap convention is interpreted. A normal box is
    /// its own rect; a westward span splits at the seam into `[min_lon, 180]`
    /// and `[-180, max_lon]`. Everything downstream — point membership, the
    /// nearest-point distance, the spatial index's query ranges, the
    /// index-soundness tests — consumes the halves, so no other code reads
    /// the wrap.
    pub fn halves(&self) -> ViewportHalves {
        let (min_lat, max_lat) = (self.sw.lat(), self.ne.lat());
        if self.sw.lon() <= self.ne.lon() {
            ViewportHalves {
                first: IndexRect {
                    min_lat,
                    max_lat,
                    min_lon: self.sw.lon(),
                    max_lon: self.ne.lon(),
                },
                second: None,
            }
        } else {
            ViewportHalves {
                first: IndexRect {
                    min_lat,
                    max_lat,
                    min_lon: self.sw.lon(),
                    max_lon: 180.0,
                },
                second: Some(IndexRect {
                    min_lat,
                    max_lat,
                    min_lon: -180.0,
                    max_lon: self.ne.lon(),
                }),
            }
        }
    }

    /// Whether `p` lies within the box, inclusive on every edge — some
    /// [half](Self::halves) holds it.
    pub fn contains(&self, p: &GeoPoint) -> bool {
        self.halves()
            .into_iter()
            .any(|half| half.contains_point(p.lat(), p.lon()))
    }

    /// Great-circle distance from `p` to the nearest point of the box, zero
    /// when the box contains it. The halves share the seam edge, so the
    /// distance to their union is the minimum over the halves.
    pub(crate) fn geodesic_distance_to(&self, p: &GeoPoint) -> Meters {
        let mut best = f64::INFINITY;
        for half in self.halves() {
            best = best.min(half.geodesic_distance_to(p).0);
        }
        Meters(best)
    }
}

/// The non-wrapping halves of a [`Viewport`] — one rect, or two for a
/// seam-crossing span. A stack pair rather than a `Vec`: membership and
/// distance re-derive the halves per test, so the split must cost two
/// comparisons, not an allocation. Iterate it; the pair has no other
/// surface.
#[derive(Debug, Clone, Copy)]
pub struct ViewportHalves {
    first: IndexRect,
    second: Option<IndexRect>,
}

impl IntoIterator for ViewportHalves {
    type Item = IndexRect;
    type IntoIter = std::iter::Chain<std::iter::Once<IndexRect>, std::option::IntoIter<IndexRect>>;

    fn into_iter(self) -> Self::IntoIter {
        std::iter::once(self.first).chain(self.second)
    }
}

/// The circular gap between two longitudes, in degrees `[0, 180]`.
fn lon_gap(a: f64, b: f64) -> f64 {
    let d = (a - b).abs() % 360.0;
    d.min(360.0 - d)
}

// ============================================================================
// IndexRect
// ============================================================================

/// A plain `min ≤ max` interval box in latitude/longitude — the only shape a
/// spatial-index dimension can store and compare. [`Viewport`] encodes an
/// antimeridian crossing as a westward span (`min_lon > max_lon` is
/// meaningful); stored raw, that convention reads as an empty interval
/// matching nothing. Keeping the index-row shape a separate type forces
/// every region and viewport through the seam split ([`Viewport::halves`], the
/// split rects of `cap_bounding_rects`) before it can touch the index — a
/// wrapping span can't silently become an unmatchable row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IndexRect {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
}

impl IndexRect {
    /// Whether this rect meets an axis-aligned query window, edge-inclusive —
    /// the interval test the sqlite rtree query performs, mirrored here so
    /// core's tests can pin the index-soundness invariant the SQL relies on.
    #[cfg(test)]
    pub(crate) fn intersects(
        &self,
        min_lat: f64,
        max_lat: f64,
        min_lon: f64,
        max_lon: f64,
    ) -> bool {
        self.min_lat <= max_lat
            && self.max_lat >= min_lat
            && self.min_lon <= max_lon
            && self.max_lon >= min_lon
    }

    /// Whether the rect holds `(lat, lon)`, inclusive on every edge — plain
    /// intervals on both axes, the no-wrap guarantee at work.
    fn contains_point(&self, lat: f64, lon: f64) -> bool {
        self.min_lat <= lat && lat <= self.max_lat && self.min_lon <= lon && lon <= self.max_lon
    }

    /// The in-interval longitude circularly nearest to `lon`: `lon` itself
    /// when inside, otherwise the circularly closer edge — a rect against
    /// the seam can be nearer the short way round it.
    fn nearest_lon(&self, lon: f64) -> f64 {
        if self.min_lon <= lon && lon <= self.max_lon {
            return lon;
        }
        if lon_gap(lon, self.min_lon) <= lon_gap(lon, self.max_lon) {
            self.min_lon
        } else {
            self.max_lon
        }
    }

    /// WGS84 distance from `p` to the nearest point of the rect, zero inside.
    /// The candidate boundary points are enumerated on the sphere and the
    /// returned distance to the closest is measured on WGS84 — a sub-meter
    /// approximation, since the true ellipsoidal nearest point drifts slightly
    /// from the sphere-derived one. Fixtures clear their viewport by kilometers,
    /// far above that drift, so the verdict is unaffected. The seam and the
    /// poles need no special cases beyond the circular nearest-longitude clamp.
    ///
    /// The nearest boundary point lies on one of the four edges. Along the
    /// north/south edges (fixed latitude) the distance is monotone in the
    /// longitude gap, so the circularly-nearest in-interval longitude is
    /// that edge's nearest point. Along the west/east edges (fixed
    /// longitude) the point-to-point cosine is `A·sin φ + B·cos φ` in the
    /// edge latitude `φ`; its interior stationary point sits at
    /// `atan2(A, B)`, and the segment maximum is there or at an endpoint, so
    /// those three candidates cover every case.
    fn geodesic_distance_to(&self, p: &GeoPoint) -> Meters {
        if self.contains_point(p.lat(), p.lon()) {
            return Meters(0.0);
        }
        let mut best = f64::INFINITY;
        let lon = self.nearest_lon(p.lon());
        for lat in [self.min_lat, self.max_lat] {
            best = best.min(p.distance(&GeoPoint::from_wgs84(lat, lon)).0);
        }
        let a = p.lat().to_radians().sin();
        for lon in [self.min_lon, self.max_lon] {
            let b = p.lat().to_radians().cos() * (p.lon() - lon).to_radians().cos();
            let stationary = a.atan2(b).to_degrees();
            for lat in [
                stationary.clamp(self.min_lat, self.max_lat),
                self.min_lat,
                self.max_lat,
            ] {
                best = best.min(p.distance(&GeoPoint::from_wgs84(lat, lon)).0);
            }
        }
        Meters(best)
    }
}

/// The bounding rects of a cap, a guaranteed superset of the WGS84 disk:
/// latitude padded by the shortest WGS84 meridian degree, longitude by
/// `asin(sin r / cos φ)` — the sphere's exact extremal longitude, which
/// over-covers the wider-radius ellipsoid. A cap reaching either pole spans
/// every longitude; a longitude span crossing the ±180° seam splits into two
/// rects, which also keeps a cap centered on the seam findable from both sides.
pub(crate) fn cap_bounding_rects(center: &GeoPoint, radius: Meters) -> Vec<IndexRect> {
    // Pad by the cap's effective radius — the distance the rim-inclusive
    // predicate accepts out to — so the rects and the predicate share one
    // rim definition.
    let cap = Circle::new(*center, radius);
    let r_m = cap.effective_radius().0;
    // Latitude: the disk's poleward extent is reached by a meridian geodesic, so
    // bound it by the shortest WGS84 meridian degree — the equatorial one,
    // `M(0)·π/180 = a(1−e²)·π/180`, derived from the ellipsoid the library ships.
    // That global minimum over-pads at every other latitude, so the rect stays a
    // guaranteed superset of the WGS84 predicate.
    let geod = Geodesic::wgs84();
    let f = geod.flattening();
    let min_meridian_m_per_deg =
        geod.equatorial_radius() * (1.0 - (2.0 * f - f * f)) * std::f64::consts::PI / 180.0;
    let r_deg = r_m / min_meridian_m_per_deg;
    let min_lat = center.lat() - r_deg;
    let max_lat = center.lat() + r_deg;
    if max_lat >= 90.0 || min_lat <= -90.0 {
        return vec![IndexRect {
            min_lat: min_lat.max(-90.0),
            max_lat: max_lat.min(90.0),
            min_lon: -180.0,
            max_lon: 180.0,
        }];
    }
    // Longitude: the sphere's parallel degree (radius 6371 km) is shorter than
    // WGS84's (prime-vertical radius 6378–6400 km), so the sphere's exact
    // extremal longitude `asin(sin r / cos φ)` over-covers the ellipsoid — the
    // conservative bound. Clear of the poles `sin r < cos φ` holds exactly; `min`
    // absorbs the last-ulp rounding so `asin` stays in domain.
    let r_rad = r_m / EARTH_RADIUS_M;
    let dlon = (r_rad.sin() / center.lat().to_radians().cos())
        .min(1.0)
        .asin()
        .to_degrees();
    let (min_lon, max_lon) = (center.lon() - dlon, center.lon() + dlon);
    if min_lon < -180.0 {
        vec![
            IndexRect {
                min_lat,
                max_lat,
                min_lon: min_lon + 360.0,
                max_lon: 180.0,
            },
            IndexRect {
                min_lat,
                max_lat,
                min_lon: -180.0,
                max_lon,
            },
        ]
    } else if max_lon > 180.0 {
        vec![
            IndexRect {
                min_lat,
                max_lat,
                min_lon,
                max_lon: 180.0,
            },
            IndexRect {
                min_lat,
                max_lat,
                min_lon: -180.0,
                max_lon: max_lon - 360.0,
            },
        ]
    } else {
        vec![IndexRect {
            min_lat,
            max_lat,
            min_lon,
            max_lon,
        }]
    }
}

// ============================================================================
// Meters
// ============================================================================

/// A meter-valued scalar: a WGS84 geodesic distance or a radius. Carrying the
/// unit in the type keeps `_m`-suffixed names off the call surface and stops a
/// raw `f64` being passed where a meter count is meant.
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
// Circle
// ============================================================================

/// A cap: every point within a meter radius of a center point, the WGS84 disk
/// [`crate::location::Location::Circle`] denotes. Compute-side only — never
/// serialized.
///
/// Membership is WGS84 at any radius up to a hemisphere, with no projection,
/// frame, or seam: [`covers`](Self::covers) compares the geodesic distance to
/// the radius within a tolerance, and
/// [`boundary_intersections`](Self::boundary_intersections) solves the
/// circle-circle crossing on the same ellipsoid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Circle {
    center: GeoPoint,
    radius: Meters,
}

/// Relative meter tolerance for cap membership and containment — absorbs the
/// rounding of the geodesic trig, scaled by the radius so a continental cap
/// gets proportionally more slack.
const CAP_TOL_REL: f64 = 1e-9;
/// Absolute meter floor for the tolerance, so a zero-radius cap (a point) still
/// admits its own center under rounding.
const CAP_TOL_ABS_M: f64 = 1e-3;

/// The WGS84 crossings of two geodesic circles `(c1, r1)` and `(c2, r2)` — the
/// points at distance `r1` from `c1` and `r2` from `c2`. Empty when the circles
/// are disjoint (`d > r1 + r2`), nested (`d < |r1 − r2|`), or concentric
/// (`d ≈ 0`); otherwise two points, coincident at exact tangency.
///
/// Each crossing lies on the `r1`-circle at bearing `azi12 + α` off the `c1→c2`
/// line, where `d` and `azi12` come from the ellipsoidal inverse solution. The
/// distance from that rim point to `c2` is monotone in `α` on each side of the
/// line — one root on `[0, +180°]`, one on `[0, −180°]` — and the ellipsoid
/// breaks the sphere's mirror symmetry, so each side is bisected on its own.
///
/// The one-root-per-side split rests on that monotonicity, which holds for the
/// sub-quarter-meridian radii the caps carry; only a rim near the quarter-
/// meridian — well past `MAX_UNCERTAINTY_RADIUS` — could fold and hide a second
/// root.
fn circle_crossings(c1: GeoPoint, r1: Meters, c2: GeoPoint, r2: Meters) -> Vec<GeoPoint> {
    let geod = Geodesic::wgs84();
    let (lat1, lon1) = (c1.lat(), c1.lon());
    let (lat2, lon2) = (c2.lat(), c2.lon());
    let (d, azi12, _, _): (f64, f64, f64, f64) = geod.inverse(lat1, lon1, lat2, lon2);
    if d > r1.0 + r2.0 || d < (r1.0 - r2.0).abs() || d < 1e-9 {
        return Vec::new();
    }
    // Signed miss of the rim point at bearing `azi12 + α`: its WGS84 distance to
    // `c2` less `r2`. Negative inside `c2`'s circle, positive outside.
    let g = |alpha_deg: f64| -> f64 {
        let (plat, plon): (f64, f64) = geod.direct(lat1, lon1, azi12 + alpha_deg, r1.0);
        let dist: f64 = geod.inverse(plat, plon, lat2, lon2);
        dist - r2.0
    };
    // Bisect the one root on `[lo, hi]`: `g(0) ≤ 0` and `g(±180) ≥ 0`, so the
    // invariant `g(lo) ≤ 0 < g(hi)` holds and 60 halvings pin the crossing.
    let solve = |mut lo: f64, mut hi: f64| -> GeoPoint {
        for _ in 0..60 {
            let mid = (lo + hi) * 0.5;
            if g(mid) <= 0.0 {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let (plat, plon): (f64, f64) = geod.direct(lat1, lon1, azi12 + (lo + hi) * 0.5, r1.0);
        GeoPoint::from_wgs84(plat, plon)
    };
    vec![solve(0.0, 180.0), solve(0.0, -180.0)]
}

impl Circle {
    /// A cap of the given meter radius about a center point.
    pub fn new(center: GeoPoint, radius: Meters) -> Self {
        Self { center, radius }
    }

    /// The cap's center — a candidate point for an emptiness witness.
    pub fn center(&self) -> GeoPoint {
        self.center
    }

    /// The cap's meter tolerance: a relative slop that grows with the radius,
    /// plus a fixed floor.
    fn tol(&self) -> Meters {
        Meters(self.radius.0 * CAP_TOL_REL + CAP_TOL_ABS_M)
    }

    /// Plain rim-inclusive membership: the geodesic distance from the center to
    /// `p` is within the cap's [`effective_radius`](Self::effective_radius) —
    /// the stored radius plus the tolerance.
    pub(crate) fn covers(&self, p: &GeoPoint) -> bool {
        self.covers_within(self.center.distance(p))
    }

    /// The distance the rim-inclusive membership tests accept out to: the
    /// stored radius plus the tolerance. [`covers_within`](Self::covers_within)
    /// compares against it and [`cap_bounding_rects`] pads by it — one
    /// definition, so a tolerance-band acceptance can never fall outside the
    /// stored rects.
    fn effective_radius(&self) -> Meters {
        Meters(self.radius.0 + self.tol().0)
    }

    /// Whether a point at `distance` from the cap's center lies in the cap —
    /// [`covers`](Self::covers) with the distance supplied by the caller, for
    /// geometry (nearest-point-of-a-region tests) that computes it elsewhere.
    pub(crate) fn covers_within(&self, distance: Meters) -> bool {
        distance.0 <= self.effective_radius().0
    }

    /// Whether this cap contains `other` entirely: the centers' separation plus
    /// `other`'s radius fits within this radius, up to the tolerance.
    pub fn contains(&self, other: &Circle) -> bool {
        let between = self.center.distance(&other.center).0;
        between + other.radius.0 <= self.radius.0 + self.tol().0
    }

    /// The WGS84 crossing points of two caps' bounding circles: zero (disjoint,
    /// nested, or concentric centers), or two — coincident at exact tangency.
    /// The two crossings are the corners of the lens-shaped overlap of the two
    /// caps. Delegates to [`circle_crossings`], the numeric ellipsoidal solve.
    pub fn boundary_intersections(&self, other: &Circle) -> Vec<GeoPoint> {
        circle_crossings(self.center, self.radius, other.center, other.radius)
    }

    /// The point on the geodesic between the two caps' centers where the signed
    /// depths past each rim balance — at distance `(d + r_self − r_other) / 2`
    /// from this center toward the other, clamped to the segment. It lies in
    /// both caps exactly when they overlap, so it witnesses an overlap whose
    /// rims meet only tangentially, the degenerate case
    /// [`boundary_intersections`](Self::boundary_intersections) returns nothing
    /// for. `None` for coincident centers, where the shared center already
    /// witnesses any overlap.
    pub fn balance_point(&self, other: &Circle) -> Option<GeoPoint> {
        let geod = Geodesic::wgs84();
        let (lat1, lon1) = (self.center.lat(), self.center.lon());
        let (lat2, lon2) = (other.center.lat(), other.center.lon());
        let (d, azi12, _, _): (f64, f64, f64, f64) = geod.inverse(lat1, lon1, lat2, lon2);
        if d < 1e-9 {
            return None;
        }
        let t = ((d + self.radius.0 - other.radius.0) * 0.5).clamp(0.0, d);
        let (plat, plon): (f64, f64) = geod.direct(lat1, lon1, azi12, t);
        Some(GeoPoint::from_wgs84(plat, plon))
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

    // --- Viewport tests ---

    #[test]
    fn viewport_accepts_valid() -> TestResult {
        let sw = GeoPoint::new(40.0, -74.0)?;
        let ne = GeoPoint::new(41.0, -73.0)?;
        let b = Viewport::new(sw, ne)?;
        assert_eq!(b.sw(), &sw);
        assert_eq!(b.ne(), &ne);
        Ok(())
    }

    #[test]
    fn viewport_from_coords_maps_corners_and_validates() -> TestResult {
        // (min_lat, max_lat, min_lon, max_lon) → sw=(min_lat,min_lon),
        // ne=(max_lat,max_lon). Pins the positional mapping so a swapped
        // lat/lon argument is caught.
        let b = Viewport::from_coords(40.0, 41.0, -74.0, -73.0)?;
        assert_eq!(b.sw(), &GeoPoint::new(40.0, -74.0)?);
        assert_eq!(b.ne(), &GeoPoint::new(41.0, -73.0)?);
        assert_eq!(
            (b.min_lat(), b.max_lat(), b.min_lon(), b.max_lon()),
            (40.0, 41.0, -74.0, -73.0)
        );
        // An out-of-range coordinate is rejected by the corner constructor.
        assert!(matches!(
            Viewport::from_coords(0.0, 1.0, 0.0, 200.0),
            Err(ViewportCoordsError::Northeast(
                GeoPointError::LongitudeOutOfRange { .. }
            ))
        ));
        // Inverted latitude is rejected by `Viewport::new`.
        assert!(matches!(
            Viewport::from_coords(50.0, 40.0, 0.0, 1.0),
            Err(ViewportCoordsError::Viewport(
                ViewportError::LatitudeInverted { .. }
            ))
        ));
        Ok(())
    }

    #[test]
    fn viewport_rejects_sw_north_of_ne() -> TestResult {
        // sw.lat (40) > ne.lat (30) — sw is north of ne.
        let sw = GeoPoint::new(40.0, 0.0)?;
        let ne = GeoPoint::new(30.0, 1.0)?;
        assert!(matches!(
            Viewport::new(sw, ne),
            Err(ViewportError::LatitudeInverted { .. })
        ));
        Ok(())
    }

    #[test]
    fn viewport_accepts_westward_longitude_as_antimeridian_wrap() -> TestResult {
        // sw.lon (170) > ne.lon (-170): a 20°-wide box straddling the
        // antimeridian, so construction accepts it — the wrap convention, not
        // a transposition.
        let sw = GeoPoint::new(0.0, 170.0)?;
        let ne = GeoPoint::new(1.0, -170.0)?;
        let b = Viewport::new(sw, ne)?;
        assert_eq!(b.sw(), &sw);
        assert_eq!(b.ne(), &ne);
        Ok(())
    }

    #[test]
    fn viewport_contains_is_edge_inclusive() -> TestResult {
        let b = Viewport::new(GeoPoint::new(40.0, -74.0)?, GeoPoint::new(41.0, -73.0)?)?;
        assert!(b.contains(&GeoPoint::new(40.5, -73.5)?), "interior point");
        assert!(b.contains(&GeoPoint::new(40.0, -74.0)?), "sw corner is in");
        assert!(b.contains(&GeoPoint::new(41.0, -73.0)?), "ne corner is in");
        assert!(
            !b.contains(&GeoPoint::new(41.5, -73.5)?),
            "north of the box"
        );
        assert!(!b.contains(&GeoPoint::new(40.5, -72.5)?), "east of the box");
        Ok(())
    }

    #[test]
    fn viewport_contains_wrapped_box_spans_the_seam() -> TestResult {
        // A box from 170°E eastward across ±180° to 170°W (a 20° span).
        let b = Viewport::new(GeoPoint::new(0.0, 170.0)?, GeoPoint::new(10.0, -170.0)?)?;
        assert!(b.contains(&GeoPoint::new(5.0, 175.0)?), "175°E is inside");
        assert!(b.contains(&GeoPoint::new(5.0, -175.0)?), "175°W is inside");
        assert!(
            b.contains(&GeoPoint::new(5.0, 180.0)?),
            "the seam itself is inside"
        );
        assert!(
            b.contains(&GeoPoint::new(5.0, 170.0)?),
            "the west edge is inclusive"
        );
        assert!(
            b.contains(&GeoPoint::new(5.0, -170.0)?),
            "the east edge is inclusive"
        );
        assert!(
            !b.contains(&GeoPoint::new(5.0, 0.0)?),
            "the far meridian is outside"
        );
        assert!(
            !b.contains(&GeoPoint::new(5.0, 160.0)?),
            "just west of the box is outside"
        );
        assert!(
            !b.contains(&GeoPoint::new(15.0, 175.0)?),
            "north of the box is outside even at an in-range longitude"
        );
        Ok(())
    }

    // --- Circle tests ---
    //
    // These exercise the regimes a `Location` proptest can't reach: the
    // `MAX_UNCERTAINTY_RADIUS` cap bounds radii through `Location::circle`, but
    // `Circle::new` is uncapped, so continental and near-hemisphere caps,
    // high latitude, and antimeridian straddles all live here. Each would be
    // wrong under a planar projection — the seam wrap, the `cos(lat)` collapse,
    // and the flat-distance error at large radius are exactly what the geodesic
    // model removes.

    fn cap(lat: f64, lon: f64, radius_m: f64) -> Result<Circle, GeoPointError> {
        Ok(Circle::new(GeoPoint::new(lat, lon)?, Meters(radius_m)))
    }

    #[test]
    fn distance_matches_known_arc() -> TestResult {
        // One degree of latitude along the equatorial meridian is ~110.57 km on
        // WGS84 — the ellipsoid's shortest meridian degree, notably under the
        // sphere's uniform 111.19 km.
        let a = GeoPoint::new(0.0, 0.0)?;
        let b = GeoPoint::new(1.0, 0.0)?;
        let d = a.distance(&b).0;
        assert!((d - 110_574.4).abs() < 1.0, "got {d}");
        Ok(())
    }

    #[test]
    fn distance_antipodal_is_half_circumference_no_nan() -> TestResult {
        // Karney's inverse solution must stay total at the near-antipodal
        // degenerate — the regime where naive great-circle code returns NaN.
        let a = GeoPoint::new(0.0, 0.0)?;
        let b = GeoPoint::new(0.0, 180.0)?;
        let d = a.distance(&b).0;
        assert!(d.is_finite(), "antipodal distance must be finite");
        // Equatorial antipodes connect over the pole on an oblate ellipsoid, so
        // this is the pole-to-pole (half-meridian) distance, not the equatorial
        // half-circumference (π·a ≈ 20 037 508 m).
        assert!((d - 20_003_931.5).abs() < 1.0, "got {d}");
        Ok(())
    }

    #[test]
    fn distance_coincident_is_zero() -> TestResult {
        let a = GeoPoint::new(12.3, 45.6)?;
        assert!(a.distance(&a).0.abs() < 1e-6);
        Ok(())
    }

    #[test]
    fn cap_covers_antimeridian_straddle() -> TestResult {
        // A continental cap centered just west of the antimeridian covers a
        // point just east of it — the two are physically ~22 km apart. A planar
        // `lon − lon0` projection would place them ~40000 km apart and miss.
        let c = cap(0.0, 179.9, 50_000.0)?;
        let east = GeoPoint::new(0.0, -179.9)?;
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
        let across = GeoPoint::new(89.99, 180.0)?;
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
        let near_edge = GeoPoint::new(0.0, 80.0)?;
        let past_rim = GeoPoint::new(0.0, 100.0)?;
        assert!(c.covers(&near_edge), "point inside the ~81° rim must be in");
        assert!(!c.covers(&past_rim), "point past the rim must be out");
        Ok(())
    }

    #[test]
    fn cap_rim_point_is_inclusive() -> TestResult {
        // A point exactly one cap-radius north of the center, placed by the
        // WGS84 direct geodesic so it lands on the rim the ellipsoidal `covers`
        // measures — inclusive under the tolerance.
        let c = cap(40.0, -74.0, 100_000.0)?;
        let (lat, lon): (f64, f64) = Geodesic::wgs84().direct(40.0, -74.0, 0.0, 100_000.0);
        let rim = GeoPoint::new(lat, lon)?;
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
        // Two caps ~100 km apart with 80 km radii cross at two points. The
        // crossings are WGS84-exact, so each sits on both caps' rims.
        let r = 80_000.0;
        let a = cap(40.0, -74.0, r)?;
        let b = cap(40.9, -74.0, r)?; // ~100 km north
        let pts = a.boundary_intersections(&b);
        assert_eq!(pts.len(), 2, "overlapping caps meet at two points");
        for p in &pts {
            assert!((a.center().distance(p).0 - r).abs() < 1e-2, "on a's rim");
            assert!((b.center().distance(p).0 - r).abs() < 1e-2, "on b's rim");
        }
        Ok(())
    }

    #[test]
    fn cap_small_close_overlap_meets_at_two_points() -> TestResult {
        // Two 5 km caps ~9 km apart — the small-and-close regime real places sit
        // in, where the centers are nearly collinear with the crossings. The
        // WGS84 solve keeps the two genuine crossings, each on both rims.
        let r = 5_000.0;
        let a = cap(0.0, -0.0405, r)?;
        let b = cap(0.0, 0.0405, r)?; // ~9 km east, overlap margin ~1 km
        let pts = a.boundary_intersections(&b);
        assert_eq!(pts.len(), 2, "small overlapping caps meet at two points");
        for p in &pts {
            assert!((a.center().distance(p).0 - r).abs() < 1e-2, "on a's rim");
            assert!((b.center().distance(p).0 - r).abs() < 1e-2, "on b's rim");
        }
        Ok(())
    }

    #[test]
    fn cap_tangent_meets_at_a_single_locus() -> TestResult {
        // Near-external tangency from the WGS84 center separation: set the second
        // radius so the rims overlap by a sliver (1 nm), comfortably above the
        // float noise of the distance solve so a crossing reliably exists, yet
        // small enough that the two crossings collapse toward the single tangent
        // locus — within centimeters on these ~80 km rims.
        let center_a = GeoPoint::new(40.0, -74.0)?;
        let center_b = GeoPoint::new(41.5, -74.0)?;
        let d = center_a.distance(&center_b).0;
        let r_a = 80_000.0;
        let a = Circle::new(center_a, Meters(r_a));
        let b = Circle::new(center_b, Meters(d - r_a + 1e-9));
        let pts = a.boundary_intersections(&b);
        assert_eq!(pts.len(), 2, "near-tangent caps meet");
        assert!(
            pts[0].distance(&pts[1]).0 < 1e-1,
            "the two crossings collapse toward the tangent locus"
        );
        Ok(())
    }

    #[test]
    fn cap_strictly_disjoint_with_gap_has_no_intersection() -> TestResult {
        // Centres ~200 km apart, radii 80 km each: the center separation exceeds
        // the radius sum, so the rims never meet and nothing is returned.
        let a = cap(40.0, -74.0, 80_000.0)?;
        let b = cap(40.0 + 1.8, -74.0, 80_000.0)?; // ~200 km north
        assert!(a.boundary_intersections(&b).is_empty());
        Ok(())
    }

    // --- Viewport::geodesic_distance_to ---

    /// lat `[40, 41]`, lon `[-74, -73]` — the spatial fixtures' box.
    fn sample_box() -> Result<Viewport, Box<dyn std::error::Error>> {
        Ok(Viewport::new(
            GeoPoint::new(40.0, -74.0)?,
            GeoPoint::new(41.0, -73.0)?,
        )?)
    }

    #[test]
    fn viewport_distance_zero_inside_and_on_edges() -> TestResult {
        let b = sample_box()?;
        assert_eq!(b.geodesic_distance_to(&GeoPoint::new(40.5, -73.5)?).0, 0.0);
        assert_eq!(b.geodesic_distance_to(&GeoPoint::new(40.0, -74.0)?).0, 0.0);
        Ok(())
    }

    #[test]
    fn viewport_distance_due_east_hits_the_meridian_edge() -> TestResult {
        // 0.5° east of the east edge at the box's mid latitude: the nearest
        // point sits on the lon = -73 edge at ~the same latitude, ≈ 0.5° of
        // longitude at 40.5°N ≈ 42.4 km on Geodesic::wgs84().
        let b = sample_box()?;
        let d = b.geodesic_distance_to(&GeoPoint::new(40.5, -72.5)?).0;
        let expect: f64 = Geodesic::wgs84().inverse(40.5, -72.5, 40.5, -73.0);
        assert!((d - expect).abs() < 100.0, "got {d}, expected ≈{expect}");
        Ok(())
    }

    #[test]
    fn viewport_distance_due_north_hits_the_parallel_edge() -> TestResult {
        // 1° north of the north edge, longitude inside the arc: one degree of
        // WGS84 meridian arc at ~41°N ≈ 111.06 km.
        let b = sample_box()?;
        let d = b.geodesic_distance_to(&GeoPoint::new(42.0, -73.5)?).0;
        let expect: f64 = Geodesic::wgs84().inverse(42.0, -73.5, 41.0, -73.5);
        assert!((d - expect).abs() < 1.0, "got {d}, expected ≈{expect}");
        Ok(())
    }

    #[test]
    fn viewport_distance_to_corner_from_the_diagonal() -> TestResult {
        // Northeast of the box on both axes: the nearest point is the NE
        // corner itself.
        let b = sample_box()?;
        let p = GeoPoint::new(42.0, -72.0)?;
        let corner = GeoPoint::new(41.0, -73.0)?;
        let d = b.geodesic_distance_to(&p).0;
        let direct = p.distance(&corner).0;
        assert!((d - direct).abs() < 1.0, "got {d}, corner at {direct}");
        Ok(())
    }

    #[test]
    fn viewport_distance_crosses_the_antimeridian_seam() -> TestResult {
        // A wrapped box `[170°E .. 170°W]`; a point at 168°E is 2° of
        // longitude from the west edge, not 358° the long way round.
        let b = Viewport::new(GeoPoint::new(0.0, 170.0)?, GeoPoint::new(10.0, -170.0)?)?;
        let d = b.geodesic_distance_to(&GeoPoint::new(5.0, 168.0)?).0;
        // Nearest point is (5, 170) on the west edge — 2° of longitude at 5°N.
        let expect: f64 = Geodesic::wgs84().inverse(5.0, 168.0, 5.0, 170.0);
        assert!((d - expect).abs() < 100.0, "got {d}, expected ≈{expect}");
        // A point just across the seam is inside.
        assert_eq!(b.geodesic_distance_to(&GeoPoint::new(5.0, -175.0)?).0, 0.0);
        Ok(())
    }

    #[test]
    fn viewport_distance_reaches_over_the_pole() -> TestResult {
        // A box touching the north pole; a point on the far side of the pole
        // at a distant longitude is ~0.5° (≈ 55.6 km) away through the pole,
        // not a quarter of the globe around the parallel.
        let b = Viewport::new(GeoPoint::new(89.0, 0.0)?, GeoPoint::new(90.0, 10.0)?)?;
        let d = b.geodesic_distance_to(&GeoPoint::new(89.5, 170.0)?).0;
        // The pole corner (90, 10) is the nearest point: 0.5° of meridian
        // through the pole, ≈ 55.8 km on Geodesic::wgs84().
        let expect: f64 = Geodesic::wgs84().inverse(89.5, 170.0, 90.0, 10.0);
        assert!((d - expect).abs() < 100.0, "got {d}, expected ≈{expect}");
        Ok(())
    }

    #[test]
    fn viewport_distance_equatorial_point_far_meridian_edge() -> TestResult {
        // An equator point 110° of longitude from the box: past 90° of
        // separation a higher latitude is *closer* on the ellipsoid, so the
        // nearest point is the (1, 10) corner, not the equatorial one — the
        // regime where the meridian-edge stationary latitude leaves
        // `[-90, 90]` and an endpoint must win.
        let b = Viewport::new(GeoPoint::new(0.0, 0.0)?, GeoPoint::new(1.0, 10.0)?)?;
        let p = GeoPoint::new(0.0, 120.0)?;
        let ne_corner = GeoPoint::new(1.0, 10.0)?;
        let equatorial = GeoPoint::new(0.0, 10.0)?;
        let d = b.geodesic_distance_to(&p).0;
        let direct = p.distance(&ne_corner).0;
        assert!(
            direct < p.distance(&equatorial).0,
            "past 90° the higher corner is closer"
        );
        assert!((d - direct).abs() < 1.0, "got {d}, corner at {direct}");
        Ok(())
    }

    // --- cap_bounding_rects ---

    /// The rect set covers a query window iff some rect meets it.
    fn rects_hit(
        rects: &[IndexRect],
        min_lat: f64,
        max_lat: f64,
        min_lon: f64,
        max_lon: f64,
    ) -> bool {
        rects
            .iter()
            .any(|r| r.intersects(min_lat, max_lat, min_lon, max_lon))
    }

    #[test]
    fn cap_rects_mid_latitude_padded_by_wgs84_meridian() -> TestResult {
        let rects = cap_bounding_rects(&GeoPoint::new(60.0, 10.0)?, Meters(100_000.0));
        assert_eq!(rects.len(), 1);
        let r = rects[0];
        // The poleward edges cover the disk's true meridian extent (the due
        // north/south geodesic endpoint) and sit just beyond it — the equatorial
        // meridian degree over-pads at 60°N by under 0.01°.
        let (north_lat, _): (f64, f64) = Geodesic::wgs84().direct(60.0, 10.0, 0.0, 100_000.0);
        let (south_lat, _): (f64, f64) = Geodesic::wgs84().direct(60.0, 10.0, 180.0, 100_000.0);
        assert!(r.max_lat >= north_lat && r.max_lat - north_lat < 0.01);
        assert!(r.min_lat <= south_lat && south_lat - r.min_lat < 0.01);
        // Longitude keeps the sphere's exact extremal `asin(sin r / cos φ)`,
        // wider than the latitude pad by the ~1/cos 60° factor.
        let want_dlon = ((100_000.0 / EARTH_RADIUS_M).sin() / 60.0_f64.to_radians().cos())
            .asin()
            .to_degrees();
        assert!((r.max_lon - (10.0 + want_dlon)).abs() < 1e-6);
        assert!(
            r.max_lat - 60.0 < want_dlon,
            "longitude pad is wider at 60°N"
        );
        Ok(())
    }

    #[test]
    fn cap_rects_split_at_the_antimeridian() -> TestResult {
        let rects = cap_bounding_rects(&GeoPoint::new(0.0, 179.5)?, Meters(100_000.0));
        assert_eq!(rects.len(), 2, "seam-crossing cap covers both sides");
        // One half ends at 180, the other starts at -180.
        assert!(
            rects
                .iter()
                .any(|r| r.max_lon == 180.0 && r.min_lon > 178.0)
        );
        assert!(
            rects
                .iter()
                .any(|r| r.min_lon == -180.0 && r.max_lon < -179.0)
        );
        // Both sides of the seam are findable with plain interval queries.
        assert!(rects_hit(&rects, -1.0, 1.0, 179.0, 180.0));
        assert!(rects_hit(&rects, -1.0, 1.0, -180.0, -179.4));
        assert!(!rects_hit(&rects, -1.0, 1.0, 0.0, 10.0));
        Ok(())
    }

    #[test]
    fn cap_rects_over_the_pole_span_every_longitude() -> TestResult {
        let rects = cap_bounding_rects(&GeoPoint::new(89.5, 42.0)?, Meters(100_000.0));
        assert_eq!(rects.len(), 1);
        let r = rects[0];
        assert_eq!((r.min_lon, r.max_lon), (-180.0, 180.0));
        assert_eq!(r.max_lat, 90.0, "clamped at the pole");
        assert!(r.min_lat < 89.0);
        // Findable from any longitude near the pole.
        assert!(rects_hit(&rects, 89.0, 90.0, -170.0, -160.0));
        Ok(())
    }
}
