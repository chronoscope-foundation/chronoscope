//! Typed Wikidata entity model.
//!
//! Strongly-typed representation of Wikidata entities, parsed eagerly at the
//! API/dump boundary. This catches malformed data early and provides typed
//! field access throughout the ingestion pipeline.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// =============================================================================
// String newtypes
// =============================================================================

/// Define a newtype wrapper over `String` with standard traits for use as
/// typed identifiers, map keys, and serde-transparent serialization.
macro_rules! string_newtype {
    ($(#[$meta:meta])* $vis:vis $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        $vis struct $name(pub String);

        impl $name {
            /// Get the inner string value.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl std::borrow::Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }
    };
}

/// A validated Wikidata entity identifier (e.g., "Q42", "P31", "L123").
///
/// Construction requires validation: the string must start with `Q`, `P`, or `L`
/// followed by one or more ASCII digits. This is enforced by [`TryFrom<String>`],
/// making a `WikidataId` value proof that the format is valid.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct WikidataId(String);

impl WikidataId {
    /// Get the inner string value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for WikidataId {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        let valid = s.len() > 1
            && matches!(s.as_bytes()[0], b'Q' | b'P' | b'L')
            && s[1..].bytes().all(|b| b.is_ascii_digit());
        if valid {
            Ok(Self(s))
        } else {
            Err(format!("invalid Wikidata entity ID: '{s}'"))
        }
    }
}

impl From<WikidataId> for String {
    fn from(id: WikidataId) -> String {
        id.0
    }
}

impl std::fmt::Display for WikidataId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::borrow::Borrow<str> for WikidataId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for WikidataId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for WikidataId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// A MediaWiki page revision identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RevisionId(pub u64);

impl std::fmt::Display for RevisionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A MediaWiki page identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PageId(pub u64);

impl std::fmt::Display for PageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A validated Wikidata property identifier (e.g., "P31", "P571").
///
/// Construction requires validation: the string must start with `P` followed by
/// one or more ASCII digits. This is enforced by [`TryFrom<String>`], making a
/// `PropertyId` value proof that the format is valid.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PropertyId(String);

impl PropertyId {
    /// Get the inner string value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for PropertyId {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        let valid =
            s.len() > 1 && s.as_bytes()[0] == b'P' && s[1..].bytes().all(|b| b.is_ascii_digit());
        if valid {
            Ok(Self(s))
        } else {
            Err(format!("invalid Wikidata property ID: '{s}'"))
        }
    }
}

impl From<PropertyId> for String {
    fn from(id: PropertyId) -> String {
        id.0
    }
}

impl std::fmt::Display for PropertyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::borrow::Borrow<str> for PropertyId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for PropertyId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for PropertyId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

string_newtype! {
    /// A BCP 47 language code as used in Wikidata labels (e.g., "en", "fr", "zh-hans").
    pub LanguageCode
}

string_newtype! {
    /// A Wikimedia site identifier (e.g., "enwiki", "commonswiki").
    pub SiteId
}

/// A validated Wikidata time string (e.g., "+1889-03-31T00:00:00Z").
///
/// Wikidata timestamps differ from standard ISO 8601 in ways that prevent
/// using a generic parser like `chrono::NaiveDateTime`:
/// - Mandatory `+`/`-` sign prefix for CE/BCE (not part of ISO 8601)
/// - Variable-width year fields, zero-padded to arbitrary lengths
///   (e.g., `+00000001889-03-31T00:00:00Z`)
/// - Years that exceed `chrono`'s range (e.g., `+13800000000-01-01T00:00:00Z`)
///
/// Construction validates the structural pattern: sign prefix, one or more
/// year digits, then `-MM-DDTHH:MM:SSZ`. The exact string is preserved for
/// faithful round-tripping through JSONL.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct WikidataTimestamp(String);

impl WikidataTimestamp {
    /// Get the inner string value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for WikidataTimestamp {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        let bytes = s.as_bytes();

        // Must start with + or -
        if bytes.is_empty() || (bytes[0] != b'+' && bytes[0] != b'-') {
            return Err(format!(
                "invalid Wikidata timestamp '{s}': must start with '+' or '-'"
            ));
        }

        // Must end with Z
        if !s.ends_with('Z') {
            return Err(format!(
                "invalid Wikidata timestamp '{s}': must end with 'Z'"
            ));
        }

        // Must contain T
        let t_pos = s
            .find('T')
            .ok_or_else(|| format!("invalid Wikidata timestamp '{s}': must contain 'T'"))?;

        // Year part: digits between sign and first '-' after the sign
        let after_sign = &s[1..];
        let first_dash = after_sign
            .find('-')
            .ok_or_else(|| format!("invalid Wikidata timestamp '{s}': missing date separator"))?;
        let year_part = &after_sign[..first_dash];
        if year_part.is_empty() || !year_part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!(
                "invalid Wikidata timestamp '{s}': year must be one or more digits"
            ));
        }

        // Suffix after the year should match -XX-XXTXX:XX:XXZ (16 chars)
        let suffix = &after_sign[first_dash..];
        if suffix.len() != 16 {
            return Err(format!(
                "invalid Wikidata timestamp '{s}': suffix after year must be 16 characters (-MM-DDTHH:MM:SSZ)"
            ));
        }

        let sb = suffix.as_bytes();
        // Check structural characters: -XX-XXTXX:XX:XXZ
        if sb[0] != b'-'
            || sb[3] != b'-'
            || sb[6] != b'T'
            || sb[9] != b':'
            || sb[12] != b':'
            || sb[15] != b'Z'
        {
            return Err(format!(
                "invalid Wikidata timestamp '{s}': expected format +YYYY-MM-DDTHH:MM:SSZ"
            ));
        }

        // Check digit positions in suffix
        let digit_positions = [1, 2, 4, 5, 7, 8, 10, 11, 13, 14];
        for &pos in &digit_positions {
            if !sb[pos].is_ascii_digit() {
                return Err(format!(
                    "invalid Wikidata timestamp '{s}': non-digit at position {pos} in date/time suffix"
                ));
            }
        }

        // Verify T position is consistent
        if t_pos != 1 + year_part.len() + 6 {
            return Err(format!(
                "invalid Wikidata timestamp '{s}': T in wrong position"
            ));
        }

        Ok(Self(s))
    }
}

impl From<WikidataTimestamp> for String {
    fn from(ts: WikidataTimestamp) -> String {
        ts.0
    }
}

impl std::fmt::Display for WikidataTimestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::borrow::Borrow<str> for WikidataTimestamp {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for WikidataTimestamp {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for WikidataTimestamp {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

string_newtype! {
    /// A quantity amount as a decimal string (e.g., "+123", "-0.5").
    pub QuantityAmount
}

string_newtype! {
    /// A Wikimedia Commons filename (e.g., "Example.jpg").
    ///
    /// Used for filenames returned from gallery parsing and accepted by
    /// [`url_for_filename`](super::commons::url_for_filename).
    pub CommonsFilename
}

// =============================================================================
// Entity
// =============================================================================

/// Raw entity content without a revision ID.
///
/// This is what appears inside `action=query&rvprop=content` responses,
/// where the revision ID lives in the enclosing API structure rather than
/// in the entity JSON itself. Use [`with_revision`](Self::with_revision)
/// to pair it with the externally-known revision ID.
#[derive(Debug, Clone, Deserialize)]
pub struct WikidataEntityContent {
    /// Entity ID (e.g., "Q42", "P31").
    pub id: WikidataId,

    /// Entity type: "item", "property", or "lexeme".
    #[serde(rename = "type")]
    pub entity_type: WikidataEntityType,

    /// Labels keyed by language code (e.g., "en", "de").
    #[serde(default)]
    pub labels: BTreeMap<LanguageCode, Label>,

    /// Claims keyed by property ID (e.g., "P31", "P571").
    #[serde(default)]
    pub claims: BTreeMap<PropertyId, Vec<Claim>>,

    /// Sitelinks keyed by site ID (e.g., "enwiki", "commonswiki").
    #[serde(default)]
    pub sitelinks: BTreeMap<SiteId, Sitelink>,
}

impl WikidataEntityContent {
    /// Pair with an externally-known revision ID to produce a full
    /// [`WikidataEntity`].
    pub fn with_revision(self, revision_id: RevisionId) -> WikidataEntity {
        WikidataEntity {
            id: self.id,
            entity_type: self.entity_type,
            lastrevid: revision_id,
            labels: self.labels,
            claims: self.claims,
            sitelinks: self.sitelinks,
        }
    }
}

/// A Wikidata entity (item or property) with a guaranteed revision ID.
///
/// Deserialized from `wbgetentities` responses or NDJSON dumps where
/// `lastrevid` is always present. For revision-content queries where the
/// revision ID is external, deserialize as [`WikidataEntityContent`] and
/// call [`with_revision`](WikidataEntityContent::with_revision).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WikidataEntity {
    /// Entity ID (e.g., "Q42", "P31").
    pub id: WikidataId,

    /// Entity type: "item", "property", or "lexeme".
    #[serde(rename = "type")]
    pub entity_type: WikidataEntityType,

    /// Revision ID of this entity snapshot — always present.
    pub lastrevid: RevisionId,

    /// Labels keyed by language code (e.g., "en", "de").
    #[serde(default)]
    pub labels: BTreeMap<LanguageCode, Label>,

    /// Claims keyed by property ID (e.g., "P31", "P571").
    #[serde(default)]
    pub claims: BTreeMap<PropertyId, Vec<Claim>>,

    /// Sitelinks keyed by site ID (e.g., "enwiki", "commonswiki").
    #[serde(default)]
    pub sitelinks: BTreeMap<SiteId, Sitelink>,
}

/// Wikidata entity type.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WikidataEntityType {
    Item,
    Property,
    Lexeme,
}

/// A label in a specific language.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Label {
    pub language: LanguageCode,
    pub value: String,
}

/// A sitelink to a Wikimedia project page.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Sitelink {
    pub title: String,
}

// =============================================================================
// Claims and Snaks
// =============================================================================

/// A Wikidata claim (statement about an entity).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Claim {
    /// The main value assertion.
    pub mainsnak: Snak,

    /// Qualifier snaks keyed by property ID.
    #[serde(default)]
    pub qualifiers: BTreeMap<PropertyId, Vec<Snak>>,

    /// Statement rank.
    pub rank: Rank,
}

impl Claim {
    /// Create a claim with the given snak, no qualifiers, and normal rank.
    #[must_use]
    pub fn simple(snak: Snak) -> Self {
        Self {
            mainsnak: snak,
            qualifiers: BTreeMap::new(),
            rank: Rank::Normal,
        }
    }
}

/// Statement rank controlling claim selection priority.
///
/// Wikidata rank semantics: if any claims have `Preferred` rank, use only
/// those. Otherwise use `Normal`. Never use `Deprecated` — those represent
/// known-incorrect data retained for provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Rank {
    Preferred,
    Normal,
    Deprecated,
}

/// A snak (value assertion with type information).
///
/// In Wikidata JSON, snaks have a `snaktype` discriminator and an optional
/// `datavalue` field. This enum makes the relationship between type and value
/// statically enforced: `Value` always carries a `DataValue`, while `NoValue`
/// and `SomeValue` carry nothing.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "snaktype", content = "datavalue")]
#[serde(rename_all = "lowercase")]
pub enum Snak {
    /// Normal value — carries a typed data value.
    Value(DataValue),
    /// Explicitly stated to have no value.
    #[serde(rename = "novalue")]
    NoValue,
    /// Known to have a value, but the value is unknown.
    #[serde(rename = "somevalue")]
    SomeValue,
}

impl Snak {
    /// Get a string value from string-typed data values.
    pub fn string_value(&self) -> Option<&str> {
        match self {
            Self::Value(DataValue::String(s)) => Some(s),
            _ => None,
        }
    }

    /// Get an entity ID from a `WikibaseEntityId` value.
    pub fn entity_id(&self) -> Option<&WikidataId> {
        match self {
            Self::Value(DataValue::WikibaseEntityId(e)) => Some(&e.id),
            _ => None,
        }
    }

    /// Whether this snak has a special snaktype (novalue or somevalue).
    pub fn is_special(&self) -> bool {
        matches!(self, Self::NoValue | Self::SomeValue)
    }
}

// =============================================================================
// Data Values
// =============================================================================

/// A typed data value from a Wikidata snak.
///
/// Wikidata `datavalue.type` is one of a small fixed set: `"string"`,
/// `"time"`, `"globecoordinate"`, `"wikibase-entityid"`, `"monolingualtext"`,
/// `"quantity"`. Finer distinctions (commonsMedia, url, external-id) live in
/// the snak's `datatype` field, not the datavalue.
///
/// Serializes as adjacently-tagged (`{"type": "...", "value": ...}`).
/// Deserialization returns an error for unrecognized type tags — we want to
/// fail loudly on schema changes rather than silently dropping data.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", content = "value")]
pub enum DataValue {
    /// String value (also used for commonsMedia, url, and external-id).
    #[serde(rename = "string")]
    String(String),

    /// Point in time with precision.
    #[serde(rename = "time")]
    Time(TimeValue),

    /// Geographic coordinates.
    #[serde(rename = "globecoordinate")]
    GlobeCoordinate(CoordinateValue),

    /// Reference to another Wikidata entity.
    #[serde(rename = "wikibase-entityid")]
    WikibaseEntityId(EntityRefValue),

    /// Text in a specific language.
    #[serde(rename = "monolingualtext")]
    MonolingualText(MonolingualTextValue),

    /// Numeric quantity with optional unit and bounds.
    #[serde(rename = "quantity")]
    Quantity(QuantityValue),
}

// =============================================================================
// Value types
// =============================================================================

/// Wikidata time precision levels.
///
/// See <https://www.wikidata.org/wiki/Help:Dates#Precision>
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub enum WikidataPrecision {
    BillionYears,
    HundredMillionYears,
    TenMillionYears,
    MillionYears,
    HundredThousandYears,
    TenThousandYears,
    Millennium,
    Century,
    Decade,
    Year,
    Month,
    Day,
}

impl TryFrom<u64> for WikidataPrecision {
    type Error = String;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::BillionYears),
            1 => Ok(Self::HundredMillionYears),
            2 => Ok(Self::TenMillionYears),
            3 => Ok(Self::MillionYears),
            4 => Ok(Self::HundredThousandYears),
            5 => Ok(Self::TenThousandYears),
            6 => Ok(Self::Millennium),
            7 => Ok(Self::Century),
            8 => Ok(Self::Decade),
            9 => Ok(Self::Year),
            10 => Ok(Self::Month),
            11 => Ok(Self::Day),
            n => Err(format!("unknown Wikidata precision value: {n}")),
        }
    }
}

impl From<WikidataPrecision> for u64 {
    fn from(p: WikidataPrecision) -> u64 {
        match p {
            WikidataPrecision::BillionYears => 0,
            WikidataPrecision::HundredMillionYears => 1,
            WikidataPrecision::TenMillionYears => 2,
            WikidataPrecision::MillionYears => 3,
            WikidataPrecision::HundredThousandYears => 4,
            WikidataPrecision::TenThousandYears => 5,
            WikidataPrecision::Millennium => 6,
            WikidataPrecision::Century => 7,
            WikidataPrecision::Decade => 8,
            WikidataPrecision::Year => 9,
            WikidataPrecision::Month => 10,
            WikidataPrecision::Day => 11,
        }
    }
}

/// A point in time with precision.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TimeValue {
    /// ISO 8601-like time string (e.g., "+1920-01-01T00:00:00Z").
    pub time: WikidataTimestamp,
    /// Precision level, parsed eagerly at the API boundary.
    pub precision: WikidataPrecision,
}

/// Geographic coordinates on a globe.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CoordinateValue {
    pub latitude: f64,
    pub longitude: f64,
    /// Coordinate precision in degrees.
    pub precision: Option<f64>,
}

/// Reference to a Wikidata entity (as it appears in `wikibase-entityid` values).
///
/// The JSON contains `entity-type`, `numeric-id`, and `id` fields. We only
/// keep `id` (e.g., "Q42") since it's the canonical identifier.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EntityRefValue {
    /// Entity ID (e.g., "Q42").
    pub id: WikidataId,
}

/// Text in a specific language.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MonolingualTextValue {
    pub text: String,
    pub language: LanguageCode,
}

/// Numeric quantity with unit.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QuantityValue {
    /// Amount as a decimal string (e.g., "+123", "-0.5").
    pub amount: QuantityAmount,
    /// Unit of measurement.
    pub unit: QuantityUnit,
}

/// Unit of measurement for a quantity value.
///
/// In Wikidata JSON, units are represented as a string: `"1"` for
/// dimensionless quantities, or a Wikidata entity URI
/// (e.g., `"http://www.wikidata.org/entity/Q11573"` for metres).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum QuantityUnit {
    /// Dimensionless quantity (Wikidata uses `"1"`).
    Dimensionless,
    /// Unit is a Wikidata entity (e.g., Q11573 for metres).
    WikidataEntity(WikidataId),
}

impl TryFrom<String> for QuantityUnit {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        if s == "1" {
            Ok(Self::Dimensionless)
        } else if let Some(qid) = s
            .strip_prefix("http://www.wikidata.org/entity/")
            .or_else(|| s.strip_prefix("https://www.wikidata.org/entity/"))
        {
            let id = WikidataId::try_from(qid.to_string())
                .map_err(|e| format!("invalid entity ID in quantity unit: {e}"))?;
            Ok(Self::WikidataEntity(id))
        } else {
            Err(format!("unknown quantity unit format: '{s}'"))
        }
    }
}

impl From<QuantityUnit> for String {
    fn from(u: QuantityUnit) -> String {
        match u {
            QuantityUnit::Dimensionless => "1".to_string(),
            QuantityUnit::WikidataEntity(id) => {
                format!("http://www.wikidata.org/entity/{id}")
            }
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn test_deserialize_entity() -> TestResult {
        let json = r#"{
            "type": "item",
            "id": "Q243",
            "lastrevid": 123456,
            "labels": {
                "en": { "language": "en", "value": "Eiffel Tower" },
                "fr": { "language": "fr", "value": "Tour Eiffel" }
            },
            "claims": {
                "P31": [{
                    "mainsnak": {
                        "snaktype": "value",
                        "datavalue": {
                            "type": "wikibase-entityid",
                            "value": { "entity-type": "item", "numeric-id": 12345, "id": "Q12345" }
                        }
                    },
                    "rank": "normal"
                }]
            },
            "sitelinks": {
                "enwiki": { "title": "Eiffel Tower" }
            }
        }"#;

        let entity: WikidataEntity = serde_json::from_str(json)?;
        assert_eq!(entity.id, "Q243");
        assert_eq!(entity.entity_type, WikidataEntityType::Item);
        assert_eq!(entity.lastrevid, RevisionId(123456));
        assert_eq!(entity.labels.len(), 2);
        assert_eq!(entity.labels["en"].value, "Eiffel Tower");
        assert_eq!(entity.claims["P31"].len(), 1);
        assert!(
            entity.claims["P31"][0]
                .mainsnak
                .entity_id()
                .is_some_and(|id| id == "Q12345")
        );
        assert_eq!(entity.claims["P31"][0].rank, Rank::Normal);
        assert_eq!(entity.sitelinks["enwiki"].title, "Eiffel Tower");
        Ok(())
    }

    #[test]
    fn test_deserialize_time_value() -> TestResult {
        let json = r#"{
            "mainsnak": {
                "snaktype": "value",
                "datavalue": {
                    "type": "time",
                    "value": {
                        "time": "+1889-03-31T00:00:00Z",
                        "timezone": 0,
                        "before": 0,
                        "after": 0,
                        "precision": 11,
                        "calendarmodel": "http://www.wikidata.org/entity/Q1985727"
                    }
                }
            },
            "rank": "preferred"
        }"#;

        let claim: Claim = serde_json::from_str(json)?;
        let Snak::Value(DataValue::Time(tv)) = &claim.mainsnak else {
            return Err("expected time value".into());
        };
        assert_eq!(tv.time, "+1889-03-31T00:00:00Z");
        assert_eq!(tv.precision, WikidataPrecision::Day);
        assert_eq!(claim.rank, Rank::Preferred);
        Ok(())
    }

    #[test]
    fn test_deserialize_coordinate_value() -> TestResult {
        let json = r#"{
            "mainsnak": {
                "snaktype": "value",
                "datavalue": {
                    "type": "globecoordinate",
                    "value": {
                        "latitude": 48.8584,
                        "longitude": 2.2945,
                        "precision": 0.0001,
                        "globe": "http://www.wikidata.org/entity/Q2"
                    }
                }
            },
            "rank": "normal"
        }"#;

        let claim: Claim = serde_json::from_str(json)?;
        let Snak::Value(DataValue::GlobeCoordinate(cv)) = &claim.mainsnak else {
            return Err("expected coordinate value".into());
        };
        assert!((cv.latitude - 48.8584).abs() < 0.0001);
        assert!((cv.longitude - 2.2945).abs() < 0.0001);
        Ok(())
    }

    #[test]
    fn test_deserialize_string_value() -> TestResult {
        let json = r#"{
            "mainsnak": {
                "snaktype": "value",
                "datavalue": { "type": "string", "value": "hello world" }
            },
            "rank": "normal"
        }"#;

        let claim: Claim = serde_json::from_str(json)?;
        assert_eq!(claim.mainsnak.string_value(), Some("hello world"));
        Ok(())
    }

    #[test]
    fn test_deserialize_monolingual_text() -> TestResult {
        let json = r#"{
            "mainsnak": {
                "snaktype": "value",
                "datavalue": {
                    "type": "monolingualtext",
                    "value": { "text": "Tour Eiffel", "language": "fr" }
                }
            },
            "rank": "normal"
        }"#;

        let claim: Claim = serde_json::from_str(json)?;
        let Snak::Value(DataValue::MonolingualText(mt)) = &claim.mainsnak else {
            return Err("expected monolingual text".into());
        };
        assert_eq!(mt.text, "Tour Eiffel");
        assert_eq!(mt.language, "fr");
        Ok(())
    }

    #[test]
    fn test_deserialize_quantity_with_unit() -> TestResult {
        let json = r#"{
            "mainsnak": {
                "snaktype": "value",
                "datavalue": {
                    "type": "quantity",
                    "value": {
                        "amount": "+324",
                        "unit": "http://www.wikidata.org/entity/Q11573"
                    }
                }
            },
            "rank": "normal"
        }"#;

        let claim: Claim = serde_json::from_str(json)?;
        let Snak::Value(DataValue::Quantity(qv)) = &claim.mainsnak else {
            return Err("expected quantity value".into());
        };
        assert_eq!(qv.amount, "+324");
        assert_eq!(
            qv.unit,
            QuantityUnit::WikidataEntity(WikidataId::try_from("Q11573".to_string())?)
        );
        Ok(())
    }

    #[test]
    fn test_deserialize_quantity_dimensionless() -> TestResult {
        let json = r#"{
            "mainsnak": {
                "snaktype": "value",
                "datavalue": {
                    "type": "quantity",
                    "value": { "amount": "+1", "unit": "1" }
                }
            },
            "rank": "normal"
        }"#;

        let claim: Claim = serde_json::from_str(json)?;
        let Snak::Value(DataValue::Quantity(qv)) = &claim.mainsnak else {
            return Err("expected quantity value".into());
        };
        assert_eq!(qv.amount, "+1");
        assert_eq!(qv.unit, QuantityUnit::Dimensionless);
        Ok(())
    }

    #[test]
    fn test_deserialize_novalue_snak() -> TestResult {
        let json = r#"{ "mainsnak": { "snaktype": "novalue" }, "rank": "normal" }"#;
        let claim: Claim = serde_json::from_str(json)?;
        assert!(claim.mainsnak.is_special());
        assert!(matches!(claim.mainsnak, Snak::NoValue));
        Ok(())
    }

    #[test]
    fn test_deserialize_somevalue_snak() -> TestResult {
        let json = r#"{ "mainsnak": { "snaktype": "somevalue" }, "rank": "normal" }"#;
        let claim: Claim = serde_json::from_str(json)?;
        assert!(claim.mainsnak.is_special());
        Ok(())
    }

    #[test]
    fn test_unknown_datavalue_type_is_error() {
        let json = r#"{
            "mainsnak": {
                "snaktype": "value",
                "datavalue": { "type": "future-type", "value": "something" }
            },
            "rank": "normal"
        }"#;

        let result = serde_json::from_str::<Claim>(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_deserialize_qualifiers() -> TestResult {
        let json = r#"{
            "mainsnak": {
                "snaktype": "value",
                "datavalue": {
                    "type": "wikibase-entityid",
                    "value": { "entity-type": "item", "numeric-id": 385378, "id": "Q385378" }
                }
            },
            "qualifiers": {
                "P580": [{
                    "snaktype": "value",
                    "datavalue": {
                        "type": "time",
                        "value": { "time": "+1887-01-28T00:00:00Z", "precision": 11 }
                    }
                }],
                "P582": [{
                    "snaktype": "value",
                    "datavalue": {
                        "type": "time",
                        "value": { "time": "+1889-03-31T00:00:00Z", "precision": 11 }
                    }
                }]
            },
            "rank": "normal"
        }"#;

        let claim: Claim = serde_json::from_str(json)?;
        assert!(claim.mainsnak.entity_id().is_some_and(|id| id == "Q385378"));
        assert_eq!(claim.qualifiers["P580"].len(), 1);
        assert_eq!(claim.qualifiers["P582"].len(), 1);

        let Snak::Value(DataValue::Time(p580_time)) = &claim.qualifiers["P580"][0] else {
            return Err("expected time".into());
        };
        assert_eq!(p580_time.time, "+1887-01-28T00:00:00Z");
        Ok(())
    }

    #[test]
    fn test_entity_minimal_fields() -> TestResult {
        let json = r#"{ "type": "item", "id": "Q1", "lastrevid": 42 }"#;
        let entity: WikidataEntity = serde_json::from_str(json)?;
        assert_eq!(entity.id, "Q1");
        assert!(entity.labels.is_empty());
        assert!(entity.claims.is_empty());
        assert!(entity.sitelinks.is_empty());
        assert_eq!(entity.lastrevid, RevisionId(42));
        Ok(())
    }

    #[test]
    fn test_entity_missing_revision_id_is_error() {
        let json = r#"{ "type": "item", "id": "Q1" }"#;
        assert!(serde_json::from_str::<WikidataEntity>(json).is_err());
    }

    #[test]
    fn test_entity_content_with_revision() -> TestResult {
        let json = r#"{ "type": "item", "id": "Q1" }"#;
        let content: WikidataEntityContent = serde_json::from_str(json)?;
        assert_eq!(content.id, "Q1");
        let entity = content.with_revision(RevisionId(99));
        assert_eq!(entity.lastrevid, RevisionId(99));
        Ok(())
    }

    #[test]
    fn test_rank_missing_is_error() {
        let json = r#"{ "mainsnak": { "snaktype": "novalue" } }"#;
        assert!(serde_json::from_str::<Claim>(json).is_err());
    }

    #[test]
    fn test_deprecated_rank() -> TestResult {
        let json = r#"{
            "mainsnak": { "snaktype": "value", "datavalue": { "type": "string", "value": "x" } },
            "rank": "deprecated"
        }"#;
        let claim: Claim = serde_json::from_str(json)?;
        assert_eq!(claim.rank, Rank::Deprecated);
        Ok(())
    }

    #[test]
    fn test_precision_round_trip() -> TestResult {
        let json = r#"{
            "mainsnak": {
                "snaktype": "value",
                "datavalue": {
                    "type": "time",
                    "value": { "time": "+1920-01-01T00:00:00Z", "precision": 9 }
                }
            },
            "rank": "normal"
        }"#;
        let claim: Claim = serde_json::from_str(json)?;
        let Snak::Value(DataValue::Time(tv)) = &claim.mainsnak else {
            return Err("expected time".into());
        };
        assert_eq!(tv.precision, WikidataPrecision::Year);

        // Round-trip through JSON
        let serialized = serde_json::to_value(tv)?;
        assert_eq!(serialized["precision"], 9);
        Ok(())
    }

    #[test]
    fn test_unknown_precision_is_error() {
        let json = r#"{
            "mainsnak": {
                "snaktype": "value",
                "datavalue": {
                    "type": "time",
                    "value": { "time": "+1920-01-01T00:00:00Z", "precision": 14 }
                }
            },
            "rank": "normal"
        }"#;
        assert!(serde_json::from_str::<Claim>(json).is_err());
    }

    #[test]
    fn test_unknown_entity_type_is_error() {
        let json = r#"{ "type": "future-entity-type", "id": "X1" }"#;
        assert!(serde_json::from_str::<WikidataEntity>(json).is_err());
    }

    #[test]
    fn test_unknown_quantity_unit_is_error() {
        let json = r#"{
            "mainsnak": {
                "snaktype": "value",
                "datavalue": {
                    "type": "quantity",
                    "value": { "amount": "+1", "unit": "https://example.com/not-wikidata" }
                }
            },
            "rank": "normal"
        }"#;
        assert!(serde_json::from_str::<Claim>(json).is_err());
    }

    #[test]
    fn test_quantity_unit_round_trip() -> TestResult {
        let unit = QuantityUnit::WikidataEntity(WikidataId::try_from("Q11573".to_string())?);
        let s: String = unit.clone().into();
        assert_eq!(s, "http://www.wikidata.org/entity/Q11573");
        let round_tripped = QuantityUnit::try_from(s)?;
        assert_eq!(round_tripped, unit);

        let dimensionless = QuantityUnit::Dimensionless;
        let s: String = dimensionless.clone().into();
        assert_eq!(s, "1");
        let round_tripped = QuantityUnit::try_from(s)?;
        assert_eq!(round_tripped, QuantityUnit::Dimensionless);
        Ok(())
    }
}
