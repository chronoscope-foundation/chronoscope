//! Image cluster — claims about an image and its underlying artifact.
//!
//! Cluster module for the `Image` variant of
//! [`crate::facts::assertions::FactualAssertion`]. Holds claims about the
//! image's byte-level provenance ([`Fact::Source`]) and metadata about the
//! underlying photograph, painting, or scan the bytes represent
//! ([`Fact::Author`], [`Fact::CreatedDate`]).
//!
//! Role-specific attributes — capture date / location for pictures, scale /
//! georeferencing for maps — live in the [`crate::facts::picture`] and
//! [`crate::facts::map`] clusters. The split is by use:
//!
//! - **Image cluster** (this module): facts that propagate across
//!   [`crate::facts::identity::Fact::SameArtifact`] equivalence classes —
//!   author and creation date apply to the underlying artifact, so different
//!   scans of the same painting share them.
//! - **Picture cluster**: facts about the picture-as-instance — `CapturedDate`
//!   of this photograph (distinct from when the subject painting was created),
//!   `CapturedLocation` of the photographer's vantage.
//!
//! # Error states (rejected at submit time)
//!
//! Image-cluster facts have no per-kind structural rejection rules beyond the
//! wire-boundary validation each variant's payload already enforces (URL
//! parsing, non-empty `Author.name`, [`crate::date::UncertainDate`]
//! well-formedness).
//!
//! # Conflicts (surfaced at projection time)
//!
//! - **Multiple `Source` URLs.** Different ingestion runs may attribute the
//!   same `ImageId` to different source URLs (different mirrors, the same image
//!   found via different referers). All are preserved; downstream consumers
//!   pick by recency or source reputation.
//! - **Author / created-date disagreement.** Sources may disagree on who
//!   authored a photograph or when a painting was made. Dates unify via
//!   [`crate::date::UncertainDate`] interval meet; empty meet surfaces as a
//!   conflict. Author names don't unify (they're localized strings); multiple
//!   distinct claims surface as conflicts.

use chronoscope_macros::grammar_type;
use oxilangtag::LanguageTag;
use url::Url;

use crate::date::UncertainDate;

/// Image-cluster fact. Claims about the image's bytes and the
/// underlying artifact those bytes represent.
///
/// Generic over the image reference type `ImgId`.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(bound(
    serialize = "ImgId: ::serde::Serialize",
    deserialize = "ImgId: ::serde::de::DeserializeOwned"
))]
#[schemars(bound = "ImgId: ::schemars::JsonSchema")]
pub enum Fact<ImgId> {
    /// URL the image was sourced from. A re-scan or alternate-resolution
    /// copy has a different `ImageId` and its own `Source` fact.
    Source {
        image: ImgId,
        #[schemars(with = "String")]
        url: Url,
    },
    /// Author of the underlying artifact — the photographer who took
    /// the photograph, the painter who painted the painting. The name
    /// pairs with a BCP-47 language tag because authorship is recorded
    /// in different languages across sources (a Russian-language
    /// archive's attribution carries its own language tag). The submit
    /// layer rejects an empty `name`.
    ///
    /// Propagates across
    /// [`crate::facts::identity::Fact::SameArtifact`] equivalence
    /// classes: every scan of the same painting carries the same author
    /// claim.
    Author {
        image: ImgId,
        /// The author's name as the source recorded it.
        name: String,
        /// BCP-47 language tag for the name.
        #[schemars(with = "String")]
        language: LanguageTag<String>,
    },
    /// When the underlying artifact was created — when the photograph
    /// was originally taken, when the painting was painted, when the
    /// manuscript was inscribed. Distinct from
    /// [`crate::facts::picture::Fact::CapturedDate`]: a 1990 scan of an
    /// 1820 daguerreotype has `CreatedDate: 1820` here and the scan's
    /// own capture date over in `picture`.
    ///
    /// Propagates across
    /// [`crate::facts::identity::Fact::SameArtifact`] equivalence
    /// classes.
    CreatedDate {
        image: ImgId,
        /// The source-claimed interval for the creation.
        bound: UncertainDate,
    },
}

impl<ImgId> Fact<ImgId> {
    /// Visit the single image id this fact mentions.
    pub fn for_each_id(&self, fi: &mut impl FnMut(&ImgId)) {
        match self {
            Self::Source { image, .. }
            | Self::Author { image, .. }
            | Self::CreatedDate { image, .. } => fi(image),
        }
    }

    /// Relabel the single image id through the fallible closure, producing
    /// a `Fact<I2>`.
    pub fn try_map_ids<I2, Err>(
        &self,
        fi: &mut impl FnMut(&ImgId) -> Result<I2, Err>,
    ) -> Result<Fact<I2>, Err> {
        match self {
            Self::Source { image, url } => Ok(Fact::Source {
                image: fi(image)?,
                url: url.clone(),
            }),
            Self::Author {
                image,
                name,
                language,
            } => Ok(Fact::Author {
                image: fi(image)?,
                name: name.clone(),
                language: language.clone(),
            }),
            Self::CreatedDate { image, bound } => Ok(Fact::CreatedDate {
                image: fi(image)?,
                bound: bound.clone(),
            }),
        }
    }
}
