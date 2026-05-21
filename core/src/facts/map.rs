//! Map cluster — map role-claim and map-specific attributes.
//!
//! Cluster module for the `Map` variant of
//! [`crate::facts::assertions::FactualAssertion`]. A "map" is a image
//! being treated as a cartographic representation — Sanborn fire
//! insurance maps, historical street maps, architectural plans. The
//! classification is a *claim* asserted by [`Fact::IsMap`], not a
//! structural property of the image.
//!
//! Byte-level provenance (source URL) and author / creation metadata
//! live in [`crate::facts::image`] — those facts are image-level
//! concerns shared between pictures and maps.
//!
//! # Currently single-variant; expected expansion
//!
//! The cluster is intentionally thin today. Cartographic attributes
//! land here as downstream consumers need them; the variants we expect
//! to add when the use case arrives are:
//!
//! - `Scale { image, ratio }` — map scale (1:24000, 1:600, etc.).
//! - `Orientation { image, north_bearing }` — angular offset between
//!   sheet-up and true north, when the sheet isn't conventionally
//!   oriented.
//! - `ReferencePoint { image, image_point, geo_point }` — one
//!   georeferencing tie-point pairing a pixel coordinate on the sheet
//!   with a real-world [`crate::facts::geometry::GeoPoint`]. Two or more
//!   such facts give the solver enough to assemble an affine transform.
//!
//! # Error states (rejected at submit time)
//!
//! - **Map-attribute fact on a picture-role image.** When `Scale`,
//!   `Orientation`, and `ReferencePoint` land they will be valid only
//!   on images that carry (or could plausibly carry) the map role.
//!   Attaching them to an image already asserted as a picture via
//!   [`crate::facts::picture::Fact::IsPicture`] is malformed.
//!
//! # Conflicts (surfaced at projection time)
//!
//! - **Role disagreement.** A image carrying both [`Fact::IsMap`] and
//!   [`crate::facts::picture::Fact::IsPicture`] is a conflict the
//!   solver surfaces; sources sometimes classify the same image
//!   differently and a human picks the winner.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Map-cluster fact.
///
/// Generic over the image reference type `ImgId`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(bound(
    serialize = "ImgId: Serialize",
    deserialize = "ImgId: serde::de::DeserializeOwned"
))]
#[schemars(bound = "ImgId: JsonSchema")]
pub enum Fact<ImgId> {
    /// Asserts the image is being treated as a map — a cartographic
    /// representation rather than a figurative depiction. Mutually
    /// exclusive with [`crate::facts::picture::Fact::IsPicture`] on the
    /// same image.
    IsMap {
        /// Which image carries the map-role claim.
        image: ImgId,
    },
}
