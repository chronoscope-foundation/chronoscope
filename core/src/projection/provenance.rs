//! Provenance carried through the merge: the [`Label`] instance and the
//! citation atoms it accumulates.
//!
//! A projected value is a join of many facts. Provenance records *why* a bound
//! holds, and the two bounds of a [`Bracket`](super::Bracket) accumulate it by
//! different semiring operations: the extent (join, `+`) and the consensus
//! (meet, `·`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::algebra::semiring::{Label, Semiring, Support};
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

/// An inference rule that can tighten a bound beyond what a source stated.
///
/// One variant per rule, so a reader can always name *why* a value is narrower
/// than any single claim. A rule rides the support as a [`Premise::Rule`] atom
/// sharing its environment with the evidence it consumed.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DerivationRule {
    /// `construction ≤ W`, because the entity was witnessed existing at `W`.
    ExistenceWitness,
}

/// One reason a value holds: evidence a source supplied, or an inference step
/// that participated in producing it.
///
/// A rule shares its environment with the facts it consumed, so "which rule used
/// which evidence" is structural rather than a parallel field that can drift.
///
/// Erase for truth, read for provenance. A rule atom is an always-true
/// indeterminate, not a retractable assumption: it can never be the retraction
/// that repairs a contradiction. So anything reasoning about *what could be
/// false* — nogoods, minimal fighting sets, which fact to blame — filters to
/// [`Premise::Fact`] first, and only the provenance read looks at the rules.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Premise<F> {
    /// A source's own claim.
    Fact(F),
    /// An inference step that participated.
    Rule(DerivationRule),
}

impl<F> Premise<F> {
    /// The evidence this premise carries, when it is evidence — the erasure a
    /// truth-facing pass applies before reasoning about what could be false.
    pub fn fact(&self) -> Option<&F> {
        match self {
            Premise::Fact(f) => Some(f),
            Premise::Rule(_) => None,
        }
    }
}

/// The support an inference emits: its rule joined with the evidence it
/// consumed, in one environment.
///
/// A rule stamped onto evidence carrying no atoms yields the semiring zero, so a
/// marker backed by nothing is unrepresentable here rather than left to producer
/// discipline. `times` alone closes only half of that: `zero` annihilates, but
/// `one()` (the `{∅}` tautology) is its identity and would pass a lone rule atom
/// through, so the atom-free evidence check covers both.
///
/// Stamp only when the rule actually created or changed the claim. A rule that
/// fires and narrows nothing records nothing, which is what lets a reader treat
/// "any rule atom present" as "this bound goes beyond what a source stated".
pub fn derived<F: Ord + Clone>(
    rule: DerivationRule,
    evidence: Label<Premise<F>>,
) -> Label<Premise<F>> {
    if evidence.atoms().next().is_none() {
        return Label::empty();
    }
    Label::premise(Premise::Rule(rule)).times(evidence)
}

/// The member-aware lineage: a [`Label`] over citation atoms, each atom
/// retaining the source id (the minted entity the fact spoke to) beside its
/// citation.
///
/// Keeping the id makes load-bearing computable downstream — a field's
/// contributing ids fall out of the support set, so the read side can ask which
/// `SameEntity` judgments span them (see [`connecting_glue`](super::connecting_glue)).
pub type MemberLineage<EntId, ImgId> = Label<Premise<(EntId, Citation<ImgId>)>>;

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeSet;

    /// Both atom-free evidences yield nothing: the zero label (no derivation) and
    /// the `one()` tautology (`{∅}`, which `times` would pass straight through).
    /// A marker citing no fact is unrepresentable, whichever way a producer
    /// arrives at empty evidence.
    #[test]
    fn a_rule_with_no_evidence_stamps_nothing() {
        let on_zero: Label<Premise<u8>> = derived(DerivationRule::ExistenceWitness, Label::empty());
        assert_eq!(
            on_zero,
            Label::empty(),
            "stamping a rule onto the zero label yields nothing"
        );

        let on_one: Label<Premise<u8>> = derived(DerivationRule::ExistenceWitness, Label::one());
        assert_eq!(
            on_one,
            Label::empty(),
            "stamping a rule onto the tautology yields nothing, not a fact-free marker"
        );
    }

    /// The stamp joins the rule to the evidence in one environment, so both ride
    /// the support the flatten reads.
    #[test]
    fn a_stamped_rule_rides_alongside_its_evidence() {
        let stamped = derived(
            DerivationRule::ExistenceWitness,
            Label::premise(Premise::Fact(7u8)),
        );
        let atoms: BTreeSet<&Premise<u8>> = stamped.atoms().collect();
        assert_eq!(
            atoms,
            BTreeSet::from([
                &Premise::Fact(7u8),
                &Premise::Rule(DerivationRule::ExistenceWitness),
            ]),
            "the support names the evidence and the rule that used it"
        );
    }
}
