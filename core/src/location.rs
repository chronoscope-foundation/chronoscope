//! Location types with uncertainty support.
//!
//! Types for representing locations with various levels of precision and uncertainty.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{EntityId, OhmId, OsmElementType, OsmId};

/// Errors from location construction or validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocationError {
    /// `MultipleConstraints` requires at least 2 constraints.
    TooFewConstraints { count: usize },
    /// `MultipleConstraints` cannot be nested recursively.
    RecursiveConstraints,
    /// Coordinate values are out of range or non-finite.
    InvalidCoordinates { reason: String },
}

impl fmt::Display for LocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooFewConstraints { count } => {
                write!(
                    f,
                    "MultipleConstraints requires at least 2 constraints, got {count}"
                )
            }
            Self::RecursiveConstraints => {
                write!(
                    f,
                    "MultipleConstraints cannot contain nested MultipleConstraints"
                )
            }
            Self::InvalidCoordinates { reason } => {
                write!(f, "invalid coordinates: {reason}")
            }
        }
    }
}

impl std::error::Error for LocationError {}

/// Location representation with multiple possible forms.
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UncertainLocation {
    #[non_exhaustive]
    Coordinates {
        lat: f64,
        lon: f64,
        elevation: Option<Elevation>,
        precision_m: Option<u32>,
    },
    #[serde(rename = "osm_reference")]
    OsmReference {
        osm_type: OsmElementType,
        osm_id: OsmId,
    },
    /// `OpenHistoricalMap` element reference.
    #[serde(rename = "ohm_reference")]
    OhmReference {
        ohm_id: OhmId,
    },
    NamedLocation {
        name: String,
    },
    Address {
        address_text: String,
    },
    NearEntity {
        reference_entity: EntityId,
        distance: Option<Distance>,
    },
    #[non_exhaustive]
    MultipleConstraints {
        constraints: Vec<UncertainLocation>,
    },
}

impl UncertainLocation {
    /// Create a validated `Coordinates` location.
    ///
    /// Checks that latitude is in `[-90, 90]`, longitude is in `[-180, 180]`,
    /// and neither value is `NaN` or infinite.
    pub fn coordinates(
        lat: f64,
        lon: f64,
        elevation: Option<Elevation>,
        precision_m: Option<u32>,
    ) -> Result<Self, LocationError> {
        if !lat.is_finite() || !lon.is_finite() {
            return Err(LocationError::InvalidCoordinates {
                reason: "lat/lon must be finite".to_string(),
            });
        }
        if !(-90.0..=90.0).contains(&lat) {
            return Err(LocationError::InvalidCoordinates {
                reason: format!("latitude {lat} out of range [-90, 90]"),
            });
        }
        if !(-180.0..=180.0).contains(&lon) {
            return Err(LocationError::InvalidCoordinates {
                reason: format!("longitude {lon} out of range [-180, 180]"),
            });
        }
        Ok(Self::Coordinates {
            lat,
            lon,
            elevation,
            precision_m,
        })
    }

    /// Create a validated `MultipleConstraints` location.
    ///
    /// Checks that at least 2 constraints are provided and none are themselves
    /// `MultipleConstraints` (no recursive nesting).
    pub fn multiple_constraints(
        constraints: Vec<UncertainLocation>,
    ) -> Result<Self, LocationError> {
        if constraints.len() < 2 {
            return Err(LocationError::TooFewConstraints {
                count: constraints.len(),
            });
        }
        for c in &constraints {
            if matches!(c, Self::MultipleConstraints { .. }) {
                return Err(LocationError::RecursiveConstraints);
            }
        }
        Ok(Self::MultipleConstraints { constraints })
    }
}

// Custom Deserialize that validates Coordinates and MultipleConstraints on deserialization.
impl<'de> Deserialize<'de> for UncertainLocation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Intermediate type that deserializes without validation.
        #[serde_with::skip_serializing_none]
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum UncertainLocationRaw {
            Coordinates {
                lat: f64,
                lon: f64,
                elevation: Option<Elevation>,
                precision_m: Option<u32>,
            },
            #[serde(rename = "osm_reference")]
            OsmReference {
                osm_type: OsmElementType,
                osm_id: OsmId,
            },
            #[serde(rename = "ohm_reference")]
            OhmReference {
                ohm_id: OhmId,
            },
            NamedLocation {
                name: String,
            },
            Address {
                address_text: String,
            },
            NearEntity {
                reference_entity: EntityId,
                distance: Option<Distance>,
            },
            MultipleConstraints {
                constraints: Vec<UncertainLocation>,
            },
        }

        let raw = UncertainLocationRaw::deserialize(deserializer)?;
        match raw {
            UncertainLocationRaw::Coordinates {
                lat,
                lon,
                elevation,
                precision_m,
            } => UncertainLocation::coordinates(lat, lon, elevation, precision_m)
                .map_err(serde::de::Error::custom),
            UncertainLocationRaw::OsmReference { osm_type, osm_id } => {
                Ok(UncertainLocation::OsmReference { osm_type, osm_id })
            }
            UncertainLocationRaw::OhmReference { ohm_id } => {
                Ok(UncertainLocation::OhmReference { ohm_id })
            }
            UncertainLocationRaw::NamedLocation { name } => {
                Ok(UncertainLocation::NamedLocation { name })
            }
            UncertainLocationRaw::Address { address_text } => {
                Ok(UncertainLocation::Address { address_text })
            }
            UncertainLocationRaw::NearEntity {
                reference_entity,
                distance,
            } => Ok(UncertainLocation::NearEntity {
                reference_entity,
                distance,
            }),
            UncertainLocationRaw::MultipleConstraints { constraints } => {
                UncertainLocation::multiple_constraints(constraints)
                    .map_err(serde::de::Error::custom)
            }
        }
    }
}

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

    #[test]
    fn valid_coordinates() {
        let loc = UncertainLocation::coordinates(40.7505, -73.9934, None, Some(10));
        assert!(loc.is_ok());
    }

    #[test]
    fn coordinates_rejects_out_of_range_lat() {
        let loc = UncertainLocation::coordinates(91.0, 0.0, None, None);
        assert!(matches!(loc, Err(LocationError::InvalidCoordinates { .. })));
    }

    #[test]
    fn coordinates_rejects_out_of_range_lon() {
        let loc = UncertainLocation::coordinates(0.0, 181.0, None, None);
        assert!(matches!(loc, Err(LocationError::InvalidCoordinates { .. })));
    }

    #[test]
    fn coordinates_rejects_nan() {
        let loc = UncertainLocation::coordinates(f64::NAN, 0.0, None, None);
        assert!(matches!(loc, Err(LocationError::InvalidCoordinates { .. })));
    }

    #[test]
    fn coordinates_rejects_infinity() {
        let loc = UncertainLocation::coordinates(0.0, f64::INFINITY, None, None);
        assert!(matches!(loc, Err(LocationError::InvalidCoordinates { .. })));
    }

    #[test]
    fn coordinates_boundary_values() {
        assert!(UncertainLocation::coordinates(90.0, 180.0, None, None).is_ok());
        assert!(UncertainLocation::coordinates(-90.0, -180.0, None, None).is_ok());
    }

    #[test]
    fn multiple_constraints_valid() {
        let loc = UncertainLocation::multiple_constraints(vec![
            UncertainLocation::NamedLocation {
                name: "Paris".to_string(),
            },
            UncertainLocation::Address {
                address_text: "123 Main St".to_string(),
            },
        ]);
        assert!(loc.is_ok());
    }

    #[test]
    fn multiple_constraints_rejects_too_few() {
        let loc = UncertainLocation::multiple_constraints(vec![UncertainLocation::NamedLocation {
            name: "Paris".to_string(),
        }]);
        assert!(matches!(
            loc,
            Err(LocationError::TooFewConstraints { count: 1 })
        ));
    }

    #[test]
    fn multiple_constraints_rejects_empty() {
        let loc = UncertainLocation::multiple_constraints(vec![]);
        assert!(matches!(
            loc,
            Err(LocationError::TooFewConstraints { count: 0 })
        ));
    }

    #[test]
    fn multiple_constraints_rejects_recursive() {
        let inner = UncertainLocation::multiple_constraints(vec![
            UncertainLocation::NamedLocation {
                name: "A".to_string(),
            },
            UncertainLocation::NamedLocation {
                name: "B".to_string(),
            },
        ])
        .unwrap();

        let loc = UncertainLocation::multiple_constraints(vec![
            inner,
            UncertainLocation::NamedLocation {
                name: "C".to_string(),
            },
        ]);
        assert!(matches!(loc, Err(LocationError::RecursiveConstraints)));
    }

    #[test]
    fn deserialize_rejects_invalid_coordinates() {
        let json = r#"{"type":"coordinates","lat":91.0,"lon":0.0}"#;
        assert!(serde_json::from_str::<UncertainLocation>(json).is_err());
    }

    #[test]
    fn deserialize_rejects_single_constraint() {
        let json = r#"{"type":"multiple_constraints","constraints":[{"type":"named_location","name":"Paris"}]}"#;
        assert!(serde_json::from_str::<UncertainLocation>(json).is_err());
    }

    #[test]
    fn deserialize_valid_coordinates() {
        let json = r#"{"type":"coordinates","lat":40.7505,"lon":-73.9934,"precision_m":10}"#;
        let loc: UncertainLocation = serde_json::from_str(json).unwrap();
        assert!(matches!(loc, UncertainLocation::Coordinates { .. }));
    }
}
