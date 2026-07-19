//! ID types for external system references.
//!
//! Type-safe wrappers around identifiers from external knowledge bases
//! (`OpenStreetMap`, `OpenHistoricalMap`, Wikidata, `GeoNames`, Getty TGN,
//! Pleiades, NRHP). Construction-from-untrusted-input is a validating
//! parser that rejects malformed values at the boundary, and a manual
//! `Deserialize` routes wire input through that same parser. The
//! Wikidata IDs are `u64`-backed (the numeric part of `Q<n>`/`P<n>`),
//! so a malformed id is unrepresentable; `NrhpReferenceNumber` stays a
//! string because its leading zeros and letter suffixes are part of its
//! identity.
//!
//! Internal infrastructure IDs (`UserId`, `IngesterRunId`,
//! `AnalyzerProcess`, etc.) live in [`crate::grammar::ids`] and follow the same
//! "parse at the wire boundary" pattern via the
//! [`string_id_newtype`](crate::string_id_newtype) macro.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ============================================================================
// Errors
// ============================================================================

/// Error returned by the wire-boundary `Deserialize` impl for the
/// remaining string-shaped external ID (`NrhpReferenceNumber`).
///
/// `NrhpReferenceNumber::new` is infallible — the non-empty invariant
/// is enforced only at the wire boundary, where untrusted input enters.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExternalStringIdError {
    /// The supplied string was empty.
    #[error("external id must not be empty")]
    Empty,
}

/// Error returned when parsing a Wikidata ID from its string form
/// (`<prefix><digits>`, e.g. `"Q12345"` / `"P1448"`).
///
/// Carries enough context to diagnose a rejection at the boundary: the
/// offending input and which prefix was expected. Surfaced by
/// [`WikidataEntityId`]'s and [`WikidataPropertyId`]'s `FromStr` /
/// `Deserialize` impls.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WikidataIdParseError {
    /// The input was empty.
    #[error("wikidata id must not be empty")]
    Empty,
    /// The first byte was not the expected prefix (`'Q'` for entities,
    /// `'P'` for properties). Holds the expected prefix and the input.
    #[error("wikidata id {found:?} must start with {expected:?}")]
    WrongPrefix { expected: char, found: String },
    /// The prefix was present but no digits followed it.
    #[error("wikidata id {found:?} must have digits after the {expected:?} prefix")]
    MissingDigits { expected: char, found: String },
    /// A byte after the prefix was not an ASCII digit.
    #[error("wikidata id {found:?} must be {expected:?} followed only by ASCII digits")]
    NonDigitTail { expected: char, found: String },
    /// The digits parsed but overflowed `u64`.
    #[error("wikidata id {found:?} numeric part overflows u64")]
    Overflow { found: String },
}

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
            pub fn new(id: u64) -> Self {
                Self { inner: id }
            }

            #[doc = "The underlying integer."]
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
// OSM element type enum
// ============================================================================

/// `OpenStreetMap` element types.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(feature = "sqlx", sqlx(type_name = "TEXT", rename_all = "snake_case"))]
#[serde(rename_all = "snake_case")]
pub enum OsmElementType {
    Node,
    Way,
    Relation,
}

impl OsmElementType {
    /// The URL path segment naming this element type — `node`, `way`, or
    /// `relation` — as used in OSM and OHM element URLs.
    pub fn segment(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Way => "way",
            Self::Relation => "relation",
        }
    }

    /// The element type a URL path segment names, the inverse of
    /// [`segment`](Self::segment); `None` for any other segment.
    pub fn from_segment(segment: &str) -> Option<Self> {
        match segment {
            "node" => Some(Self::Node),
            "way" => Some(Self::Way),
            "relation" => Some(Self::Relation),
            _ => None,
        }
    }
}

// ============================================================================
// Wikidata IDs
// ============================================================================

/// Parser core shared by the two Wikidata ID newtypes: validate that
/// `s` is `<prefix><digits>` and return the numeric part as `u64`.
///
/// Rejects empty input, a wrong/missing prefix, a missing or non-digit
/// tail, and `u64` overflow — each with a context-carrying
/// [`WikidataIdParseError`]. This is the single boundary parse; the
/// newtypes' `FromStr`/`Deserialize` impls delegate here so a value of
/// either type is proof the format is valid.
fn parse_wikidata_id(s: &str, prefix: u8) -> Result<u64, WikidataIdParseError> {
    let expected = prefix as char;
    let bytes = s.as_bytes();
    match bytes {
        [] => Err(WikidataIdParseError::Empty),
        [head, rest @ ..] if *head == prefix => {
            if rest.is_empty() {
                return Err(WikidataIdParseError::MissingDigits {
                    expected,
                    found: s.to_owned(),
                });
            }
            if !rest.iter().all(u8::is_ascii_digit) {
                return Err(WikidataIdParseError::NonDigitTail {
                    expected,
                    found: s.to_owned(),
                });
            }
            // `rest` is all ASCII digits, so it is valid UTF-8 and the
            // only way `parse` fails here is u64 overflow.
            let digits = &s[1..];
            digits
                .parse::<u64>()
                .map_err(|_| WikidataIdParseError::Overflow {
                    found: s.to_owned(),
                })
        }
        _ => Err(WikidataIdParseError::WrongPrefix {
            expected,
            found: s.to_owned(),
        }),
    }
}

/// Build a `JsonSchema` describing the JSON **string** wire form of a
/// Wikidata ID. The in-memory representation is a `u64`, but the
/// serialized shape — and therefore the OpenAPI contract — is a string
/// (`"Q12345"`), so we delegate to `String`'s schema rather than
/// describing the integer.
fn wikidata_id_schema(
    generator: &mut schemars::r#gen::SchemaGenerator,
) -> schemars::schema::Schema {
    String::json_schema(generator)
}

/// Wikidata entity ID (e.g., `"Q12345"`), stored as the numeric part.
///
/// Construction-from-string validates the full `Q`-prefix-plus-digits
/// format: [`FromStr`](std::str::FromStr) (and the inherent
/// [`WikidataEntityId::parse`]) reject empty input, a wrong/missing
/// prefix, a non-digit tail, and `u64` overflow, so a value of this type
/// is proof the format is valid. [`WikidataEntityId::new`] wraps an
/// already-numeric id (e.g. from a typed source or a test). The wire
/// form is the canonical string `"Q<n>"`; `Deserialize` routes wire
/// input through the same parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WikidataEntityId {
    inner: u64,
}

impl WikidataEntityId {
    /// Wrap an already-numeric entity id (the `n` in `Q<n>`). Infallible
    /// — for callers that hold the numeric id directly; string input
    /// must instead go through [`FromStr`](std::str::FromStr) /
    /// [`WikidataEntityId::parse`], which validate the format.
    pub fn new(n: u64) -> Self {
        Self { inner: n }
    }

    /// Parse the canonical `Q<digits>` string form, validating the
    /// prefix and digits at the boundary.
    ///
    /// # Errors
    /// Returns [`WikidataIdParseError`] for empty input, a wrong/missing
    /// `Q` prefix, a non-digit tail, or `u64` overflow.
    pub fn parse(s: &str) -> Result<Self, WikidataIdParseError> {
        parse_wikidata_id(s, b'Q').map(|inner| Self { inner })
    }

    /// The numeric part (the `n` in `Q<n>`).
    pub fn get(self) -> u64 {
        self.inner
    }
}

impl std::str::FromStr for WikidataEntityId {
    type Err = WikidataIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl std::fmt::Display for WikidataEntityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Q{}", self.inner)
    }
}

impl Serialize for WikidataEntityId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&format!("Q{}", self.inner))
    }
}

impl<'de> Deserialize<'de> for WikidataEntityId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for WikidataEntityId {
    fn schema_name() -> String {
        "WikidataEntityId".to_string()
    }

    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        wikidata_id_schema(generator)
    }
}

/// Wikidata property ID (e.g., `"P571"` for inception), stored as the
/// numeric part.
///
/// Construction-from-string validates the full `P`-prefix-plus-digits
/// format: [`FromStr`](std::str::FromStr) (and the inherent
/// [`WikidataPropertyId::parse`]) reject empty input, a wrong/missing
/// prefix, a non-digit tail, and `u64` overflow, so a value of this type
/// is proof the format is valid. [`WikidataPropertyId::new`] wraps an
/// already-numeric id (e.g. the constant `P1448`). The wire form is the
/// canonical string `"P<n>"`; `Deserialize` routes wire input through
/// the same parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WikidataPropertyId {
    inner: u64,
}

impl WikidataPropertyId {
    /// Wrap an already-numeric property id (the `n` in `P<n>`).
    /// Infallible — for callers that hold the numeric id directly (e.g.
    /// a hardcoded property constant); string input must instead go
    /// through [`FromStr`](std::str::FromStr) /
    /// [`WikidataPropertyId::parse`], which validate the format.
    pub fn new(n: u64) -> Self {
        Self { inner: n }
    }

    /// Parse the canonical `P<digits>` string form, validating the
    /// prefix and digits at the boundary.
    ///
    /// # Errors
    /// Returns [`WikidataIdParseError`] for empty input, a wrong/missing
    /// `P` prefix, a non-digit tail, or `u64` overflow.
    pub fn parse(s: &str) -> Result<Self, WikidataIdParseError> {
        parse_wikidata_id(s, b'P').map(|inner| Self { inner })
    }

    /// The numeric part (the `n` in `P<n>`).
    pub fn get(self) -> u64 {
        self.inner
    }
}

impl std::str::FromStr for WikidataPropertyId {
    type Err = WikidataIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl std::fmt::Display for WikidataPropertyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "P{}", self.inner)
    }
}

impl Serialize for WikidataPropertyId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&format!("P{}", self.inner))
    }
}

impl<'de> Deserialize<'de> for WikidataPropertyId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for WikidataPropertyId {
    fn schema_name() -> String {
        "WikidataPropertyId".to_string()
    }

    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        wikidata_id_schema(generator)
    }
}

// ============================================================================
// NRHP reference number
// ============================================================================

/// US National Register of Historic Places reference number (typically
/// an 8-digit string, occasionally with letter suffixes for amendments).
/// Wrapped as a string rather than an integer because the leading zeros
/// are part of the identity and the letter suffix is legitimate.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct NrhpReferenceNumber {
    inner: String,
}

impl NrhpReferenceNumber {
    /// Wrap an in-process string. Infallible — non-empty validation
    /// only fires at the wire boundary via `Deserialize`.
    pub fn new(number: impl Into<String>) -> Self {
        Self {
            inner: number.into(),
        }
    }

    /// The underlying string slice.
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

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn osm_id_deserialize_rejects_negative_wire_input() {
        let result: Result<OsmId, _> = serde_json::from_str("-5");
        assert!(result.is_err(), "negative must fail at deserialize");
    }

    #[test]
    fn wikidata_entity_id_parse_rejects_p_prefix() {
        assert!(
            "P571".parse::<WikidataEntityId>().is_err(),
            "P-prefixed value is not a QID"
        );
    }

    #[test]
    fn wikidata_entity_id_parse_rejects_letter_in_digits() {
        assert!("Q1a3".parse::<WikidataEntityId>().is_err());
    }

    #[test]
    fn wikidata_entity_id_parse_rejects_q_only() {
        assert!(
            "Q".parse::<WikidataEntityId>().is_err(),
            "Q alone is not a QID"
        );
    }

    #[test]
    fn wikidata_entity_id_parse_rejects_empty() {
        assert!(
            "".parse::<WikidataEntityId>().is_err(),
            "empty is not a QID"
        );
    }

    #[test]
    fn wikidata_entity_id_deserialize_rejects_empty() {
        let result: Result<WikidataEntityId, _> = serde_json::from_str("\"\"");
        assert!(result.is_err(), "empty must fail at deserialize");
    }

    #[test]
    fn wikidata_entity_id_parse_rejects_u64_overflow() {
        // u64::MAX is 18446744073709551615; append a digit to overflow.
        assert!(
            "Q184467440737095516150"
                .parse::<WikidataEntityId>()
                .is_err(),
            "value past u64::MAX must be rejected"
        );
    }

    #[test]
    fn wikidata_entity_id_new_displays_canonical_string() {
        assert_eq!(WikidataEntityId::new(12345).to_string(), "Q12345");
    }

    #[test]
    fn wikidata_entity_id_parse_preserves_numeric_part() -> TestResult {
        let id: WikidataEntityId = "Q12345".parse()?;
        assert_eq!(id.get(), 12345);
        Ok(())
    }

    #[test]
    fn wikidata_entity_id_serde_round_trips_wire_string() -> TestResult {
        let id: WikidataEntityId = serde_json::from_str("\"Q12345\"")?;
        assert_eq!(id, WikidataEntityId::new(12345));
        // Serialization re-emits the canonical string, not a bare number.
        assert_eq!(serde_json::to_string(&id)?, "\"Q12345\"");
        Ok(())
    }

    #[test]
    fn wikidata_property_id_parse_accepts_p_rejects_q() {
        assert!("P571".parse::<WikidataPropertyId>().is_ok());
        assert!(
            "Q571".parse::<WikidataPropertyId>().is_err(),
            "Q-prefixed value is not a PID"
        );
    }

    #[test]
    fn wikidata_property_id_serde_round_trips_wire_string() -> TestResult {
        let id: WikidataPropertyId = serde_json::from_str("\"P1448\"")?;
        assert_eq!(id, WikidataPropertyId::new(1448));
        assert_eq!(serde_json::to_string(&id)?, "\"P1448\"");
        Ok(())
    }

    #[test]
    fn nrhp_reference_number_deserialize_rejects_empty() {
        let result: Result<NrhpReferenceNumber, _> = serde_json::from_str("\"\"");
        assert!(result.is_err(), "empty must fail at deserialize");
    }
}
