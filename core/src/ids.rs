//! ID types for external system references.
//!
//! Type-safe wrappers around identifiers from external knowledge bases
//! (`OpenStreetMap`, `OpenHistoricalMap`, Wikidata, `GeoNames`, Getty TGN,
//! Pleiades, NRHP). Each carries a smart constructor that rejects
//! malformed values at the boundary and a manual `Deserialize` that
//! routes wire input through that same constructor.
//!
//! Internal infrastructure IDs (`EntityId`, `LifetimeEventId`,
//! `ImageId`, etc.) live in [`crate::facts::ids`] and follow the same
//! "parse at the wire boundary" pattern via the
//! [`string_id_newtype`](crate::string_id_newtype) macro.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ============================================================================
// Errors
// ============================================================================

/// Error returned by the wire-boundary `Deserialize` impls for the
/// string-shaped external IDs (`WikidataEntityId`, `WikidataPropertyId`,
/// `NrhpReferenceNumber`).
///
/// In-process constructors (`Self::new`) are infallible — the
/// non-empty invariant is enforced only at the wire boundary, where
/// untrusted input enters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalStringIdError {
    /// The supplied string was empty.
    Empty,
}

impl std::fmt::Display for ExternalStringIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "external id must not be empty"),
        }
    }
}

impl std::error::Error for ExternalStringIdError {}

// ============================================================================
// Numeric-ID macro
// ============================================================================

/// Emit a transparent `u64`-shaped newtype.
///
/// `u64` makes negative values structurally unrepresentable. The
/// generated type carries: infallible constructor `Self::new`, `get`,
/// `Display`, plus the full derive set including `Serialize` and
/// `Deserialize` — `#[serde(transparent)]` means the wire format is a
/// plain JSON number that the serde-default `u64` deserializer already
/// rejects on overflow or negative input.
#[macro_export]
macro_rules! numeric_id_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            ::serde::Serialize,
            ::serde::Deserialize,
            ::schemars::JsonSchema,
        )]
        #[serde(transparent)]
        pub struct $name {
            inner: u64,
        }

        impl $name {
            #[doc = "Wrap a raw `u64` external identifier."]
            #[must_use]
            pub fn new(id: u64) -> Self {
                Self { inner: id }
            }

            #[doc = "The underlying integer."]
            #[must_use]
            pub fn get(self) -> u64 {
                self.inner
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                ::std::fmt::Display::fmt(&self.inner, f)
            }
        }
    };
}

// ============================================================================
// Numeric IDs (macro-generated)
// ============================================================================

numeric_id_newtype! {
    /// `OpenStreetMap` element ID. Combined with [`OsmElementType`] to
    /// disambiguate node / way / relation references.
    OsmId
}

numeric_id_newtype! {
    /// `OpenHistoricalMap` element ID.
    OhmId
}

numeric_id_newtype! {
    /// `GeoNames` geographical feature ID.
    GeoNamesId
}

numeric_id_newtype! {
    /// Getty Thesaurus of Geographic Names entry ID.
    GettyTgnId
}

numeric_id_newtype! {
    /// Pleiades gazetteer place ID.
    PleiadesPlaceId
}

// ============================================================================
// OSM element type enum (untouched by smart-constructor work)
// ============================================================================

/// `OpenStreetMap` element types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(feature = "sqlx", sqlx(type_name = "TEXT", rename_all = "snake_case"))]
#[serde(rename_all = "snake_case")]
pub enum OsmElementType {
    Node,
    Way,
    Relation,
}

// ============================================================================
// Wikidata IDs
// ============================================================================

/// Wikidata entity ID (e.g., `"Q12345"`).
///
/// The smart constructor enforces non-empty input but not the full
/// `Q`-prefix-plus-digits format. Format-strict callers should use
/// [`WikidataEntityId::is_well_formed`] to check, or call the
/// URL-parser in [`crate::facts::citations::ExternalReference::from_url`]
/// which validates format before constructing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct WikidataEntityId {
    inner: String,
}

impl WikidataEntityId {
    /// Wrap an in-process string. Infallible — non-empty validation
    /// only fires at the wire boundary via `Deserialize`.
    #[must_use]
    pub fn new(qid: impl Into<String>) -> Self {
        Self { inner: qid.into() }
    }

    /// The underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.inner
    }

    /// Whether this id matches the canonical Wikidata QID shape:
    /// uppercase `Q` followed by one or more ASCII digits.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        is_wikidata_id(&self.inner, b'Q')
    }
}

impl std::fmt::Display for WikidataEntityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.inner, f)
    }
}

impl AsRef<str> for WikidataEntityId {
    fn as_ref(&self) -> &str {
        &self.inner
    }
}

impl<'de> Deserialize<'de> for WikidataEntityId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if s.is_empty() {
            return Err(serde::de::Error::custom(ExternalStringIdError::Empty));
        }
        Ok(Self { inner: s })
    }
}

/// Wikidata property ID (e.g., `"P571"` for inception).
///
/// The smart constructor enforces non-empty input but not the full
/// `P`-prefix-plus-digits format. Format-strict callers should use
/// [`WikidataPropertyId::is_well_formed`] to check.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct WikidataPropertyId {
    inner: String,
}

impl WikidataPropertyId {
    /// Wrap an in-process string. Infallible — non-empty validation
    /// only fires at the wire boundary via `Deserialize`.
    #[must_use]
    pub fn new(pid: impl Into<String>) -> Self {
        Self { inner: pid.into() }
    }

    /// The underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.inner
    }

    /// Whether this id matches the canonical Wikidata property shape:
    /// uppercase `P` followed by one or more ASCII digits.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        is_wikidata_id(&self.inner, b'P')
    }
}

impl std::fmt::Display for WikidataPropertyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.inner, f)
    }
}

impl AsRef<str> for WikidataPropertyId {
    fn as_ref(&self) -> &str {
        &self.inner
    }
}

impl<'de> Deserialize<'de> for WikidataPropertyId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if s.is_empty() {
            return Err(serde::de::Error::custom(ExternalStringIdError::Empty));
        }
        Ok(Self { inner: s })
    }
}

/// Shape check shared by [`WikidataEntityId::is_well_formed`] and
/// [`WikidataPropertyId::is_well_formed`]: a single-byte ASCII prefix
/// (`b'Q'` or `b'P'`) followed by one or more ASCII digits.
fn is_wikidata_id(s: &str, prefix: u8) -> bool {
    let bytes = s.as_bytes();
    match bytes {
        [head, rest @ ..] if *head == prefix && !rest.is_empty() => {
            rest.iter().all(u8::is_ascii_digit)
        }
        _ => false,
    }
}

// ============================================================================
// NRHP reference number
// ============================================================================

/// US National Register of Historic Places reference number (typically
/// an 8-digit string, occasionally with letter suffixes for amendments).
/// Wrapped as a string rather than an integer because the leading zeros
/// are part of the identity and the letter suffix is legitimate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct NrhpReferenceNumber {
    inner: String,
}

impl NrhpReferenceNumber {
    /// Wrap an in-process string. Infallible — non-empty validation
    /// only fires at the wire boundary via `Deserialize`.
    #[must_use]
    pub fn new(number: impl Into<String>) -> Self {
        Self {
            inner: number.into(),
        }
    }

    /// The underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.inner
    }
}

impl std::fmt::Display for NrhpReferenceNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.inner, f)
    }
}

impl AsRef<str> for NrhpReferenceNumber {
    fn as_ref(&self) -> &str {
        &self.inner
    }
}

impl<'de> Deserialize<'de> for NrhpReferenceNumber {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if s.is_empty() {
            return Err(serde::de::Error::custom(ExternalStringIdError::Empty));
        }
        Ok(Self { inner: s })
    }
}

// ============================================================================
// TriggerEventId
// ============================================================================

/// Reference to an external event that triggered a transition.
///
/// Currently a simple string wrapper (e.g., Wikidata event ID `"Q362"`
/// for WWII). Pre-existing tuple struct retained for the duration of
/// the old entity-model migration; the new fact-store grammar doesn't
/// reference this type and the surrounding old-ADT code is scheduled
/// for removal with the entity-model demolition.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct TriggerEventId(pub String);

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osm_id_deserialize_rejects_negative_wire_input() {
        let result: Result<OsmId, _> = serde_json::from_str("-5");
        assert!(result.is_err(), "negative must fail at deserialize");
    }

    #[test]
    fn wikidata_entity_id_well_formed_rejects_p_prefix() {
        let id = WikidataEntityId::new("P571");
        assert!(!id.is_well_formed(), "P-prefixed value is not a QID");
    }

    #[test]
    fn wikidata_entity_id_well_formed_rejects_letter_in_digits() {
        let id = WikidataEntityId::new("Q1a3");
        assert!(!id.is_well_formed());
    }

    #[test]
    fn wikidata_entity_id_well_formed_rejects_q_only() {
        let id = WikidataEntityId::new("Q");
        assert!(!id.is_well_formed(), "Q alone is not a QID");
    }

    #[test]
    fn wikidata_entity_id_deserialize_rejects_empty() {
        let result: Result<WikidataEntityId, _> = serde_json::from_str("\"\"");
        assert!(result.is_err(), "empty must fail at deserialize");
    }

    #[test]
    fn wikidata_property_id_well_formed() {
        let id = WikidataPropertyId::new("P571");
        assert!(id.is_well_formed());
        let mismatch = WikidataPropertyId::new("Q571");
        assert!(!mismatch.is_well_formed(), "Q-prefixed value is not a PID");
    }

    #[test]
    fn nrhp_reference_number_deserialize_rejects_empty() {
        let result: Result<NrhpReferenceNumber, _> = serde_json::from_str("\"\"");
        assert!(result.is_err(), "empty must fail at deserialize");
    }
}
