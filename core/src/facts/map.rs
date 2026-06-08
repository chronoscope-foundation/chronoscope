//! Map cluster — map role-claim and map-specific attributes.
//!
//! Cluster module for the `Map` variant of
//! [`crate::facts::assertions::FactualAssertion`]. A "map" is an image being
//! treated as a cartographic representation — Sanborn fire insurance maps,
//! historical street maps, architectural plans. The classification is a claim
//! asserted by [`Fact::IsMap`], not a structural property of the image.
//!
//! Byte-level provenance (source URL) and author / creation metadata live in
//! [`crate::facts::image`] — image-level concerns shared between pictures and
//! maps.
//!
//! # Single-variant today; expected expansion
//!
//! Cartographic attributes land here as downstream consumers need them. The
//! variants we expect to add:
//!
//! - `Scale { image, ratio }` — map scale (1:24000, 1:600, etc.).
//! - `Orientation { image, north_bearing }` — angular offset between sheet-up
//!   and true north, when the sheet isn't conventionally oriented.
//! - `ReferencePoint { image, image_point, geo_point }` — one georeferencing
//!   tie-point pairing a pixel coordinate on the sheet with a real-world
//!   [`crate::geo::GeoPoint`]. Two or more give the solver enough to assemble
//!   an affine transform.
//!
//! # Error states (rejected at submit time)
//!
//! - **Map-attribute fact on a picture-role image.** When `Scale`,
//!   `Orientation`, and `ReferencePoint` land they will be valid only on images
//!   that carry (or could plausibly carry) the map role. Attaching them to an
//!   image already asserted as a picture via
//!   [`crate::facts::picture::Fact::IsPicture`] is malformed.
//!
//! # Conflicts (surfaced at projection time)
//!
//! - **Role disagreement.** An image carrying both [`Fact::IsMap`] and
//!   [`crate::facts::picture::Fact::IsPicture`] is a conflict the solver
//!   surfaces; sources sometimes classify the same image differently and a
//!   human picks the winner.

use chronoscope_macros::grammar_type;

/// Map-cluster fact.
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
    /// Asserts the image is being treated as a map — a cartographic
    /// representation rather than a figurative depiction. Mutually
    /// exclusive with [`crate::facts::picture::Fact::IsPicture`] on the
    /// same image.
    IsMap {
        /// Which image carries the map-role claim.
        image: ImgId,
    },
}

impl<ImgId> Fact<ImgId> {
    /// Visit the single image id this fact mentions.
    pub fn for_each_id(&self, fi: &mut impl FnMut(&ImgId)) {
        match self {
            Self::IsMap { image } => fi(image),
        }
    }

    /// Relabel the single image id through the fallible closure, producing
    /// a `Fact<I2>`.
    pub fn try_map_ids<I2, Err>(
        &self,
        fi: &mut impl FnMut(&ImgId) -> Result<I2, Err>,
    ) -> Result<Fact<I2>, Err> {
        match self {
            Self::IsMap { image } => Ok(Fact::IsMap { image: fi(image)? }),
        }
    }
}
