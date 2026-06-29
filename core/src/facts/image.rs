//! Image cluster — claims about an image and the artifact its bytes represent.
//!
//! Cluster module for the `Image` variant of
//! [`crate::facts::assertions::FactualAssertion`]. One image type carries every
//! image-level claim: byte-level provenance ([`Fact::Source`]), the underlying
//! artifact's [`Fact::Author`] and [`Fact::CreatedDate`], the survey/record
//! [`Fact::CapturedDate`] and viewpoint [`Fact::CapturedLocation`], and a
//! descriptive [`Fact::Medium`].
//!
//! "Picture" and "map" are not structural kinds that gate which facts an image
//! may carry — they describe a medium. [`Fact::Medium`] records that medium as a
//! cited, non-gating hint; every other fact applies to any image.
//!
//! Author and created-date describe the underlying artifact, so they propagate
//! across [`crate::facts::identity::Fact::SameArtifact`] classes — every scan of
//! one painting shares them. A survey date or viewpoint is a property of a scan
//! instance, so the projection that consumes it owns the artifact-class-vs-scan
//! reconciliation.
//!
//! Orientation, scale, and georeferencing are planned, design pending. They are
//! projection parameters of an image, read off any image with enough structure
//! to support them: a photogrammetry solver supplies the full pose for an
//! oblique or perspective image, while a top-down map exposes the 2D bearing
//! directly. The grammar will hold the solver's output; the solver is deferred.
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
//! - **Author / created-date / capture / medium disagreement.** Sources may
//!   disagree on who authored a photograph, when it was made or captured, or
//!   what medium it is. Dates unify via [`crate::date::UncertainDate`] interval
//!   meet; empty meet surfaces as a conflict. Author names, media, and locations
//!   surface disagreement as a conflict for human review.

use chronoscope_macros::grammar_type;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::date::UncertainDate;
use crate::facts::citations::Language;
use crate::location::UnresolvedLocation;

/// What kind of image this is — a render hint, never a gate.
///
/// `PictorialMap` is the explicit "genuinely both" value a catalog or classifier
/// asserts directly, one value rather than a coexisting `Picture` + `Map` pair.
/// Serialized `snake_case`, the same leaf-value-enum shape as
/// [`crate::facts::depiction::Perspective`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ImageMedium {
    /// A photograph, painting, drawing, print, or other figurative depiction.
    Picture,
    /// A cartographic representation — a Sanborn sheet, a street map, a plan.
    Map,
    /// A pictorial map: genuinely both at once, asserted as one value.
    PictorialMap,
}

/// Image-cluster fact. Claims about the image's bytes and the
/// underlying artifact those bytes represent.
///
/// Generic over the image reference type `ImgId`. A capture location carries an
/// [`UnresolvedLocation`] whose entity-scale containment is expressed via
/// [`crate::facts::attribute::Fact::Relationship`] on the relevant entities, not
/// embedded in the location reference.
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
        /// BCP-47 language tag for the name, in canonical form.
        language: Language,
    },
    /// When the underlying artifact was created — when the photograph
    /// was originally taken, when the painting was painted, when the
    /// manuscript was inscribed. Distinct from [`Fact::CapturedDate`]: a
    /// 1990 scan of an 1820 daguerreotype has `CreatedDate: 1820` here
    /// and `CapturedDate: 1990` for the scan.
    ///
    /// Propagates across
    /// [`crate::facts::identity::Fact::SameArtifact`] equivalence
    /// classes.
    CreatedDate {
        image: ImgId,
        /// The source-claimed interval for the creation.
        bound: UncertainDate,
    },
    /// When this image was captured — the survey or record date of the
    /// photograph, scan, or drawing, as an uncertain interval. A property
    /// of the scan instance, distinct from the artifact's
    /// [`Fact::CreatedDate`].
    CapturedDate {
        /// Which image the bound applies to.
        image: ImgId,
        /// The source-claimed interval for the capture.
        bound: UncertainDate,
    },
    /// Where this image was captured — the viewpoint location.
    CapturedLocation {
        /// Which image the location applies to.
        image: ImgId,
        /// The capture location.
        location: UnresolvedLocation,
    },
    /// What kind of image this is. Descriptive, never gating — it serves as a
    /// render hint and preserves a catalog's citeable "this is a map" claim
    /// without constraining any other fact.
    Medium {
        /// Which image the medium describes.
        image: ImgId,
        /// The descriptive medium.
        medium: ImageMedium,
    },
}

impl<ImgId> Fact<ImgId> {
    /// Visit the single image id this fact mentions.
    pub fn for_each_id(&self, fi: &mut impl FnMut(&ImgId)) {
        match self {
            Self::Source { image, .. }
            | Self::Author { image, .. }
            | Self::CreatedDate { image, .. }
            | Self::CapturedDate { image, .. }
            | Self::CapturedLocation { image, .. }
            | Self::Medium { image, .. } => fi(image),
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
            Self::CapturedDate { image, bound } => Ok(Fact::CapturedDate {
                image: fi(image)?,
                bound: bound.clone(),
            }),
            Self::CapturedLocation { image, location } => Ok(Fact::CapturedLocation {
                image: fi(image)?,
                location: location.clone(),
            }),
            Self::Medium { image, medium } => Ok(Fact::Medium {
                image: fi(image)?,
                medium: *medium,
            }),
        }
    }
}
