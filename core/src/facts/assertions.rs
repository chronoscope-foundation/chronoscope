//! The three assertion sums: factual, judgment, and meta.
//!
//! Each fact in the bag is one of three categories, paired with the matching
//! citation flavor from [`crate::facts::citations`]:
//!
//! - **Factual** ([`FactualAssertion`]) — claims about the external world,
//!   with a [`crate::facts::citations::FactualCitation`].
//! - **Judgment** ([`JudgmentAssertion`]) — interpretive conclusions
//!   (equivalence, depictions, observations), with a
//!   [`crate::facts::citations::JudgmentSource`].
//! - **Meta** ([`MetaAssertion`]) — facts about facts (retractions,
//!   supersedings), with a [`crate::facts::citations::MetaSource`].
//!
//! The two parametric sums dispatch into per-cluster `Fact` enums. Both levels
//! use internal tagging on `type`; the outer tag picks the cluster, the inner
//! `fact` field carries the cluster's tagged fact. Bulk variant code lives in
//! the cluster modules:
//!
//! | Cluster                          | Outer variant                    |
//! |----------------------------------|----------------------------------|
//! | [`crate::facts::attribute`]      | `FactualAssertion::Attribute`    |
//! | [`crate::facts::bookend`]        | `FactualAssertion::Construction`, `FactualAssertion::Demolition` |
//! | [`crate::facts::event`]          | `FactualAssertion::Event`        |
//! | [`crate::facts::image`]          | `FactualAssertion::Image`        |
//! | [`crate::facts::identity`]       | `JudgmentAssertion::Identity`    |
//! | [`crate::facts::depiction`]      | `JudgmentAssertion::Depiction`   |
//! | [`crate::facts::observation`]    | `JudgmentAssertion::Observation` |
//! | [`crate::facts::composites`]     | `JudgmentAssertion::Composite`   |
//!
//! The outer variants are generic over reference shapes (`EntId`, `EvtId`,
//! `ImgId`). Every image-level fact is keyed by `ImgId`. The same enum serves
//! submission (bundle-local indices) and storage (persistent ids); call sites
//! pick the parameters.

use chronoscope_macros::grammar_type;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::facts::ids::{CommitId, FactId};
use crate::facts::{
    attribute, bookend, composites, depiction, event, identity, image, observation,
};

/// Factual assertion — a claim about the external world.
///
/// Construction and demolition are flat per-entity bookend facts, not
/// event-mediated, so once-ness is structural. Other life-stage information
/// attaches to a [`LifetimeEventId`](crate::facts::ids::LifetimeEventId) via the
/// event cluster.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(bound(
    serialize = "EntId: Ord, EvtId: Ord, attribute::Fact<EntId>: ::serde::Serialize, bookend::Fact<EntId>: ::serde::Serialize, event::Fact<EntId, EvtId>: ::serde::Serialize, event::GapBounds<EntId, EvtId>: ::serde::Serialize, image::Fact<ImgId>: ::serde::Serialize",
    deserialize = "EntId: Ord, EvtId: Ord, attribute::Fact<EntId>: ::serde::de::DeserializeOwned, bookend::Fact<EntId>: ::serde::de::DeserializeOwned, event::Fact<EntId, EvtId>: ::serde::de::DeserializeOwned, event::GapBounds<EntId, EvtId>: ::serde::de::DeserializeOwned, image::Fact<ImgId>: ::serde::de::DeserializeOwned"
))]
#[schemars(
    bound = "EntId: ::schemars::JsonSchema + Ord, EvtId: ::schemars::JsonSchema + Ord, ImgId: ::schemars::JsonSchema, attribute::Fact<EntId>: ::schemars::JsonSchema, bookend::Fact<EntId>: ::schemars::JsonSchema, event::Fact<EntId, EvtId>: ::schemars::JsonSchema, event::GapBounds<EntId, EvtId>: ::schemars::JsonSchema, image::Fact<ImgId>: ::schemars::JsonSchema"
)]
pub enum FactualAssertion<EntId: Ord, EvtId: Ord, ImgId> {
    /// Entity-level attribute claims (names, external refs, relationships).
    Attribute { fact: attribute::Fact<EntId> },
    /// Construction bookend (start / completion / location).
    Construction { fact: bookend::Fact<EntId> },
    /// Demolition bookend (start / completion / location).
    Demolition { fact: bookend::Fact<EntId> },
    /// Interior-lifetime event facts.
    Event { fact: event::Fact<EntId, EvtId> },
    /// A temporal-ordering relationship between two events or entity bookends.
    /// A gap names two endpoints rather than one event subject, so it sits
    /// beside the event cluster rather than inside [`event::Fact`].
    Gap {
        /// The cross-event gap bounds (endpoints plus day range).
        bounds: event::GapBounds<EntId, EvtId>,
    },
    /// Image-level facts: source URL, author, created / capture dates,
    /// capture location, and the descriptive medium.
    Image { fact: image::Fact<ImgId> },
}

impl<EntId: Ord, EvtId: Ord, ImgId> FactualAssertion<EntId, EvtId, ImgId> {
    /// Visit every id this assertion mentions, dispatching each to its kind's
    /// closure. The collector half of the id-traversal.
    ///
    /// Holds all three closures and hands each cluster the subset it needs
    /// (attribute / bookend get entity, event and gap get entity + event,
    /// image gets image).
    pub fn for_each_id(
        &self,
        fe: &mut impl FnMut(&EntId),
        fv: &mut impl FnMut(&EvtId),
        fi: &mut impl FnMut(&ImgId),
    ) {
        match self {
            Self::Attribute { fact } => fact.for_each_id(fe),
            Self::Construction { fact } | Self::Demolition { fact } => fact.for_each_id(fe),
            Self::Event { fact } => fact.for_each_id(fe, fv),
            Self::Gap { bounds } => bounds.for_each_id(fe, fv),
            Self::Image { fact } => fact.for_each_id(fi),
        }
    }

    /// Relabel every id through the kind-matching fallible closure,
    /// producing a `FactualAssertion<E2, V2, I2>`.
    ///
    /// Threads the three closures into each cluster's `try_map_ids`, names
    /// the concrete error ([`crate::facts::identity::IdMapError<E2, V2, I2>`]),
    /// and supplies the attribute `Relationship` arm's `on_self_loop`, which
    /// wraps a pair collapse as
    /// [`crate::facts::identity::SelfLoop::Relationship`]. A leaf-lookup
    /// rejection or that collapse propagates as the `IdMapError`.
    pub fn try_map_ids<E2, V2, I2>(
        &self,
        fe: &mut impl FnMut(&EntId) -> Result<E2, crate::facts::identity::IdMapError<E2, V2, I2>>,
        fv: &mut impl FnMut(&EvtId) -> Result<V2, crate::facts::identity::IdMapError<E2, V2, I2>>,
        fi: &mut impl FnMut(&ImgId) -> Result<I2, crate::facts::identity::IdMapError<E2, V2, I2>>,
    ) -> Result<FactualAssertion<E2, V2, I2>, crate::facts::identity::IdMapError<E2, V2, I2>>
    where
        E2: Ord,
        V2: Ord,
    {
        use crate::facts::identity::{IdMapError, SelfLoop};
        match self {
            Self::Attribute { fact } => Ok(FactualAssertion::Attribute {
                fact: fact
                    .try_map_ids(fe, |id| IdMapError::SelfLoop(SelfLoop::Relationship(id)))?,
            }),
            Self::Construction { fact } => Ok(FactualAssertion::Construction {
                fact: fact.try_map_ids(fe)?,
            }),
            Self::Demolition { fact } => Ok(FactualAssertion::Demolition {
                fact: fact.try_map_ids(fe)?,
            }),
            Self::Event { fact } => Ok(FactualAssertion::Event {
                fact: fact.try_map_ids(fe, fv)?,
            }),
            Self::Gap { bounds } => Ok(FactualAssertion::Gap {
                bounds: bounds.try_map_ids(fe, fv)?,
            }),
            Self::Image { fact } => Ok(FactualAssertion::Image {
                fact: fact.try_map_ids(fi)?,
            }),
        }
    }
}

/// Judgment assertion — an interpretive conclusion about entities or media.
///
/// Depiction claims live here, not on the factual side: "entity X appears in
/// medium M" is interpretive even with a caption, since the researcher or model
/// reads the content rather than recording a directly-observed fact.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(bound(
    serialize = "EntId: Ord, EvtId: Ord, ImgId: Ord, identity::Fact<EntId, EvtId, ImgId>: ::serde::Serialize, depiction::Fact<EntId, ImgId>: ::serde::Serialize, observation::Fact<EntId>: ::serde::Serialize, composites::Fact<ImgId>: ::serde::Serialize",
    deserialize = "EntId: Ord, EvtId: Ord, ImgId: Ord, identity::Fact<EntId, EvtId, ImgId>: ::serde::de::DeserializeOwned, depiction::Fact<EntId, ImgId>: ::serde::de::DeserializeOwned, observation::Fact<EntId>: ::serde::de::DeserializeOwned, composites::Fact<ImgId>: ::serde::de::DeserializeOwned"
))]
#[schemars(
    bound = "EntId: ::schemars::JsonSchema + Ord, EvtId: ::schemars::JsonSchema + Ord, ImgId: ::schemars::JsonSchema + Ord, identity::Fact<EntId, EvtId, ImgId>: ::schemars::JsonSchema, depiction::Fact<EntId, ImgId>: ::schemars::JsonSchema, observation::Fact<EntId>: ::schemars::JsonSchema, composites::Fact<ImgId>: ::schemars::JsonSchema"
)]
pub enum JudgmentAssertion<EntId: Ord, EvtId: Ord, ImgId: Ord> {
    /// Same-entity / same-artifact / same-event equivalence judgments.
    Identity {
        fact: identity::Fact<EntId, EvtId, ImgId>,
    },
    /// Entity-in-image depiction judgments.
    Depiction { fact: depiction::Fact<EntId, ImgId> },
    /// Feature and spatial-relation claims about entities (basis lives
    /// in the citation).
    Observation { fact: observation::Fact<EntId> },
    /// Composite-image sub-region structural facts.
    Composite { fact: composites::Fact<ImgId> },
}

impl<EntId: Ord, EvtId: Ord, ImgId: Ord> JudgmentAssertion<EntId, EvtId, ImgId> {
    /// Visit every id this assertion mentions, dispatching each to its
    /// kind's closure. The collector half of the id-traversal.
    ///
    /// Hands each cluster the subset it needs — identity gets all three
    /// (each variant uses one), depiction gets entity + image, observation
    /// gets entity, composite gets image.
    pub fn for_each_id(
        &self,
        fe: &mut impl FnMut(&EntId),
        fv: &mut impl FnMut(&EvtId),
        fi: &mut impl FnMut(&ImgId),
    ) {
        match self {
            Self::Identity { fact } => fact.for_each_id(fe, fv, fi),
            Self::Depiction { fact } => fact.for_each_id(fe, fi),
            Self::Observation { fact } => fact.for_each_id(fe),
            Self::Composite { fact } => fact.for_each_id(fi),
        }
    }

    /// Relabel every id through the kind-matching fallible closure,
    /// producing a `JudgmentAssertion<E2, V2, I2>`.
    ///
    /// Threads the three closures into each cluster's `try_map_ids` and
    /// names the same [`crate::facts::identity::IdMapError<E2, V2, I2>`] the
    /// factual dispatch does. Identity builds its own
    /// [`crate::facts::identity::SelfLoop`] wrappers, so only the
    /// observation `Spatial` arm supplies an `on_self_loop` here (wrapping a
    /// collapse as [`crate::facts::identity::SelfLoop::Spatial`]). A
    /// leaf-lookup rejection or any pair collapse propagates as the
    /// `IdMapError`.
    pub fn try_map_ids<E2, V2, I2>(
        &self,
        fe: &mut impl FnMut(&EntId) -> Result<E2, crate::facts::identity::IdMapError<E2, V2, I2>>,
        fv: &mut impl FnMut(&EvtId) -> Result<V2, crate::facts::identity::IdMapError<E2, V2, I2>>,
        fi: &mut impl FnMut(&ImgId) -> Result<I2, crate::facts::identity::IdMapError<E2, V2, I2>>,
    ) -> Result<JudgmentAssertion<E2, V2, I2>, crate::facts::identity::IdMapError<E2, V2, I2>>
    where
        E2: Ord,
        V2: Ord,
        I2: Ord,
    {
        use crate::facts::identity::{IdMapError, SelfLoop};
        match self {
            Self::Identity { fact } => Ok(JudgmentAssertion::Identity {
                fact: fact.try_map_ids(fe, fv, fi)?,
            }),
            Self::Depiction { fact } => Ok(JudgmentAssertion::Depiction {
                fact: fact.try_map_ids(fe, fi)?,
            }),
            Self::Observation { fact } => Ok(JudgmentAssertion::Observation {
                fact: fact.try_map_ids(fe, |id| IdMapError::SelfLoop(SelfLoop::Spatial(id)))?,
            }),
            Self::Composite { fact } => Ok(JudgmentAssertion::Composite {
                fact: fact.try_map_ids(fi)?,
            }),
        }
    }
}

/// Meta-assertion — a fact about other facts.
///
/// Retractions remove a fact from the active projection; supersedings
/// replace one with a corrected fact. One targeting rule (enforced at
/// submit time): a retract or supersede may not target a fact in the same
/// commit. A retraction is itself retractable from a later commit — the
/// projection walks the log in commit order, so retracting a retraction
/// restores the original.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

#[cfg(test)]
mod traversal_props {
    //! Whole-grammar property tests for the id-traversal
    //! (`try_map_ids` / `for_each_id`).
    //!
    //! The traversal is hand-written per cluster and dispatched here at the
    //! `FactualAssertion` / `JudgmentAssertion` level. The `event` and
    //! `identity` unit tests cover the trickiest shapes (nested `Gap`
    //! recursion; kinded self-loop); the other clusters' arms are covered only
    //! incidentally through the submit pipeline. These props generate every
    //! variant of every cluster and assert two laws that fail on a dropped or
    //! reordered field, a same-kind position-swap, or drift between the two
    //! methods.
    //!
    //! `try_map_ids` carries every non-id field verbatim and touches only ids,
    //! so the generators vary two things: which variant and the id values.
    //! Non-id fields are fixed valid sentinels — varying them would test
    //! `Clone`, not the traversal.
    //!
    //! Ids are distinct `Memory*Id` values (via [`distinct_u64_pair`] for
    //! same-kind pairs). The three id kinds are distinct Rust types, so a
    //! closure-dispatch swap can't typecheck; distinct values catch the rest:
    //!
    //! - The identity law catches a same-kind position-swap (e.g.
    //!   `IsSubimageOf { subimage, parent }` reconstructed swapped), since the
    //!   output no longer equals the distinct-valued input. The pair types
    //!   canonicalize, but the input's constructors already ran, so an identity
    //!   remap reproduces the same canonical form.
    //! - The coverage-agreement law catches `for_each_id` visiting a different
    //!   id, count, or order than `try_map_ids`.

    use proptest::prelude::*;

    use crate::WikidataEntityId;
    use crate::date::{DatePrecision, UncertainDate};
    use crate::facts::citations::{ExternalReference, Language};
    use crate::facts::composites::SubimageRegion;
    use crate::facts::geometry::{ImageGeometry, ProportionalPolyline};
    use crate::facts::identity::IdMapError;
    use crate::facts::lifecycle::{
        DamageCause, DurationalKind, DurationalRole, LifetimeEventKind, MoveMethod, PointKind,
        Usage,
    };
    use crate::facts::memory::{MemoryEntityId, MemoryEventId, MemoryImageId};
    use crate::facts::spatial::TopologicalRel;
    use crate::facts::{
        attribute, bookend, composites, depiction, event, identity, image, observation,
    };
    use crate::location::{LocationReference, UnresolvedLocation};

    use super::{FactualAssertion, JudgmentAssertion};

    // The three concrete id types under test.
    type FactualA = FactualAssertion<MemoryEntityId, MemoryEventId, MemoryImageId>;
    type JudgmentA = JudgmentAssertion<MemoryEntityId, MemoryEventId, MemoryImageId>;
    // The traversal's error. The identity remaps below never produce it; naming
    // it keeps the helper return types concrete.
    type MapErr = IdMapError<MemoryEntityId, MemoryEventId, MemoryImageId>;

    // ------------------------------------------------------------------
    // Leaf id strategies
    // ------------------------------------------------------------------

    fn arb_entity() -> impl Strategy<Value = MemoryEntityId> {
        any::<u64>().prop_map(MemoryEntityId)
    }

    fn arb_event() -> impl Strategy<Value = MemoryEventId> {
        any::<u64>().prop_map(MemoryEventId)
    }

    fn arb_image() -> impl Strategy<Value = MemoryImageId> {
        any::<u64>().prop_map(MemoryImageId)
    }

    /// Two distinct `u64`s without rejection sampling: `b = a + d` for
    /// `d ∈ 1..=u64::MAX`, which (wrapping) never equals `a`. Distinctness makes
    /// a same-kind position-swap observable and keeps the distinct-pair
    /// constructors from rejecting the input.
    fn distinct_u64_pair() -> impl Strategy<Value = (u64, u64)> {
        (any::<u64>(), 1u64..=u64::MAX).prop_map(|(a, d)| (a, a.wrapping_add(d)))
    }

    fn arb_distinct_entities() -> impl Strategy<Value = (MemoryEntityId, MemoryEntityId)> {
        distinct_u64_pair().prop_map(|(a, b)| (MemoryEntityId(a), MemoryEntityId(b)))
    }

    fn arb_distinct_events() -> impl Strategy<Value = (MemoryEventId, MemoryEventId)> {
        distinct_u64_pair().prop_map(|(a, b)| (MemoryEventId(a), MemoryEventId(b)))
    }

    fn arb_distinct_images() -> impl Strategy<Value = (MemoryImageId, MemoryImageId)> {
        distinct_u64_pair().prop_map(|(a, b)| (MemoryImageId(a), MemoryImageId(b)))
    }

    // ------------------------------------------------------------------
    // Fixed valid sentinels for non-id fields
    //
    // Built inside `prop_filter_map` closures so the fallible constructors stay
    // `?`-propagated rather than unwrapped. The inputs are constant and valid,
    // so the filter never rejects.
    // ------------------------------------------------------------------

    fn sentinel_date() -> impl Strategy<Value = UncertainDate> {
        Just(()).prop_filter_map("valid sentinel date", |()| {
            let date = chrono::NaiveDate::from_ymd_opt(1900, 1, 1)?;
            UncertainDate::with_precision(date, DatePrecision::Year).ok()
        })
    }

    fn sentinel_location() -> UnresolvedLocation {
        UnresolvedLocation::Reference(LocationReference::NamedPlace {
            name: "sentinel".to_owned(),
        })
    }

    fn sentinel_external_reference() -> ExternalReference {
        ExternalReference::Wikidata {
            qid: WikidataEntityId::new(243),
        }
    }

    fn sentinel_image_bbox() -> impl Strategy<Value = ImageGeometry> {
        Just(()).prop_filter_map("valid sentinel bbox", |()| {
            ImageGeometry::bbox(0.0, 0.0, 0.5, 0.5).ok()
        })
    }

    fn sentinel_proportional_polyline() -> impl Strategy<Value = ProportionalPolyline> {
        Just(()).prop_filter_map("valid sentinel polyline", |()| {
            ProportionalPolyline::new(vec![(0.1, 0.2), (0.3, 0.4)]).ok()
        })
    }

    /// A random [`image::ImageMedium`] across all three values, so the `Medium`
    /// arm exercises picture, map, and pictorial-map rather than one fixed kind.
    fn arb_image_medium() -> impl Strategy<Value = image::ImageMedium> {
        prop_oneof![
            Just(image::ImageMedium::Picture),
            Just(image::ImageMedium::Map),
            Just(image::ImageMedium::PictorialMap),
        ]
    }

    fn sentinel_subimage_region() -> impl Strategy<Value = SubimageRegion> {
        Just(()).prop_filter_map("valid sentinel subimage region", |()| {
            SubimageRegion::rect(0.0, 0.0, 0.5, 0.5).ok()
        })
    }

    fn sentinel_language() -> impl Strategy<Value = Language> {
        Just(()).prop_filter_map("valid sentinel language tag", |()| Language::new("en").ok())
    }

    fn sentinel_url() -> impl Strategy<Value = url::Url> {
        Just(()).prop_filter_map("valid sentinel url", |()| {
            url::Url::parse("https://example.com/img.jpg").ok()
        })
    }

    // ------------------------------------------------------------------
    // Per-cluster Fact strategies — `prop_oneof!` over EVERY variant
    // ------------------------------------------------------------------

    /// `attribute::Fact` — all three variants. `Relationship` feeds two
    /// distinct entities into the directional `DistinctPair`.
    fn arb_attribute() -> impl Strategy<Value = attribute::Fact<MemoryEntityId>> {
        prop_oneof![
            (arb_entity(), sentinel_language()).prop_map(|(entity, language)| {
                attribute::Fact::Name {
                    entity,
                    name: attribute::NameText::new("name"),
                    language,
                    name_type: attribute::NameType::Common,
                    valid_from: None,
                    valid_to: None,
                }
            }),
            arb_entity().prop_map(|entity| attribute::Fact::ExternalReference {
                entity,
                reference: sentinel_external_reference(),
            }),
            arb_distinct_entities().prop_filter_map("distinct relationship pair", |(from, to)| {
                attribute::Fact::relationship(from, to, attribute::EntityRelationType::Contains)
                    .ok()
            }),
        ]
    }

    /// `bookend::Fact` — all three variants. Shared by the `Construction`
    /// and `Demolition` arms, both generated in `arb_factual_assertion`.
    fn arb_bookend() -> impl Strategy<Value = bookend::Fact<MemoryEntityId>> {
        prop_oneof![
            (arb_entity(), sentinel_date())
                .prop_map(|(entity, bound)| bookend::Fact::Started { entity, bound }),
            (arb_entity(), sentinel_date())
                .prop_map(|(entity, bound)| bookend::Fact::Completed { entity, bound }),
            arb_entity().prop_map(|entity| bookend::Fact::Location {
                entity,
                location: sentinel_location(),
            }),
        ]
    }

    /// `event::OrderableEvent` — all three variants. `Event` carries an
    /// event id, the two bookend-anchored ones an entity id, so a mixed
    /// `Gap` routes through both closures.
    fn arb_orderable_event()
    -> impl Strategy<Value = event::OrderableEvent<MemoryEntityId, MemoryEventId>> {
        prop_oneof![
            arb_event().prop_map(|event| event::OrderableEvent::Event { event }),
            arb_entity()
                .prop_map(|entity| event::OrderableEvent::ConstructionCompletion { entity }),
            arb_entity().prop_map(|entity| event::OrderableEvent::DemolitionStart { entity }),
        ]
    }

    /// `event::Fact` — all nine variants, including `HasEvent` (the only one
    /// carrying both an entity and an event ref, with a random declared kind so
    /// both category arms and every subtype are exercised).
    fn arb_event_fact() -> impl Strategy<Value = event::Fact<MemoryEntityId, MemoryEventId>> {
        prop_oneof![
            (arb_entity(), arb_event(), arb_lifetime_event_kind()).prop_map(
                |(entity, event, kind)| event::Fact::HasEvent {
                    entity,
                    event,
                    kind,
                }
            ),
            (arb_event(), sentinel_date()).prop_map(|(event, bound)| {
                event::Fact::DurationalDate {
                    event,
                    role: DurationalRole::Started,
                    bound,
                }
            }),
            (arb_event(), sentinel_date())
                .prop_map(|(event, bound)| event::Fact::PointDate { event, bound }),
            arb_event().prop_map(|event| event::Fact::MovedToLocation {
                event,
                location: sentinel_location(),
            }),
            arb_event().prop_map(|event| event::Fact::DamageCause {
                event,
                cause: DamageCause::Fire,
            }),
            arb_event().prop_map(|event| event::Fact::MoveMethod {
                event,
                method: MoveMethod::Whole,
            }),
            arb_event().prop_map(|event| event::Fact::UsageChange {
                event,
                new_usages: std::iter::once(Usage::Commercial).collect(),
            }),
            arb_event().prop_map(|event| event::Fact::Designation {
                event,
                designation: "landmark".to_owned(),
            }),
            arb_event().prop_map(|event| event::Fact::Description {
                event,
                text: "text".to_owned(),
            }),
        ]
    }

    /// A random [`LifetimeEventKind`] across both category arms and every
    /// subtype, so the `HasEvent` generator exercises the full kind grammar.
    fn arb_lifetime_event_kind() -> impl Strategy<Value = LifetimeEventKind> {
        prop_oneof![
            prop_oneof![
                Just(DurationalKind::Modified),
                Just(DurationalKind::Damaged),
                Just(DurationalKind::Repaired),
                Just(DurationalKind::Moved),
            ]
            .prop_map(|kind| LifetimeEventKind::Durational { kind }),
            prop_oneof![Just(PointKind::UsageChanged), Just(PointKind::Designated),]
                .prop_map(|kind| LifetimeEventKind::Point { kind }),
        ]
    }

    /// `event::GapBounds` — both endpoints generated independently, so a mixed
    /// gap routes through both id closures. Forces the nested
    /// `GapBounds -> OrderableEvent` recursion.
    fn arb_gap_bounds() -> impl Strategy<Value = event::GapBounds<MemoryEntityId, MemoryEventId>> {
        (arb_orderable_event(), arb_orderable_event()).prop_filter_map(
            "valid gap bounds",
            |(from, to)| {
                event::GapBounds::new(
                    from,
                    to,
                    Some(event::Days::new(1)),
                    Some(event::Days::new(5)),
                )
                .ok()
            },
        )
    }

    /// `image::Fact` — all six variants (`Source`, `Author`, `CreatedDate`,
    /// `CapturedDate`, `CapturedLocation`, `Medium`).
    fn arb_image_fact() -> impl Strategy<Value = image::Fact<MemoryImageId>> {
        prop_oneof![
            (arb_image(), sentinel_url())
                .prop_map(|(image, url)| image::Fact::Source { image, url }),
            (arb_image(), sentinel_language()).prop_map(|(image, language)| image::Fact::Author {
                image,
                name: "author".to_owned(),
                language,
            }),
            (arb_image(), sentinel_date())
                .prop_map(|(image, bound)| image::Fact::CreatedDate { image, bound }),
            (arb_image(), sentinel_date())
                .prop_map(|(image, bound)| image::Fact::CapturedDate { image, bound }),
            arb_image().prop_map(|image| image::Fact::CapturedLocation {
                image,
                location: sentinel_location(),
            }),
            (arb_image(), arb_image_medium())
                .prop_map(|(image, medium)| image::Fact::Medium { image, medium }),
        ]
    }

    /// `identity::Fact` — all three variants, each fed two distinct
    /// same-kind ids through its `OrderedDistinctPair`.
    fn arb_identity()
    -> impl Strategy<Value = identity::Fact<MemoryEntityId, MemoryEventId, MemoryImageId>> {
        prop_oneof![
            arb_distinct_entities().prop_filter_map("distinct same_entity", |(a, b)| {
                identity::Fact::same_entity(a, b).ok()
            }),
            arb_distinct_images().prop_filter_map("distinct same_artifact", |(a, b)| {
                identity::Fact::same_artifact(a, b).ok()
            }),
            arb_distinct_events().prop_filter_map("distinct same_event", |(a, b)| {
                identity::Fact::same_event(a, b).ok()
            }),
        ]
    }

    /// `depiction::Fact` — the merged depiction struct, generated across the
    /// localization-present and bare shapes (the optional fields carry no ids,
    /// so the identity law pins the entity/image reconstruction either way).
    fn arb_depiction() -> impl Strategy<Value = depiction::Fact<MemoryEntityId, MemoryImageId>> {
        prop_oneof![
            (arb_entity(), arb_image(), sentinel_image_bbox()).prop_map(
                |(entity, image, geometry)| depiction::Fact {
                    entity,
                    image,
                    localization: Some(geometry),
                    perspective: Some(depiction::Perspective::Exterior),
                }
            ),
            (arb_entity(), arb_image(), sentinel_proportional_polyline()).prop_map(
                |(entity, image, polyline)| depiction::Fact {
                    entity,
                    image,
                    localization: Some(ImageGeometry::Polyline { polyline }),
                    perspective: Some(depiction::Perspective::Interior),
                }
            ),
            (arb_entity(), arb_image()).prop_map(|(entity, image)| depiction::Fact {
                entity,
                image,
                localization: None,
                perspective: None,
            }),
        ]
    }

    /// `spatial::TopologicalRel` — all six variants; three carry a
    /// separator/axis entity id, three carry none.
    fn arb_topological_rel() -> impl Strategy<Value = TopologicalRel<MemoryEntityId>> {
        prop_oneof![
            Just(TopologicalRel::Adjacent),
            arb_entity().prop_map(|separator| TopologicalRel::AcrossFrom { separator }),
            Just(TopologicalRel::PartOf),
            arb_entity().prop_map(|separator| TopologicalRel::SameSide { separator }),
            arb_entity().prop_map(|axis| TopologicalRel::LinedAlong { axis }),
            Just(TopologicalRel::Surrounds),
        ]
    }

    /// `observation::Fact` — both variants. `Spatial` carries a
    /// `DistinctPair` plus a `TopologicalRel` whose separator/axis is a
    /// third, independent entity id — the traversal visits the pair first.
    fn arb_observation() -> impl Strategy<Value = observation::Fact<MemoryEntityId>> {
        prop_oneof![
            arb_entity().prop_map(|entity| observation::Fact::Feature {
                entity,
                feature: crate::facts::features::Feature::StoryCount { stories: 2 },
            }),
            (arb_distinct_entities(), arb_topological_rel())
                .prop_filter_map("distinct spatial pair", |((a, b), relation)| {
                    observation::Fact::spatial(a, b, relation).ok()
                }),
        ]
    }

    /// `composites::Fact` — the single `IsSubimageOf` variant. The two image
    /// ids don't canonicalize, so they're drawn distinct to make a
    /// `subimage`/`parent` swap observable under the identity law.
    fn arb_composite() -> impl Strategy<Value = composites::Fact<MemoryImageId>> {
        (arb_distinct_images(), sentinel_subimage_region()).prop_map(
            |((subimage, parent), region)| composites::Fact::IsSubimageOf {
                subimage,
                parent,
                region,
            },
        )
    }

    // ------------------------------------------------------------------
    // Assertion-level strategies — `prop_oneof!` over every cluster
    // ------------------------------------------------------------------

    /// Covers every factual cluster. Bookend appears twice — `Construction`
    /// and `Demolition` — to exercise both dispatch arms.
    fn arb_factual_assertion() -> impl Strategy<Value = FactualA> {
        prop_oneof![
            arb_attribute().prop_map(|fact| FactualAssertion::Attribute { fact }),
            arb_bookend().prop_map(|fact| FactualAssertion::Construction { fact }),
            arb_bookend().prop_map(|fact| FactualAssertion::Demolition { fact }),
            arb_event_fact().prop_map(|fact| FactualAssertion::Event { fact }),
            arb_gap_bounds().prop_map(|bounds| FactualAssertion::Gap { bounds }),
            arb_image_fact().prop_map(|fact| FactualAssertion::Image { fact }),
        ]
    }

    /// Covers every judgment cluster.
    fn arb_judgment_assertion() -> impl Strategy<Value = JudgmentA> {
        prop_oneof![
            arb_identity().prop_map(|fact| JudgmentAssertion::Identity { fact }),
            arb_depiction().prop_map(|fact| JudgmentAssertion::Depiction { fact }),
            arb_observation().prop_map(|fact| JudgmentAssertion::Observation { fact }),
            arb_composite().prop_map(|fact| JudgmentAssertion::Composite { fact }),
        ]
    }

    // ------------------------------------------------------------------
    // Per-kind id recorders
    //
    // The visited ids, bucketed by kind. The coverage-agreement property
    // asserts `for_each_id` and `try_map_ids` produce identical buckets.
    // ------------------------------------------------------------------

    #[derive(Default, PartialEq, Eq, Debug)]
    struct Visited {
        entities: Vec<MemoryEntityId>,
        events: Vec<MemoryEventId>,
        images: Vec<MemoryImageId>,
    }

    fn factual_for_each_order(a: &FactualA) -> Visited {
        let mut v = Visited::default();
        a.for_each_id(
            &mut |e: &MemoryEntityId| v.entities.push(*e),
            &mut |ev: &MemoryEventId| v.events.push(*ev),
            &mut |i: &MemoryImageId| v.images.push(*i),
        );
        v
    }

    fn factual_try_map_order(a: &FactualA) -> Result<Visited, MapErr> {
        let mut v = Visited::default();
        // Identity remap recording each visited id by kind. The closures never
        // reject, so the visit order is what the traversal threads.
        let _: FactualA = a.try_map_ids(
            &mut |e: &MemoryEntityId| {
                v.entities.push(*e);
                Ok(*e)
            },
            &mut |ev: &MemoryEventId| {
                v.events.push(*ev);
                Ok(*ev)
            },
            &mut |i: &MemoryImageId| {
                v.images.push(*i);
                Ok(*i)
            },
        )?;
        Ok(v)
    }

    fn judgment_for_each_order(a: &JudgmentA) -> Visited {
        let mut v = Visited::default();
        a.for_each_id(
            &mut |e: &MemoryEntityId| v.entities.push(*e),
            &mut |ev: &MemoryEventId| v.events.push(*ev),
            &mut |i: &MemoryImageId| v.images.push(*i),
        );
        v
    }

    fn judgment_try_map_order(a: &JudgmentA) -> Result<Visited, MapErr> {
        let mut v = Visited::default();
        let _: JudgmentA = a.try_map_ids(
            &mut |e: &MemoryEntityId| {
                v.entities.push(*e);
                Ok(*e)
            },
            &mut |ev: &MemoryEventId| {
                v.events.push(*ev);
                Ok(*ev)
            },
            &mut |i: &MemoryImageId| {
                v.images.push(*i);
                Ok(*i)
            },
        )?;
        Ok(v)
    }

    proptest! {
        /// Identity law (factual): an identity remap reproduces the input. With
        /// distinct-valued id positions, this fails on a dropped or reordered
        /// field, a same-kind swap, or a wrong reconstruction in any factual
        /// `try_map_ids` arm.
        #[test]
        fn factual_try_map_ids_identity_is_input(a in arb_factual_assertion()) {
            let out: FactualA = a.try_map_ids(
                &mut |e: &MemoryEntityId| Ok(*e),
                &mut |ev: &MemoryEventId| Ok(*ev),
                &mut |i: &MemoryImageId| Ok(*i),
            )?;
            prop_assert_eq!(out, a);
        }

        /// Coverage agreement (factual): `for_each_id` visits the same ids
        /// in the same per-kind order as `try_map_ids`. Fails if either
        /// skips, duplicates, reorders, or substitutes relative to the other.
        #[test]
        fn factual_for_each_matches_try_map(a in arb_factual_assertion()) {
            let visited = factual_for_each_order(&a);
            let mapped = factual_try_map_order(&a)?;
            prop_assert_eq!(visited, mapped);
        }

        /// Identity law (judgment) — see the factual counterpart.
        #[test]
        fn judgment_try_map_ids_identity_is_input(a in arb_judgment_assertion()) {
            let out: JudgmentA = a.try_map_ids(
                &mut |e: &MemoryEntityId| Ok(*e),
                &mut |ev: &MemoryEventId| Ok(*ev),
                &mut |i: &MemoryImageId| Ok(*i),
            )?;
            prop_assert_eq!(out, a);
        }

        /// Coverage agreement (judgment) — see the factual counterpart.
        #[test]
        fn judgment_for_each_matches_try_map(a in arb_judgment_assertion()) {
            let visited = judgment_for_each_order(&a);
            let mapped = judgment_try_map_order(&a)?;
            prop_assert_eq!(visited, mapped);
        }

        /// Serde round-trip law (factual): `from_str ∘ to_string == id`.
        ///
        /// Fails when serialize and deserialize disagree — the regression it
        /// guards is a deserialize expecting different field names or nesting
        /// than serialize emits (e.g. flat `{from, to}` against nested
        /// `pair: {from, to}`). Every generated assertion is valid, so the parse
        /// side accepts it and it must come back equal.
        #[test]
        fn factual_serde_round_trip_is_identity(a in arb_factual_assertion()) {
            let json = serde_json::to_string(&a)?;
            let back: FactualA = serde_json::from_str(&json)?;
            prop_assert_eq!(back, a);
        }

        /// Serde round-trip law (judgment) — see the factual counterpart.
        /// Exercises the identity cluster (flattened `OrderedDistinctPair`)
        /// and the observation cluster (nested `DistinctPair` plus a
        /// `TopologicalRel`), the two shapes whose deserialize had drifted.
        #[test]
        fn judgment_serde_round_trip_is_identity(a in arb_judgment_assertion()) {
            let json = serde_json::to_string(&a)?;
            let back: JudgmentA = serde_json::from_str(&json)?;
            prop_assert_eq!(back, a);
        }
    }
}
