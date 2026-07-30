//! Image cluster — claims about an image and the artifact its bytes represent.
//!
//! Cluster module for the `Image` variant of
//! [`crate::grammar::assertions::FactualAssertion`]. One image type carries every
//! image-level claim: byte-level provenance ([`Fact::Source`]), the underlying
//! artifact's [`Fact::Author`], [`Fact::CreatedDate`], and [`Fact::SubjectDate`],
//! the survey/record [`Fact::CapturedDate`] and viewpoint
//! [`Fact::CapturedLocation`], and a descriptive [`Fact::Medium`].
//!
//! "Picture" and "map" are not structural kinds that gate which facts an image
//! may carry — they describe a medium. [`Fact::Medium`] records that medium as a
//! cited, non-gating hint; every other fact applies to any image.
//!
//! Author, created-date, and subject-date describe the underlying artifact, so
//! they propagate across [`crate::grammar::identity::Fact::SameArtifact`] classes —
//! every scan of one painting shares them. A survey date or viewpoint is a
//! property of a scan instance, so the projection that consumes it owns the
//! artifact-class-vs-scan reconciliation.
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
//! parsing, [`Text`] for the free-text payloads,
//! [`crate::date::UncertainDate`] well-formedness).
//!
//! # Conflicts (surfaced at projection time)
//!
//! - **Multiple `Source` URLs.** Different ingestion runs may attribute the
//!   same image id to different source URLs (different mirrors, the same image
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
use crate::grammar::citations::Language;
use crate::grammar::ids::IdScheme;
use crate::grammar::text::Text;
use crate::location::UnresolvedLocation;

/// What kind of image this is — a render hint, never a gate.
///
/// `PictorialMap` is the explicit "genuinely both" value a catalog or classifier
/// asserts directly, one value rather than a coexisting `Picture` + `Map` pair.
/// Serialized `snake_case`, the same leaf-value-enum shape as
/// [`crate::grammar::depiction::Perspective`].
///
/// `EnumIter` is what lets a generator or a test matrix cover the medium space
/// from the definition instead of a list beside it.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    strum::EnumIter,
)]
#[serde(rename_all = "snake_case")]
pub enum ImageMedium {
    /// A photograph, painting, drawing, print, or other figurative depiction.
    Picture,
    /// A cartographic representation — a Sanborn sheet, a street map.
    Map,
    /// An orthographic plan view of a structure: a floor plan, a ground plan, a
    /// site plan. Distinct from [`Self::Map`] because it depicts one building's
    /// own layout rather than a piece of terrain, and reads wrong when captioned
    /// as a map.
    Plan,
    /// A pictorial map: genuinely both at once, asserted as one value.
    PictorialMap,
}

/// Image-cluster fact. Claims about the image's bytes and the
/// underlying artifact those bytes represent.
///
/// Generic over one id scheme `R: IdScheme`, reading `R::Image`. A capture
/// location carries an [`UnresolvedLocation`] whose entity-scale containment is
/// expressed via [`crate::grammar::attribute::Fact::Relationship`] on the
/// relevant entities, not embedded in the location reference.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Fact<R: IdScheme> {
    /// URL the image was sourced from. A re-scan or alternate-resolution
    /// copy has a different image id and its own `Source` fact.
    Source {
        image: R::Image,
        #[schemars(with = "String")]
        url: Url,
    },
    /// Author of the underlying artifact — the photographer who took
    /// the photograph, the painter who painted the painting. The name
    /// pairs with a BCP-47 language tag because authorship is recorded
    /// in different languages across sources (a Russian-language
    /// archive's attribution carries its own language tag).
    ///
    /// Propagates across
    /// [`crate::grammar::identity::Fact::SameArtifact`] equivalence
    /// classes: every scan of the same painting carries the same author
    /// claim.
    Author {
        image: R::Image,
        /// The author's name as the source recorded it.
        name: Text,
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
    /// [`crate::grammar::identity::Fact::SameArtifact`] equivalence
    /// classes.
    CreatedDate {
        image: R::Image,
        /// The source-claimed interval for the creation.
        #[date_role = "ImageCreated"]
        bound: UncertainDate,
    },
    /// The moment this image portrays its subject as existing — the
    /// subject-date. An 1890s bird's-eye view drawn of a city *as it stood in
    /// 1850* portrays an 1850 subject; that is this date, distinct from the
    /// 1890s [`Fact::CreatedDate`] when the drawing was made and from any later
    /// [`Fact::CapturedDate`] when a copy was scanned.
    ///
    /// A property of the artifact's content, shared by every scan, so it
    /// propagates across
    /// [`crate::grammar::identity::Fact::SameArtifact`] equivalence classes —
    /// artifact-level, the same side as [`Fact::CreatedDate`], where
    /// [`Fact::CapturedDate`] and [`Fact::CapturedLocation`] are scan-level.
    SubjectDate {
        image: R::Image,
        /// The source-claimed interval the subject is portrayed as existing in.
        #[date_role = "ImageSubject"]
        bound: UncertainDate,
    },
    /// When this image was captured — the survey or record date of the
    /// photograph, scan, or drawing, as an uncertain interval. A property
    /// of the scan instance, distinct from the artifact's
    /// [`Fact::CreatedDate`].
    CapturedDate {
        /// Which image the bound applies to.
        image: R::Image,
        /// The source-claimed interval for the capture.
        #[date_role = "ImageCaptured"]
        bound: UncertainDate,
    },
    /// Where this image was captured — the viewpoint location.
    CapturedLocation {
        /// Which image the location applies to.
        image: R::Image,
        /// The capture location.
        #[location_role = "ImageCaptured"]
        location: UnresolvedLocation,
    },
    /// What kind of image this is. Descriptive, never gating — it serves as a
    /// render hint and preserves a catalog's citeable "this is a map" claim
    /// without constraining any other fact.
    Medium {
        /// Which image the medium describes.
        image: R::Image,
        /// The descriptive medium.
        medium: ImageMedium,
    },
}
