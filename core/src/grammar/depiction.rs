//! Depiction cluster — entity-in-image localization judgments.
//!
//! Cluster module for the `Depiction` variant of
//! [`crate::grammar::assertions::JudgmentAssertion`]. One depiction concept ties an
//! entity to an image, carrying optional image-space localization and optional
//! [`Perspective`] view-classification.
//!
//! Classification ([`Perspective`]) and localization (the `localization`
//! field) are independent axes. Ingestion paths provide them
//! independently: a Wikidata P18 image fact arrives with neither; a VLM
//! analysis may produce a perspective tag, a localization, or both; a human
//! curator may tag the view kind without drawing a region.
//!
//! `localization` is an [`ImageGeometry`] — a mask, a bbox, or a proportional
//! polyline, all anchored to the image's own pixel / proportional frame. A trace
//! on a map sheet is authored once, in image space; its geographic rendering is
//! derived later by running the trace through the image's projection.
//!
//! Relative entity positions and an image's orientation are planned, design
//! pending: single-image 3D reconstruction yields the relative positions of the
//! entities in an image even with no absolute coordinates, and the image's pose
//! gives its orientation. The grammar will hold a future photogrammetry solver's
//! output; the solver is deferred.
//!
//! # Conflicts (surfaced at projection time)
//!
//! - **Disagreement on perspective or localization.** Two depictions on the
//!   same entity-image pair with different `Perspective` values or incompatible
//!   geometries surface as user-resolvable conflicts. Sources sometimes classify
//!   the same image differently (an interior shot misread as exterior, or two
//!   bboxes bounding the same building tightly vs loosely); the projection
//!   preserves both for human review.

use chronoscope_macros::grammar_type;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::grammar::geometry::ImageGeometry;
use crate::grammar::ids::IdScheme;

/// View or framing classification of a depiction relative to its
/// depicted entity.
///
/// A binary axis with no pre-analysis default — absence is structural, carried
/// by the depicting fact's `Option<Perspective>`. Once a downstream worker (VLM
/// or human) classifies the image, the perspective settles to `Exterior` or
/// `Interior`. The localization axis — *where* in the frame the entity sits — is
/// independent and lives on the depicting fact as a sibling field.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Perspective {
    /// Exterior elevation.
    Exterior,
    /// Interior view.
    Interior,
}

/// Depiction-cluster fact — one entity↔image depiction.
///
/// The entity appears in the image; the optional `localization` says where in
/// the image's own frame, and the optional `perspective` classifies the view.
/// The two annotation axes are independent — ingestion paths may supply either,
/// both, or neither, and a later fact refines a bare depiction.
///
/// A single grammar struct (a product): the
/// [`crate::grammar::assertions::JudgmentAssertion::Depiction`] wrapper already
/// tags it, so the inner fact needs no tag of its own.
///
/// Generic over one id scheme `R: IdScheme`, reading `R::Entity` and `R::Image`.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(bound(serialize = "R: IdScheme", deserialize = "R: IdScheme"))]
#[schemars(bound = "R: IdScheme + ::schemars::JsonSchema")]
pub struct Fact<R: IdScheme> {
    /// The depicted entity.
    pub entity: R::Entity,
    /// The image the entity appears in.
    pub image: R::Image,
    /// Where in the image the entity sits, when localized.
    pub localization: Option<ImageGeometry>,
    /// The view classification, when a source supplies one.
    pub perspective: Option<Perspective>,
}

impl<R: IdScheme> Fact<R> {
    /// Visit every id this fact mentions, dispatching to the closure for
    /// the id's kind. Entity before image, matching the field order.
    pub fn for_each_id(&self, fe: &mut impl FnMut(&R::Entity), fi: &mut impl FnMut(&R::Image)) {
        fe(&self.entity);
        fi(&self.image);
    }

    /// Relabel every id through the kind-matching fallible closure,
    /// producing a `Fact<R2>`.
    pub fn try_map_ids<R2: IdScheme, Err>(
        &self,
        fe: &mut impl FnMut(&R::Entity) -> Result<R2::Entity, Err>,
        fi: &mut impl FnMut(&R::Image) -> Result<R2::Image, Err>,
    ) -> Result<Fact<R2>, Err> {
        Ok(Fact {
            entity: fe(&self.entity)?,
            image: fi(&self.image)?,
            localization: self.localization.clone(),
            perspective: self.perspective,
        })
    }
}
