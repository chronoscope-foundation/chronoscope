//! Citations and source vocabularies for fact assertions.
//!
//! Every assertion in the fact store is paired with a citation:
//!
//! - **Factual** assertions pair with a [`FactualCitation`] — an
//!   [`ExternalSource`] plus one or more verbatim [`Excerpt`]s. The
//!   excerpt list is type-level non-empty so every factual claim
//!   carries a quoted passage.
//! - **Judgment** assertions pair with a [`JudgmentSource`] directly.
//!   Each judgment variant carries its own warrant inline — a
//!   [`Justification`] for `PersonalKnowledge` / `Analysis`, an
//!   [`Observer`] for `ImageObservation`, an embedded [`ExternalSource`]
//!   for `External` — so the source enum is the citation.
//! - **Meta** assertions pair with a [`MetaSource`] directly, same
//!   pattern: per-variant `justification` / `source` field.
//!
//! [`ExternalReference`] lives in this module too — it's distinct from
//! [`ExternalSource`] (it names *which external system* an entity has an
//! ID in, rather than *where a particular claim came from*) but the two
//! types sit close together so the doc-comments can cross-reference each
//! other. Every [`ExternalSource`] translates to one of these — the
//! [`ExternalReference::from_url`] constructor makes that mapping explicit
//! for URL-only sources.

use oxilangtag::LanguageTag;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::date::UncertainDate;
use crate::facts::geometry::ImageRegion;
use crate::facts::ids::{FactId, ImageId, IngesterRunId, UserId};
use crate::ids::{
    GeoNamesId, GettyTgnId, NrhpReferenceNumber, OhmId, OsmElementType, OsmId, PleiadesPlaceId,
    WikidataEntityId, WikidataPropertyId,
};
use crate::nonempty::NonEmptyVec;

// ============================================================================
// Excerpt
// ============================================================================

/// A verbatim snippet of source material backing an assertion.
///
/// Excerpts are the wire-level proof that a fact's source actually said
/// what the fact claims. The smart constructor [`Excerpt::new`] enforces
/// non-emptiness and a length cap at the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct Excerpt {
    inner: String,
}

/// Maximum length for an [`Excerpt`] in characters. Sized to fit a long
/// paragraph of prose while preventing pathological dumps.
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

    /// The underlying string slice.
    #[must_use]
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
// WikimediaCategoryName
// ============================================================================

crate::validated_string_newtype! {
    /// A Wikimedia Commons category title (the part after `Category:`).
    ///
    /// Wire shape: a transparent string. Empty strings fail at the wire
    /// boundary via the shared
    /// [`crate::facts::ids::ValidatedStringError`] — a category page
    /// with no name is a malformed reference.
    WikimediaCategoryName, min = 1
}

// ============================================================================
// Isbn
// ============================================================================

/// Validated ISBN-10 or ISBN-13 identifier.
///
/// Wire shape: a transparent string. The smart constructor strips
/// hyphens and spaces, verifies length (10 or 13), digit-shape, and
/// check digit, and stores the canonical digit-only form. The stored
/// form round-trips as a hyphen-free string; if a source supplied
/// hyphens they're discarded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
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
    #[must_use]
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
/// Sized to reject placeholder strings while keeping the requirement
/// achievable in a short sentence. Downstream review steps can require
/// stricter substance separately.
pub const JUSTIFICATION_MIN_LEN: usize = 10;

crate::validated_string_newtype! {
    /// A researcher's written rationale, attached to judgment- and
    /// meta-side citations that don't quote an external source.
    ///
    /// Justifications back claims like "I judge that A and B are the
    /// same entity" or "I'm retracting this fact because the source
    /// turned out to be fabricated" — situations where the warrant for
    /// the claim is the researcher's own reasoning. The smart constructor
    /// trims whitespace and enforces [`JUSTIFICATION_MIN_LEN`] so
    /// downstream review (human or LLM-assisted) has substantive prose
    /// to work with.
    Justification, min = JUSTIFICATION_MIN_LEN, trim = true
}

// ============================================================================
// FactualCitation
// ============================================================================

/// Citation paired with an [`ExternalSource`] — backs every
/// [`crate::facts::assertions::FactualAssertion`].
///
/// Carries the source plus one or more verbatim excerpts. The excerpt
/// list is [`NonEmptyVec`]-typed: every factual citation must carry at
/// least one quoted passage from the source, even when the source is
/// re-fetchable (a URL's content can change; a Wikidata revision can be
/// reverted). The constructor enforces this at the wire boundary so the
/// typed interior never has to defend against zero-excerpt citations.
///
/// Judgment and meta citations carry the source enum directly (with
/// per-variant `Justification` fields where appropriate); they don't
/// share this shape because their warrant isn't always a quoted
/// passage. See [`JudgmentSource`] and [`MetaSource`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FactualCitation {
    source: ExternalSource,
    excerpts: NonEmptyVec<Excerpt>,
}

impl FactualCitation {
    /// Construct a factual citation. Rejects empty excerpt lists.
    pub fn new(source: ExternalSource, excerpts: Vec<Excerpt>) -> Result<Self, CitationError> {
        let excerpts =
            NonEmptyVec::try_from(excerpts).map_err(|_| CitationError::ExcerptRequired)?;
        Ok(Self { source, excerpts })
    }

    /// The source the claim came from.
    #[must_use]
    pub fn source(&self) -> &ExternalSource {
        &self.source
    }

    /// The verbatim excerpts backing the claim.
    #[must_use]
    pub fn excerpts(&self) -> &NonEmptyVec<Excerpt> {
        &self.excerpts
    }
}

impl<'de> Deserialize<'de> for FactualCitation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
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
// ExternalSource
// ============================================================================

/// An independently checkable source for a factual claim.
///
/// `ExternalSource` answers the question "where did this assertion come
/// from?". It's distinct from [`ExternalReference`], which answers "which
/// external knowledge base can this entity be looked up in?" — see the
/// doc-comment on [`ExternalReference`] for the contrast.
///
/// Variants split along re-fetchability: [`ExternalSource::Url`],
/// [`ExternalSource::Wikidata`], and [`ExternalSource::Dbpedia`] point at
/// resources that can be re-fetched and machine-compared against the
/// ingest-time `value` field; [`ExternalSource::Book`] and
/// [`ExternalSource::Archive`] point at physical artifacts that can only
/// be verified by a human consulting the source, so the citation must
/// carry at least one [`Excerpt`].
///
/// Each non-structured variant carries a per-variant date field that
/// records **when the cited material was authored or produced**, not
/// when it was retrieved. The field is named for what the act actually
/// is in that context: [`ExternalSource::Url`] and [`ExternalSource::Book`]
/// use `published`; [`ExternalSource::Archive`] uses `created`
/// (publication isn't the right word for a photograph or a manuscript).
/// The field carries the temporal-distance-from-events signal a historian
/// uses to weigh evidence (a 1923 newspaper photo of a 1923 fire is
/// differently evidential from a 1975 memoir describing the same fire).
/// For the structured variants [`ExternalSource::Wikidata`] and
/// [`ExternalSource::Dbpedia`], the existing `revision_id` / `version`
/// fields already pin which version of the source is being cited, so a
/// separate publication date would be redundant noise; those variants
/// don't carry one.
///
/// Retrieval-time / "accessed-at" semantics are deliberately not
/// captured here. The durable answer for that concern is to submit the
/// page to a web-archive service (archive.is, archive.org) at ingest
/// time and cite the resulting permalink as the canonical URL — the
/// archived snapshot pins both the content and the retrieval moment,
/// and the citation that lands in the fact bag points at the archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExternalSource {
    /// A URL crawled at ingest time or pointed at by another source.
    Url {
        /// The fetched URL.
        #[schemars(with = "String")]
        url: Url,
        /// When the page was published, when extractable from page
        /// metadata (`<meta property="article:published_time">`, a
        /// visible dateline, the structured-data block on a news
        /// article). Not the retrieval date and not inferred from
        /// surrounding content. `None` when the page carries no honest
        /// publication-date signal.
        published: Option<UncertainDate>,
    },
    /// A specific Wikidata statement pinned to a revision id. Allows
    /// re-fetching the original statement to verify it still says what
    /// we recorded. The `value` field carries the property value as it
    /// appeared in Wikidata at ingest time so verification can run
    /// without a network round-trip.
    Wikidata {
        /// The Wikidata entity that bears the statement.
        entity_id: WikidataEntityId,
        /// The Wikidata property the statement asserts.
        property_id: WikidataPropertyId,
        /// The pinned revision id; lets us re-fetch the exact statement.
        revision_id: u64,
        /// The property value as observed at ingest time.
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
        /// The book's title.
        title: String,
        /// ISBN, when known.
        isbn: Option<Isbn>,
        /// Page reference (free-form, e.g. `"p. 142"` or `"pp. 12-15"`).
        page: Option<String>,
        /// Publication date of the cited edition (the date on its
        /// copyright page), when known. The edition's date, not the
        /// date of the underlying work — a 1995 reprint of an 1820
        /// memoir is `published: 1995` (the edition) with the 1820
        /// authorship date carried in the excerpts or surrounding
        /// material rather than this field.
        published: Option<UncertainDate>,
    },
    /// An archival or museum-collection item. The citation must carry
    /// at least one excerpt.
    Archive {
        /// The collection's name.
        collection: String,
        /// Catalog or accession identifier, when known.
        catalog_id: Option<String>,
        /// Date the artifact itself was created — when the photograph
        /// was taken, the letter written, the manuscript inscribed.
        /// Not the date the archive accessioned the artifact, and not
        /// the date the artifact was digitized.
        created: Option<UncertainDate>,
    },
}

// ============================================================================
// JudgmentSource
// ============================================================================

/// Source for a [`crate::facts::assertions::JudgmentAssertion`].
///
/// Judgment assertions are interpretive — same-entity claims, depiction
/// claims, feature observations, spatial relations — and accept four
/// flavors of warrant: an external citation, the researcher's personal
/// knowledge, a chain of reasoning over existing facts, or a direct
/// image observation with optional region attribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JudgmentSource {
    /// External evidence wrapping any [`ExternalSource`]. Preferred when
    /// available.
    External {
        /// The wrapped external source.
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
    /// A judgment derived from comparing existing facts in the bag,
    /// without any external source. Trivially-inferrable combinations
    /// are typically caught by solver rules
    /// (`SameEntity` closure, depiction propagation, etc.) — `Analysis`
    /// is the human-judgment fallback for chains the solvers don't
    /// model. A growing population of `Analysis` judgments with a
    /// recognizable pattern is a useful signal that a new solver rule
    /// may pay for itself.
    Analysis {
        /// The specific facts the analysis compared.
        input_facts: Vec<FactId>,
        /// The researcher's reasoning — preserves the rationale so
        /// future reviewers can dispute the conclusion or propose a
        /// generalization.
        reasoning: Justification,
    },
    /// A direct image observation. Backs feature observations, spatial
    /// relations, and depiction-equivalence judgments that were made by
    /// looking at a specific image (often at a specific region within
    /// it). The [`Observer`] field records whether the looker was a
    /// human user or an automated pipeline — material per-fact
    /// information that the commit-level
    /// [`crate::facts::assertions::FactualAssertion`] ingester
    /// attribution doesn't capture finely enough.
    ImageObservation {
        /// The image the observer looked at.
        image: ImageId,
        /// Where in the image the observer focused, when known.
        /// `None` is honest for observations that don't point at a
        /// specific region (a whole-building condition impression made
        /// without bounding the entity).
        region: Option<ImageRegion>,
        /// Who or what made the observation.
        observer: Observer,
    },
}

/// Who made an image observation.
///
/// Per-citation observer attribution distinct from the commit-level
/// `IngestedBy`: a single commit can mix human-curator and
/// pipeline-derived observations, and downstream trust/QA needs the
/// per-fact distinction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Observer {
    /// A human user looked at the image and asserted the observation.
    User {
        /// The user who made the observation.
        user: UserId,
        /// Optional free-text justification. The bbox plus image often
        /// suffice on their own; users opt in to a justification for
        /// contested or non-obvious claims.
        justification: Option<Justification>,
    },
    /// An automated pipeline run made the observation. The run record
    /// carries the model name, version, prompt template, and run
    /// timestamp; the citation only carries the run id so per-citation
    /// payload stays small.
    Pipeline {
        /// The pipeline run that produced the observation.
        ingester_run: IngesterRunId,
    },
}

// ============================================================================
// MetaSource
// ============================================================================

/// Source for a [`crate::facts::assertions::MetaAssertion`].
///
/// Meta-assertions are facts about facts (retractions, supersedings); the
/// citation explains *why* the meta-action was taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MetaSource {
    /// External evidence that the underlying fact is wrong, fabricated,
    /// or otherwise warrants the meta-action.
    External {
        /// The wrapped external source.
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
/// Each variant carries the system-specific identifier inline as a typed
/// field rather than a free-form string. Wire-level ingesters that
/// already have the structured id construct the matching variant
/// directly; URL-only ingesters use [`ExternalReference::from_url`] for
/// best-effort dispatch — every [`ExternalSource`] that arrives as a URL
/// translates to one of these variants (or to
/// [`ExternalReference::UnmodeledUrl`] when the host isn't recognized).
///
/// `ExternalReference` is **distinct from** [`ExternalSource`]:
///
/// - [`ExternalReference`] names a *destination* for entity lookups —
///   description-shaped records about the entity in an external
///   knowledge graph. An entity can have multiple `ExternalReference`
///   facts pointing at different reference systems.
/// - [`ExternalSource`] names the *origin* of a specific claim. Every
///   fact (including an `ExternalReference` itself) has exactly one
///   external source backing it via the citation.
///
/// A Wikidata QID fact, for example, points at a `Wikidata` reference
/// and is itself cited from an `ExternalSource::Wikidata` source.
///
/// Media-as-identifier cases (Sanborn map panels, Wikimedia Commons file
/// pages) deliberately do **not** live here. The honest claim "entity X
/// is depicted on Sanborn LOC-xyz panel 7" is a depiction fact attached
/// to the ingested map/image, not a flattened identifier. See
/// [`crate::facts::depiction`] for the depiction grammar.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExternalReference {
    /// Wikidata entity (e.g. `Q243`).
    Wikidata {
        /// The Wikidata QID.
        qid: WikidataEntityId,
    },
    /// `OpenStreetMap` element.
    OpenStreetMap {
        /// Whether the element is a node, way, or relation.
        element_type: OsmElementType,
        /// The OSM element id.
        id: OsmId,
    },
    /// `OpenHistoricalMap` element. OHM is a fork of OSM with the same
    /// node/way/relation data model, so [`OsmElementType`] applies.
    OpenHistoricalMap {
        /// Whether the element is a node, way, or relation.
        element_type: OsmElementType,
        /// The OHM element id.
        id: OhmId,
    },
    /// Wikipedia article in a specific language edition.
    Wikipedia {
        /// BCP-47 language tag matching the `*.wikipedia.org` subdomain.
        #[schemars(with = "String")]
        language: LanguageTag<String>,
        /// The article title (URL-decoded, no `_` substitutions).
        title: String,
    },
    /// `GeoNames` feature.
    GeoNames {
        /// The GeoNames feature id.
        id: GeoNamesId,
    },
    /// Getty Thesaurus of Geographic Names entry.
    GettyTgn {
        /// The TGN entry id.
        id: GettyTgnId,
    },
    /// Pleiades gazetteer entry for an ancient place.
    Pleiades {
        /// The Pleiades place id.
        place_id: PleiadesPlaceId,
    },
    /// United States National Register of Historic Places entry.
    Nrhp {
        /// The NRHP reference number.
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
    UnmodeledUrl(#[schemars(with = "String")] Url),
}

impl ExternalReference {
    /// Dispatch a URL to the structured variant matching its host *and*
    /// path shape. Falls through to [`ExternalReference::UnmodeledUrl`]
    /// whenever any expected component is missing or malformed —
    /// including Commons `File:` pages (which are not modeled here; they
    /// get ingested as images) and any host that isn't on the recognized
    /// list.
    ///
    /// Defensively: each known host has its full path shape encoded in
    /// the match arm, so a URL like `https://wikidata.org/wiki/foo` (no
    /// QID-shaped segment) falls through rather than being silently
    /// adopted with garbage data. Ingesters that already hold the
    /// structured id should construct the matching variant directly.
    #[must_use]
    pub fn from_url(url: &Url) -> Self {
        parse_recognized_url(url).unwrap_or_else(|| Self::UnmodeledUrl(url.clone()))
    }
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
            // Reject property pages (P-prefix), lexemes (L-prefix), and
            // anything else that isn't a canonical QID. The constructor
            // is infallible; the format check lives on the type.
            let candidate = WikidataEntityId::new(*qid);
            if !candidate.is_well_formed() {
                return None;
            }
            Some(ExternalReference::Wikidata { qid: candidate })
        }
        ("openstreetmap.org", [ty, id, ..]) => Some(ExternalReference::OpenStreetMap {
            element_type: parse_osm_element_type(ty)?,
            id: OsmId::new(id.parse().ok()?),
        }),
        ("openhistoricalmap.org", [ty, id, ..]) => Some(ExternalReference::OpenHistoricalMap {
            element_type: parse_osm_element_type(ty)?,
            id: OhmId::new(id.parse().ok()?),
        }),
        ("geonames.org", [id, ..]) => Some(ExternalReference::GeoNames {
            id: GeoNamesId::new(id.parse().ok()?),
        }),
        ("vocab.getty.edu", ["tgn", id, ..]) => Some(ExternalReference::GettyTgn {
            id: GettyTgnId::new(id.parse().ok()?),
        }),
        // Wikipedia is a wildcard-subdomain match (en.wikipedia.org,
        // fr.wikipedia.org, ...). Handled after the exact-host arms so
        // explicit hosts can't be hijacked by a misparsed
        // `.wikipedia.org` suffix.
        _ => parse_wikipedia(host, &segments),
    }
}

fn parse_osm_element_type(segment: &str) -> Option<OsmElementType> {
    match segment {
        "node" => Some(OsmElementType::Node),
        "way" => Some(OsmElementType::Way),
        "relation" => Some(OsmElementType::Relation),
        _ => None,
    }
}

fn parse_wikipedia(host: &str, segments: &[&str]) -> Option<ExternalReference> {
    let language = host.strip_suffix(".wikipedia.org")?;
    if language.contains('.') {
        return None;
    }
    let language = LanguageTag::parse(language.to_owned()).ok()?;
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
            Err(crate::facts::ids::ValidatedStringError::TooShort { .. })
        ));
    }

    #[test]
    fn justification_rejects_too_short() {
        assert!(matches!(
            Justification::new("short"),
            Err(crate::facts::ids::ValidatedStringError::TooShort { .. })
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
                qid: WikidataEntityId::new("Q243"),
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
            ExternalReference::UnmodeledUrl(_)
        ));
        Ok(())
    }

    #[test]
    fn external_reference_from_url_falls_through_to_unmodeled() -> TestResult {
        let url = Url::parse("https://example.com/some/path")?;
        match ExternalReference::from_url(&url) {
            ExternalReference::UnmodeledUrl(u) => assert_eq!(u, url),
            other => return Err(format!("expected UnmodeledUrl, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn external_reference_from_url_rejects_bare_wikipedia_org() -> TestResult {
        let url = Url::parse("https://wikipedia.org/")?;
        assert!(matches!(
            ExternalReference::from_url(&url),
            ExternalReference::UnmodeledUrl(_)
        ));
        Ok(())
    }

    #[test]
    fn external_reference_from_url_rejects_wikidata_property_page() -> TestResult {
        // P571 is a property id, not a QID. Must fall through.
        let url = Url::parse("https://www.wikidata.org/wiki/Property:P571")?;
        assert!(matches!(
            ExternalReference::from_url(&url),
            ExternalReference::UnmodeledUrl(_)
        ));
        Ok(())
    }

    #[test]
    fn external_reference_from_url_rejects_wikidata_garbage_qid() -> TestResult {
        // QIDs must be Q followed by digits; reject Q-prefixed garbage.
        let url = Url::parse("https://www.wikidata.org/wiki/Qfoo")?;
        assert!(matches!(
            ExternalReference::from_url(&url),
            ExternalReference::UnmodeledUrl(_)
        ));
        Ok(())
    }

    #[test]
    fn external_reference_from_url_rejects_wikidata_without_wiki_segment() -> TestResult {
        // Bare /Q243 (no /wiki/ or /entity/ prefix) — defensive reject.
        let url = Url::parse("https://www.wikidata.org/Q243")?;
        assert!(matches!(
            ExternalReference::from_url(&url),
            ExternalReference::UnmodeledUrl(_)
        ));
        Ok(())
    }

    #[test]
    fn external_reference_from_url_rejects_osm_unknown_element_type() -> TestResult {
        // Only node/way/relation are valid; reject other prefixes.
        let url = Url::parse("https://www.openstreetmap.org/user/foo")?;
        assert!(matches!(
            ExternalReference::from_url(&url),
            ExternalReference::UnmodeledUrl(_)
        ));
        Ok(())
    }

    #[test]
    fn external_reference_from_url_rejects_osm_non_numeric_id() -> TestResult {
        let url = Url::parse("https://www.openstreetmap.org/way/notanumber")?;
        assert!(matches!(
            ExternalReference::from_url(&url),
            ExternalReference::UnmodeledUrl(_)
        ));
        Ok(())
    }

    #[test]
    fn external_reference_from_url_rejects_geonames_non_numeric_id() -> TestResult {
        let url = Url::parse("https://www.geonames.org/about.html")?;
        assert!(matches!(
            ExternalReference::from_url(&url),
            ExternalReference::UnmodeledUrl(_)
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
            ExternalReference::UnmodeledUrl(_)
        ));
        Ok(())
    }

    #[test]
    fn external_reference_from_url_rejects_pleiades_non_places_path() -> TestResult {
        // Pleiades also has /people/, /sites/, etc.
        let url = Url::parse("https://pleiades.stoa.org/help/")?;
        assert!(matches!(
            ExternalReference::from_url(&url),
            ExternalReference::UnmodeledUrl(_)
        ));
        Ok(())
    }
}
