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
    /// `demolition ≥ W`, because the entity was witnessed existing at `W`.
    DemolitionAfterWitness,
    /// `construction ≤ D`, because a thing is built before it is removed.
    ConstructionBeforeDemolition,
    /// `demolition ≥ C`, because a thing is removed after it is built.
    DemolitionAfterConstruction,
}

/// One reason a value holds: a source's claim on the value itself, an inference
/// step that participated in producing it, or a fact that step consumed.
///
/// A rule shares its environment with the facts it consumed, so "which rule used
/// which evidence" is structural rather than a parallel field that can drift.
/// [`Consumed`](Premise::Consumed) is what keeps that sharing readable: a rule
/// bounding a construction against a removal leaves the removal's fact in the
/// construction slot's support, and the atom says in which capacity it is there.
/// Every reader that asks "what did a source claim *here*" — the fighting-rival
/// reconstruction, the slot's citations and fact ids, the date a bookend fact
/// contributes — reads [`Fact`](Premise::Fact), and the provenance read reaches
/// for the rest.
///
/// Erase for truth, read for provenance. A rule atom is an always-true
/// indeterminate, not a retractable assumption: it can never be the retraction
/// that repairs a contradiction. So anything reasoning about *what could be
/// false* — nogoods, minimal fighting sets, which fact to blame — filters to
/// [`Premise::Fact`] first, and only the provenance read looks at the rules.
/// Retracting a consumed fact loosens the bound back to what the source stated
/// rather than withdrawing a claim, so it too sits outside that filter.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Premise<F> {
    /// A source's own claim on the value this supports.
    Fact(F),
    /// An inference step that participated.
    Rule(DerivationRule),
    /// A fact an inference step consumed, riding the value its rule moved.
    Consumed {
        /// The rule that read it.
        rule: DerivationRule,
        /// The source's claim, about some other value.
        fact: F,
    },
}

impl<F> Premise<F> {
    /// The claim this premise makes on the value it supports, when it makes one
    /// — the erasure a truth-facing pass applies before reasoning about what
    /// could be false.
    pub fn fact(&self) -> Option<&F> {
        match self {
            Premise::Fact(f) => Some(f),
            Premise::Rule(_) | Premise::Consumed { .. } => None,
        }
    }

    /// The evidence this premise carries and the rule that read it, when it is a
    /// consumed fact.
    pub fn consumed(&self) -> Option<(DerivationRule, &F)> {
        match self {
            Premise::Consumed { rule, fact } => Some((*rule, fact)),
            Premise::Fact(_) | Premise::Rule(_) => None,
        }
    }
}

/// The support an inference emits: its rule joined with the evidence it
/// consumed, in one environment.
///
/// The evidence is retagged [`Premise::Consumed`] on the way in. It rides the
/// value the rule moved, which may be a slot a source also claimed into, and the
/// tag is what tells the two apart there — so a removal that capped a
/// construction cannot be read back as a rival construction date.
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
    let consumed = evidence.map_atoms(|premise| match premise {
        Premise::Fact(fact) => Premise::Consumed { rule, fact },
        // A rule reading another rule's output keeps the inner attribution: the
        // atom already says which step first read the source.
        already @ (Premise::Rule(_) | Premise::Consumed { .. }) => already,
    });
    Label::premise(Premise::Rule(rule)).times(consumed)
}

/// Emitting a rule's stamp from a fold that is generic over its provenance.
///
/// The merge folds one entity over any `T: Semiring`, and the bounds propagation
/// runs inside it, so a rule marker has to be reachable through the carrier
/// rather than through a concrete label type.
///
/// Non-defaulting by design: a semiring that cannot name a rule says so by not
/// implementing this. A default returning `one()` would let a producer stamp
/// into a carrier that silently drops the marker.
pub trait Stamp: Semiring {
    /// This carrier's [`derived`] — the rule joined with the evidence it used.
    fn stamp(rule: DerivationRule, evidence: Self) -> Self;
}

impl<F: Ord + Clone> Stamp for Label<Premise<F>> {
    fn stamp(rule: DerivationRule, evidence: Self) -> Self {
        derived(rule, evidence)
    }
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
    /// the support the flatten reads — the evidence tagged as consumed, which is
    /// what stops a slot's own reader from taking it for a claim made here.
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
                &Premise::Consumed {
                    rule: DerivationRule::ExistenceWitness,
                    fact: 7u8
                },
                &Premise::Rule(DerivationRule::ExistenceWitness),
            ]),
            "the support names the evidence and the rule that used it"
        );
        assert!(
            stamped.atoms().all(|atom| atom.fact().is_none()),
            "no atom here claims anything about the value the rule moved"
        );
    }
}
