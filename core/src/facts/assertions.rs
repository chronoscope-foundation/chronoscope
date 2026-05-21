//! The three assertion sums: factual, judgment, and meta.
//!
//! Each fact in the bag is one of three categories, paired with the
//! matching citation flavor from [`crate::facts::citations`]:
//!
//! - **Factual** ([`FactualAssertion`]) — claims about the external world,
//!   pair with [`crate::facts::citations::FactualCitation`]
//!   ([`crate::facts::citations::ExternalSource`] plus a non-empty
//!   excerpt list).
//! - **Judgment** ([`JudgmentAssertion`]) — interpretive conclusions
//!   (same-entity equivalence, depictions, observations), pair with a
//!   [`crate::facts::citations::JudgmentSource`] directly (per-variant
//!   warrants carry their own `justification` / `observer` / nested
//!   external source).
//! - **Meta** ([`MetaAssertion`]) — facts about facts (retractions,
//!   supersedings), pair with a [`crate::facts::citations::MetaSource`]
//!   directly (per-variant justification or external source).
//!
//! The two parametric sums dispatch into per-cluster `Fact` enums. The
//! outer wire shape uses adjacent tagging (`category` + `fact`); the
//! inner cluster shape uses internal tagging (`kind`). Bulk variant code
//! lives in the cluster modules:
//!
//! | Cluster                          | Outer variant                    |
//! |----------------------------------|----------------------------------|
//! | [`crate::facts::attribute`]      | `FactualAssertion::Attribute`    |
//! | [`crate::facts::bookend`]        | `FactualAssertion::Construction`, `FactualAssertion::Demolition` |
//! | [`crate::facts::event`]          | `FactualAssertion::Event`        |
//! | [`crate::facts::image`]          | `FactualAssertion::Image`        |
//! | [`crate::facts::picture`]        | `FactualAssertion::Picture`      |
//! | [`crate::facts::map`]            | `FactualAssertion::Map`          |
//! | [`crate::facts::identity`]       | `JudgmentAssertion::Identity`    |
//! | [`crate::facts::depiction`]      | `JudgmentAssertion::Depiction`   |
//! | [`crate::facts::observation`]    | `JudgmentAssertion::Observation` |
//! | [`crate::facts::composites`]     | `JudgmentAssertion::Composite`   |
//!
//! All factual / judgment outer variants are generic over reference
//! shapes (`EntId` for entity refs, `EvtId` for lifetime-event refs, `ImgId` for
//! the image id). Both picture-role and map-role facts are keyed by
//! `ImgId` since the role is a claim on a image, not a separate id type.
//! The same enum is used both during submission (with bundle-local
//! indices) and after storage (with persistent ids); call sites pick the
//! parameters that match.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::facts::ids::{CommitId, FactId};
use crate::facts::{
    attribute, bookend, composites, depiction, event, identity, image, map, observation, picture,
};

/// Factual assertion — a claim about the external world.
///
/// Construction and demolition are flat per-entity bookend facts rather
/// than event-mediated; once-ness is structural for those slots. Other
/// life-stage information attaches to a
/// [`LifetimeEventId`](crate::facts::ids::LifetimeEventId) via the
/// event-relative cluster.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "category", content = "fact", rename_all = "snake_case")]
#[serde(bound(
    serialize = "attribute::Fact<EntId>: Serialize, bookend::Fact<EntId>: Serialize, event::Fact<EntId, EvtId>: Serialize, image::Fact<ImgId>: Serialize, picture::Fact<ImgId>: Serialize, map::Fact<ImgId>: Serialize",
    deserialize = "attribute::Fact<EntId>: serde::de::DeserializeOwned, bookend::Fact<EntId>: serde::de::DeserializeOwned, event::Fact<EntId, EvtId>: serde::de::DeserializeOwned, image::Fact<ImgId>: serde::de::DeserializeOwned, picture::Fact<ImgId>: serde::de::DeserializeOwned, map::Fact<ImgId>: serde::de::DeserializeOwned"
))]
#[schemars(
    bound = "EntId: JsonSchema, EvtId: JsonSchema, ImgId: JsonSchema, attribute::Fact<EntId>: JsonSchema, bookend::Fact<EntId>: JsonSchema, event::Fact<EntId, EvtId>: JsonSchema, image::Fact<ImgId>: JsonSchema, picture::Fact<ImgId>: JsonSchema, map::Fact<ImgId>: JsonSchema"
)]
pub enum FactualAssertion<EntId, EvtId, ImgId> {
    /// Entity-level attribute claims (names, external refs, relationships).
    Attribute(attribute::Fact<EntId>),
    /// Construction bookend (start / completion / location).
    Construction(bookend::Fact<EntId>),
    /// Demolition bookend (start / completion / location).
    Demolition(bookend::Fact<EntId>),
    /// Interior-lifetime event facts plus cross-event gaps.
    Event(event::Fact<EntId, EvtId>),
    /// Byte-level image facts (source URL).
    Image(image::Fact<ImgId>),
    /// Pictorial role-claim plus picture-specific attributes (capture
    /// date, capture location).
    Picture(picture::Fact<ImgId>),
    /// Map role-claim plus map-specific attributes.
    Map(map::Fact<ImgId>),
}

/// Judgment assertion — an interpretive conclusion about the relationship
/// between entities or media.
///
/// Depiction claims live here (not on the factual side) because "entity
/// X appears in medium M" is interpretive even when supported by a
/// caption: the researcher or model interprets the visual or textual
/// content rather than recording a directly-observed external fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "category", content = "fact", rename_all = "snake_case")]
#[serde(bound(
    serialize = "identity::Fact<EntId, EvtId, ImgId>: Serialize, depiction::Fact<EntId, ImgId>: Serialize, observation::Fact<EntId>: Serialize, composites::Fact<ImgId>: Serialize",
    deserialize = "identity::Fact<EntId, EvtId, ImgId>: serde::de::DeserializeOwned, depiction::Fact<EntId, ImgId>: serde::de::DeserializeOwned, observation::Fact<EntId>: serde::de::DeserializeOwned, composites::Fact<ImgId>: serde::de::DeserializeOwned"
))]
#[schemars(
    bound = "EntId: JsonSchema, EvtId: JsonSchema, ImgId: JsonSchema, identity::Fact<EntId, EvtId, ImgId>: JsonSchema, depiction::Fact<EntId, ImgId>: JsonSchema, observation::Fact<EntId>: JsonSchema, composites::Fact<ImgId>: JsonSchema"
)]
pub enum JudgmentAssertion<EntId, EvtId, ImgId> {
    /// Same-entity / same-artifact / same-event equivalence judgments.
    Identity(identity::Fact<EntId, EvtId, ImgId>),
    /// Entity-in-picture / entity-on-map depiction judgments.
    Depiction(depiction::Fact<EntId, ImgId>),
    /// Feature and spatial-relation claims about entities (basis lives
    /// in the citation).
    Observation(observation::Fact<EntId>),
    /// Composite-image sub-region structural facts.
    Composite(composites::Fact<ImgId>),
}

/// Meta-assertion — a fact about other facts.
///
/// Retractions remove a fact from the active projection; supersedings
/// replace an existing fact with a corrected one. The one targeting
/// restriction (enforced at submit time once the submit pipeline
/// lands): a retract or supersede may not target a fact in the same
/// commit — that's a noise-prevention rule, not a structural one. A
/// retraction *is* retractable from a later commit, which is how an
/// accidental retraction gets undone; the projection walks the log in
/// strict commit order, so retracting a retraction unambiguously
/// restores the original fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MetaAssertion {
    /// Retract a single fact.
    RetractFact {
        /// The fact being retracted.
        target: FactId,
        /// Why the fact is being retracted.
        reason: RetractionReason,
    },
    /// Retract an entire commit (all of its facts).
    RetractCommit {
        /// The commit being retracted.
        target: CommitId,
        /// Why the commit is being retracted.
        reason: RetractionReason,
    },
    /// Replace an existing fact with a new one. The replacement fact is
    /// in the same commit (or a prior one) and carries the corrected
    /// assertion.
    SupersedeFact {
        /// The fact being superseded.
        target: FactId,
        /// The fact replacing it.
        replacement: FactId,
        /// Why the supersession is being recorded.
        reason: RetractionReason,
    },
}

/// Reason a retraction or supersession is being recorded.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RetractionReason {
    /// Deliberate damage to the data (e.g. a malicious edit).
    Vandalism,
    /// The original claim turned out to be factually wrong.
    FactualError,
    /// Action taken by a moderator under platform policy.
    ModeratorAction,
}
