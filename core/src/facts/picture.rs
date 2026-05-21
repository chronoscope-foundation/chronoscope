//! Picture cluster — pictorial role-claim plus picture-specific
//! attributes.
//!
//! Cluster module for the `Picture` variant of
//! [`crate::facts::assertions::FactualAssertion`]. A "picture" is a
//! image being treated as a photograph, painting, drawing, or other
//! figurative depiction — distinct from a map (cartographic
//! representation). The classification is a *claim* asserted by
//! [`Fact::IsPicture`], not a structural property of the image.
//!
//! Capture metadata (when the photograph was taken, where) lives here
//! because it's picture-role-specific — maps don't have a "capture date"
//! in the same sense. Byte-level provenance (source URL) lives in the
//! [`crate::facts::image`] cluster.
//!
//! # Error states (rejected at submit time)
//!
//! - **Picture-attribute fact on a map-role image.** [`Fact::CapturedDate`]
//!   and [`Fact::CapturedLocation`] are valid only on images that carry
//!   (or could plausibly carry) the picture role. Attaching them to an
//!   image already asserted as a map via
//!   [`crate::facts::map::Fact::IsMap`] is malformed.
//!
//! # Conflicts (surfaced at projection time)
//!
//! - **Role disagreement.** A image carrying both [`Fact::IsPicture`]
//!   and [`crate::facts::map::Fact::IsMap`] is a conflict the solver
//!   surfaces; sources sometimes classify the same image differently
//!   and a human picks the winner.
//! - **Capture date / location disagreement.** Multiple
//!   [`Fact::CapturedDate`] or [`Fact::CapturedLocation`] facts unify
//!   the same way bookend dates and locations do — interval meet for
//!   dates, the location subsumption lattice for locations. Empty meet
//!   or contradictory locations surface as user-resolvable conflicts.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::date::UncertainDate;
use crate::location::UnresolvedLocation;

/// Picture-cluster fact.
///
/// Generic over the image reference type `ImgId`. Picture facts no
/// longer reference entities directly — `CapturedLocation` carries an
/// [`UnresolvedLocation`] whose entity-scale containment is expressed
/// via [`crate::facts::attribute::Fact::Relationship`] on the relevant
/// entities, not embedded in the location reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(bound(
    serialize = "ImgId: Serialize",
    deserialize = "ImgId: serde::de::DeserializeOwned"
))]
#[schemars(bound = "ImgId: JsonSchema")]
pub enum Fact<ImgId> {
    /// Asserts the image is being treated as a picture — a photograph,
    /// painting, drawing, print, or other figurative depiction.
    /// Mutually exclusive with [`crate::facts::map::Fact::IsMap`] on the
    /// same image.
    IsPicture {
        /// Which image carries the picture-role claim.
        image: ImgId,
    },
    /// When the picture was captured (photographed, painted, drawn), as
    /// an uncertain interval.
    CapturedDate {
        /// Which image the bound applies to.
        image: ImgId,
        /// The source-claimed interval for the capture.
        bound: UncertainDate,
    },
    /// Where the picture was captured.
    CapturedLocation {
        /// Which image the location applies to.
        image: ImgId,
        /// The capture location.
        location: UnresolvedLocation,
    },
}
