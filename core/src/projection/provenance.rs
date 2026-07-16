//! Provenance carried through the merge: the [`Label`] instance and the
//! citation atoms it accumulates.
//!
//! A projected value is a join of many facts. Provenance records *why* a bound
//! holds, and the two bounds of a [`Bracket`](super::Bracket) accumulate it by
//! different semiring operations: the extent (join, `+`) and the consensus
//! (meet, `·`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::algebra::semiring::Label;
use crate::grammar::citations::{FactualCitation, JudgmentSource};

/// A lattice value paired with the provenance of one derivation of it. The two
/// bounds of a [`Bracket`](super::Bracket) are each a `Cited` (consensus by `·`,
/// extent by `+`); a [`FactMap`](super::FactMap) entry is a `Cited` over its
/// key's membership support.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cited<V, T> {
    /// The lattice value (or value slot).
    pub value: V,
    /// The support behind this derivation of it.
    pub support: T,
}

/// Provenance of one projected value: the facts backing it.
///
/// Two arms per stored-fact category that can warrant a value — a factual claim
/// cites a [`FactualCitation`], a judgment a [`JudgmentSource`]. A meta fact
/// never backs a value, so it has no arm here.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(bound(deserialize = "ImgId: ::serde::de::DeserializeOwned"))]
pub enum Citation<ImgId> {
    /// A factual claim's citation.
    Factual { citation: FactualCitation },
    /// A judgment's source (e.g. why two ids are one entity).
    Judgment { source: JudgmentSource<ImgId> },
}

/// The member-aware lineage: a [`Label`] over citation atoms, each atom
/// retaining the source id (the minted entity the fact spoke to) beside its
/// citation.
///
/// Keeping the id makes load-bearing computable downstream — a field's
/// contributing ids fall out of the support set, so the read side can ask which
/// `SameEntity` judgments span them (see [`connecting_glue`](super::connecting_glue)).
pub type MemberLineage<EntId, ImgId> = Label<(EntId, Citation<ImgId>)>;
