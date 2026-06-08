//! Location types with uncertainty support.
//!
//! Three types model locations at different stages of resolution:
//!
//! - [`Location`] — resolved geometry (circles, unions, unbounded). The lattice
//!   (merge via subsumption + union) is defined here.
//! - [`UnresolvedLocation`] — may contain symbolic [`LocationReference`]s that
//!   need external resolution (geocoding, OSM lookup, etc.).
//! - [`LocationReference`] — a symbolic pointer to a location in an external
//!   system (OSM, OHM, named place, address, or "near" any of these).
//!
//! Merge operates on `UnresolvedLocation` (bag union / `OneOf` construction).
//! Resolution maps `LocationReference` → `Location` via a caller-provided
//! closure. The lattice operates on `Location` only.
//!
//! # Entity vs. region scope
//!
//! Entity-scale things — individual buildings, the Forbidden City as a complex,
//! a fortress, a citadel — are first-class entities with their own
//! [`crate::facts::ids::EntityId`]. Containment between them is expressed by
//! [`crate::facts::attribute::Fact::Relationship`] with
//! [`crate::facts::attribute::EntityRelationType::Contains`], not by a location
//! reference.
//!
//! Region-scale things — cities, neighborhoods, contested geographical areas
//! like "Manhattan" or "Newark" — are not entities in the fact-store grammar.
//! They live as opaque names inside [`LocationReference::NamedPlace`], resolved
//! against an external gazetteer at projection time. The grammar doesn't
//! enforce this split structurally; it's a convention the submission and
//! projection layers both follow.

use std::cmp::Ordering;
use std::fmt;

use chronoscope_macros::grammar_type;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geo::{GeoPoint, GeoPointError};
use crate::ids::{OhmId, OsmElementType, OsmId};

/// Errors from location construction or validation.
///
/// Coordinate validation (range / finiteness / negative-zero normalization)
/// lives on [`GeoPoint`]; a circle's center error surfaces through the
/// [`Self::Center`] wrapper. `LocationError`'s own variants are the
/// location-specific checks: the radius bounds and the minimum-entry count for
/// `UnionOf`/`OneOf`.
///
/// `PartialEq` only — the radius variants carry pre-validation `f64` that may be
/// NaN/Inf. Errors aren't part of the content-addressed-fact graph, so missing
/// `Eq`/`Ord` doesn't ripple.
#[derive(Debug, Clone, PartialEq)]
pub enum LocationError {
    /// `OneOf` / `UnionOf` requires at least 2 entries.
    TooFewEntries { count: usize },
    /// The circle's center coordinate failed [`GeoPoint`] validation.
    Center(GeoPointError),
    /// Radius was negative.
    NegativeRadius { radius_m: f64 },
    /// Radius was `NaN` or infinite.
    NonFiniteRadius { radius_m: f64 },
}

impl fmt::Display for LocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooFewEntries { count } => {
                write!(f, "OneOf/UnionOf requires at least 2 entries, got {count}")
            }
            Self::Center(e) => write!(f, "circle center: {e}"),
            Self::NegativeRadius { radius_m } => {
                write!(f, "radius_m must be non-negative, got {radius_m}")
            }
            Self::NonFiniteRadius { radius_m } => {
                write!(f, "radius_m must be finite, got {radius_m}")
            }
        }
    }
}

impl std::error::Error for LocationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Center(e) => Some(e),
            _ => None,
        }
    }
}

impl From<GeoPointError> for LocationError {
    fn from(e: GeoPointError) -> Self {
        Self::Center(e)
    }
}

// ==================== Resolved Location ====================

/// Resolved location geometry.
///
/// No external references — purely geometric. The lattice (merge via subsumption
/// + union) is defined on this type.
///
/// `Eq`/`Ord`/`Hash` are hand-implemented because the `Circle` variant carries
/// an `f64` radius (needed for `BTreeSet<SubmitFact>` dedup of facts that
/// transitively reach `Location`). The center's coordinate handling is
/// [`GeoPoint`]'s; these impls delegate the center to it and add only the radius
/// `total_cmp`/`to_bits`. The smart constructor [`Location::circle`] rejects
/// NaN/Inf radii and normalizes `-0.0` so the manual `Hash`/`Ord` stay
/// consistent with the derived `PartialEq`.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Location {
    /// A point with radius of uncertainty (circle on the earth's surface).
    Circle { center: GeoPoint, radius_m: f64 },
    /// Disjoint or partially-overlapping shapes: "one of these is true."
    /// Parallels [`UnresolvedLocation::OneOf`] at the resolved level.
    /// Flattened when nested. Requires ≥2 children.
    #[serde(rename = "union_of")]
    UnionOf { members: Vec<Location> },
    /// No geometric information (resolution failed, or unknown).
    Unbounded,
}

impl Eq for Location {}

impl PartialOrd for Location {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Location {
    fn cmp(&self, other: &Self) -> Ordering {
        // Variant discriminant first, then payload. `Circle`'s f64s use
        // `total_cmp` (NaN/Inf rejected at construction).
        fn variant_index(loc: &Location) -> u8 {
            match loc {
                Location::Circle { .. } => 0,
                Location::UnionOf { .. } => 1,
                Location::Unbounded => 2,
            }
        }
        let self_idx = variant_index(self);
        let other_idx = variant_index(other);
        if self_idx != other_idx {
            return self_idx.cmp(&other_idx);
        }
        match (self, other) {
            (
                Self::Circle {
                    center: c1,
                    radius_m: r1,
                },
                Self::Circle {
                    center: c2,
                    radius_m: r2,
                },
                // `center` orders via `GeoPoint`'s `total_cmp`-based `Ord`; the
                // radius `f64` uses `total_cmp` for the same NaN-free reason.
            ) => c1.cmp(c2).then_with(|| r1.total_cmp(r2)),
            (Self::UnionOf { members: a }, Self::UnionOf { members: b }) => a.cmp(b),
            (Self::Unbounded, Self::Unbounded) => Ordering::Equal,
            // Mixed variants are handled by the discriminant check above; these
            // arms are unreachable but spelled out so adding a variant forces a
            // decision.
            (Self::Circle { .. }, _) | (Self::UnionOf { .. }, _) | (Self::Unbounded, _) => {
                Ordering::Equal
            }
        }
    }
}

impl std::hash::Hash for Location {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Circle { center, radius_m } => {
                // `center` hashes via `GeoPoint`'s `to_bits`-based `Hash`;
                // the radius hashes by bits for the same reason.
                center.hash(state);
                radius_m.to_bits().hash(state);
            }
            Self::UnionOf { members } => members.hash(state),
            Self::Unbounded => {}
        }
    }
}

impl<'de> Deserialize<'de> for Location {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        #[serde(deny_unknown_fields)]
        enum Raw {
            Circle {
                center: GeoPoint,
                radius_m: f64,
            },
            #[serde(rename = "union_of")]
            UnionOf {
                members: Vec<Location>,
            },
            Unbounded,
        }

        let raw = Raw::deserialize(deserializer)?;
        match raw {
            // `center` is validated by `GeoPoint`'s own `Deserialize`;
            // `circle` adds the radius checks.
            Raw::Circle { center, radius_m } => {
                Location::circle(center, radius_m).map_err(serde::de::Error::custom)
            }
            Raw::UnionOf { members } => {
                Location::union_of(members).map_err(serde::de::Error::custom)
            }
            Raw::Unbounded => Ok(Location::Unbounded),
        }
    }
}

impl Location {
    /// Create a validated `Circle` location from an already-validated
    /// [`GeoPoint`] center and a radius.
    ///
    /// The center carries [`GeoPoint`]'s guarantees (in-range, finite,
    /// negative-zero-normalized). This constructor adds the radius checks:
    /// non-negative and finite. `-0.0` radius is normalized to `+0.0` so
    /// the manual `Hash`/`Ord` (bit-level / `total_cmp`) stay consistent
    /// with the derived `PartialEq`.
    pub fn circle(center: GeoPoint, radius_m: f64) -> Result<Self, LocationError> {
        if !radius_m.is_finite() {
            return Err(LocationError::NonFiniteRadius { radius_m });
        }
        if radius_m < 0.0 {
            return Err(LocationError::NegativeRadius { radius_m });
        }
        // Normalize `-0.0` to `+0.0`: `-0.0 + 0.0 == +0.0`, and adding
        // `0.0` is a no-op for every other finite value.
        let radius_m = radius_m + 0.0;
        Ok(Self::Circle { center, radius_m })
    }

    /// Create a zero-radius `Circle` at a [`GeoPoint`] — a point taken at
    /// face value, with no explicit precision.
    ///
    /// Infallible: the center is already validated and a `0.0` radius
    /// always passes the radius checks. The lattice treats this as a
    /// zero-radius circle for geometric operations.
    pub fn point(center: GeoPoint) -> Self {
        Self::Circle {
            center,
            radius_m: 0.0,
        }
    }

    /// Create a validated `UnionOf` with at least 2 children.
    pub fn union_of(children: Vec<Location>) -> Result<Self, LocationError> {
        if children.len() < 2 {
            return Err(LocationError::TooFewEntries {
                count: children.len(),
            });
        }
        Ok(Self::UnionOf { members: children })
    }

    /// Merge two locations in the subsumption semilattice.
    ///
    /// If one location's region contains the other, returns the tighter (more
    /// precise) one. Otherwise returns `UnionOf` — "one of these, but we can't
    /// determine which geometrically."
    ///
    /// `Unbounded` is the identity: `Unbounded.merge(x) == x`.
    pub fn merge(&self, other: &Self) -> Self {
        if self.contains(other) {
            return other.clone();
        }
        if other.contains(self) {
            return self.clone();
        }
        // Neither contains the other — produce a union
        let mut children = Vec::new();
        // Flatten existing UnionOf children
        match self {
            Self::UnionOf { members } => children.extend(members.iter().cloned()),
            other_val => children.push(other_val.clone()),
        }
        match other {
            Self::UnionOf { members } => children.extend(members.iter().cloned()),
            other_val => children.push(other_val.clone()),
        }
        Self::UnionOf { members: children }
    }

    /// Check if this location's region contains another's entirely.
    ///
    /// Private — subsumption is implied by `merge(a, b) == b` when `a` contains `b`.
    fn contains(&self, other: &Self) -> bool {
        match (self, other) {
            // Unbounded contains everything
            (Self::Unbounded, _) => true,
            // Nothing (except Unbounded) contains Unbounded
            (_, Self::Unbounded) => false,
            // Circle containment: distance between centers + other radius <= self radius
            (
                Self::Circle {
                    center: c1,
                    radius_m: r1,
                },
                Self::Circle {
                    center: c2,
                    radius_m: r2,
                },
            ) => {
                let dist = haversine_meters(c1, c2);
                dist + r2 <= *r1
            }
            // UnionOf vs UnionOf: self contains other if every child of other
            // is contained by some child of self.
            (
                Self::UnionOf {
                    members: self_children,
                },
                Self::UnionOf {
                    members: other_children,
                },
            ) => other_children
                .iter()
                .all(|oc| self_children.iter().any(|sc| sc.contains(oc))),
            // UnionOf contains X if any child contains X
            (Self::UnionOf { members }, other_loc) => members.iter().any(|c| c.contains(other_loc)),
            // X contains UnionOf if X contains every child
            (_, Self::UnionOf { members }) => members.iter().all(|c| self.contains(c)),
            // Circle doesn't contain Unbounded (handled above)
        }
    }
}

/// Haversine distance in meters between two geo-points.
fn haversine_meters(a_point: &GeoPoint, b_point: &GeoPoint) -> f64 {
    const EARTH_RADIUS_M: f64 = 6_371_000.0;
    let (lat1, lon1) = (a_point.lat(), a_point.lon());
    let (lat2, lon2) = (b_point.lat(), b_point.lon());
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    EARTH_RADIUS_M * c
}

// ==================== Unresolved Location ====================

/// A location that may contain unresolved symbolic references.
///
/// Adjacently-tagged serde (`tag` + `content`) because `Resolved` wraps a
/// [`Location`] with its own internal `type` tag — internal tagging on both
/// levels would produce duplicate `type` fields.
///
/// `Eq`/`Ord` propagate through [`Location`]'s hand-implemented Ord; see that
/// type's doc-comment for the f64 handling.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum UnresolvedLocation {
    /// Already resolved to geometry.
    Resolved(Location),
    /// Symbolic reference needing external resolution.
    Reference(LocationReference),
    /// One of these, don't know which (conflicting sources).
    /// Flattened when nested. Requires ≥2 entries.
    ///
    /// Construct via [`UnresolvedLocation::one_of`] to enforce the minimum.
    OneOf(Vec<UnresolvedLocation>),
}

impl UnresolvedLocation {
    /// Create a validated `OneOf` with at least 2 entries.
    pub fn one_of(entries: Vec<UnresolvedLocation>) -> Result<Self, LocationError> {
        if entries.len() < 2 {
            return Err(LocationError::TooFewEntries {
                count: entries.len(),
            });
        }
        Ok(Self::OneOf(entries))
    }
}

impl<'de> Deserialize<'de> for UnresolvedLocation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type", content = "value", rename_all = "snake_case")]
        #[serde(deny_unknown_fields)]
        enum Raw {
            Resolved(Location),
            Reference(LocationReference),
            OneOf(Vec<UnresolvedLocation>),
        }

        let raw = Raw::deserialize(deserializer)?;
        match raw {
            Raw::Resolved(loc) => Ok(Self::Resolved(loc)),
            Raw::Reference(r) => Ok(Self::Reference(r)),
            Raw::OneOf(entries) => {
                if entries.len() < 2 {
                    return Err(serde::de::Error::custom(LocationError::TooFewEntries {
                        count: entries.len(),
                    }));
                }
                Ok(Self::OneOf(entries))
            }
        }
    }
}

// ==================== Location Reference ====================

/// A symbolic reference to a location in an external system.
///
/// These need external resolution (geocoding, OSM/OHM lookup, etc.) to produce
/// a [`Location`]. References are region-scale: entity-scale containment lives
/// on [`crate::facts::attribute::Fact::Relationship`], not here. See the
/// module-level "Entity vs. region scope" note.
#[serde_with::skip_serializing_none]
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LocationReference {
    /// OpenStreetMap element reference.
    #[serde(rename = "osm_reference")]
    Osm {
        osm_type: OsmElementType,
        osm_id: OsmId,
    },
    /// `OpenHistoricalMap` element reference.
    #[serde(rename = "ohm_reference")]
    Ohm { ohm_id: OhmId },
    /// Human-readable place name (e.g., "Paris", "Brooklyn Bridge").
    NamedPlace { name: String },
    /// Street address (e.g., "123 Main St, Springfield").
    Address { address_text: String },
    /// Near some other reference, with optional qualitative distance.
    /// "Near Paris", "near 1600 Penn Ave" all use this.
    Near {
        reference: Box<LocationReference>,
        distance: Option<Distance>,
    },
}

// ==================== Supporting Types ====================

/// Elevation representation with different reference systems.
// TODO: "Current" in CurrentGroundOffset is awkward — ground level changes over time
// (e.g., landfill, excavation, natural erosion). May need temporal qualification later.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Elevation {
    /// Offset from ground level (0 = ground, negative = below ground).
    CurrentGroundOffset { meters: i32 },
    /// Offset from sea level (positive = above, negative = below).
    SeaLevelOffset { meters: i32 },
}

/// Qualitative distance descriptions.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Distance {
    Adjacent,
    AcrossStreet,
    WalkingDistance,
    SameNeighborhood,
    SameDistrict,
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Build a [`GeoPoint`] for test fixtures, surfacing construction
    /// failure as the test's error rather than a panic.
    fn gp(lat: f64, lon: f64) -> Result<GeoPoint, GeoPointError> {
        GeoPoint::new(lat, lon)
    }

    // --- Location (resolved) tests ---

    #[test]
    fn valid_circle() -> TestResult {
        let loc = Location::circle(gp(40.7505, -73.9934)?, 10.0);
        assert!(loc.is_ok());
        Ok(())
    }

    #[test]
    fn valid_point() -> TestResult {
        assert!(matches!(
            Location::point(gp(40.7505, -73.9934)?),
            Location::Circle { radius_m, .. } if radius_m == 0.0
        ));
        Ok(())
    }

    #[test]
    fn circle_rejects_negative_radius() -> TestResult {
        let loc = Location::circle(gp(0.0, 0.0)?, -1.0);
        assert!(matches!(loc, Err(LocationError::NegativeRadius { .. })));
        Ok(())
    }

    #[test]
    fn circle_rejects_non_finite_radius() -> TestResult {
        let loc = Location::circle(gp(0.0, 0.0)?, f64::INFINITY);
        assert!(matches!(loc, Err(LocationError::NonFiniteRadius { .. })));
        Ok(())
    }

    #[test]
    fn circle_deserialize_rejects_out_of_range_center() {
        // The center routes through `GeoPoint`'s validating `Deserialize`,
        // so an out-of-range coordinate is rejected at the `Location`
        // boundary as a `Center` error rather than reaching the interior.
        let result: Result<Location, _> = serde_json::from_str(
            r#"{"type":"circle","center":{"lat":91.0,"lon":0.0},"radius_m":10.0}"#,
        );
        assert!(
            result.is_err(),
            "out-of-range center must fail at deserialize"
        );
    }

    #[test]
    fn circle_boundary_values() -> TestResult {
        assert!(Location::circle(gp(90.0, 180.0)?, 0.0).is_ok());
        assert!(Location::circle(gp(-90.0, -180.0)?, 0.0).is_ok());
        Ok(())
    }

    #[test]
    fn circle_negative_zero_radius_is_consistent_across_eq_hash_ord() -> TestResult {
        use std::collections::HashSet;
        use std::hash::{Hash, Hasher};

        // A `-0.0` radius must normalize to `+0.0` so the two forms are
        // indistinguishable under Eq, Hash, and Ord — the contract the
        // manual Hash/Ord impls would otherwise violate. (Center
        // normalization is `GeoPoint`'s, covered in `geo.rs`; here the
        // same center is reused so the radius is the only variable.)
        let center = gp(0.0, 0.0)?;
        let neg = Location::circle(center, -0.0)?;
        let pos = Location::circle(center, 0.0)?;

        assert_eq!(neg, pos, "negative and positive zero radius must be equal");

        let mut hasher_neg = std::collections::hash_map::DefaultHasher::new();
        let mut hasher_pos = std::collections::hash_map::DefaultHasher::new();
        neg.hash(&mut hasher_neg);
        pos.hash(&mut hasher_pos);
        assert_eq!(
            hasher_neg.finish(),
            hasher_pos.finish(),
            "equal circles must hash equally"
        );

        let mut set = HashSet::new();
        set.insert(neg.clone());
        set.insert(pos.clone());
        assert_eq!(set.len(), 1, "the two zero-forms must dedup to one entry");

        assert_eq!(
            neg.cmp(&pos),
            std::cmp::Ordering::Equal,
            "equal circles must order Equal"
        );
        Ok(())
    }

    // --- UnresolvedLocation tests ---

    #[test]
    fn one_of_serde_roundtrip() -> TestResult {
        let loc = UnresolvedLocation::OneOf(vec![
            UnresolvedLocation::Reference(LocationReference::NamedPlace {
                name: "Paris".to_string(),
            }),
            UnresolvedLocation::Reference(LocationReference::Address {
                address_text: "123 Main St".to_string(),
            }),
        ]);

        let json = serde_json::to_string(&loc)?;
        let deserialized: UnresolvedLocation = serde_json::from_str(&json)?;
        assert_eq!(loc, deserialized);
        Ok(())
    }

    #[test]
    fn one_of_rejects_single_entry() -> TestResult {
        // Construct a single-entry OneOf — serialization succeeds but
        // deserialization must reject it (minimum 2 entries).
        let loc = UnresolvedLocation::OneOf(vec![UnresolvedLocation::Reference(
            LocationReference::NamedPlace {
                name: "Paris".to_string(),
            },
        )]);
        let json = serde_json::to_string(&loc)?;
        assert!(serde_json::from_str::<UnresolvedLocation>(&json).is_err());
        Ok(())
    }

    #[test]
    fn resolved_serde_roundtrip() -> TestResult {
        let loc = UnresolvedLocation::Resolved(Location::circle(gp(48.8584, 2.2945)?, 10.0)?);
        let json = serde_json::to_string(&loc)?;
        let deserialized: UnresolvedLocation = serde_json::from_str(&json)?;
        assert_eq!(loc, deserialized);
        Ok(())
    }

    #[test]
    fn near_reference_serde_roundtrip() -> TestResult {
        let loc = UnresolvedLocation::Reference(LocationReference::Near {
            reference: Box::new(LocationReference::NamedPlace {
                name: "Paris".to_string(),
            }),
            distance: Some(Distance::WalkingDistance),
        });
        let json = serde_json::to_string(&loc)?;
        let deserialized: UnresolvedLocation = serde_json::from_str(&json)?;
        assert_eq!(loc, deserialized);
        Ok(())
    }

    // --- Location merge (lattice) tests ---

    #[test]
    fn merge_unbounded_is_identity() -> TestResult {
        let c = Location::circle(gp(48.8, 2.3)?, 100.0)?;
        assert_eq!(Location::Unbounded.merge(&c), c);
        assert_eq!(c.merge(&Location::Unbounded), c);
        Ok(())
    }

    #[test]
    fn merge_circle_subsumes_smaller() -> TestResult {
        let big = Location::circle(gp(48.8, 2.3)?, 1000.0)?;
        let small = Location::circle(gp(48.8, 2.3)?, 10.0)?;
        // big contains small → merge returns the tighter (small)
        assert_eq!(big.merge(&small), small);
        assert_eq!(small.merge(&big), small);
        Ok(())
    }

    #[test]
    fn merge_disjoint_circles_produces_union() -> TestResult {
        let paris = Location::circle(gp(48.8, 2.3)?, 10.0)?;
        let london = Location::circle(gp(51.5, -0.1)?, 10.0)?;
        let merged = paris.merge(&london);
        assert!(matches!(merged, Location::UnionOf { ref members } if members.len() == 2));
        Ok(())
    }

    #[test]
    fn merge_flattens_union() -> TestResult {
        let a = Location::circle(gp(48.8, 2.3)?, 10.0)?;
        let b = Location::circle(gp(51.5, -0.1)?, 10.0)?;
        let c = Location::circle(gp(40.7, -74.0)?, 10.0)?;
        // (a ∪ b) merge c should flatten to [a, b, c], not [[a, b], c]
        let ab = a.merge(&b);
        let abc = ab.merge(&c);
        assert!(matches!(abc, Location::UnionOf { ref members } if members.len() == 3));
        Ok(())
    }

    #[test]
    fn merge_idempotent_circle() -> TestResult {
        let c = Location::circle(gp(48.8, 2.3)?, 100.0)?;
        assert_eq!(c.merge(&c), c);
        Ok(())
    }

    #[test]
    fn merge_union_with_union() -> TestResult {
        let a = Location::circle(gp(48.8, 2.3)?, 10.0)?;
        let b = Location::circle(gp(51.5, -0.1)?, 10.0)?;
        let c = Location::circle(gp(40.7, -74.0)?, 10.0)?;
        let d_loc = Location::circle(gp(35.7, 139.7)?, 10.0)?;
        let ab = a.merge(&b); // UnionOf([a, b])
        let cd = c.merge(&d_loc); // UnionOf([c, d])
        let merged = ab.merge(&cd);
        assert!(matches!(merged, Location::UnionOf { ref members } if members.len() == 4));
        Ok(())
    }

    #[test]
    fn merge_offset_circle_containment() -> TestResult {
        // Big circle centered at origin with 1000km radius
        let big = Location::circle(gp(0.0, 0.0)?, 1_000_000.0)?;
        // Small circle offset but still within big
        let small = Location::circle(gp(1.0, 1.0)?, 10.0)?;
        // big should contain small → merge returns small
        assert_eq!(big.merge(&small), small);
        Ok(())
    }

    #[test]
    fn merge_offset_circle_not_contained() -> TestResult {
        // Small circle at origin with 100m radius
        let small_a = Location::circle(gp(0.0, 0.0)?, 100.0)?;
        // Another small circle ~111km away (1 degree of latitude)
        let small_b = Location::circle(gp(1.0, 0.0)?, 100.0)?;
        // Neither contains the other → UnionOf
        let merged = small_a.merge(&small_b);
        assert!(matches!(merged, Location::UnionOf { .. }));
        Ok(())
    }

    // --- Location merge property tests ---

    use proptest::prelude::*;

    fn arb_circle() -> impl Strategy<Value = Location> {
        (-90.0f64..=90.0, -180.0f64..=180.0, 0.0f64..10000.0).prop_filter_map(
            "valid circle",
            |(lat, lon, r)| {
                let center = GeoPoint::new(lat, lon).ok()?;
                Location::circle(center, r).ok()
            },
        )
    }

    fn arb_location() -> impl Strategy<Value = Location> {
        let union = (arb_circle(), arb_circle()).prop_map(|(a, b)| Location::UnionOf {
            members: vec![a, b],
        });
        prop_oneof![
            6 => arb_circle(),
            2 => union,
            2 => Just(Location::Unbounded),
        ]
    }

    proptest! {
        #[test]
        fn prop_location_merge_commutative(a in arb_location(), b in arb_location()) {
            // Commutativity: merge(a, b) == merge(b, a)
            // Note: UnionOf children may be in different order, so we compare
            // by checking mutual containment.
            let ab = a.merge(&b);
            let ba = b.merge(&a);
            // For simple cases (non-union results), direct equality works.
            // For UnionOf, the children are in insertion order which depends on
            // which side is self vs other. We verify by checking that both
            // results contain the same geometry semantically.
            match (&ab, &ba) {
                (Location::UnionOf { members: cs1 }, Location::UnionOf { members: cs2 }) => {
                    prop_assert_eq!(cs1.len(), cs2.len());
                    // Both should contain the same elements (possibly reordered)
                    for c in cs1 {
                        prop_assert!(cs2.contains(c), "ab contains {:?} not in ba", c);
                    }
                }
                _ => prop_assert_eq!(ab, ba),
            }
        }

        #[test]
        fn prop_location_merge_identity(a in arb_location()) {
            prop_assert_eq!(Location::Unbounded.merge(&a), a.clone());
            prop_assert_eq!(a.merge(&Location::Unbounded), a);
        }

        #[test]
        fn prop_location_merge_idempotent(a in arb_circle()) {
            // Idempotent for circles (self-containment is trivially true).
            // UnionOf idempotence is more complex due to child duplication,
            // so we test only atomic locations here.
            prop_assert_eq!(a.merge(&a), a);
        }
    }
}
