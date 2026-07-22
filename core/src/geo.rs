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
//! - [`QuadLevel`], [`quadkey`], [`QuadTileRange`], and [`viewport_tiles`] are
//!   the web-mercator quadkey facet: a location's 48-bit Morton (z-order) code
//!   and the Morton tile ranges a viewport spans at a chosen tile level.
//! - [`Meters`] is a meter-valued scalar — a WGS84 geodesic distance or radius.
//! - `Circle` is the compute-side primitive: a cap (disk) about a
//!   [`GeoPoint`] center. Distance, membership, and the circle-circle boundary
//!   crossing all solve on WGS84 via [`geographiclib_rs`]
//!   (`Circle::boundary_intersections`). It is never serialized.

use geographiclib_rs::{DirectGeodesic, Geodesic, InverseGeodesic};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::finite::Finite;
use crate::location::UnresolvedLocation;

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
/// The `lat`/`lon` fields are `Finite`: finite by
/// construction with `-0.0` normalized, so `Eq`/`Hash`/`Ord` derive honestly —
/// the total ordering `BTreeSet<SubmitFact>` relies on, with no `f64`
/// knife-edges to hand-handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, JsonSchema)]
pub struct GeoPoint {
    lat: Finite,
    lon: Finite,
}

impl GeoPoint {
    /// Construct a geo-point. `lat` must lie in `[-90, 90]`, `lon` in
    /// `[-180, 180]`, and both must be finite. `Finite` normalizes `-0.0`
    /// to `+0.0`.
    pub fn new(lat: f64, lon: f64) -> Result<Self, GeoPointError> {
        let lat_v = Finite::new(lat).ok_or(GeoPointError::NotFinite { lat, lon })?;
        let lon_v = Finite::new(lon).ok_or(GeoPointError::NotFinite { lat, lon })?;
        if !(-90.0..=90.0).contains(&lat) {
            return Err(GeoPointError::LatitudeOutOfRange { lat });
        }
        if !(-180.0..=180.0).contains(&lon) {
            return Err(GeoPointError::LongitudeOutOfRange { lon });
        }
        Ok(Self {
            lat: lat_v,
            lon: lon_v,
        })
    }

    /// Latitude in degrees, in `[-90, 90]`.
    pub fn lat(&self) -> f64 {
        self.lat.get()
    }

    /// Longitude in degrees, in `[-180, 180]`.
    pub fn lon(&self) -> f64 {
        self.lon.get()
    }

    /// The WGS84 geodesic distance to another point (Karney's inverse solution).
    fn distance(&self, other: &GeoPoint) -> Meters {
        Meters::new_unchecked(Geodesic::wgs84().inverse(
            self.lat.get(),
            self.lon.get(),
            other.lat.get(),
            other.lon.get(),
        ))
    }

    /// A point minted from a WGS84 geodesic solve (`direct`/`inverse` outputs),
    /// which geographiclib already returns in-range; the clamp absorbs last-ulp
    /// drift so this is total without a fallible check in the solver hot paths.
    fn from_wgs84(lat: f64, lon: f64) -> Self {
        // Finiteness is the geodesic solver's promise; check the raw output,
        // since the clamp below maps only last-ulp range drift.
        debug_assert!(
            lat.is_finite() && lon.is_finite(),
            "geodesic output must be finite: ({lat}, {lon})"
        );
        Self {
            lat: Finite::new_unchecked(lat.clamp(-90.0, 90.0)),
            lon: Finite::new_unchecked(lon.clamp(-180.0, 180.0)),
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
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum GeoPointError {
    /// One or more coordinates were not finite.
    #[error("geo-point coordinates must be finite, got lat={lat}, lon={lon}")]
    NotFinite { lat: f64, lon: f64 },
    /// `lat` was outside `[-90, 90]`.
    #[error("latitude must be in [-90, 90], got {lat}")]
    LatitudeOutOfRange { lat: f64 },
    /// `lon` was outside `[-180, 180]`.
    #[error("longitude must be in [-180, 180], got {lon}")]
    LongitudeOutOfRange { lon: f64 },
}

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
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ViewportError {
    /// `sw`'s latitude exceeds `ne`'s — the box is inverted north-to-south.
    /// Longitude ordering is free: `sw.lon() > ne.lon()` denotes an
    /// antimeridian-wrapping box (see [`Viewport`]).
    #[error(
        "viewport sw latitude ({}) must be at or below ne latitude ({})",
        .sw.lat(),
        .ne.lat(),
    )]
    LatitudeInverted {
        /// The southwest corner as supplied.
        sw: GeoPoint,
        /// The northeast corner as supplied.
        ne: GeoPoint,
    },
}

/// Errors from [`Viewport::from_coords`]: a corner coordinate out of range or
/// non-finite (from [`GeoPoint::new`]), or a latitude-inverted corner pair
/// (from [`Viewport::new`]).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ViewportCoordsError {
    /// The southwest corner's latitude or longitude failed [`GeoPoint::new`].
    #[error("southwest viewport corner: {0}")]
    Southwest(#[source] GeoPointError),
    /// The northeast corner's latitude or longitude failed [`GeoPoint::new`].
    #[error("northeast viewport corner: {0}")]
    Northeast(#[source] GeoPointError),
    /// The corner pair was latitude-inverted (see [`ViewportError`]).
    #[error(transparent)]
    Viewport(#[from] ViewportError),
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
            best = best.min(half.geodesic_distance_to(p).get());
        }
        Meters::new_unchecked(best)
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
            return Meters::new_unchecked(0.0);
        }
        let mut best = f64::INFINITY;
        let lon = self.nearest_lon(p.lon());
        for lat in [self.min_lat, self.max_lat] {
            best = best.min(p.distance(&GeoPoint::from_wgs84(lat, lon)).get());
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
                best = best.min(p.distance(&GeoPoint::from_wgs84(lat, lon)).get());
            }
        }
        Meters::new_unchecked(best)
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
    let r_m = cap.effective_radius().get();
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
// Quadkey / web-mercator tiling
// ============================================================================

/// Depth of the web-mercator tile pyramid the quadkey facet is built on:
/// `2^24` tiles per axis (~2.4 m at the equator), so a [`quadkey`] is a 48-bit
/// Morton code with room to spare inside an `i64`.
const MAX_QUAD_LEVEL: u8 = 24;

/// The web-mercator latitude cutoff `atan(sinh(π))`. The projection runs to
/// infinity at the poles, so the standard slippy-map tiling clamps here to keep
/// the unit square square.
const MERCATOR_LAT_LIMIT: f64 = 85.051_128_779_806_59;

/// The largest `f64` below `1.0`. Mercator coordinates live in the half-open
/// unit square `[0, 1)`, so clamping to this keeps `floor(u · 2^level)` inside
/// `[0, 2^level)` at the `lon = 180°` seam. (`f64::EPSILON` is one ulp at
/// `1.0`; half of it is the step down to the predecessor.)
const UNIT_MAX: f64 = 1.0 - f64::EPSILON / 2.0;

/// A web-mercator tile-pyramid level, `0..=MAX_QUAD_LEVEL`. Level `0` is the
/// whole world in one tile; each step down halves the tile on each axis.
///
/// Internal to coordinate math — no wire surface. The API validates a raw
/// `u8` into one at its boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct QuadLevel(u8);

/// Errors from [`QuadLevel::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuadLevelError {
    /// The level exceeded `MAX_QUAD_LEVEL`.
    TooDeep {
        /// The rejected level.
        level: u8,
    },
}

impl std::fmt::Display for QuadLevelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooDeep { level } => {
                write!(
                    f,
                    "quad level must be in [0, {MAX_QUAD_LEVEL}], got {level}"
                )
            }
        }
    }
}

impl std::error::Error for QuadLevelError {}

impl QuadLevel {
    /// Construct a level. Rejects `level > MAX_QUAD_LEVEL`, past which the tile
    /// grid outgrows the 48-bit Morton code [`quadkey`] promises.
    pub fn new(level: u8) -> Result<Self, QuadLevelError> {
        if level > MAX_QUAD_LEVEL {
            return Err(QuadLevelError::TooDeep { level });
        }
        Ok(Self(level))
    }

    /// A level clamped into `0..=MAX_QUAD_LEVEL` — the total constructor for
    /// arithmetic that already proves its result in range (the level searches
    /// and the split-level derivation), so there's no error to thread.
    pub const fn saturating(level: u8) -> Self {
        Self(if level > MAX_QUAD_LEVEL {
            MAX_QUAD_LEVEL
        } else {
            level
        })
    }

    /// The level as a raw depth in `0..=MAX_QUAD_LEVEL`.
    pub fn get(&self) -> u8 {
        self.0
    }
}

/// Web-mercator x for a longitude, in `[0, 1)`. Linear in `lon`, so it carries
/// no latitude.
pub fn mercator_x(lon: f64) -> f64 {
    clamp_unit((lon + 180.0) / 360.0)
}

/// Web-mercator y for a latitude, in `[0, 1)` and increasing southward.
/// Latitude is clamped to the mercator limit first, keeping the `tan`/`sec`
/// terms finite near the poles.
pub fn mercator_y(lat: f64) -> f64 {
    let lat_rad = lat
        .clamp(-MERCATOR_LAT_LIMIT, MERCATOR_LAT_LIMIT)
        .to_radians();
    let y = (1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0;
    clamp_unit(y)
}

/// Hold a coordinate inside the half-open unit interval `[0, 1)`.
fn clamp_unit(u: f64) -> f64 {
    u.clamp(0.0, UNIT_MAX)
}

/// Web-mercator projection of a point into the `[0, 1)` unit square: `x`
/// eastward from the antimeridian, `y` southward from the north limit.
fn mercator_unit(point: &GeoPoint) -> (f64, f64) {
    (mercator_x(point.lon()), mercator_y(point.lat()))
}

/// Longitude of a linear web-mercator x, inverting [`mercator_x`]: `lon = x·360
/// − 180`. An `x` in `[0, 1)` maps back into `[-180, 180)`.
pub fn mercator_x_to_lon(x: f64) -> f64 {
    x * 360.0 - 180.0
}

/// Latitude of a web-mercator y, inverting [`mercator_y`]. With `y = (1 −
/// asinh(tan φ)/π)/2`, the inverse is `φ = atan(sinh(π·(1 − 2y)))`; the result is
/// clamped to the mercator limit so the `[0, 1)` edges round-trip to the same
/// ±85.05° latitude `mercator_y` folds the poles onto.
pub fn mercator_y_to_lat(y: f64) -> f64 {
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * y))
        .sinh()
        .atan()
        .to_degrees();
    lat.clamp(-MERCATOR_LAT_LIMIT, MERCATOR_LAT_LIMIT)
}

/// The tile index along one axis at `level` for a unit coordinate, held in
/// `[0, 2^level - 1]`. A coordinate on the eastern seam sits at `u → 1.0`;
/// clamping — never a modulo — keeps it in the last tile instead of wrapping
/// back to tile `0`.
fn unit_to_tile(unit: f64, level: u8) -> u32 {
    let tiles = 1u32 << level;
    (unit * f64::from(tiles))
        .floor()
        .clamp(0.0, f64::from(tiles - 1)) as u32
}

/// The Morton (z-order) code of a location at `MAX_QUAD_LEVEL`, the single
/// source of the stored quadkey. Both the sqlite column and the in-memory
/// backend project through this one function, so their keys never drift.
pub fn quadkey(point: &GeoPoint) -> i64 {
    let (x, y) = mercator_unit(point);
    let tx = unit_to_tile(x, MAX_QUAD_LEVEL);
    let ty = unit_to_tile(y, MAX_QUAD_LEVEL);
    morton_encode(tx, ty)
}

/// A location's clustering key: the [`quadkey`] of the single point it pins,
/// or `None` when it pins none. Only a resolved circle pins one — its center;
/// a combinator or symbolic reference denotes a region, so it returns `None`.
///
/// The one decision "which locations cluster, and where" — the write-path
/// quadkey column and the in-memory backend both key through here, so the
/// stored key and the in-memory key can't drift, and extent-based keying for
/// coarse locations slots in at this one seam.
pub fn quadkey_of_location(location: &UnresolvedLocation) -> Option<i64> {
    location.point().map(quadkey)
}

/// The Morton prefix identifying which tile at `level` a max-level [`quadkey`]
/// falls in — the high `2·level` bits of the 48-bit code, the low interior bits
/// dropped. Two quadkeys share a tile at `level` iff their prefixes match, so
/// this is the bucket key the per-sub-tile clustering fold groups on. The
/// quadkey is a non-negative 48-bit value, so the arithmetic shift is a logical
/// one.
pub fn quadkey_tile_prefix(quadkey: i64, level: QuadLevel) -> i64 {
    let shift = 2 * u32::from(MAX_QUAD_LEVEL - level.get());
    quadkey >> shift
}

/// The finest level at which a set of located facts subdivides into more than
/// one tile. Pass the minimum and maximum of the distinct max-level
/// [`quadkey`]s; a sorted set's longest common Morton prefix is the prefix its
/// two extremes share, so those two codes decide the split.
///
/// Computed over the *surviving* quadkeys, so a top-N truncation that drops
/// codes only shrinks their spread — biasing the split finer, never coarser.
/// A client refetching at the returned level still finds the tile subdivided.
pub fn split_level(min_quadkey: i64, max_quadkey: i64) -> QuadLevel {
    let diff = (min_quadkey ^ max_quadkey) as u64;
    let level = if diff == 0 {
        MAX_QUAD_LEVEL
    } else {
        // The code is 48-bit inside a 64-bit word, so `leading_zeros` counts 16
        // padding bits before the real prefix: the shared prefix is
        // `P = leading_zeros - 16` bits. Two interleaved lanes share a tile down
        // to `floor(P/2)` and first separate one level deeper.
        let common_bits = diff.leading_zeros().saturating_sub(16);
        ((common_bits / 2) + 1).min(u32::from(MAX_QUAD_LEVEL)) as u8
    };
    QuadLevel::saturating(level)
}

/// Spread the low 24 bits of `v` into even bit positions of a 48-bit field,
/// zeros interleaved — the x-lane of a Morton code (y rides the odd bits, one
/// step left).
fn spread_even(v: u32) -> u64 {
    let mut x = u64::from(v);
    x = (x | (x << 16)) & 0x0000_ffff_0000_ffff;
    x = (x | (x << 8)) & 0x00ff_00ff_00ff_00ff;
    x = (x | (x << 4)) & 0x0f0f_0f0f_0f0f_0f0f;
    x = (x | (x << 2)) & 0x3333_3333_3333_3333;
    (x | (x << 1)) & 0x5555_5555_5555_5555
}

/// Interleave two tile indices into a 48-bit Morton code: `x` on even bits,
/// `y` on odd. The result is non-negative and fits an `i64` with 15 bits to
/// spare.
fn morton_encode(x: u32, y: u32) -> i64 {
    (spread_even(x) | (spread_even(y) << 1)) as i64
}

/// Gather the even bits of `z` back into a dense integer — the inverse of
/// [`spread_even`].
#[cfg(test)]
fn compact_even(z: u64) -> u32 {
    let mut x = z & 0x5555_5555_5555_5555;
    x = (x | (x >> 1)) & 0x3333_3333_3333_3333;
    x = (x | (x >> 2)) & 0x0f0f_0f0f_0f0f_0f0f;
    x = (x | (x >> 4)) & 0x00ff_00ff_00ff_00ff;
    x = (x | (x >> 8)) & 0x0000_ffff_0000_ffff;
    x = (x | (x >> 16)) & 0x0000_0000_ffff_ffff;
    x as u32
}

/// Split a Morton code back into its `(x, y)` tile indices — the exact inverse
/// of [`morton_encode`], for pinning the encode round-trip.
#[cfg(test)]
fn morton_decode(z: i64) -> (u32, u32) {
    let z = z as u64;
    (compact_even(z), compact_even(z >> 1))
}

/// The inclusive Morton range `[lo, hi]` a single tile covers on the quadkey
/// index. A power-of-two-aligned tile is a contiguous Morton block, so one
/// `BETWEEN lo AND hi` scan selects exactly its keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuadTileRange {
    /// Lowest Morton code in the tile.
    pub lo: i64,
    /// Highest Morton code in the tile.
    pub hi: i64,
}

impl QuadTileRange {
    /// The Morton range of tile `(x, y)` at `level`. The tile's low
    /// `2·(MAX_QUAD_LEVEL − level)` Morton bits range over its interior, so
    /// setting them fills `hi` out from `lo`.
    ///
    /// Internal — callers reach it either through [`viewport_tiles`], which
    /// derives `(x, y)` from the projection, or through [`TileId`], whose smart
    /// constructor has already proved the indices name a real tile. A raw
    /// off-grid `x`/`y` here would shift past the 48-bit Morton field and yield a
    /// garbage range, which is why [`TileId::new`] is the sole entry for a
    /// client-supplied coordinate.
    fn for_tile(x: u32, y: u32, level: QuadLevel) -> Self {
        let shift = MAX_QUAD_LEVEL - level.get();
        let lo = morton_encode(x << shift, y << shift);
        let span = (1i64 << (2 * u32::from(shift))) - 1;
        Self { lo, hi: lo | span }
    }
}

/// A safety backstop on [`TileId::child_ranges`]'s fan-out depth: even an
/// accidental deep `depth` yields at most `4^MAX_CELL_DEPTH` = 4096 ranges,
/// never the `2^48` an unclamped level-0 subdivision would enumerate. This is a
/// guardrail, not the tuning knob — the real per-tile clustering depth
/// [`CELL_DEPTH`] sits well under this cap.
///
/// [`CELL_DEPTH`]: crate::store::schema::CELL_DEPTH
pub const MAX_CELL_DEPTH: u8 = 6;

/// A validated slippy-map tile coordinate `(level, x, y)` — the container the
/// per-tile clustering read folds. The smart constructor [`TileId::new`] is the
/// sole off-grid guard: every `TileId` names a real tile (`x, y < 2^level`), so
/// the clustering path carries no re-validation and its Morton math can't
/// overflow the 48-bit field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileId {
    level: QuadLevel,
    x: u32,
    y: u32,
}

impl TileId {
    /// Construct a tile coordinate, validating that `(x, y)` names a real tile
    /// at `level` — both indices below `2^level`. The boundary constructor for a
    /// client-supplied coordinate (the `/tiles/{z}/{x}/{y}` endpoint); off-grid
    /// indices are rejected here so nothing downstream re-checks.
    pub fn new(level: QuadLevel, x: u32, y: u32) -> Result<Self, TileCoordError> {
        let side = 1u32 << level.get();
        if x >= side || y >= side {
            return Err(TileCoordError::OffGrid {
                level: level.get(),
                x,
                y,
            });
        }
        Ok(Self { level, x, y })
    }

    /// The tile's level.
    pub fn level(&self) -> QuadLevel {
        self.level
    }

    /// The tile's x index, in `[0, 2^level)`.
    pub fn x(&self) -> u32 {
        self.x
    }

    /// The tile's y index, in `[0, 2^level)`.
    pub fn y(&self) -> u32 {
        self.y
    }

    /// The tile's own inclusive Morton range `[lo, hi]` — the contiguous block
    /// its quadkeys occupy on the index.
    pub fn range(&self) -> QuadTileRange {
        QuadTileRange::for_tile(self.x, self.y, self.level)
    }

    /// The Morton ranges of the child tiles `depth` levels finer that partition
    /// this tile — a lazy, bounded iterator, ascending by `lo`.
    ///
    /// The tile is one contiguous Morton block `[lo, hi]`; its `4^depth`
    /// children at `level + depth` split it into `4^depth` equal contiguous
    /// sub-ranges. Rather than expand `(x, y)` and re-encode, the split is
    /// arithmetic: `child_span = (hi − lo + 1) / 4^depth`, and child `k` is
    /// `[lo + k·child_span, lo + (k+1)·child_span − 1]`. No gap, no overlap, so
    /// folding each child once covers the tile exactly once.
    ///
    /// `depth` is clamped by [`MAX_CELL_DEPTH`] and by the levels of headroom
    /// below this tile (`MAX_QUAD_LEVEL − level`), so even `child_ranges(24)` on
    /// a level-0 tile yields at most `4^MAX_CELL_DEPTH` ranges — never the `2^48`
    /// an unclamped subdivision would. A `depth` of zero (or a tile already at
    /// the finest level) yields the tile itself as the sole range.
    ///
    /// The iterator captures owned primitives, borrowing nothing, so it is
    /// `Send` and streams without allocating the range vector.
    pub fn child_ranges(&self, depth: u8) -> impl Iterator<Item = QuadTileRange> {
        let eff_depth = depth
            .min(MAX_CELL_DEPTH)
            .min(MAX_QUAD_LEVEL - self.level.get());
        let range = self.range();
        let lo = range.lo;
        let count = 1u64 << (2 * u32::from(eff_depth));
        // Exact: the block span is 4^(MAX_QUAD_LEVEL − level) and count is
        // 4^eff_depth with eff_depth ≤ that exponent, so the division has no
        // remainder.
        let child_span = (range.hi - lo + 1) / count as i64;
        (0..count).map(move |k| {
            let base = lo + k as i64 * child_span;
            QuadTileRange {
                lo: base,
                hi: base + child_span - 1,
            }
        })
    }
}

/// A tile coordinate that names no tile at its level: `x` or `y` reached
/// `2^level`, past the `[0, 2^level)` grid. From [`TileId::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TileCoordError {
    /// An index sat at or past `2^level`.
    OffGrid {
        /// The tile level whose grid the indices overran.
        level: u8,
        /// The x index supplied.
        x: u32,
        /// The y index supplied.
        y: u32,
    },
}

impl std::fmt::Display for TileCoordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OffGrid { level, x, y } => write!(
                f,
                "tile ({x}, {y}) is off the grid at level {level}: both indices must be < 2^{level}"
            ),
        }
    }
}

impl std::error::Error for TileCoordError {}

/// Cap on the tiles one [`viewport_tiles`] call may span — an OOM guard sitting
/// far above any real marker budget. A caller that trips it picked a level too
/// deep for its viewport.
const MAX_VIEWPORT_TILES: u64 = 65_536;

/// Errors from [`viewport_tiles`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewportTilesError {
    /// The viewport's tile span exceeded `MAX_VIEWPORT_TILES` at the level.
    TooManyTiles {
        /// The tile count that tripped the cap.
        count: u64,
    },
}

impl std::fmt::Display for ViewportTilesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyTiles { count } => {
                write!(
                    f,
                    "viewport spans {count} tiles at this level, over the cap"
                )
            }
        }
    }
}

impl std::error::Error for ViewportTilesError {}

/// The inclusive `(x0, x1, y0, y1)` tile block each viewport half spans at
/// `level`. Both [`viewport_tile_count`]'s sum and [`viewport_tiles`]'
/// enumeration fold over this, so the projection lives in one place and the cap
/// guard and the collection can never span different tiles.
fn viewport_blocks(viewport: &Viewport, level: QuadLevel) -> Vec<(u32, u32, u32, u32)> {
    let depth = level.get();
    viewport
        .halves()
        .into_iter()
        .map(|half| {
            let x0 = unit_to_tile(mercator_x(half.min_lon), depth);
            let x1 = unit_to_tile(mercator_x(half.max_lon), depth);
            // Mercator y grows southward: max_lat is the northern (smaller-y)
            // edge, min_lat the southern (larger-y) edge.
            let y0 = unit_to_tile(mercator_y(half.max_lat), depth);
            let y1 = unit_to_tile(mercator_y(half.min_lat), depth);
            (x0, x1, y0, y1)
        })
        .collect()
}

/// The number of tiles `viewport` spans at `level`, as the per-half
/// block-count sum — the same figure [`viewport_tiles`] guards its cap on,
/// computed without building the tile set. It over-counts only the sub-column
/// seam gap a wrap shares between its halves, so it's an upper bound on the
/// tiles [`viewport_tiles`] enumerates.
pub fn viewport_tile_count(viewport: &Viewport, level: QuadLevel) -> u64 {
    viewport_blocks(viewport, level)
        .into_iter()
        .map(|(x0, x1, y0, y1)| u64::from(x1 - x0 + 1) * u64::from(y1 - y0 + 1))
        .sum()
}

/// The deduped `(x, y)` tiles a viewport covers at `level`, ordered ascending
/// by `(x, y)`. The single enumeration [`viewport_tiles`] and
/// [`viewport_tile_xys`] share: the antimeridian sweep folds both
/// [halves](Viewport::halves)' blocks together here, and a sub-column wrap gap
/// that lands one boundary column in both halves dedupes to a single entry — so
/// the two callers can never span different tile sets.
fn viewport_tile_xy_set(
    viewport: &Viewport,
    level: QuadLevel,
) -> std::collections::BTreeSet<(u32, u32)> {
    let mut tiles = std::collections::BTreeSet::new();
    for (x0, x1, y0, y1) in viewport_blocks(viewport, level) {
        for x in x0..=x1 {
            for y in y0..=y1 {
                tiles.insert((x, y));
            }
        }
    }
    tiles
}

/// The Morton tile ranges a `viewport` spans at `level` — one per tile the
/// viewport touches, so a viewport query is the union of their range scans.
/// They come back ascending by `lo` and pairwise disjoint, so a caller can
/// binary-search a Morton code to its tile.
///
/// Each [`Viewport::halves`] rect is already non-wrapping (`min_lon ≤
/// max_lon`) and becomes a rectangular block of tiles; a seam-crossing
/// viewport is the two halves' blocks together. Tile indices are clamped into
/// `[0, 2^level)`, so the eastern seam lands in the last tile rather than
/// wrapping to the first.
///
/// A wrap whose uncovered longitude gap is narrower than one tile column shares
/// its boundary column between both halves; a sorted set folds that to a single
/// range. The per-half block sizes sum to an upper bound on the span (dedup only
/// shrinks it), so the `MAX_VIEWPORT_TILES` cap is checked against that sum and
/// a too-deep level errors before any tile is collected.
pub fn viewport_tiles(
    viewport: &Viewport,
    level: QuadLevel,
) -> Result<Vec<QuadTileRange>, ViewportTilesError> {
    // The cap is checked against the per-half block-count sum — the same upper
    // bound [`viewport_tile_count`] reports — so a level that overruns is
    // rejected before any tile is enumerated.
    let count = viewport_tile_count(viewport, level);
    if count > MAX_VIEWPORT_TILES {
        return Err(ViewportTilesError::TooManyTiles { count });
    }

    let mut ranges: Vec<QuadTileRange> = viewport_tile_xy_set(viewport, level)
        .into_iter()
        .map(|(x, y)| QuadTileRange::for_tile(x, y, level))
        .collect();
    // The set orders by (x, y); Morton `lo` interleaves those bits, so resort
    // into the Morton order the range scans and binary searches expect.
    ranges.sort_by_key(|r| r.lo);
    Ok(ranges)
}

/// The `(x, y)` tile indices a `viewport` covers at `level` — the index form of
/// [`viewport_tiles`], stopping before the Morton-range projection. A caller
/// wanting the raw grid indices (a client enumerating tiles to fetch) reads
/// these rather than re-deriving the antimeridian sweep: a seam-crossing
/// viewport's two [halves](Viewport::halves) contribute their blocks together,
/// so the wrap is interpreted here once. Indices come back ascending by
/// `(x, y)` and deduplicated — a sub-column wrap gap that lands one column in
/// both halves folds to a single entry.
///
/// Unlike [`viewport_tiles`], no `MAX_VIEWPORT_TILES` cap: that bound guards
/// the Morton range-scan path, whereas this is the index primitive a caller
/// sizes its own request budget against.
pub fn viewport_tile_xys(viewport: &Viewport, level: QuadLevel) -> Vec<(u32, u32)> {
    viewport_tile_xy_set(viewport, level).into_iter().collect()
}

// ============================================================================
// Meters
// ============================================================================

/// A meter-valued scalar: a WGS84 geodesic distance or a radius. Carrying the
/// unit in the type keeps `_m`-suffixed names off the call surface and stops a
/// raw `f64` being passed where a meter count is meant.
///
/// The inner value is a `Finite`: finite by construction with `-0.0`
/// normalized, so `Eq`/`Hash`/`Ord` derive honestly for the meter-bearing types
/// (a circle radius) that reach `BTreeSet<SubmitFact>`. [`try_new`](Meters::try_new)
/// validates an untrusted `f64` at the boundary; the typed interior mints from
/// trusted arithmetic with `new_unchecked`.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
#[schemars(transparent)]
pub struct Meters(Finite);

impl Meters {
    /// A meter value known finite by construction — a geodesic-solver output,
    /// arithmetic over finite values, or a literal. `debug_assert` catches a
    /// broken promise in dev.
    pub(crate) const fn new_unchecked(value: f64) -> Self {
        Self(Finite::new_unchecked(value))
    }

    /// A meter value from an untrusted `f64` (external input crossing into the
    /// typed interior); `Err` when it is not finite.
    pub fn try_new(value: f64) -> Result<Self, MetersError> {
        Finite::new(value)
            .map(Self)
            .ok_or(MetersError::NonFinite { value })
    }

    /// The wrapped meter count.
    pub fn get(self) -> f64 {
        self.0.get()
    }
}

/// Errors from [`Meters::try_new`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum MetersError {
    /// The value was not finite.
    #[error("meter value must be finite, got {value}")]
    NonFinite { value: f64 },
}

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
    if d > r1.get() + r2.get() || d < (r1.get() - r2.get()).abs() || d < 1e-9 {
        return Vec::new();
    }
    // Signed miss of the rim point at bearing `azi12 + α`: its WGS84 distance to
    // `c2` less `r2`. Negative inside `c2`'s circle, positive outside.
    let g = |alpha_deg: f64| -> f64 {
        let (plat, plon): (f64, f64) = geod.direct(lat1, lon1, azi12 + alpha_deg, r1.get());
        let dist: f64 = geod.inverse(plat, plon, lat2, lon2);
        dist - r2.get()
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
        let (plat, plon): (f64, f64) = geod.direct(lat1, lon1, azi12 + (lo + hi) * 0.5, r1.get());
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
        Meters::new_unchecked(self.radius.get() * CAP_TOL_REL + CAP_TOL_ABS_M)
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
        Meters::new_unchecked(self.radius.get() + self.tol().get())
    }

    /// Whether a point at `distance` from the cap's center lies in the cap —
    /// [`covers`](Self::covers) with the distance supplied by the caller, for
    /// geometry (nearest-point-of-a-region tests) that computes it elsewhere.
    pub(crate) fn covers_within(&self, distance: Meters) -> bool {
        distance.get() <= self.effective_radius().get()
    }

    /// Whether this cap contains `other` entirely: the centers' separation plus
    /// `other`'s radius fits within this radius, up to the tolerance.
    pub fn contains(&self, other: &Circle) -> bool {
        let between = self.center.distance(&other.center).get();
        between + other.radius.get() <= self.radius.get() + self.tol().get()
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
        let t = ((d + self.radius.get() - other.radius.get()) * 0.5).clamp(0.0, d);
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
        // contract `Finite` upholds so the derived impls stay consistent.
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
        Ok(Circle::new(
            GeoPoint::new(lat, lon)?,
            Meters::new_unchecked(radius_m),
        ))
    }

    #[test]
    fn distance_matches_known_arc() -> TestResult {
        // One degree of latitude along the equatorial meridian is ~110.57 km on
        // WGS84 — the ellipsoid's shortest meridian degree, notably under the
        // sphere's uniform 111.19 km.
        let a = GeoPoint::new(0.0, 0.0)?;
        let b = GeoPoint::new(1.0, 0.0)?;
        let d = a.distance(&b).get();
        assert!((d - 110_574.4).abs() < 1.0, "got {d}");
        Ok(())
    }

    #[test]
    fn distance_antipodal_is_half_circumference_no_nan() -> TestResult {
        // Karney's inverse solution must stay total at the near-antipodal
        // degenerate — the regime where naive great-circle code returns NaN.
        let a = GeoPoint::new(0.0, 0.0)?;
        let b = GeoPoint::new(0.0, 180.0)?;
        let d = a.distance(&b).get();
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
        assert!(a.distance(&a).get().abs() < 1e-6);
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
            assert!(
                (a.center().distance(p).get() - r).abs() < 1e-2,
                "on a's rim"
            );
            assert!(
                (b.center().distance(p).get() - r).abs() < 1e-2,
                "on b's rim"
            );
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
            assert!(
                (a.center().distance(p).get() - r).abs() < 1e-2,
                "on a's rim"
            );
            assert!(
                (b.center().distance(p).get() - r).abs() < 1e-2,
                "on b's rim"
            );
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
        let d = center_a.distance(&center_b).get();
        let r_a = 80_000.0;
        let a = Circle::new(center_a, Meters::new_unchecked(r_a));
        let b = Circle::new(center_b, Meters::new_unchecked(d - r_a + 1e-9));
        let pts = a.boundary_intersections(&b);
        assert_eq!(pts.len(), 2, "near-tangent caps meet");
        assert!(
            pts[0].distance(&pts[1]).get() < 1e-1,
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
        assert_eq!(
            b.geodesic_distance_to(&GeoPoint::new(40.5, -73.5)?).get(),
            0.0
        );
        assert_eq!(
            b.geodesic_distance_to(&GeoPoint::new(40.0, -74.0)?).get(),
            0.0
        );
        Ok(())
    }

    #[test]
    fn viewport_distance_due_east_hits_the_meridian_edge() -> TestResult {
        // 0.5° east of the east edge at the box's mid latitude: the nearest
        // point sits on the lon = -73 edge at ~the same latitude, ≈ 0.5° of
        // longitude at 40.5°N ≈ 42.4 km on Geodesic::wgs84().
        let b = sample_box()?;
        let d = b.geodesic_distance_to(&GeoPoint::new(40.5, -72.5)?).get();
        let expect: f64 = Geodesic::wgs84().inverse(40.5, -72.5, 40.5, -73.0);
        assert!((d - expect).abs() < 100.0, "got {d}, expected ≈{expect}");
        Ok(())
    }

    #[test]
    fn viewport_distance_due_north_hits_the_parallel_edge() -> TestResult {
        // 1° north of the north edge, longitude inside the arc: one degree of
        // WGS84 meridian arc at ~41°N ≈ 111.06 km.
        let b = sample_box()?;
        let d = b.geodesic_distance_to(&GeoPoint::new(42.0, -73.5)?).get();
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
        let d = b.geodesic_distance_to(&p).get();
        let direct = p.distance(&corner).get();
        assert!((d - direct).abs() < 1.0, "got {d}, corner at {direct}");
        Ok(())
    }

    #[test]
    fn viewport_distance_crosses_the_antimeridian_seam() -> TestResult {
        // A wrapped box `[170°E .. 170°W]`; a point at 168°E is 2° of
        // longitude from the west edge, not 358° the long way round.
        let b = Viewport::new(GeoPoint::new(0.0, 170.0)?, GeoPoint::new(10.0, -170.0)?)?;
        let d = b.geodesic_distance_to(&GeoPoint::new(5.0, 168.0)?).get();
        // Nearest point is (5, 170) on the west edge — 2° of longitude at 5°N.
        let expect: f64 = Geodesic::wgs84().inverse(5.0, 168.0, 5.0, 170.0);
        assert!((d - expect).abs() < 100.0, "got {d}, expected ≈{expect}");
        // A point just across the seam is inside.
        assert_eq!(
            b.geodesic_distance_to(&GeoPoint::new(5.0, -175.0)?).get(),
            0.0
        );
        Ok(())
    }

    #[test]
    fn viewport_distance_reaches_over_the_pole() -> TestResult {
        // A box touching the north pole; a point on the far side of the pole
        // at a distant longitude is ~0.5° (≈ 55.6 km) away through the pole,
        // not a quarter of the globe around the parallel.
        let b = Viewport::new(GeoPoint::new(89.0, 0.0)?, GeoPoint::new(90.0, 10.0)?)?;
        let d = b.geodesic_distance_to(&GeoPoint::new(89.5, 170.0)?).get();
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
        let d = b.geodesic_distance_to(&p).get();
        let direct = p.distance(&ne_corner).get();
        assert!(
            direct < p.distance(&equatorial).get(),
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
        let rects = cap_bounding_rects(
            &GeoPoint::new(60.0, 10.0)?,
            Meters::new_unchecked(100_000.0),
        );
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
        let rects = cap_bounding_rects(
            &GeoPoint::new(0.0, 179.5)?,
            Meters::new_unchecked(100_000.0),
        );
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
        let rects = cap_bounding_rects(
            &GeoPoint::new(89.5, 42.0)?,
            Meters::new_unchecked(100_000.0),
        );
        assert_eq!(rects.len(), 1);
        let r = rects[0];
        assert_eq!((r.min_lon, r.max_lon), (-180.0, 180.0));
        assert_eq!(r.max_lat, 90.0, "clamped at the pole");
        assert!(r.min_lat < 89.0);
        // Findable from any longitude near the pole.
        assert!(rects_hit(&rects, 89.0, 90.0, -170.0, -160.0));
        Ok(())
    }

    // --- Quadkey / mercator tiling ---

    use proptest::prelude::*;

    /// A validated point spanning the whole WGS-84 domain. In-range coordinates
    /// always satisfy `GeoPoint::new`, so nothing is discarded.
    fn arb_geo_point() -> impl Strategy<Value = GeoPoint> {
        (-90.0f64..=90.0, -180.0f64..=180.0)
            .prop_filter_map("in-range point", |(lat, lon)| GeoPoint::new(lat, lon).ok())
    }

    /// A tile level across the full accepted `[0, MAX_QUAD_LEVEL]` range.
    fn arb_quad_level() -> impl Strategy<Value = QuadLevel> {
        (0u8..=MAX_QUAD_LEVEL).prop_filter_map("in-range level", |l| QuadLevel::new(l).ok())
    }

    /// A viewport over the whole domain. The two latitudes are ordered into a
    /// `min ≤ max` span; the two longitudes stay unordered, so `lon_a > lon_b`
    /// builds an antimeridian-wrapping box — the seam case the tiling must get
    /// right.
    fn arb_viewport() -> impl Strategy<Value = Viewport> {
        (
            -90.0f64..=90.0,
            -90.0f64..=90.0,
            -180.0f64..=180.0,
            -180.0f64..=180.0,
        )
            .prop_filter_map("valid viewport", |(lat_p, lat_q, lon_a, lon_b)| {
                let (min_lat, max_lat) = if lat_p <= lat_q {
                    (lat_p, lat_q)
                } else {
                    (lat_q, lat_p)
                };
                Viewport::from_coords(min_lat, max_lat, lon_a, lon_b).ok()
            })
    }

    proptest! {
        /// Morton encode/decode is a bijection over the tile grid, and every
        /// code is a non-negative 48-bit value — the interleave keeps the two
        /// lanes from bleeding into each other or the sign bit.
        #[test]
        fn prop_morton_round_trips(
            x in 0u32..(1u32 << MAX_QUAD_LEVEL),
            y in 0u32..(1u32 << MAX_QUAD_LEVEL),
        ) {
            let z = morton_encode(x, y);
            prop_assert!(z >= 0, "a quadkey is never negative");
            prop_assert!(
                z < (1i64 << (2 * u32::from(MAX_QUAD_LEVEL))),
                "a quadkey fits 48 bits"
            );
            prop_assert_eq!(morton_decode(z), (x, y), "decode inverts encode");
        }

        /// A point's quadkey sits inside the Morton range of the tile it decodes
        /// to at any level: the stored key and the range scan agree on which tile
        /// owns the point.
        #[test]
        fn prop_quadkey_lands_in_its_own_tile(p in arb_geo_point(), level in arb_quad_level()) {
            let key = quadkey(&p);
            let shift = MAX_QUAD_LEVEL - level.get();
            let (tx, ty) = morton_decode(key);
            let range = QuadTileRange::for_tile(tx >> shift, ty >> shift, level);
            prop_assert!(
                range.lo <= key && key <= range.hi,
                "quadkey {key} must sit in its own tile"
            );
        }

        /// The tiles a viewport spans come back ascending by `lo` as a disjoint
        /// cover: each range is well-formed, they never overlap — the invariant
        /// a missing dedup broke on a sub-column seam gap — and a point known to
        /// lie in the viewport falls in exactly one range.
        #[test]
        fn prop_viewport_tiles_disjoint_and_covering(
            vp in arb_viewport(),
            level in arb_quad_level(),
            t_lat in 0.0f64..=1.0,
            t_lon in 0.0f64..=1.0,
            half_sel in 0usize..2,
        ) {
            let Ok(ranges) = viewport_tiles(&vp, level) else {
                return Ok(());
            };

            for r in &ranges {
                prop_assert!(r.lo <= r.hi, "each range is well-formed");
            }
            for pair in ranges.windows(2) {
                prop_assert!(
                    pair[0].hi < pair[1].lo,
                    "viewport_tiles returns ranges ascending by lo and pairwise disjoint"
                );
            }

            // Interpolate a point within one of the viewport's non-wrapping
            // halves, so containment holds by construction — no floor-tiling
            // boundary flake to guard against.
            let halves: Vec<IndexRect> = vp.halves().into_iter().collect();
            let half = halves[half_sel % halves.len()];
            let lat = half.min_lat + t_lat * (half.max_lat - half.min_lat);
            let lon = half.min_lon + t_lon * (half.max_lon - half.min_lon);
            let Some(p) = GeoPoint::new(lat, lon).ok() else {
                return Ok(());
            };
            prop_assert!(vp.contains(&p), "the interpolated point must be in the viewport");
            let key = quadkey(&p);
            let hits = ranges.iter().filter(|r| r.lo <= key && key <= r.hi).count();
            prop_assert_eq!(hits, 1, "an inside point lands in exactly one tile range");
        }

        /// The projection is monotone with no seam wrap: the x-tile never
        /// decreases as longitude rises (a `mod` at the seam would break this at
        /// 180°), the y-tile never decreases as latitude falls, and every tile —
        /// even beyond the ±85° mercator limit — stays inside `[0, 2^level)`.
        #[test]
        fn prop_projection_is_monotone_and_in_range(
            lon_a in -180.0f64..=180.0,
            lon_b in -180.0f64..=180.0,
            lat_a in -90.0f64..=90.0,
            lat_b in -90.0f64..=90.0,
            level in arb_quad_level(),
        ) {
            let depth = level.get();
            let tiles = 1u32 << depth;

            let (west, east) = if lon_a <= lon_b {
                (lon_a, lon_b)
            } else {
                (lon_b, lon_a)
            };
            let x_west = unit_to_tile(mercator_x(west), depth);
            let x_east = unit_to_tile(mercator_x(east), depth);
            prop_assert!(x_west <= x_east, "x-tile is monotone in longitude");

            // Mercator y grows southward, so the higher latitude maps to the
            // smaller-or-equal y-tile.
            let (north, south) = if lat_a >= lat_b {
                (lat_a, lat_b)
            } else {
                (lat_b, lat_a)
            };
            let y_north = unit_to_tile(mercator_y(north), depth);
            let y_south = unit_to_tile(mercator_y(south), depth);
            prop_assert!(y_north <= y_south, "y-tile grows southward");

            for t in [x_west, x_east, y_north, y_south] {
                prop_assert!(t < tiles, "tile index stays in [0, 2^level)");
            }
        }

        /// `split_level`'s leading-zeros arithmetic and the shift-scan oracle
        /// agree on the split of any two 48-bit quadkeys. Both are
        /// order-independent, so the min/max ordering only feeds `split_level`
        /// its `min ≤ max` contract.
        #[test]
        fn prop_split_level_agrees_with_prefix_scan(
            a in 0u64..(1u64 << 48),
            b in 0u64..(1u64 << 48),
        ) {
            let min = a.min(b) as i64;
            let max = a.max(b) as i64;
            prop_assert_eq!(
                split_level(min, max).get(),
                split_level_by_prefix_scan(min, max),
                "leading-zeros split agrees with the prefix scan"
            );
        }

        /// `viewport_tile_count` is an upper bound on the tiles `viewport_tiles`
        /// enumerates: the arithmetic sum only ever over-counts a seam-gap
        /// column the dedup collapses.
        #[test]
        fn prop_viewport_tile_count_upper_bounds_enumeration(
            vp in arb_viewport(),
            level in arb_quad_level(),
        ) {
            let Ok(ranges) = viewport_tiles(&vp, level) else {
                return Ok(());
            };
            prop_assert!(
                viewport_tile_count(&vp, level) >= ranges.len() as u64,
                "the count never under-reports the enumerated ranges"
            );
        }

        /// A container's child ranges partition its Morton block: `4^depth`
        /// ranges, each well-formed, sorted, pairwise disjoint and contiguous
        /// (each `hi` abuts the next `lo`), together spanning exactly the
        /// container's `[lo, hi]` — so every quadkey in the container lands in
        /// exactly one child.
        #[test]
        fn prop_child_ranges_partition_container(
            level in arb_quad_level(),
            extra in 0u8..=5,
            tx in 0u32..(1u32 << MAX_QUAD_LEVEL),
            ty in 0u32..(1u32 << MAX_QUAD_LEVEL),
        ) {
            let side = 1u32 << level.get();
            let (x, y) = (tx % side, ty % side);
            // `saturating` folds the depth against the finest level, so `depth`
            // is the headroom below the container — never above `MAX_CELL_DEPTH`
            // here, so `child_ranges` reproduces it exactly.
            let cell_level = QuadLevel::saturating(level.get() + extra);
            let depth = cell_level.get() - level.get();
            let Ok(tile) = TileId::new(level, x, y) else {
                return Ok(());
            };
            let container = tile.range();
            let ranges: Vec<QuadTileRange> = tile.child_ranges(depth).collect();

            prop_assert_eq!(
                ranges.len() as u64,
                1u64 << (2 * u32::from(depth)),
                "one child range per 4^depth sub-tile"
            );
            for r in &ranges {
                prop_assert!(r.lo <= r.hi, "each child range is well-formed");
            }
            for pair in ranges.windows(2) {
                prop_assert_eq!(
                    pair[0].hi + 1,
                    pair[1].lo,
                    "child ranges are contiguous — no gap, no overlap"
                );
            }
            prop_assert_eq!(
                ranges.first().map(|r| r.lo),
                Some(container.lo),
                "the children start at the container's lo"
            );
            prop_assert_eq!(
                ranges.last().map(|r| r.hi),
                Some(container.hi),
                "the children end at the container's hi"
            );
        }
    }

    #[test]
    fn child_ranges_of_the_world_at_cell_depth_is_bounded() -> TestResult {
        // The level-0 container is the whole Morton space; its children three
        // levels down are the 64 level-3 tiles, so even the z=0 tile read stays
        // bounded (64 ranges × the per-range LIMIT). A depth reaching past the
        // finest level collapses the container to a single child.
        let world = TileId::new(QuadLevel::new(0)?, 0, 0)?;
        let ranges: Vec<QuadTileRange> = world.child_ranges(3).collect();
        assert_eq!(ranges.len(), 64, "the world fans into 64 level-3 children");
        assert_eq!(ranges.first().map(|r| r.lo), Some(0), "children start at 0");

        let finest = TileId::new(QuadLevel::saturating(MAX_QUAD_LEVEL), 7, 11)?;
        let one: Vec<QuadTileRange> = finest.child_ranges(3).collect();
        assert_eq!(one.len(), 1, "a finest-level container folds as one child");
        assert_eq!(
            one.first(),
            Some(&finest.range()),
            "the sole child is the tile itself"
        );
        Ok(())
    }

    #[test]
    fn child_ranges_stays_bounded_under_an_absurd_depth() -> TestResult {
        // The safety backstop: an accidental deep `depth` must not enumerate
        // `2^48` ranges. A low-level tile has ample headroom, so MAX_CELL_DEPTH —
        // not the grid — is what caps the fan-out.
        let tile = TileId::new(QuadLevel::new(2)?, 1, 1)?;
        let cap = 1u64 << (2 * u32::from(MAX_CELL_DEPTH));
        let count = tile.child_ranges(30).count() as u64;
        assert_eq!(
            count, cap,
            "child_ranges caps the fan-out at 4^MAX_CELL_DEPTH, not the requested depth"
        );
        Ok(())
    }

    #[test]
    fn tile_id_rejects_an_off_grid_coordinate() {
        // A coordinate off its level's grid can't name a real tile; the smart
        // constructor is the sole off-grid guard, so it rejects here rather than
        // let the Morton math overflow downstream.
        let level = QuadLevel::saturating(4);
        assert_eq!(
            TileId::new(level, 1 << 4, 0),
            Err(TileCoordError::OffGrid {
                level: 4,
                x: 1 << 4,
                y: 0,
            }),
        );
    }

    #[test]
    fn quadkey_matches_reference_slippy_map_tiles() {
        // OpenStreetMap slippy tile numbers pin the projection to real
        // web-mercator, not just a monotonic stand-in: Berlin and Sydney at
        // zoom 10, each from `xtile = floor((lon+180)/360 · 2^z)` and
        // `ytile = floor((1 − asinh(tan(lat))/π)/2 · 2^z)`.
        let cases = [
            (52.5200, 13.4050, 550, 335),   // Berlin
            (-33.8688, 151.2093, 942, 614), // Sydney
        ];
        for (lat, lon, xtile, ytile) in cases {
            assert_eq!(
                unit_to_tile(mercator_x(lon), 10),
                xtile,
                "x tile at {lat}, {lon}"
            );
            assert_eq!(
                unit_to_tile(mercator_y(lat), 10),
                ytile,
                "y tile at {lat}, {lon}"
            );
        }
    }

    #[test]
    fn wrapped_viewport_with_subcolumn_gap_has_disjoint_tiles() -> TestResult {
        // The uncovered gap (0.3°E..0.5°E, ~0.2°) is narrower than one level-8
        // tile column (~1.4°), so the seam column lands in both halves. Dedup
        // must leave the ranges pairwise disjoint.
        let viewport = Viewport::from_coords(0.0, 10.0, 0.5, 0.3)?;
        let level = QuadLevel::new(8)?;
        let mut ranges = viewport_tiles(&viewport, level)?;
        ranges.sort_by_key(|r| r.lo);
        for pair in ranges.windows(2) {
            assert!(pair[0].lo <= pair[0].hi, "each range is well-formed");
            assert!(pair[0].hi < pair[1].lo, "no duplicated boundary column");
        }
        Ok(())
    }

    #[test]
    fn viewport_tile_xys_covers_a_normal_viewport() -> TestResult {
        // A non-wrapping box: the covering indices are exactly the tiles
        // viewport_tiles projects to Morton ranges — one (x, y) per range.
        let vp = Viewport::from_coords(40.0, 40.1, -74.0, -73.9)?;
        let level = QuadLevel::new(14)?;
        let xys = viewport_tile_xys(&vp, level);
        let ranges = viewport_tiles(&vp, level)?;
        assert_eq!(xys.len(), ranges.len(), "one covering index per tile range");
        let range_los: std::collections::BTreeSet<i64> = ranges.iter().map(|r| r.lo).collect();
        for &(x, y) in &xys {
            let lo = QuadTileRange::for_tile(x, y, level).lo;
            assert!(
                range_los.contains(&lo),
                "tile ({x}, {y}) must name a covered range"
            );
        }
        Ok(())
    }

    #[test]
    fn viewport_tile_xys_covers_both_sides_of_the_seam() -> TestResult {
        // A westward span (min_lon > max_lon) wraps the antimeridian: the covering
        // indices include the far-west (x = 0) and far-east (x = last) seam
        // columns but never the interior mid-longitude gap between them.
        let vp = Viewport::from_coords(0.0, 10.0, 179.0, -179.0)?;
        let level = QuadLevel::new(8)?;
        let xys = viewport_tile_xys(&vp, level);
        let last = (1u32 << 8) - 1;
        assert!(
            xys.iter().any(|&(x, _)| x == 0),
            "the west seam column is covered"
        );
        assert!(
            xys.iter().any(|&(x, _)| x == last),
            "the east seam column is covered"
        );
        let mid = unit_to_tile(mercator_x(0.0), 8);
        assert!(
            !xys.iter().any(|&(x, _)| x == mid),
            "the interior gap ({mid}) is outside the wrap"
        );
        assert_eq!(
            xys.len(),
            viewport_tiles(&vp, level)?.len(),
            "the same deduped tile set viewport_tiles enumerates"
        );
        Ok(())
    }

    #[test]
    fn viewport_tiles_rejects_a_viewport_spanning_too_many_tiles() -> TestResult {
        // A near-global viewport at level 16 spans billions of tiles, well past
        // the OOM cap.
        let viewport = Viewport::from_coords(-85.0, 85.0, -179.0, 179.0)?;
        let level = QuadLevel::new(16)?;
        assert!(
            matches!(
                viewport_tiles(&viewport, level),
                Err(ViewportTilesError::TooManyTiles { .. })
            ),
            "a near-global deep-level viewport is rejected"
        );
        Ok(())
    }

    // --- split_level / tile count ---

    /// The deepest level whose `2L`-bit Morton prefix both codes share, plus
    /// one — derived by per-level shift compare, a different mechanism than the
    /// leading-zeros arithmetic [`split_level`] runs, so a shared bug can't hide
    /// the two agreeing.
    fn split_level_by_prefix_scan(q1: i64, q2: i64) -> u8 {
        let mut shared = 0u8;
        for l in 0..=MAX_QUAD_LEVEL {
            let shift = 2 * u32::from(MAX_QUAD_LEVEL - l);
            if (q1 >> shift) == (q2 >> shift) {
                shared = l;
            } else {
                break;
            }
        }
        (shared + 1).min(MAX_QUAD_LEVEL)
    }

    #[test]
    fn split_level_identical_codes_are_maximally_deep() -> TestResult {
        // No spread among the survivors → they share every level, so the split
        // is the terminal level rather than any subdivision.
        assert_eq!(split_level(0, 0).get(), MAX_QUAD_LEVEL);
        let q = quadkey(&GeoPoint::new(12.3, 45.6)?);
        assert_eq!(split_level(q, q).get(), MAX_QUAD_LEVEL);
        Ok(())
    }

    #[test]
    fn split_level_lowest_bit_splits_at_the_finest_level() {
        // Two codes apart only in Morton bit 0 share the top 47 bits, so they
        // first fall into separate tiles at the finest level.
        assert_eq!(split_level(0, 1).get(), MAX_QUAD_LEVEL);
    }

    #[test]
    fn split_level_applies_the_16_bit_offset() {
        // Morton bit 47 is the top of the 48-bit code. Codes differing there
        // share no prefix, so they split at the coarsest subdivision, level 1.
        assert_eq!(split_level(0, 1i64 << 47).get(), 1);
        // Dropping the 48-vs-64-bit offset would read the 16 zero pad bits as
        // shared prefix and report level 9 — eight levels too coarse. Pin that
        // wrong value so the offset can't be silently removed.
        let naive = ((1u64 << 47).leading_zeros() / 2 + 1) as u8;
        assert_eq!(naive, 9);
    }

    #[test]
    fn viewport_tile_count_matches_enumerated_tiles_for_a_normal_viewport() -> TestResult {
        // A non-wrapping box has no seam-gap double count, so the arithmetic sum
        // equals the deduped enumerated set exactly.
        let vp = Viewport::from_coords(40.0, 40.1, -74.0, -73.9)?;
        let level = QuadLevel::new(14)?;
        let enumerated = viewport_tiles(&vp, level)?.len() as u64;
        assert_eq!(viewport_tile_count(&vp, level), enumerated);
        Ok(())
    }

    #[test]
    fn viewport_tile_count_upper_bounds_a_seam_gap_wrap() -> TestResult {
        // The sub-column seam gap lands one column in both halves; the sum
        // counts that column's rows twice while the enumerated set dedups them,
        // so the count is a strict upper bound here.
        let vp = Viewport::from_coords(0.0, 10.0, 0.5, 0.3)?;
        let level = QuadLevel::new(8)?;
        let enumerated = viewport_tiles(&vp, level)?.len() as u64;
        assert!(viewport_tile_count(&vp, level) > enumerated);
        Ok(())
    }

    // --- mercator inverse ---

    #[test]
    fn mercator_x_round_trips_through_its_inverse() {
        for lon in [-179.0, -90.0, 0.0, 45.0, 179.0] {
            let back = mercator_x_to_lon(mercator_x(lon));
            assert!((back - lon).abs() < 1e-9, "lon {lon} round-trips to {back}");
        }
    }

    #[test]
    fn mercator_y_round_trips_through_its_inverse() {
        // Includes the ±mercator limit, where `mercator_y` folds to the `[0, 1)`
        // edges and the inverse must return the same clamped latitude.
        for lat in [
            0.0,
            30.0,
            -45.0,
            60.0,
            -80.0,
            85.0,
            MERCATOR_LAT_LIMIT,
            -MERCATOR_LAT_LIMIT,
        ] {
            let back = mercator_y_to_lat(mercator_y(lat));
            assert!((back - lat).abs() < 1e-6, "lat {lat} round-trips to {back}");
        }
    }
}
