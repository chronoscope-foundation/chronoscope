//! Citations and source vocabularies for fact assertions.
//!
//! Every assertion is paired with a citation:
//!
//! - **Factual** — a [`FactualCitation`]: an [`ExternalSource`] plus one or
//!   more verbatim [`Excerpt`]s (the list is type-level non-empty).
//! - **Judgment** — a [`JudgmentSource`], whose every variant carries its own
//!   warrant inline ([`Justification`], [`Observer`], or an embedded
//!   [`ExternalSource`]).
//! - **Meta** — a [`MetaSource`], same per-variant pattern.
//!
//! [`ExternalReference`] also lives here. It's distinct from [`ExternalSource`]
//! — it names which external system an entity has an ID in, rather than where a
//! claim came from — but sits nearby for the cross-references.
//! [`ExternalReference::from_url`] maps a URL source to one.

use std::collections::BTreeSet;

use chronoscope_macros::{DateWalk, grammar_type};
use oxilangtag::LanguageTag;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::date::UncertainDate;
use crate::external_ids::{
    GeoNamesId, GettyTgnId, NrhpReferenceNumber, OhmId, OsmElementType, OsmId, PleiadesPlaceId,
    WikidataEntityId, WikidataPropertyId,
};
use crate::grammar::geometry::ImageGeometry;
use crate::grammar::ids::{AnalyzerProcess, AnalyzerVersion, FactId, IngesterRunId, UserId};
use crate::nonempty::NonEmptyVec;

// ============================================================================
// Excerpt
// ============================================================================

/// A verbatim snippet of source material backing an assertion — the wire-level
/// proof the source said what the fact claims. [`Excerpt::new`] enforces
/// non-emptiness and a length cap at the boundary.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct Excerpt {
    inner: String,
}

/// Maximum length for an [`Excerpt`] in characters. Fits a long paragraph of
/// prose while preventing pathological dumps.
pub const EXCERPT_MAX_LEN: usize = 4096;

impl Excerpt {
    /// Construct an excerpt. Rejects empty strings and strings longer
    /// than [`EXCERPT_MAX_LEN`] characters.
    pub fn new(text: impl Into<String>) -> Result<Self, ExcerptError> {
        let s = text.into();
        if s.is_empty() {
            return Err(ExcerptError::Empty);
        }
        let char_count = s.chars().count();
        if char_count > EXCERPT_MAX_LEN {
            return Err(ExcerptError::TooLong {
                len: char_count,
                max: EXCERPT_MAX_LEN,
            });
        }
        Ok(Self { inner: s })
    }

    pub fn as_str(&self) -> &str {
        &self.inner
    }
}

impl<'de> Deserialize<'de> for Excerpt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::new(s).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for Excerpt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.inner, f)
    }
}

impl AsRef<str> for Excerpt {
    fn as_ref(&self) -> &str {
        &self.inner
    }
}

/// Errors from [`Excerpt::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExcerptError {
    /// The supplied text was empty.
    Empty,
    /// The supplied text exceeded [`EXCERPT_MAX_LEN`] characters.
    TooLong { len: usize, max: usize },
}

impl std::fmt::Display for ExcerptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "excerpt must not be empty"),
            Self::TooLong { len, max } => {
                write!(f, "excerpt too long: {len} chars (max {max})")
            }
        }
    }
}

impl std::error::Error for ExcerptError {}

// ============================================================================
// Language — canonical BCP-47 tag
// ============================================================================

/// A BCP-47 language tag in canonical form (`en-US`, not `en-us`).
///
/// Tags compare byte-for-byte, so two casings of one tag would otherwise be
/// distinct values — and distinct matcher anchor keys. The constructor
/// canonicalizes its input, so commits are built and hashed from the
/// canonical form. Wire input must already be canonical: a stored value
/// re-serializes to exactly the bytes its commit was hashed over, so
/// [`Language::deserialize`] rejects non-canonical input.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct Language {
    inner: String,
}

impl Language {
    /// Parse a BCP-47 tag and canonicalize it (case normalization per RFC
    /// 5646; `en-us` becomes `en-US`).
    pub fn new(tag: impl AsRef<str>) -> Result<Self, LanguageError> {
        let raw = tag.as_ref();
        let inner = LanguageTag::parse_and_normalize(raw)
            .map_err(|e| LanguageError::Unparseable {
                input: raw.to_owned(),
                message: e.to_string(),
            })?
            .as_str()
            .to_owned();
        Ok(Self { inner })
    }

    pub fn as_str(&self) -> &str {
        &self.inner
    }
}

impl<'de> Deserialize<'de> for Language {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let canonical = Self::new(&s).map_err(serde::de::Error::custom)?;
        if canonical.as_str() != s {
            return Err(serde::de::Error::custom(LanguageError::NotCanonical {
                input: s,
                canonical: canonical.as_str().to_owned(),
            }));
        }
        Ok(canonical)
    }
}

/// Errors from [`Language`] construction and deserialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LanguageError {
    /// The input failed BCP-47 parsing.
    Unparseable { input: String, message: String },
    /// Wire input parsed but was not in canonical form. The wire form feeds
    /// the commit hash, so the boundary rejects it.
    NotCanonical { input: String, canonical: String },
}

impl std::fmt::Display for LanguageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unparseable { input, message } => {
                write!(f, "language tag {input:?} failed BCP-47 parsing: {message}")
            }
            Self::NotCanonical { input, canonical } => {
                write!(
                    f,
                    "language tag {input:?} is not canonical (canonical form is {canonical:?})"
                )
            }
        }
    }
}

impl std::error::Error for LanguageError {}

// ============================================================================
// WikimediaCategoryName
// ============================================================================

crate::validated_string_newtype! {
    /// A Wikimedia Commons category title (the part after `Category:`).
    /// Wire shape: a transparent string; empty fails at the boundary via
    /// [`crate::grammar::ids::ValidatedStringError`].
    WikimediaCategoryName, min = 1
}

// ============================================================================
// Isbn
// ============================================================================

/// Validated ISBN-10 or ISBN-13 identifier.
///
/// Wire shape: a transparent string. The constructor strips hyphens and
/// spaces, verifies length, digit-shape, and check digit, and stores the
/// canonical digit-only form (so the round-trip is hyphen-free).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct Isbn {
    inner: String,
}

impl Isbn {
    /// Construct an `Isbn` from a source string. Hyphens and spaces
    /// are stripped before validation.
    pub fn parse(s: impl AsRef<str>) -> Result<Self, IsbnError> {
        let canonical: String = s
            .as_ref()
            .chars()
            .filter(|c| !matches!(*c, '-' | ' '))
            .collect();
        match canonical.len() {
            10 => validate_isbn10(&canonical)?,
            13 => validate_isbn13(&canonical)?,
            len => return Err(IsbnError::WrongLength { len }),
        }
        Ok(Self { inner: canonical })
    }

    /// The canonical (digit-only) ISBN as a string slice.
    pub fn as_str(&self) -> &str {
        &self.inner
    }
}

fn validate_isbn10(s: &str) -> Result<(), IsbnError> {
    let bytes = s.as_bytes();
    let mut sum: u32 = 0;
    for (i, byte) in bytes.iter().enumerate() {
        let value = if i == 9 && (*byte == b'X' || *byte == b'x') {
            10
        } else if byte.is_ascii_digit() {
            u32::from(byte - b'0')
        } else {
            return Err(IsbnError::NonDigit);
        };
        sum += (10 - u32::try_from(i).unwrap_or(0)) * value;
    }
    if !sum.is_multiple_of(11) {
        return Err(IsbnError::BadCheckDigit);
    }
    Ok(())
}

fn validate_isbn13(s: &str) -> Result<(), IsbnError> {
    let bytes = s.as_bytes();
    let mut sum: u32 = 0;
    for (i, byte) in bytes.iter().enumerate() {
        if !byte.is_ascii_digit() {
            return Err(IsbnError::NonDigit);
        }
        let value = u32::from(byte - b'0');
        sum += if i.is_multiple_of(2) {
            value
        } else {
            3 * value
        };
    }
    if !sum.is_multiple_of(10) {
        return Err(IsbnError::BadCheckDigit);
    }
    Ok(())
}

impl std::fmt::Display for Isbn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.inner, f)
    }
}

impl AsRef<str> for Isbn {
    fn as_ref(&self) -> &str {
        &self.inner
    }
}

impl<'de> Deserialize<'de> for Isbn {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::parse(s).map_err(serde::de::Error::custom)
    }
}

/// Errors from [`Isbn::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IsbnError {
    /// Length after stripping separators isn't 10 or 13.
    WrongLength {
        /// Observed length in characters after separator stripping.
        len: usize,
    },
    /// A non-digit character was present (other than a final `X` on an
    /// ISBN-10).
    NonDigit,
    /// Check digit did not match.
    BadCheckDigit,
}

impl std::fmt::Display for IsbnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongLength { len } => {
                write!(f, "ISBN must be 10 or 13 digits, got {len}")
            }
            Self::NonDigit => write!(
                f,
                "ISBN must be all digits (with optional final X on ISBN-10)"
            ),
            Self::BadCheckDigit => write!(f, "ISBN check digit does not match"),
        }
    }
}

impl std::error::Error for IsbnError {}

// ============================================================================
// Justification
// ============================================================================

/// Minimum trimmed character count for a [`Justification`].
///
/// Rejects placeholder strings while staying achievable in a short sentence.
/// Downstream review steps can require stricter substance separately.
pub const JUSTIFICATION_MIN_LEN: usize = 10;

crate::validated_string_newtype! {
    /// A researcher's written rationale, on judgment- and meta-side
    /// citations that don't quote an external source — the warrant is the
    /// researcher's own reasoning ("I judge A and B the same entity").
    /// The constructor trims whitespace and enforces
    /// [`JUSTIFICATION_MIN_LEN`] so review has substantive prose.
    Justification, min = JUSTIFICATION_MIN_LEN, trim = true
}

// ============================================================================
// FactualCitation
// ============================================================================

/// Citation backing a [`crate::grammar::assertions::FactualAssertion`]: an
/// [`ExternalSource`] plus one or more verbatim excerpts.
///
/// The excerpt list is [`NonEmptyVec`]-typed — every factual citation carries
/// at least one quoted passage even when the source is re-fetchable (URL
/// content drifts, a Wikidata revision can be reverted). The constructor
/// enforces this at the boundary.
///
/// Judgment and meta citations carry the source enum directly; their warrant
/// isn't always a quoted passage. See [`JudgmentSource`] and [`MetaSource`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema, DateWalk)]
pub struct FactualCitation {
    #[traverse]
    pub source: ExternalSource,
    pub excerpts: NonEmptyVec<Excerpt>,
}

impl FactualCitation {
    /// Construct a factual citation. Rejects empty excerpt lists.
    pub fn new(source: ExternalSource, excerpts: Vec<Excerpt>) -> Result<Self, CitationError> {
        let excerpts =
            NonEmptyVec::try_from(excerpts).map_err(|_| CitationError::ExcerptRequired)?;
        Ok(Self { source, excerpts })
    }
}

impl<'de> Deserialize<'de> for FactualCitation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            source: ExternalSource,
            excerpts: Vec<Excerpt>,
        }
        let w = Wire::deserialize(deserializer)?;
        Self::new(w.source, w.excerpts).map_err(serde::de::Error::custom)
    }
}

/// Errors from [`FactualCitation::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CitationError {
    /// No excerpts were supplied. A factual citation requires at least
    /// one verbatim passage from the source.
    ExcerptRequired,
}

impl std::fmt::Display for CitationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExcerptRequired => write!(f, "factual citation requires at least one excerpt"),
        }
    }
}

impl std::error::Error for CitationError {}

// ============================================================================
// WikidataField
// ============================================================================

/// Which part of a Wikidata item a claim was read from — the fact-store
/// analogue of "which field", so a label- or sitelink-sourced claim needn't
/// fake a property id.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WikidataField {
    /// A property statement (P571, P625, P18, P1448, …).
    Statement { property_id: WikidataPropertyId },
    /// An entity label in a given language.
    Label { language: Language },
    /// A sitelink to another wiki (enwiki, commonswiki, …).
    Sitelink { site: String },
    /// The item record as a whole — its existence/identity (backs the
    /// entity's own QID `ExternalReference` fact).
    Item,
}

// ============================================================================
// ExternalSource
// ============================================================================

/// An independently checkable source for a factual claim — where the assertion
/// came from. Distinct from [`ExternalReference`], which names where an entity
/// can be looked up.
///
/// Variants split on re-fetchability: [`ExternalSource::Url`],
/// [`ExternalSource::Wikidata`], and [`ExternalSource::Dbpedia`] can be
/// re-fetched and machine-compared against the ingest-time `value`;
/// [`ExternalSource::Book`] and [`ExternalSource::Archive`] are
/// human-verifiable only, so the citation must carry an [`Excerpt`].
///
/// Each non-structured variant carries a date for when the material was
/// authored or produced, not retrieved — named for the act (`published` for
/// `Url`/`Book`, `created` for `Archive`). It carries the
/// temporal-distance-from-events signal a historian weighs (a 1923 photo of a
/// 1923 fire vs a 1975 memoir of it). The structured variants pin the version
/// via `revision_id` / `version`, so a separate date would be redundant.
///
/// Retrieval-time / "accessed-at" isn't captured: the durable answer is to
/// archive the page (archive.is, archive.org) at ingest and cite the permalink,
/// which pins both content and retrieval moment.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExternalSource {
    /// A URL crawled at ingest time or pointed at by another source.
    Url {
        #[schemars(with = "String")]
        url: Url,
        /// When the page was published, when extractable from page metadata
        /// (a `published_time` meta tag, a dateline). Not the retrieval date,
        /// not inferred from content. `None` when the page carries no honest
        /// signal.
        #[date_role = "CitationDate"]
        published: Option<UncertainDate>,
    },
    /// A Wikidata claim pinned to a revision id, re-fetchable for
    /// verification. `value` carries the read value at ingest time so a
    /// check needs no network round-trip.
    Wikidata {
        /// The Wikidata entity the claim was read from.
        entity_id: WikidataEntityId,
        /// Which part of the item the claim came from — a statement, a
        /// label, a sitelink, or the item record itself.
        field: WikidataField,
        /// The pinned revision id; lets us re-fetch the exact claim.
        revision_id: u64,
        /// The read value as observed at ingest time.
        value: String,
    },
    /// A specific DBpedia triple pinned to a snapshot version. RDF-style
    /// addressing: both subject and predicate are URIs because the
    /// vocabulary is open.
    Dbpedia {
        /// The DBpedia resource URI (the triple's subject).
        #[schemars(with = "String")]
        resource_uri: Url,
        /// The DBpedia property URI (the triple's predicate).
        #[schemars(with = "String")]
        property_uri: Url,
        /// The DBpedia snapshot version label.
        version: String,
        /// The property value as observed at ingest time.
        value: String,
    },
    /// A physical publication — machine-unverifiable but human-checkable.
    /// The citation must carry at least one excerpt.
    Book {
        title: String,
        /// ISBN, when known.
        isbn: Option<Isbn>,
        /// Page reference (free-form, e.g. `"p. 142"` or `"pp. 12-15"`).
        page: Option<String>,
        /// Publication date of the cited edition (its copyright page), when
        /// known — the edition's date, not the underlying work's. A 1995
        /// reprint of an 1820 memoir is `published: 1995`, with 1820 in the
        /// excerpts.
        #[date_role = "CitationDate"]
        published: Option<UncertainDate>,
    },
    /// An archival or museum-collection item. The citation must carry
    /// at least one excerpt.
    Archive {
        collection: String,
        /// Catalog or accession identifier, when known.
        catalog_id: Option<String>,
        /// When the artifact was created — the photograph taken, the letter
        /// written. Not when the archive accessioned or digitized it.
        #[date_role = "CitationDate"]
        created: Option<UncertainDate>,
    },
}

// ============================================================================
// JudgmentSource
// ============================================================================

/// Source for a [`crate::grammar::assertions::JudgmentAssertion`].
///
/// Five warrant flavors: an external citation, the researcher's personal
/// knowledge, human reasoning over existing facts, a machine derivation from
/// existing facts, or a direct image observation with optional region.
///
/// Generic over the image reference type so the observed image rides through
/// the same bundle-local-index → persistent-id substitution as the depiction
/// facts it pairs with: a producer references the observed image by
/// [`ImageIdx`](crate::submit::ImageIdx), and submission rewrites it to
/// the backend id. Only the `ImageObservation` flavor carries an image; the
/// others are id-free.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(bound(
    serialize = "ImgId: ::serde::Serialize",
    deserialize = "ImgId: ::serde::de::DeserializeOwned"
))]
#[schemars(bound = "ImgId: ::schemars::JsonSchema")]
pub enum JudgmentSource<ImgId> {
    /// External evidence wrapping any [`ExternalSource`]. Preferred when
    /// available.
    External {
        #[traverse]
        source: ExternalSource,
    },
    /// Researcher's analytical judgment, with required free-text
    /// justification.
    PersonalKnowledge {
        /// The author of the judgment.
        user: UserId,
        /// Why the researcher believes the judgment holds.
        justification: Justification,
    },
    /// A judgment derived by comparing existing facts, with no external
    /// source. Trivially-inferrable combinations are caught by solver rules;
    /// `Analysis` is the fallback for chains the solvers don't model. A growing
    /// pattern of them signals a solver rule may pay for itself.
    Analysis {
        /// The facts the analysis compared.
        input_facts: Vec<FactId>,
        /// The researcher's reasoning, preserved so reviewers can dispute or
        /// generalize the conclusion.
        reasoning: Justification,
    },
    /// A judgment computed by a versioned machine process from store facts —
    /// the most machine-checkable warrant in the system: re-run the process
    /// on the basis at the snapshot and compare. The machine counterpart of
    /// [`Self::Analysis`], with structure in place of free-text reasoning.
    Derivation {
        /// The process that computed the judgment.
        process: AnalyzerProcess,
        /// The build of the process that ran.
        version: AnalyzerVersion,
        /// The evidence facts the process consulted. A `BTreeSet` gives the
        /// basis a canonical serialized order.
        basis: BTreeSet<FactId>,
        /// Exclusive upper bound: the view the judgment was computed at.
        /// Every basis id is below it.
        snapshot: FactId,
    },
    /// A direct image observation, backing feature observations, spatial
    /// relations, and depiction judgments made by looking at an image (often a
    /// region within it). The [`Observer`] field records human vs pipeline —
    /// finer than the commit-level ingester attribution.
    ImageObservation {
        /// The image the observer looked at.
        image: ImgId,
        /// Where in the image the observer focused, when known — a mask, bbox,
        /// or polyline. `None` for a whole-image observation.
        region: Option<ImageGeometry>,
        /// Who or what made the observation.
        observer: Observer,
    },
}

impl<ImgId> JudgmentSource<ImgId> {
    /// The image this source was observed against (the `ImageObservation`
    /// flavor); `None` for the others, which reference no image.
    pub fn observed_image(&self) -> Option<&ImgId> {
        match self {
            Self::ImageObservation { image, .. } => Some(image),
            Self::External { .. }
            | Self::PersonalKnowledge { .. }
            | Self::Analysis { .. }
            | Self::Derivation { .. } => None,
        }
    }

    /// Relabel the observed image through the fallible closure, producing a
    /// `JudgmentSource<I2>`. Only `ImageObservation` carries an image; the
    /// other flavors are rebuilt verbatim.
    pub fn try_map_image<I2, Err>(
        &self,
        fi: &mut impl FnMut(&ImgId) -> Result<I2, Err>,
    ) -> Result<JudgmentSource<I2>, Err> {
        Ok(match self {
            Self::External { source } => JudgmentSource::External {
                source: source.clone(),
            },
            Self::PersonalKnowledge {
                user,
                justification,
            } => JudgmentSource::PersonalKnowledge {
                user: user.clone(),
                justification: justification.clone(),
            },
            Self::Analysis {
                input_facts,
                reasoning,
            } => JudgmentSource::Analysis {
                input_facts: input_facts.clone(),
                reasoning: reasoning.clone(),
            },
            Self::Derivation {
                process,
                version,
                basis,
                snapshot,
            } => JudgmentSource::Derivation {
                process: process.clone(),
                version: version.clone(),
                basis: basis.clone(),
                snapshot: *snapshot,
            },
            Self::ImageObservation {
                image,
                region,
                observer,
            } => JudgmentSource::ImageObservation {
                image: fi(image)?,
                region: region.clone(),
                observer: observer.clone(),
            },
        })
    }
}

/// Who made an image observation.
///
/// Per-citation, distinct from the commit-level `IngestedBy`: one commit
/// can mix human-curator and pipeline observations, and trust/QA needs the
/// per-fact distinction.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Observer {
    /// A human user looked at the image and asserted the observation.
    User {
        /// The user who made the observation.
        user: UserId,
        /// Optional free-text justification — the bbox plus image often
        /// suffice; users opt in for contested claims.
        justification: Option<Justification>,
    },
    /// An automated pipeline run made the observation. The run record holds
    /// the model, version, prompt, and timestamp; the citation carries only
    /// the run id.
    Pipeline {
        /// The pipeline run that produced the observation.
        ingester_run: IngesterRunId,
    },
}

// ============================================================================
// MetaSource
// ============================================================================

/// Source for a [`crate::grammar::assertions::MetaAssertion`].
///
/// Meta-assertions are facts about facts (retractions, supersedings); the
/// citation explains *why* the meta-action was taken.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetaSource {
    /// External evidence that the underlying fact is wrong, fabricated,
    /// or otherwise warrants the meta-action.
    External {
        #[traverse]
        source: ExternalSource,
    },
    /// Moderator or researcher judgment, with required justification.
    PersonalKnowledge {
        /// The author of the meta-action.
        user: UserId,
        /// Why the meta-action is warranted.
        justification: Justification,
    },
}

// ============================================================================
// ExternalReference
// ============================================================================

/// A typed reference to an entity in an external knowledge system.
///
/// Each variant carries the system-specific identifier inline as a typed field
/// rather than a free-form string. Ingesters that already have the structured
/// id construct the matching variant directly; URL-only ingesters use
/// [`ExternalReference::from_url`] for best-effort dispatch — every
/// [`ExternalSource`] that arrives as a URL translates to one of these variants
/// (or to [`ExternalReference::UnmodeledUrl`] when the host isn't recognized).
///
/// `ExternalReference` is distinct from [`ExternalSource`]:
///
/// - [`ExternalReference`] names a destination for entity lookups —
///   description-shaped records about the entity in an external knowledge
///   graph. An entity can have multiple `ExternalReference` facts pointing at
///   different reference systems.
/// - [`ExternalSource`] names the origin of a specific claim. Every fact
///   (including an `ExternalReference` itself) has one external source backing
///   it via the citation.
///
/// A Wikidata QID fact, for example, points at a `Wikidata` reference and is
/// itself cited from an `ExternalSource::Wikidata` source.
///
/// Media-as-identifier cases (Sanborn map panels, Wikimedia Commons file pages)
/// do not live here. The honest claim "entity X is depicted on Sanborn LOC-xyz
/// panel 7" is a depiction fact attached to the ingested map/image, not a
/// flattened identifier. See [`crate::grammar::depiction`] for the depiction
/// grammar.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExternalReference {
    /// Wikidata entity (e.g. `Q243`).
    Wikidata { qid: WikidataEntityId },
    /// `OpenStreetMap` element.
    OpenStreetMap {
        /// Whether the element is a node, way, or relation.
        element_type: OsmElementType,
        id: OsmId,
    },
    /// `OpenHistoricalMap` element. OHM is a fork of OSM with the same
    /// node/way/relation data model, so [`OsmElementType`] applies.
    OpenHistoricalMap {
        /// Whether the element is a node, way, or relation.
        element_type: OsmElementType,
        id: OhmId,
    },
    /// Wikipedia article in a specific language edition.
    Wikipedia {
        /// BCP-47 language tag matching the `*.wikipedia.org` subdomain,
        /// in canonical form.
        language: Language,
        /// The article title (URL-decoded, no `_` substitutions).
        title: String,
    },
    /// `GeoNames` feature.
    GeoNames { id: GeoNamesId },
    /// Getty Thesaurus of Geographic Names entry.
    GettyTgn { id: GettyTgnId },
    /// Pleiades gazetteer entry for an ancient place.
    Pleiades { place_id: PleiadesPlaceId },
    /// United States National Register of Historic Places entry.
    Nrhp {
        reference_number: NrhpReferenceNumber,
    },
    /// Wikimedia Commons *category* page — a description-shaped
    /// collection page about the entity. File pages (`File:...`) are
    /// not modeled here; they get ingested as images and the entity-in-
    /// image link is a depiction fact.
    WikimediaCommonsCategory {
        /// The category title without the `Category:` prefix.
        category: WikimediaCategoryName,
    },
    /// Generic fallback for an arbitrary URL whose host isn't
    /// recognized by [`ExternalReference::from_url`].
    UnmodeledUrl {
        #[schemars(with = "String")]
        url: Url,
    },
}

impl ExternalReference {
    /// Dispatch a URL to the structured variant matching its host and path
    /// shape. Falls through to [`ExternalReference::UnmodeledUrl`] whenever any
    /// expected component is missing or malformed — including Commons `File:`
    /// pages (which are ingested as images) and any host that isn't on the
    /// recognized list.
    ///
    /// Each known host has its full path shape encoded in the match arm, so
    /// only a URL whose segments match that shape produces a structured
    /// variant; a URL like `https://wikidata.org/wiki/foo` (no QID-shaped
    /// segment) falls through to [`ExternalReference::UnmodeledUrl`]. Ingesters
    /// that already hold the structured id should construct the matching variant
    /// directly.
    pub fn from_url(url: &Url) -> Self {
        parse_recognized_url(url).unwrap_or_else(|| Self::UnmodeledUrl { url: url.clone() })
    }

    /// The canonical URL for this reference — the inverse of
    /// [`ExternalReference::from_url`] over every host `from_url` recognizes, so a
    /// recognized variant round-trips (`from_url(&x.to_url()) == x`). `Nrhp` has a
    /// canonical URL too, but `from_url` doesn't parse `npgallery.nps.gov`, so it
    /// is one-way. `UnmodeledUrl` returns its wrapped URL verbatim.
    pub fn to_url(&self) -> Url {
        match self {
            Self::Wikidata { qid } => {
                let qid = qid.to_string();
                build("https://www.wikidata.org", &["wiki", qid.as_str()])
            }
            Self::OpenStreetMap { element_type, id } => {
                let id = id.to_string();
                build(
                    "https://www.openstreetmap.org",
                    &[element_type.segment(), id.as_str()],
                )
            }
            Self::OpenHistoricalMap { element_type, id } => {
                let id = id.to_string();
                build(
                    "https://www.openhistoricalmap.org",
                    &[element_type.segment(), id.as_str()],
                )
            }
            Self::Wikipedia { language, title } => {
                let title = title.replace(' ', "_");
                build(
                    &format!("https://{}.wikipedia.org", language.as_str()),
                    &["wiki", title.as_str()],
                )
            }
            Self::GeoNames { id } => {
                let id = id.to_string();
                build("https://www.geonames.org", &[id.as_str()])
            }
            Self::GettyTgn { id } => {
                let id = id.to_string();
                build("https://vocab.getty.edu", &["tgn", id.as_str()])
            }
            Self::Pleiades { place_id } => {
                let place_id = place_id.to_string();
                build("https://pleiades.stoa.org", &["places", place_id.as_str()])
            }
            Self::Nrhp { reference_number } => {
                let mut url = build("https://npgallery.nps.gov", &["NRHP", "AssetDetail"]);
                url.query_pairs_mut()
                    .append_pair("assetID", reference_number.as_str());
                url
            }
            Self::WikimediaCommonsCategory { category } => {
                let category = format!("Category:{}", category.as_str().replace(' ', "_"));
                build(
                    "https://commons.wikimedia.org",
                    &["wiki", category.as_str()],
                )
            }
            Self::UnmodeledUrl { url } => url.clone(),
        }
    }
}

/// Build a URL from a hardcoded scheme+host base and its path segments. Each
/// segment is split on `/` so a slash inside a title stays a path separator
/// (Wikipedia `AC/DC` → `/wiki/AC/DC`), while the remaining reserved characters
/// (`?`, `%`, `#`) percent-encode into their path position rather than being
/// misread as a query, escape, or fragment. The base is a literal (with at most
/// a validated language subdomain), which `Url::parse` accepts, and an https URL
/// always accepts path segments, so neither call can fail for a caller here.
#[expect(
    clippy::expect_used,
    reason = "hardcoded https scheme+host base: Url::parse and path_segments_mut cannot fail for a literal base — surfaced in the work summary"
)]
fn build(base: &str, segments: &[&str]) -> Url {
    let mut url = Url::parse(base).expect("hardcoded base URL parses");
    {
        let mut path = url
            .path_segments_mut()
            .expect("an https base URL accepts path segments");
        for &segment in segments {
            path.extend(segment.split('/'));
        }
    }
    url
}

fn parse_recognized_url(url: &Url) -> Option<ExternalReference> {
    let host = url.host_str()?;
    let canonical_host = host.strip_prefix("www.").unwrap_or(host);
    let segments: Vec<&str> = url
        .path_segments()
        .map(|iter| iter.filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();

    match (canonical_host, segments.as_slice()) {
        ("pleiades.stoa.org", ["places", id, ..]) => Some(ExternalReference::Pleiades {
            place_id: PleiadesPlaceId::new(id.parse().ok()?),
        }),
        ("commons.wikimedia.org", ["wiki", page, ..]) => {
            let decoded = urlencoding::decode(page).ok()?;
            let category = decoded.strip_prefix("Category:")?;
            Some(ExternalReference::WikimediaCommonsCategory {
                category: WikimediaCategoryName::new(category.replace('_', " ")).ok()?,
            })
        }
        ("wikidata.org", ["wiki" | "entity", qid, ..]) => {
            // The parser accepts only `Q<digits>`, so property pages (P-prefix),
            // lexemes (L-prefix), and other non-QIDs yield the None arm.
            Some(ExternalReference::Wikidata {
                qid: WikidataEntityId::parse(qid).ok()?,
            })
        }
        ("openstreetmap.org", [ty, id, ..]) => Some(ExternalReference::OpenStreetMap {
            element_type: OsmElementType::from_segment(ty)?,
            id: OsmId::new(id.parse().ok()?),
        }),
        ("openhistoricalmap.org", [ty, id, ..]) => Some(ExternalReference::OpenHistoricalMap {
            element_type: OsmElementType::from_segment(ty)?,
            id: OhmId::new(id.parse().ok()?),
        }),
        ("geonames.org", [id, ..]) => Some(ExternalReference::GeoNames {
            id: GeoNamesId::new(id.parse().ok()?),
        }),
        ("vocab.getty.edu", ["tgn", id, ..]) => Some(ExternalReference::GettyTgn {
            id: GettyTgnId::new(id.parse().ok()?),
        }),
        // Wikipedia is a wildcard-subdomain match (en.wikipedia.org,
        // fr.wikipedia.org, ...). Handled after the exact-host arms so explicit
        // hosts can't be hijacked by a misparsed `.wikipedia.org` suffix.
        // Receives the www-stripped host like every other arm, so
        // `www.wikipedia.org` collapses to the bare `wikipedia.org` (no language
        // subdomain) and falls through rather than minting a bogus `www`
        // language tag.
        _ => parse_wikipedia(canonical_host, &segments),
    }
}

fn parse_wikipedia(host: &str, segments: &[&str]) -> Option<ExternalReference> {
    let language = host.strip_suffix(".wikipedia.org")?;
    if language.contains('.') {
        return None;
    }
    let language = Language::new(language).ok()?;
    let [first, title, ..] = segments else {
        return None;
    };
    if *first != "wiki" {
        return None;
    }
    let decoded = urlencoding::decode(title).ok()?;
    if decoded.is_empty() {
        return None;
    }
    Some(ExternalReference::Wikipedia {
        language,
        title: decoded.replace('_', " "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn excerpt_rejects_empty() {
        assert_eq!(Excerpt::new(""), Err(ExcerptError::Empty));
    }

    #[test]
    fn excerpt_rejects_oversized() {
        let big = "x".repeat(EXCERPT_MAX_LEN + 1);
        assert!(matches!(
            Excerpt::new(big),
            Err(ExcerptError::TooLong { .. })
        ));
    }

    #[test]
    fn excerpt_round_trips_through_serde() -> TestResult {
        let e = Excerpt::new("page 42 of the gazette")?;
        let json = serde_json::to_string(&e)?;
        let parsed: Excerpt = serde_json::from_str(&json)?;
        assert_eq!(parsed, e);
        Ok(())
    }

    #[test]
    fn isbn_accepts_valid_isbn10() -> TestResult {
        // "Design Patterns" by GoF — 0-201-63361-2
        let isbn = Isbn::parse("0-201-63361-2")?;
        assert_eq!(isbn.as_str(), "0201633612");
        Ok(())
    }

    #[test]
    fn isbn_accepts_isbn10_with_x_check_digit() -> TestResult {
        // Well-known ISBN-10 ending in X: 0-8044-2957-X ("Zen and the
        // Art of Motorcycle Maintenance").
        let isbn = Isbn::parse("0-8044-2957-X")?;
        assert_eq!(isbn.as_str(), "080442957X");
        Ok(())
    }

    #[test]
    fn isbn_accepts_valid_isbn13() -> TestResult {
        // 978-0-13-468599-1 — "The C++ Programming Language" 4th ed.
        let isbn = Isbn::parse("978-0-13-468599-1")?;
        assert_eq!(isbn.as_str(), "9780134685991");
        Ok(())
    }

    #[test]
    fn isbn_rejects_bad_check_digit() {
        // Flip last digit of a valid ISBN-13.
        assert_eq!(
            Isbn::parse("978-0-13-468599-2"),
            Err(IsbnError::BadCheckDigit)
        );
    }

    #[test]
    fn isbn_rejects_wrong_length() {
        assert!(matches!(
            Isbn::parse("12345"),
            Err(IsbnError::WrongLength { len: 5 })
        ));
    }

    #[test]
    fn isbn_rejects_non_digit() {
        // Non-digit in body (not the X-allowed final position).
        assert_eq!(Isbn::parse("0-20a-63361-2"), Err(IsbnError::NonDigit));
    }

    #[test]
    fn wikimedia_category_deserialize_rejects_empty() {
        let result: Result<WikimediaCategoryName, _> = serde_json::from_str("\"\"");
        assert!(result.is_err(), "empty must fail at deserialize");
    }

    #[test]
    fn justification_rejects_whitespace_only() {
        assert!(matches!(
            Justification::new("   "),
            Err(crate::grammar::ids::ValidatedStringError::TooShort { .. })
        ));
    }

    #[test]
    fn justification_rejects_too_short() {
        assert!(matches!(
            Justification::new("short"),
            Err(crate::grammar::ids::ValidatedStringError::TooShort { .. })
        ));
    }

    #[test]
    fn justification_round_trips_storing_trimmed_form() -> TestResult {
        // Validates the "trim once, store once" contract — input with
        // surrounding whitespace gets trimmed before storage, so the
        // round-trip yields the trimmed value.
        let j = Justification::new("   Confirmed by archive cross-reference.   ")?;
        assert_eq!(j.as_str(), "Confirmed by archive cross-reference.");
        let json = serde_json::to_string(&j)?;
        let parsed: Justification = serde_json::from_str(&json)?;
        assert_eq!(parsed, j);
        Ok(())
    }

    #[test]
    fn factual_citation_rejects_empty_excerpts() -> TestResult {
        let url = ExternalSource::Url {
            url: Url::parse("https://example.com/")?,
            published: None,
        };
        assert_eq!(
            FactualCitation::new(url, Vec::new()),
            Err(CitationError::ExcerptRequired)
        );
        Ok(())
    }

    #[test]
    fn external_reference_from_url_recognizes_pleiades() -> TestResult {
        let url = Url::parse("https://pleiades.stoa.org/places/423025")?;
        assert_eq!(
            ExternalReference::from_url(&url),
            ExternalReference::Pleiades {
                place_id: PleiadesPlaceId::new(423025),
            }
        );
        Ok(())
    }

    #[test]
    fn external_reference_from_url_recognizes_wikipedia_with_language() -> TestResult {
        let url = Url::parse("https://en.wikipedia.org/wiki/Pantheon")?;
        match ExternalReference::from_url(&url) {
            ExternalReference::Wikipedia { language, title } => {
                assert_eq!(language.as_str(), "en");
                assert_eq!(title, "Pantheon");
            }
            other => return Err(format!("expected Wikipedia variant, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn external_reference_from_url_decodes_wikipedia_title() -> TestResult {
        let url = Url::parse("https://en.wikipedia.org/wiki/Empire_State_Building")?;
        match ExternalReference::from_url(&url) {
            ExternalReference::Wikipedia { title, .. } => {
                assert_eq!(title, "Empire State Building");
            }
            other => return Err(format!("expected Wikipedia variant, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn external_reference_from_url_recognizes_wikidata_qid() -> TestResult {
        let url = Url::parse("https://www.wikidata.org/wiki/Q243")?;
        assert_eq!(
            ExternalReference::from_url(&url),
            ExternalReference::Wikidata {
                qid: WikidataEntityId::new(243),
            }
        );
        Ok(())
    }

    #[test]
    fn external_reference_from_url_recognizes_osm_way() -> TestResult {
        // way 5013364 is the Eiffel Tower in OpenStreetMap.
        let url = Url::parse("https://www.openstreetmap.org/way/5013364")?;
        assert_eq!(
            ExternalReference::from_url(&url),
            ExternalReference::OpenStreetMap {
                element_type: OsmElementType::Way,
                id: OsmId::new(5013364),
            }
        );
        Ok(())
    }

    #[test]
    fn external_reference_from_url_recognizes_commons_category() -> TestResult {
        let url = Url::parse("https://commons.wikimedia.org/wiki/Category:Pantheon_(Rome)")?;
        match ExternalReference::from_url(&url) {
            ExternalReference::WikimediaCommonsCategory { category } => {
                assert_eq!(category.as_str(), "Pantheon (Rome)");
            }
            other => return Err(format!("expected category variant, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn external_reference_from_url_routes_commons_file_to_unmodeled() -> TestResult {
        let url = Url::parse("https://commons.wikimedia.org/wiki/File:Pantheon.jpg")?;
        assert!(matches!(
            ExternalReference::from_url(&url),
            ExternalReference::UnmodeledUrl { .. }
        ));
        Ok(())
    }

    #[test]
    fn external_reference_from_url_falls_through_to_unmodeled() -> TestResult {
        let url = Url::parse("https://example.com/some/path")?;
        match ExternalReference::from_url(&url) {
            ExternalReference::UnmodeledUrl { url: u } => assert_eq!(u, url),
            other => return Err(format!("expected UnmodeledUrl, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn external_reference_from_url_rejects_bare_wikipedia_org() -> TestResult {
        let url = Url::parse("https://wikipedia.org/")?;
        assert!(matches!(
            ExternalReference::from_url(&url),
            ExternalReference::UnmodeledUrl { .. }
        ));
        Ok(())
    }

    #[test]
    fn external_reference_from_url_treats_www_wikipedia_org_as_unmodeled() -> TestResult {
        // `www` is not a language subdomain: after www-stripping the host
        // is the bare `wikipedia.org`, which has no language label, so the
        // article-shaped URL must fall through to UnmodeledUrl rather than
        // minting a `Wikipedia { language: "www" }` reference.
        let url = Url::parse("https://www.wikipedia.org/wiki/Pantheon")?;
        match ExternalReference::from_url(&url) {
            ExternalReference::UnmodeledUrl { url: u } => assert_eq!(u, url),
            other => return Err(format!("expected UnmodeledUrl, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn external_reference_from_url_keeps_language_subdomain_after_www_handling() -> TestResult {
        // Guards against an over-broad www fix swallowing the language
        // label: `en.wikipedia.org` does not start with `www.`, so the
        // language subdomain survives normalization and resolves to "en".
        let url = Url::parse("https://en.wikipedia.org/wiki/Pantheon")?;
        match ExternalReference::from_url(&url) {
            ExternalReference::Wikipedia { language, title } => {
                assert_eq!(language.as_str(), "en");
                assert_eq!(title, "Pantheon");
            }
            other => return Err(format!("expected Wikipedia variant, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn external_reference_from_url_rejects_wikidata_property_page() -> TestResult {
        // P571 is a property id, not a QID. Must fall through.
        let url = Url::parse("https://www.wikidata.org/wiki/Property:P571")?;
        assert!(matches!(
            ExternalReference::from_url(&url),
            ExternalReference::UnmodeledUrl { .. }
        ));
        Ok(())
    }

    #[test]
    fn external_reference_from_url_rejects_wikidata_garbage_qid() -> TestResult {
        // QIDs must be Q followed by digits; reject Q-prefixed garbage.
        let url = Url::parse("https://www.wikidata.org/wiki/Qfoo")?;
        assert!(matches!(
            ExternalReference::from_url(&url),
            ExternalReference::UnmodeledUrl { .. }
        ));
        Ok(())
    }

    #[test]
    fn external_reference_from_url_rejects_wikidata_without_wiki_segment() -> TestResult {
        // Bare /Q243 (no /wiki/ or /entity/ prefix) — defensive reject.
        let url = Url::parse("https://www.wikidata.org/Q243")?;
        assert!(matches!(
            ExternalReference::from_url(&url),
            ExternalReference::UnmodeledUrl { .. }
        ));
        Ok(())
    }

    #[test]
    fn external_reference_from_url_rejects_osm_unknown_element_type() -> TestResult {
        // Only node/way/relation are valid; reject other prefixes.
        let url = Url::parse("https://www.openstreetmap.org/user/foo")?;
        assert!(matches!(
            ExternalReference::from_url(&url),
            ExternalReference::UnmodeledUrl { .. }
        ));
        Ok(())
    }

    #[test]
    fn external_reference_from_url_rejects_osm_non_numeric_id() -> TestResult {
        let url = Url::parse("https://www.openstreetmap.org/way/notanumber")?;
        assert!(matches!(
            ExternalReference::from_url(&url),
            ExternalReference::UnmodeledUrl { .. }
        ));
        Ok(())
    }

    #[test]
    fn external_reference_from_url_rejects_geonames_non_numeric_id() -> TestResult {
        let url = Url::parse("https://www.geonames.org/about.html")?;
        assert!(matches!(
            ExternalReference::from_url(&url),
            ExternalReference::UnmodeledUrl { .. }
        ));
        Ok(())
    }

    #[test]
    fn external_reference_from_url_rejects_getty_without_tgn_segment() -> TestResult {
        // vocab.getty.edu hosts other vocabularies (AAT, ULAN, etc.).
        // Only /tgn/ is modeled.
        let url = Url::parse("https://vocab.getty.edu/aat/300004979")?;
        assert!(matches!(
            ExternalReference::from_url(&url),
            ExternalReference::UnmodeledUrl { .. }
        ));
        Ok(())
    }

    #[test]
    fn external_reference_from_url_rejects_pleiades_non_places_path() -> TestResult {
        // Pleiades also has /people/, /sites/, etc.
        let url = Url::parse("https://pleiades.stoa.org/help/")?;
        assert!(matches!(
            ExternalReference::from_url(&url),
            ExternalReference::UnmodeledUrl { .. }
        ));
        Ok(())
    }

    #[test]
    fn external_reference_to_url_round_trips_every_recognized_variant() -> TestResult {
        // `to_url` is the inverse of `from_url` over the hosts `from_url` parses,
        // so each recognized variant survives a to_url → from_url round trip.
        // `UnmodeledUrl` round-trips too: its host isn't recognized, so from_url
        // rebuilds it verbatim. `Nrhp` is excluded — from_url doesn't parse
        // npgallery.nps.gov, so it is one-way (covered separately below).
        let recognized = [
            ExternalReference::Wikidata {
                qid: WikidataEntityId::new(243),
            },
            ExternalReference::OpenStreetMap {
                element_type: OsmElementType::Way,
                id: OsmId::new(5013364),
            },
            ExternalReference::OpenHistoricalMap {
                element_type: OsmElementType::Node,
                id: OhmId::new(12345),
            },
            ExternalReference::Wikipedia {
                language: Language::new("en")?,
                title: "Empire State Building".to_string(),
            },
            ExternalReference::GeoNames {
                id: GeoNamesId::new(3169070),
            },
            ExternalReference::GettyTgn {
                id: GettyTgnId::new(7000874),
            },
            ExternalReference::Pleiades {
                place_id: PleiadesPlaceId::new(423025),
            },
            ExternalReference::WikimediaCommonsCategory {
                category: WikimediaCategoryName::new("Pantheon (Rome)")?,
            },
            ExternalReference::UnmodeledUrl {
                url: Url::parse("https://example.com/some/path")?,
            },
        ];
        for reference in recognized {
            let recovered = ExternalReference::from_url(&reference.to_url());
            assert_eq!(
                recovered, reference,
                "{reference:?} must survive to_url → from_url"
            );
        }
        Ok(())
    }

    #[test]
    fn external_reference_to_url_encodes_wikipedia_spaces_as_underscores() -> TestResult {
        let reference = ExternalReference::Wikipedia {
            language: Language::new("en")?,
            title: "Empire State Building".to_string(),
        };
        assert_eq!(
            reference.to_url().as_str(),
            "https://en.wikipedia.org/wiki/Empire_State_Building"
        );
        Ok(())
    }

    #[test]
    fn external_reference_to_url_percent_encodes_reserved_chars_in_titles() -> TestResult {
        // A `?` in a title must percent-encode into the path, not be misread as a
        // query delimiter (the bug the segment-wise build fixes) — and still
        // round-trip back through from_url.
        let reference = ExternalReference::Wikipedia {
            language: Language::new("en")?,
            title: "Who? (album)".to_string(),
        };
        let url = reference.to_url();
        assert_eq!(url.as_str(), "https://en.wikipedia.org/wiki/Who%3F_(album)");
        assert_eq!(url.query(), None, "the `?` must not open a query string");
        assert_eq!(
            ExternalReference::from_url(&url),
            reference,
            "a reserved-char title must still round-trip"
        );
        Ok(())
    }

    #[test]
    fn external_reference_to_url_keeps_slash_literal_in_wikipedia_title() -> TestResult {
        // A `/` in a title stays a path separator, matching Wikipedia's canonical
        // `/wiki/AC/DC` rather than percent-encoding to `%2F`. This is one-way:
        // `from_url` reads only the first title segment (`AC`), so a slashed title
        // is excluded from the round-trip test above.
        let reference = ExternalReference::Wikipedia {
            language: Language::new("en")?,
            title: "AC/DC".to_string(),
        };
        assert_eq!(
            reference.to_url().as_str(),
            "https://en.wikipedia.org/wiki/AC/DC"
        );
        Ok(())
    }

    #[test]
    fn external_reference_to_url_builds_nrhp_asset_detail_link() {
        // NRHP is one-way (from_url doesn't parse npgallery.nps.gov), so pin its
        // canonical URL directly rather than through a round trip.
        let reference = ExternalReference::Nrhp {
            reference_number: NrhpReferenceNumber::new("66000058"),
        };
        assert_eq!(
            reference.to_url().as_str(),
            "https://npgallery.nps.gov/NRHP/AssetDetail?assetID=66000058"
        );
    }
}
