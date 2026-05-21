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
//! Merge operates on `UnresolvedLocation` (pure bag union / `OneOf` construction).
//! Resolution maps `LocationReference` → `Location` via a caller-provided closure.
//! The lattice operates on `Location` only.
//!
//! # Entity vs. region scope
//!
//! Entity-scale things — individual buildings, the Forbidden City as a
//! complex, a fortress, a citadel — are first-class entities with their
//! own [`crate::facts::ids::EntityId`]. Containment between such entities
//! is expressed by
//! [`crate::facts::attribute::Fact::Relationship`] with
//! [`crate::facts::attribute::EntityRelationType::Contains`], not by a
//! location reference.
//!
//! Region-scale things — cities, neighborhoods, contested geographical
//! areas like "Manhattan" or "Newark" — are *not* entities in the
//! fact-store grammar. They live as opaque names inside
//! [`LocationReference::NamedPlace`], to be resolved against an external
//! gazetteer at projection time. The grammar does not enforce this split
//! structurally; it is a convention the submission layer follows, and
//! the projection layer relies on the same convention when deciding how
//! to interpret a location claim.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{OhmId, OsmElementType, OsmId};

/// Errors from location construction or validation.
///
/// `Eq` is not derived because the f64-carrying variants reach
/// non-`Eq` values.
#[derive(Debug, Clone, PartialEq)]
pub enum LocationError {
    /// `OneOf` / `UnionOf` requires at least 2 entries.
    TooFewEntries { count: usize },
    /// Latitude is outside `[-90, 90]`.
    LatitudeOutOfRange { lat: f64 },
    /// Longitude is outside `[-180, 180]`.
    LongitudeOutOfRange { lon: f64 },
    /// `lat` or `lon` was `NaN` or infinite.
    NonFiniteCoordinate { lat: f64, lon: f64 },
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
            Self::LatitudeOutOfRange { lat } => {
                write!(f, "latitude {lat} out of range [-90, 90]")
            }
            Self::LongitudeOutOfRange { lon } => {
                write!(f, "longitude {lon} out of range [-180, 180]")
            }
            Self::NonFiniteCoordinate { lat, lon } => {
                write!(f, "lat/lon must be finite (got lat={lat}, lon={lon})")
            }
            Self::NegativeRadius { radius_m } => {
                write!(f, "radius_m must be non-negative, got {radius_m}")
            }
            Self::NonFiniteRadius { radius_m } => {
                write!(f, "radius_m must be finite, got {radius_m}")
            }
        }
    }
}

impl std::error::Error for LocationError {}

// ==================== Resolved Location ====================

/// Resolved location geometry.
///
/// No external references — purely geometric. The lattice (merge via subsumption
/// + union) is defined on this type.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Location {
    /// A point with radius of uncertainty (circle on the earth's surface).
    Circle { lat: f64, lon: f64, radius_m: f64 },
    /// Disjoint or partially-overlapping shapes: "one of these is true."
    /// Parallels [`UnresolvedLocation::OneOf`] at the resolved level.
    /// Flattened when nested. Requires ≥2 children.
    #[serde(rename = "union_of")]
    UnionOf(Vec<Location>),
    /// No geometric information (resolution failed, or genuinely unknown).
    Unbounded,
}

impl<'de> Deserialize<'de> for Location {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum Raw {
            Circle {
                lat: f64,
                lon: f64,
                radius_m: f64,
            },
            #[serde(rename = "union_of")]
            UnionOf(Vec<Location>),
            Unbounded,
        }

        let raw = Raw::deserialize(deserializer)?;
        match raw {
            Raw::Circle { lat, lon, radius_m } => {
                Location::circle(lat, lon, radius_m).map_err(serde::de::Error::custom)
            }
            Raw::UnionOf(children) => {
                if children.len() < 2 {
                    return Err(serde::de::Error::custom(LocationError::TooFewEntries {
                        count: children.len(),
                    }));
                }
                Ok(Location::UnionOf(children))
            }
            Raw::Unbounded => Ok(Location::Unbounded),
        }
    }
}

impl Location {
    /// Create a validated `Circle` location.
    ///
    /// Checks that latitude is in `[-90, 90]`, longitude is in `[-180, 180]`,
    /// neither value is `NaN` or infinite, and radius is non-negative and finite.
    pub fn circle(lat: f64, lon: f64, radius_m: f64) -> Result<Self, LocationError> {
        validate_coordinates(lat, lon)?;
        if !radius_m.is_finite() {
            return Err(LocationError::NonFiniteRadius { radius_m });
        }
        if radius_m < 0.0 {
            return Err(LocationError::NegativeRadius { radius_m });
        }
        Ok(Self::Circle { lat, lon, radius_m })
    }

    /// Create a `Circle` from coordinates without explicit precision.
    ///
    /// Uses `radius_m: 0.0` — the coordinate is taken at face value.
    /// The lattice treats this as a zero-radius circle for geometric operations.
    pub fn point(lat: f64, lon: f64) -> Result<Self, LocationError> {
        Self::circle(lat, lon, 0.0)
    }

    /// Create a validated `UnionOf` with at least 2 children.
    pub fn union_of(children: Vec<Location>) -> Result<Self, LocationError> {
        if children.len() < 2 {
            return Err(LocationError::TooFewEntries {
                count: children.len(),
            });
        }
        Ok(Self::UnionOf(children))
    }

    /// Merge two locations in the subsumption semilattice.
    ///
    /// If one location's region contains the other, returns the tighter (more
    /// precise) one. Otherwise returns `UnionOf` — "one of these, but we can't
    /// determine which geometrically."
    ///
    /// `Unbounded` is the identity: `Unbounded.merge(x) == x`.
    #[must_use]
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
            Self::UnionOf(cs) => children.extend(cs.iter().cloned()),
            other_val => children.push(other_val.clone()),
        }
        match other {
            Self::UnionOf(cs) => children.extend(cs.iter().cloned()),
            other_val => children.push(other_val.clone()),
        }
        Self::UnionOf(children)
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
                    lat: la1,
                    lon: lo1,
                    radius_m: r1,
                },
                Self::Circle {
                    lat: la2,
                    lon: lo2,
                    radius_m: r2,
                },
            ) => {
                let dist = haversine_meters(*la1, *lo1, *la2, *lo2);
                dist + r2 <= *r1
            }
            // UnionOf vs UnionOf: self contains other if every child of other
            // is contained by some child of self.
            (Self::UnionOf(self_children), Self::UnionOf(other_children)) => other_children
                .iter()
                .all(|oc| self_children.iter().any(|sc| sc.contains(oc))),
            // UnionOf contains X if any child contains X
            (Self::UnionOf(children), other_loc) => children.iter().any(|c| c.contains(other_loc)),
            // X contains UnionOf if X contains every child
            (_, Self::UnionOf(children)) => children.iter().all(|c| self.contains(c)),
            // Circle doesn't contain Unbounded (handled above)
        }
    }
}

/// Haversine distance in meters between two (lat, lon) points.
fn haversine_meters(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const EARTH_RADIUS_M: f64 = 6_371_000.0;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    EARTH_RADIUS_M * c
}

pub(crate) fn validate_coordinates(lat: f64, lon: f64) -> Result<(), LocationError> {
    if !lat.is_finite() || !lon.is_finite() {
        return Err(LocationError::NonFiniteCoordinate { lat, lon });
    }
    if !(-90.0..=90.0).contains(&lat) {
        return Err(LocationError::LatitudeOutOfRange { lat });
    }
    if !(-180.0..=180.0).contains(&lon) {
        return Err(LocationError::LongitudeOutOfRange { lon });
    }
    Ok(())
}

// ==================== Unresolved Location ====================

/// A location that may contain unresolved symbolic references.
///
/// Uses adjacently-tagged serde (`tag` + `content`) because `Resolved` wraps
/// a [`Location`] which has its own internal `type` tag — internal tagging
/// on both levels would produce duplicate `type` fields.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
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
/// These need external resolution (geocoding, OSM/OHM lookup, etc.) to
/// produce a [`Location`]. References are deliberately region-scale:
/// entity-scale containment lives on
/// [`crate::facts::attribute::Fact::Relationship`], not here. See the
/// module-level "Entity vs. region scope" note.
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Elevation {
    /// Offset from ground level (0 = ground, negative = below ground).
    CurrentGroundOffset { meters: i32 },
    /// Offset from sea level (positive = above, negative = below).
    SeaLevelOffset { meters: i32 },
}

/// Qualitative distance descriptions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Distance {
    Adjacent,
    AcrossStreet,
    WalkingDistance,
    SameNeighborhood,
    SameDistrict,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // --- Location (resolved) tests ---

    #[test]
    fn valid_circle() {
        let loc = Location::circle(40.7505, -73.9934, 10.0);
        assert!(loc.is_ok());
    }

    #[test]
    fn valid_point() {
        let loc = Location::point(40.7505, -73.9934);
        assert!(loc.is_ok());
        assert!(matches!(
            loc.unwrap(),
            Location::Circle { radius_m, .. } if radius_m == 0.0
        ));
    }

    #[test]
    fn circle_rejects_out_of_range_lat() {
        let loc = Location::circle(91.0, 0.0, 10.0);
        assert!(matches!(loc, Err(LocationError::LatitudeOutOfRange { .. })));
    }

    #[test]
    fn circle_rejects_out_of_range_lon() {
        let loc = Location::circle(0.0, 181.0, 10.0);
        assert!(matches!(
            loc,
            Err(LocationError::LongitudeOutOfRange { .. })
        ));
    }

    #[test]
    fn circle_rejects_nan() {
        let loc = Location::circle(f64::NAN, 0.0, 10.0);
        assert!(matches!(
            loc,
            Err(LocationError::NonFiniteCoordinate { .. })
        ));
    }

    #[test]
    fn circle_rejects_infinity() {
        let loc = Location::circle(0.0, f64::INFINITY, 10.0);
        assert!(matches!(
            loc,
            Err(LocationError::NonFiniteCoordinate { .. })
        ));
    }

    #[test]
    fn circle_rejects_negative_radius() {
        let loc = Location::circle(0.0, 0.0, -1.0);
        assert!(matches!(loc, Err(LocationError::NegativeRadius { .. })));
    }

    #[test]
    fn circle_boundary_values() {
        assert!(Location::circle(90.0, 180.0, 0.0).is_ok());
        assert!(Location::circle(-90.0, -180.0, 0.0).is_ok());
    }

    // --- UnresolvedLocation tests ---

    #[test]
    fn one_of_serde_roundtrip() {
        let loc = UnresolvedLocation::OneOf(vec![
            UnresolvedLocation::Reference(LocationReference::NamedPlace {
                name: "Paris".to_string(),
            }),
            UnresolvedLocation::Reference(LocationReference::Address {
                address_text: "123 Main St".to_string(),
            }),
        ]);

        let json = serde_json::to_string(&loc).unwrap();
        let deserialized: UnresolvedLocation = serde_json::from_str(&json).unwrap();
        assert_eq!(loc, deserialized);
    }

    #[test]
    fn one_of_rejects_single_entry() {
        // Construct a single-entry OneOf — serialization succeeds but
        // deserialization must reject it (minimum 2 entries).
        let loc = UnresolvedLocation::OneOf(vec![UnresolvedLocation::Reference(
            LocationReference::NamedPlace {
                name: "Paris".to_string(),
            },
        )]);
        let json = serde_json::to_string(&loc).unwrap();
        assert!(serde_json::from_str::<UnresolvedLocation>(&json).is_err());
    }

    #[test]
    fn resolved_serde_roundtrip() {
        let loc = UnresolvedLocation::Resolved(Location::circle(48.8584, 2.2945, 10.0).unwrap());
        let json = serde_json::to_string(&loc).unwrap();
        let deserialized: UnresolvedLocation = serde_json::from_str(&json).unwrap();
        assert_eq!(loc, deserialized);
    }

    #[test]
    fn near_reference_serde_roundtrip() {
        let loc = UnresolvedLocation::Reference(LocationReference::Near {
            reference: Box::new(LocationReference::NamedPlace {
                name: "Paris".to_string(),
            }),
            distance: Some(Distance::WalkingDistance),
        });
        let json = serde_json::to_string(&loc).unwrap();
        let deserialized: UnresolvedLocation = serde_json::from_str(&json).unwrap();
        assert_eq!(loc, deserialized);
    }

    // --- Location merge (lattice) tests ---

    #[test]
    fn merge_unbounded_is_identity() {
        let c = Location::circle(48.8, 2.3, 100.0).unwrap();
        assert_eq!(Location::Unbounded.merge(&c), c);
        assert_eq!(c.merge(&Location::Unbounded), c);
    }

    #[test]
    fn merge_circle_subsumes_smaller() {
        let big = Location::circle(48.8, 2.3, 1000.0).unwrap();
        let small = Location::circle(48.8, 2.3, 10.0).unwrap();
        // big contains small → merge returns the tighter (small)
        assert_eq!(big.merge(&small), small);
        assert_eq!(small.merge(&big), small);
    }

    #[test]
    fn merge_disjoint_circles_produces_union() {
        let paris = Location::circle(48.8, 2.3, 10.0).unwrap();
        let london = Location::circle(51.5, -0.1, 10.0).unwrap();
        let merged = paris.merge(&london);
        assert!(matches!(merged, Location::UnionOf(ref cs) if cs.len() == 2));
    }

    #[test]
    fn merge_flattens_union() {
        let a = Location::circle(48.8, 2.3, 10.0).unwrap();
        let b = Location::circle(51.5, -0.1, 10.0).unwrap();
        let c = Location::circle(40.7, -74.0, 10.0).unwrap();
        // (a ∪ b) merge c should flatten to [a, b, c], not [[a, b], c]
        let ab = a.merge(&b);
        let abc = ab.merge(&c);
        assert!(matches!(abc, Location::UnionOf(ref cs) if cs.len() == 3));
    }

    #[test]
    fn merge_idempotent_circle() {
        let c = Location::circle(48.8, 2.3, 100.0).unwrap();
        assert_eq!(c.merge(&c), c);
    }

    #[test]
    fn merge_idempotent_unbounded() {
        assert_eq!(
            Location::Unbounded.merge(&Location::Unbounded),
            Location::Unbounded
        );
    }

    #[test]
    fn merge_union_with_union() {
        let a = Location::circle(48.8, 2.3, 10.0).unwrap();
        let b = Location::circle(51.5, -0.1, 10.0).unwrap();
        let c = Location::circle(40.7, -74.0, 10.0).unwrap();
        let d_loc = Location::circle(35.7, 139.7, 10.0).unwrap();
        let ab = a.merge(&b); // UnionOf([a, b])
        let cd = c.merge(&d_loc); // UnionOf([c, d])
        let merged = ab.merge(&cd);
        assert!(matches!(merged, Location::UnionOf(ref cs) if cs.len() == 4));
    }

    #[test]
    fn merge_offset_circle_containment() {
        // Big circle centered at origin with 1000km radius
        let big = Location::circle(0.0, 0.0, 1_000_000.0).unwrap();
        // Small circle offset but still within big
        let small = Location::circle(1.0, 1.0, 10.0).unwrap();
        // big should contain small → merge returns small
        assert_eq!(big.merge(&small), small);
    }

    #[test]
    fn merge_offset_circle_not_contained() {
        // Small circle at origin with 100m radius
        let small_a = Location::circle(0.0, 0.0, 100.0).unwrap();
        // Another small circle ~111km away (1 degree of latitude)
        let small_b = Location::circle(1.0, 0.0, 100.0).unwrap();
        // Neither contains the other → UnionOf
        let merged = small_a.merge(&small_b);
        assert!(matches!(merged, Location::UnionOf(_)));
    }

    // --- Location merge property tests ---

    use proptest::prelude::*;

    fn arb_circle() -> impl Strategy<Value = Location> {
        (-90.0f64..=90.0, -180.0f64..=180.0, 0.0f64..10000.0)
            .prop_filter_map("valid circle", |(lat, lon, r)| {
                Location::circle(lat, lon, r).ok()
            })
    }

    fn arb_location() -> impl Strategy<Value = Location> {
        let union = (arb_circle(), arb_circle()).prop_map(|(a, b)| Location::UnionOf(vec![a, b]));
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
                (Location::UnionOf(cs1), Location::UnionOf(cs2)) => {
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
