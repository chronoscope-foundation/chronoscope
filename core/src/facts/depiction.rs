//! Depiction cluster — entity-in-image localization judgments.
//!
//! Cluster module for the `Depiction` variant of
//! [`crate::facts::assertions::JudgmentAssertion`]. Holds the
//! in-picture and on-map depiction shapes, plus the [`Perspective`]
//! view-classification enum that the in-picture variant carries.
//!
//! Classification ([`Perspective`]) and localization (the depicting
//! fact's `region` / `geometry` field) are independent axes. Ingestion
//! paths provide them independently: a Wikidata P18 image fact arrives
//! with `Unknown` perspective and no region; a VLM classifier may produce
//! a perspective tag, a region, or both; a human curator may tag the
//! view kind without drawing a bbox.
//!
//! `InPicture` and `OnMap` both reference an [`ImgId`] (image
//! identifier) and differ in what spatial annotation they carry —
//! pixel-precise [`ImageRegion`] for pictures, georeferenced
//! [`SpatialGeometry`] (polyline-capable) for maps.
//!
//! # Error states (rejected at submit time)
//!
//! Error states are combinations the grammar permits structurally but
//! the fact-store layer rejects at submit time. They model bugs in
//! caller code, not outside-world uncertainty.
//!
//! - **Variant / role mismatch.** [`Fact::InPicture`] must reference an
//!   image carrying a [`crate::facts::picture::Fact::IsPicture`]
//!   role-claim (or no role-claim yet); [`Fact::OnMap`] must reference
//!   an image carrying a [`crate::facts::map::Fact::IsMap`] role-claim
//!   (or none yet). A depiction whose variant contradicts an already-
//!   asserted role on the image is a malformed claim, not a
//!   disagreement to resolve.
//!
//! # Conflicts (surfaced at projection time)
//!
//! - **Disagreement on perspective or region.** Two `InPicture` facts on
//!   the same entity-image pair with different `Perspective` values or
//!   incompatible region geometries are surfaced as user-resolvable
//!   conflicts. Sources sometimes classify the same image differently
//!   (an interior shot misread as exterior, or two bboxes that bound
//!   the same building tightly vs loosely); the projection preserves
//!   both for human review.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::facts::geometry::{ImageRegion, SpatialGeometry};

/// View or framing classification of a picture relative to its
/// depicted entity.
///
/// `Unknown` is the pre-analysis default for sources that don't
/// differentiate views (e.g. a generic Wikidata P18 image fact). Once
/// a downstream worker (VLM or human) classifies the image, the
/// perspective tightens to `Exterior` or `Interior`. The localization
/// axis — *where* in the frame the entity sits — is independent and
/// lives on the depicting assertion as a sibling `region` field.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Perspective {
    /// View not yet classified, or unspecified by the source.
    Unknown,
    /// Exterior elevation.
    Exterior,
    /// Interior view.
    Interior,
}

/// Depiction-cluster fact.
///
/// Generic over the entity and image (image) reference types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(bound(
    serialize = "EntId: Serialize, ImgId: Serialize",
    deserialize = "EntId: serde::de::DeserializeOwned, ImgId: serde::de::DeserializeOwned"
))]
#[schemars(bound = "EntId: JsonSchema, ImgId: JsonSchema")]
pub enum Fact<EntId, ImgId> {
    /// The entity appears in this picture (pictorial-role image). The
    /// perspective classifies the view (interior / exterior / unknown);
    /// the optional region localizes where in the frame the entity sits.
    /// The two axes are independent — ingestion paths may provide
    /// either, both, or neither.
    InPicture {
        /// Which entity is depicted.
        entity: EntId,
        /// Which image carries the depiction. Should carry a
        /// `picture::Fact::IsPicture` role-claim.
        image: ImgId,
        /// View classification (Unknown / Exterior / Interior).
        perspective: Perspective,
        /// Where in the frame the entity appears, when known.
        region: Option<ImageRegion>,
    },
    /// The entity appears on this map (map-role image), optionally with
    /// a geometry (region mask or polyline) showing where on the sheet
    /// it lies.
    OnMap {
        /// Which entity is depicted.
        entity: EntId,
        /// Which image carries the depiction. Should carry a
        /// `map::Fact::IsMap` role-claim.
        image: ImgId,
        /// The on-sheet geometry, when traced.
        geometry: Option<SpatialGeometry>,
    },
}
