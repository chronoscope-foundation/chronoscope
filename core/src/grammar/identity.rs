//! Identity cluster — same-thing equivalence judgments.
//!
//! Cluster module for the `Identity` variant of
//! [`crate::grammar::assertions::JudgmentAssertion`]: same-entity,
//! same-artifact, and same-event equivalence claims.
//!
//! `SameArtifact` is conceptual identity across image realizations of one
//! physical artifact — different scan resolutions, B&W vs colorized scans,
//! separate ingestions of the same photograph or map sheet. The bytes differ
//! (different image ids) but the artifact is the same. Byte equality is
//! hash-derivable and deduplicated before the fact layer, so these facts
//! cover only what a hash can't decide. The equivalence forms a class;
//! projection unions the metadata, so a low-resolution ingestion inherits the
//! LOC-quality scan's capture-date, role-claim, and so on.
//!
//! # Wire-boundary invariants
//!
//! Each variant carries an [`OrderedDistinctPair<Id>`] constructible only
//! through [`OrderedDistinctPair::new`], which sorts by `Ord` and rejects
//! `a == b`. `Deserialize` routes through the constructor (via
//! `#[serde(try_from)]`), so a JSON `{ "a": "z", "b": "a" }` lands as
//! `(a: "a", b: "z")` and a self-equivalence is rejected at parse time.
//!
//! # Error states (rejected at submit time)
//!
//! - **Self-equivalence.** Each variant requires `a != b`; an id paired with
//!   itself is malformed, not an uncertainty conflict. [`OrderedDistinctPair`]'s
//!   constructor catches it at the wire boundary.
//! - **Post-resolution self-equivalence.** Two declarations may resolve to the
//!   same persistent id after index substitution; substitution re-runs
//!   [`OrderedDistinctPair::new`], so the rejection is structural here too —
//!   see the per-kind `IdentityEntitySelfEquivalence` /
//!   `IdentityEventSelfEquivalence` / `IdentityArtifactSelfEquivalence`
//!   variants on [`crate::submit::SubmitError`].
//!
//! # Conflicts (surfaced at projection time)
//!
//! - **Transitive merge contradictions.** Classes form via union-find over
//!   the asserted pairs. When a class merges entities (or artifacts, or
//!   events) carrying contradictory attributes — different construction
//!   dates, different demolition locations — the solver surfaces the
//!   contradiction inside the class rather than rejecting the equivalence.
//!   Human review picks which attribute claim to retract.

// `Same*` names mark equivalence claims, not plain id references.

use chronoscope_macros::grammar_type;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::grammar::ids::SubjectKind;

// ============================================================================
// SelfPairError — shared self-pair rejection
// ============================================================================

/// Rejection from [`OrderedDistinctPair::new`] / [`DistinctPair::new`].
///
/// Carries the typed shared id (`a == b`). Generic so the constructor serves
/// sites without an `AsRef<str>` id (e.g. the submission-side index newtypes
/// like [`crate::submit::EntityIdx`]). The `Debug` rendering works for
/// any `Id: Debug`; which fact it was lives in the surrounding error type.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("self-pair on id {id:?}")]
pub struct SelfPairError<Id> {
    /// The shared id (the value that both sides of the pair equalled).
    pub id: Id,
}

// ============================================================================
// IdMapError / SelfLoop — id-traversal failure modes (grammar layer)
// ============================================================================

/// Which grammar node produced an [`IdMapError::SelfLoop`], carrying the typed
/// collided id.
///
/// The id-traversal ([`Fact::try_map_ids`] and the cluster analogues)
/// rebuilds distinct-pair payloads through their smart constructors. When two
/// distinct inputs map to one output id, the constructor rejects the pair and
/// the enclosing node turns the rejection into a kinded `SelfLoop` carrying
/// the collided output id. The variant records which node collapsed so the
/// submit layer can map it to the matching `SubmitError` variant.
///
/// Generic over the three output id kinds: `E` (entity), `V` (event), `I`
/// (image). `Relationship` / `Spatial` / `IdentityEntity` carry an entity id,
/// `IdentityEvent` an event id, `IdentityArtifact` an image id.
///
/// Lives in the grammar layer (next to the pair types whose constructors raise
/// [`SelfPairError`]), not `submit`: the traversal methods are inherent on
/// grammar types and `submit` depends on the grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfLoop<E, V, I> {
    /// An `attribute::Fact::Relationship` `from`/`to` pair collapsed.
    Relationship(E),
    /// An `observation::Fact::Spatial` `from`/`to` pair collapsed.
    Spatial(E),
    /// An `identity::Fact::SameEntity` pair collapsed.
    IdentityEntity(E),
    /// An `identity::Fact::SameEvent` pair collapsed.
    IdentityEvent(V),
    /// An `identity::Fact::SameArtifact` pair collapsed.
    IdentityArtifact(I),
}

/// Failure modes of the fallible id-traversal ([`Fact::try_map_ids`] and the
/// cluster analogues), raised during the type-changing relabel:
///
/// - [`Self::SelfLoop`] — a distinct-pair payload collapsed because two
///   distinct inputs mapped to the same output id. Carries the typed
///   [`SelfLoop`] (which node, plus the collided id).
/// - [`Self::LeafLookup`] — a leaf closure rejected a reference. The
///   substitute traversal uses this for a bundle-local-index-out-of-range
///   failure (the index is absent from the resolution map). The payload —
///   leaf kind, offending index, declaration count — lets the submit boundary
///   rebuild the matching `EntityIdxOutOfRange` / `EventIdxOutOfRange` /
///   `ImageIdxOutOfRange` verbatim.
///
/// Generic over the three output id kinds (`E` entity, `V` event, `I` image)
/// so the collided id stays typed to the submit boundary. One
/// `IdMapError<E2, V2, I2>` flows through a traversal that changes the id type
/// mid-walk; the params name the post-map types.
///
/// The `#[error]` messages are kind-only — they don't format the carried id —
/// so neither this nor [`SelfLoop`] needs a `Display` bound on the id kinds.
/// The traversal stays parametric over ids that may not be `Display` (e.g. raw
/// `u64` test ids); the id is carried for the lowering.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdMapError<E, V, I> {
    /// A distinct-pair payload collapsed to a self-loop: both sides
    /// resolved to one output id. The [`SelfLoop`] records which node
    /// collapsed and carries the collided id, typed.
    #[error("grammar node resolved to a self-loop during id relabel")]
    SelfLoop(SelfLoop<E, V, I>),
    /// A leaf closure rejected a reference because a bundle-local index
    /// pointed past the end of its declaration list. Carries enough for
    /// the submit boundary to rebuild the kind-specific
    /// index-out-of-range error.
    #[error("{kind} index {idx} out of range; {decl_count} declarations")]
    LeafLookup {
        /// Which subject id family the rejected reference belonged to.
        kind: SubjectKind,
        /// The offending index value (the bundle-local index that had no
        /// resolution).
        idx: usize,
        /// The number of declarations of that kind the producer supplied.
        decl_count: usize,
    },
}

/// Reject `a == b`, return the pair otherwise. Shared between
/// [`OrderedDistinctPair`] and [`DistinctPair`].
fn check_distinct<Id: PartialEq>(a: Id, b: Id) -> Result<(Id, Id), SelfPairError<Id>> {
    if a == b {
        return Err(SelfPairError { id: a });
    }
    Ok((a, b))
}

// ============================================================================
// OrderedDistinctPair — canonical-ordered, distinct pair
// ============================================================================

/// A pair `(a, b)` in canonical order (`a <= b` by `Ord`) and distinct
/// (`a != b`).
///
/// Constructible only via [`OrderedDistinctPair::new`]; private fields stop a
/// struct literal from bypassing canonicalisation or admitting a
/// self-equivalence. The identity-cluster variants embed this so every
/// equivalence fact holds the same invariant.
///
/// `Deserialize` routes through the constructor (via
/// [`serde(try_from)`](https://serde.rs/container-attrs.html#try_from) over
/// `OrderedPairMirror`), so a wire payload lands canonical-ordered and a
/// self-equivalence is rejected at parse time. The mirror copies the derived
/// `{a, b}` serialize shape, so the two directions can't drift.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(bound(serialize = "Id: Serialize"))]
#[serde(
    bound(deserialize = "Id: Deserialize<'de> + Ord + std::fmt::Debug"),
    try_from = "OrderedPairMirror<Id>"
)]
#[schemars(bound = "Id: JsonSchema")]
pub struct OrderedDistinctPair<Id: Ord> {
    /// The canonically-smaller side of the pair (`a <= b`).
    a: Id,
    /// The canonically-larger side of the pair.
    b: Id,
}

/// Deserialize mirror for [`OrderedDistinctPair`]: the derived `{a, b}`
/// serialize shape, no invariant. `TryFrom` re-imposes "ordered +
/// distinct" through [`OrderedDistinctPair::new`].
#[derive(Deserialize)]
#[serde(bound(deserialize = "Id: Deserialize<'de>"))]
#[serde(deny_unknown_fields)]
struct OrderedPairMirror<Id> {
    a: Id,
    b: Id,
}

impl<Id: Ord> TryFrom<OrderedPairMirror<Id>> for OrderedDistinctPair<Id> {
    type Error = SelfPairError<Id>;

    fn try_from(raw: OrderedPairMirror<Id>) -> Result<Self, Self::Error> {
        OrderedDistinctPair::new(raw.a, raw.b)
    }
}

impl<Id: Ord> OrderedDistinctPair<Id> {
    /// Construct an ordered, distinct pair: sorts `(a, b)` by `Ord` and
    /// rejects `a == b`.
    ///
    /// The bound is `Id: Ord` only — the error carries the typed id, so `Id`
    /// need not be string-shaped.
    pub fn new(a: Id, b: Id) -> Result<Self, SelfPairError<Id>> {
        let (a, b) = check_distinct(a, b)?;
        // `<` not `<=`: equality was just rejected, so the else branch is
        // reachable only for `a > b`.
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        Ok(Self { a: lo, b: hi })
    }

    /// Borrow the canonically-smaller side.
    pub fn a(&self) -> &Id {
        &self.a
    }

    /// Borrow the canonically-larger side.
    pub fn b(&self) -> &Id {
        &self.b
    }

    /// Map both ids through a fallible closure and rebuild through
    /// [`OrderedDistinctPair::new`], re-checking "ordered + distinct" on the
    /// mapped ids.
    ///
    /// If the two mapped ids collide, the rebuild's [`SelfPairError`] goes to
    /// `on_self_loop`, which the enclosing node supplies so the error carries
    /// the right [`SelfLoop`] variant. The pair only knows the collided id;
    /// the caller names the kind.
    ///
    /// Generic over `Err` so the pair serves callers threading a kind-specific
    /// error (the assertion layer's [`IdMapError`]) without naming it. `f` is
    /// `&mut` so a stateful closure threads through both calls and onward
    /// through the enclosing recursion.
    pub fn try_map_ids<Id2: Ord, Err>(
        &self,
        f: &mut impl FnMut(&Id) -> Result<Id2, Err>,
        on_self_loop: impl FnOnce(Id2) -> Err,
    ) -> Result<OrderedDistinctPair<Id2>, Err> {
        let a = f(&self.a)?;
        let b = f(&self.b)?;
        OrderedDistinctPair::new(a, b).map_err(|e| on_self_loop(e.id))
    }

    /// Visit both ids in canonical order.
    pub fn for_each_id(&self, f: &mut impl FnMut(&Id)) {
        f(&self.a);
        f(&self.b);
    }
}

// ============================================================================
// DistinctPair — directional, distinct pair
// ============================================================================

/// A pair `(from, to)` that preserves submission order (the directional
/// analogue of [`OrderedDistinctPair`]) and is distinct (`from != to`).
///
/// For directional facts where order carries meaning — `attribute::Fact::Relationship`
/// (`from Contains to` ≠ `to Contains from`) and `observation::Fact::Spatial`
/// (`PartOf` is directional). Constructible only via [`DistinctPair::new`];
/// private fields block a struct-literal self-pair.
///
/// `Deserialize` routes through the constructor (via
/// [`serde(try_from)`](https://serde.rs/container-attrs.html#try_from) over
/// `DistinctPairMirror`), so a wire `from == to` is rejected at parse time.
/// The mirror copies the derived `{from, to}` shape, so the two directions
/// can't drift.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(bound(serialize = "Id: Serialize"))]
#[serde(
    bound(deserialize = "Id: Deserialize<'de> + PartialEq + std::fmt::Debug"),
    try_from = "DistinctPairMirror<Id>"
)]
#[schemars(bound = "Id: JsonSchema")]
pub struct DistinctPair<Id> {
    /// The source side of the pair.
    from: Id,
    /// The target side of the pair.
    to: Id,
}

/// Deserialize mirror for [`DistinctPair`]: the derived `{from, to}`
/// serialize shape, no invariant. `TryFrom` re-imposes "distinct" through
/// [`DistinctPair::new`].
#[derive(Deserialize)]
#[serde(bound(deserialize = "Id: Deserialize<'de>"))]
#[serde(deny_unknown_fields)]
struct DistinctPairMirror<Id> {
    from: Id,
    to: Id,
}

impl<Id: PartialEq> TryFrom<DistinctPairMirror<Id>> for DistinctPair<Id> {
    type Error = SelfPairError<Id>;

    fn try_from(raw: DistinctPairMirror<Id>) -> Result<Self, Self::Error> {
        DistinctPair::new(raw.from, raw.to)
    }
}

impl<Id: PartialEq> DistinctPair<Id> {
    /// Construct a directional, distinct pair. Rejects `from == to`.
    pub fn new(from: Id, to: Id) -> Result<Self, SelfPairError<Id>> {
        let (from, to) = check_distinct(from, to)?;
        Ok(Self { from, to })
    }

    /// Map both ids through a fallible closure and rebuild through
    /// [`DistinctPair::new`], re-checking "distinct" on the mapped ids.
    /// Directional order is preserved (`from` then `to`) — no canonical
    /// re-sort, unlike [`OrderedDistinctPair::try_map_ids`].
    ///
    /// A collision hands the rebuild's [`SelfPairError`] to `on_self_loop`,
    /// which the enclosing node supplies so the error carries the right
    /// [`SelfLoop`] variant. Generic over `Err` like
    /// [`OrderedDistinctPair::try_map_ids`]; `f` is `&mut` so a stateful
    /// closure threads through both calls.
    pub fn try_map_ids<Id2: PartialEq, Err>(
        &self,
        f: &mut impl FnMut(&Id) -> Result<Id2, Err>,
        on_self_loop: impl FnOnce(Id2) -> Err,
    ) -> Result<DistinctPair<Id2>, Err> {
        let from = f(&self.from)?;
        let to = f(&self.to)?;
        DistinctPair::new(from, to).map_err(|e| on_self_loop(e.id))
    }
}

impl<Id> DistinctPair<Id> {
    /// Borrow the source side.
    pub fn from(&self) -> &Id {
        &self.from
    }

    /// Borrow the target side.
    pub fn to(&self) -> &Id {
        &self.to
    }

    /// Visit both ids in directional order (`from` then `to`).
    pub fn for_each_id(&self, f: &mut impl FnMut(&Id)) {
        f(&self.from);
        f(&self.to);
    }
}

// ============================================================================
// Fact — identity-cluster judgment fact
// ============================================================================

/// Identity-cluster fact.
///
/// Generic over the three reference kinds. Each variant wraps an
/// [`OrderedDistinctPair`], so there's no path to a self-equivalence or an
/// out-of-order pair.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(bound(
    serialize = "EntId: ::serde::Serialize + Ord, EvtId: ::serde::Serialize + Ord, ImgId: ::serde::Serialize + Ord",
    deserialize = "EntId: ::serde::Deserialize<'de> + Ord + std::fmt::Debug, \
                   EvtId: ::serde::Deserialize<'de> + Ord + std::fmt::Debug, \
                   ImgId: ::serde::Deserialize<'de> + Ord + std::fmt::Debug"
))]
#[schemars(
    bound = "EntId: ::schemars::JsonSchema + Ord, EvtId: ::schemars::JsonSchema + Ord, ImgId: ::schemars::JsonSchema + Ord"
)]
pub enum Fact<EntId, EvtId, ImgId>
where
    EntId: Ord,
    EvtId: Ord,
    ImgId: Ord,
{
    /// Two entity references describe the same entity.
    SameEntity {
        /// The two distinct entity references, in canonical order.
        pair: OrderedDistinctPair<EntId>,
    },
    /// Two images represent the same physical artifact — different scans,
    /// resolutions, or color treatments of one photograph, painting, or
    /// map sheet. Bytes differ; the artifact is the same. Metadata inherits
    /// across the class.
    SameArtifact {
        /// The two distinct image references, in canonical order.
        pair: OrderedDistinctPair<ImgId>,
    },
    /// Two lifetime-event references describe the same event.
    SameEvent {
        /// The two distinct event references, in canonical order.
        pair: OrderedDistinctPair<EvtId>,
    },
}

impl<EntId, EvtId, ImgId> Fact<EntId, EvtId, ImgId>
where
    EntId: Ord,
    EvtId: Ord,
    ImgId: Ord,
{
    /// Sorts `(a, b)` canonically by `Ord` and rejects `a == b`.
    pub fn same_entity(a: EntId, b: EntId) -> Result<Self, SelfPairError<EntId>> {
        Ok(Self::SameEntity {
            pair: OrderedDistinctPair::new(a, b)?,
        })
    }

    /// Sorts `(a, b)` canonically by `Ord` and rejects `a == b`.
    pub fn same_artifact(a: ImgId, b: ImgId) -> Result<Self, SelfPairError<ImgId>> {
        Ok(Self::SameArtifact {
            pair: OrderedDistinctPair::new(a, b)?,
        })
    }

    /// Sorts `(a, b)` canonically by `Ord` and rejects `a == b`.
    pub fn same_event(a: EvtId, b: EvtId) -> Result<Self, SelfPairError<EvtId>> {
        Ok(Self::SameEvent {
            pair: OrderedDistinctPair::new(a, b)?,
        })
    }

    /// Visit every id this fact mentions, dispatching to the closure for the
    /// id's kind.
    ///
    /// Three closures even though each variant touches one kind: the uniform
    /// `(fe, fv, fi)` shape lets one caller drive every cluster's traversal.
    pub fn for_each_id(
        &self,
        fe: &mut impl FnMut(&EntId),
        fv: &mut impl FnMut(&EvtId),
        fi: &mut impl FnMut(&ImgId),
    ) {
        match self {
            Self::SameEntity { pair } => pair.for_each_id(fe),
            Self::SameEvent { pair } => pair.for_each_id(fv),
            Self::SameArtifact { pair } => pair.for_each_id(fi),
        }
    }
}

impl<EntId, EvtId, ImgId> Fact<EntId, EvtId, ImgId>
where
    EntId: Ord,
    EvtId: Ord,
    ImgId: Ord,
{
    /// Relabel every id through the kind-matching fallible closure, rebuilding
    /// through the same constructors the wire boundary uses. Produces a
    /// `Fact<E2, V2, I2>`.
    ///
    /// Error is fixed to [`IdMapError<E2, V2, I2>`]; each arm builds its own
    /// [`SelfLoop`] wrapper — `SameEntity` → [`SelfLoop::IdentityEntity`],
    /// `SameEvent` → [`SelfLoop::IdentityEvent`], `SameArtifact` →
    /// [`SelfLoop::IdentityArtifact`] — carrying the typed output id. The
    /// submit layer maps that variant to its own `SubmitError`.
    ///
    /// Three closures, like [`Self::for_each_id`]. All three output params
    /// build the carried ids, so none is phantom.
    pub fn try_map_ids<E2, V2, I2>(
        &self,
        fe: &mut impl FnMut(&EntId) -> Result<E2, IdMapError<E2, V2, I2>>,
        fv: &mut impl FnMut(&EvtId) -> Result<V2, IdMapError<E2, V2, I2>>,
        fi: &mut impl FnMut(&ImgId) -> Result<I2, IdMapError<E2, V2, I2>>,
    ) -> Result<Fact<E2, V2, I2>, IdMapError<E2, V2, I2>>
    where
        E2: Ord,
        V2: Ord,
        I2: Ord,
    {
        match self {
            Self::SameEntity { pair } => Ok(Fact::SameEntity {
                pair: pair
                    .try_map_ids(fe, |id| IdMapError::SelfLoop(SelfLoop::IdentityEntity(id)))?,
            }),
            Self::SameEvent { pair } => Ok(Fact::SameEvent {
                pair: pair
                    .try_map_ids(fv, |id| IdMapError::SelfLoop(SelfLoop::IdentityEvent(id)))?,
            }),
            Self::SameArtifact { pair } => Ok(Fact::SameArtifact {
                pair: pair.try_map_ids(fi, |id| {
                    IdMapError::SelfLoop(SelfLoop::IdentityArtifact(id))
                })?,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::memory::{MemoryEntityId, MemoryEventId, MemoryImageId};

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type MemFact = Fact<MemoryEntityId, MemoryEventId, MemoryImageId>;

    #[test]
    fn same_entity_rejects_self_equivalence() -> TestResult {
        let a = MemoryEntityId(1);
        let b = MemoryEntityId(1);
        let result = MemFact::same_entity(a, b);
        assert!(matches!(result, Err(SelfPairError { .. })));
        Ok(())
    }

    #[test]
    fn same_artifact_rejects_self_equivalence() -> TestResult {
        let a = MemoryImageId(1);
        let b = MemoryImageId(1);
        let result = MemFact::same_artifact(a, b);
        assert!(matches!(result, Err(SelfPairError { .. })));
        Ok(())
    }

    #[test]
    fn same_event_rejects_self_equivalence() -> TestResult {
        let a = MemoryEventId(1);
        let b = MemoryEventId(1);
        let result = MemFact::same_event(a, b);
        assert!(matches!(result, Err(SelfPairError { .. })));
        Ok(())
    }

    #[test]
    fn same_entity_canonicalises_pair_ordering() -> TestResult {
        let big = MemoryEntityId(2);
        let small = MemoryEntityId(1);
        let f = MemFact::same_entity(big, small)?;
        let Fact::SameEntity { pair } = f else {
            return Err("expected SameEntity".into());
        };
        assert_eq!(*pair.a(), MemoryEntityId(1));
        assert_eq!(*pair.b(), MemoryEntityId(2));
        Ok(())
    }

    #[test]
    fn same_artifact_canonicalises_pair_ordering() -> TestResult {
        let big = MemoryImageId(2);
        let small = MemoryImageId(1);
        let f = MemFact::same_artifact(big, small)?;
        let Fact::SameArtifact { pair } = f else {
            return Err("expected SameArtifact".into());
        };
        assert_eq!(*pair.a(), MemoryImageId(1));
        assert_eq!(*pair.b(), MemoryImageId(2));
        Ok(())
    }

    #[test]
    fn deserialize_routes_through_smart_constructor_same_entity() -> TestResult {
        // Inputs reversed (a > b), to check the parse canonicalises them.
        let json = r#"{"type":"same_entity","pair":{"a":"2","b":"1"}}"#;
        let parsed: MemFact = serde_json::from_str(json)?;
        let Fact::SameEntity { pair } = parsed else {
            return Err("expected SameEntity".into());
        };
        assert_eq!(*pair.a(), MemoryEntityId(1));
        assert_eq!(*pair.b(), MemoryEntityId(2));
        Ok(())
    }

    // Three separate `#[test]`s rather than a parameterised helper: the shared
    // `Deserialize` dispatches on the `type` tag to one of three
    // variant-specific constructors, so each variant needs its own payload to
    // pin that its construction path hits the smart-constructor rejection. A
    // regression breaking one variant still surfaces.

    #[test]
    fn deserialize_rejects_self_equivalence_same_entity() {
        let json = r#"{"type":"same_entity","pair":{"a":"1","b":"1"}}"#;
        let result: Result<MemFact, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_rejects_self_equivalence_same_artifact() {
        let json = r#"{"type":"same_artifact","pair":{"a":"1","b":"1"}}"#;
        let result: Result<MemFact, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_rejects_self_equivalence_same_event() {
        let json = r#"{"type":"same_event","pair":{"a":"1","b":"1"}}"#;
        let result: Result<MemFact, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    // --- id-traversal: kinded self-loop on a rebuilt pair ---
    //
    // Each variant wraps an `OrderedDistinctPair`, and a relabel mapping two
    // distinct inputs onto one output id must surface as a correctly-kinded
    // `IdMapError::SelfLoop` carrying the typed collided id. Input ids are
    // `u64`, output ids `String` (Ord; no Display — the id is carried, not
    // rendered).

    type IdFact = Fact<u64, u64, u64>;

    #[test]
    fn try_map_ids_collapsing_same_entity_yields_kinded_self_loop() -> TestResult {
        // Distinct entity refs (1, 2) map to one output id. The rebuild rejects
        // the collision; the `SameEntity` arm wraps it as `IdentityEntity`
        // carrying the typed id.
        let fact: IdFact = Fact::same_entity(1, 2)?;
        let result: Result<Fact<String, String, String>, _> = fact.try_map_ids(
            &mut |_e: &u64| Ok("collapsed".to_owned()),
            &mut |v: &u64| Ok(format!("evt-{v}")),
            &mut |i: &u64| Ok(format!("img-{i}")),
        );
        assert!(matches!(
            result,
            Err(IdMapError::SelfLoop(SelfLoop::IdentityEntity(id))) if id == "collapsed"
        ));
        Ok(())
    }

    /// A collapsing `SameArtifact` must wrap as `IdentityArtifact`, not
    /// `IdentityEntity` — catches a copy-paste arm that reused the entity
    /// variant, and pins that the artifact arm dispatches to the image closure
    /// with the typed image-side id.
    #[test]
    fn try_map_ids_collapsing_same_artifact_yields_artifact_kind() -> TestResult {
        let fact: IdFact = Fact::same_artifact(10, 20)?;
        let result: Result<Fact<String, String, String>, _> = fact.try_map_ids(
            &mut |e: &u64| Ok(format!("ent-{e}")),
            &mut |v: &u64| Ok(format!("evt-{v}")),
            &mut |_i: &u64| Ok("collapsed-img".to_owned()),
        );
        assert!(matches!(
            result,
            Err(IdMapError::SelfLoop(SelfLoop::IdentityArtifact(id))) if id == "collapsed-img"
        ));
        Ok(())
    }

    #[test]
    fn try_map_ids_relabels_non_colliding_same_event() -> TestResult {
        // Distinct inputs to distinct outputs: the pair survives,
        // relabeled and re-canonicalised.
        let fact: IdFact = Fact::same_event(5, 9)?;
        let mapped: Fact<String, String, String> = fact.try_map_ids(
            &mut |e: &u64| Ok(format!("ent-{e}")),
            &mut |v: &u64| Ok(format!("evt-{v}")),
            &mut |i: &u64| Ok(format!("img-{i}")),
        )?;
        let Fact::SameEvent { pair } = mapped else {
            return Err("expected SameEvent".into());
        };
        assert_eq!(pair.a(), "evt-5");
        assert_eq!(pair.b(), "evt-9");
        Ok(())
    }

    #[test]
    fn for_each_id_visits_pair_through_kind_closure() -> TestResult {
        let fact: IdFact = Fact::same_entity(2, 1)?;
        let mut entities: Vec<u64> = Vec::new();
        let mut events: Vec<u64> = Vec::new();
        let mut images: Vec<u64> = Vec::new();
        fact.for_each_id(
            &mut |e: &u64| entities.push(*e),
            &mut |v: &u64| events.push(*v),
            &mut |i: &u64| images.push(*i),
        );
        // SameEntity visits only the entity closure, in canonical order (the
        // pair sorted 2,1 -> 1,2).
        assert_eq!(entities, vec![1, 2]);
        assert!(events.is_empty());
        assert!(images.is_empty());
        Ok(())
    }
}
