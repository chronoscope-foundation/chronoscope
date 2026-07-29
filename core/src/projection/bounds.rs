//! Bounds propagation along the ordering chain
//! `construction ≤ witness ≤ demolition.completed`.
//!
//! Three constraints over one chain, computed once per class from the raw fact
//! bag and applied to each claim as the merge injects it: a completion is no
//! earlier than the evidence it has to outlast, nor than the construction it
//! follows, and a construction endpoint is no later than the removal it
//! precedes. They are arc consistency on a simple temporal network, hand-written
//! while there are three of them — a fourth constraint is a row in
//! [`Chain::constraints`] rather than a pass of its own.
//!
//! `demolition.started` sits outside every rule: a removal under way is
//! compatible with the entity still standing, so it denies nothing and nothing
//! bounds it. The chain's right-hand end is the completion.
//!
//! **Rivals are alternatives, so every reading is an envelope.** A slot's rival
//! claims relax to their join — the value any one of them allows — which is the
//! standard relaxation of a disjunctive temporal problem to a simple one.
//! Meeting them instead would have a propagation assert what no single source
//! supports, which is why the ceiling here is the *latest* completion claimed
//! where the deny channel reads the earliest.
//!
//! **A rule tightens an edge a claim already has, and never creates one.** An
//! open claim ("built after 1800") pins its moment nowhere, so it affirms no
//! instant at all; closing it would hand it a whole span to affirm on an
//! inference's authority, and the edge it gained is the one the claim's own
//! existence reading anchors from — which is to say a write outside the write
//! set below. Declining open claims is what makes that set exact, at the cost of
//! deriving nothing for them.
//!
//! **The reads and the writes are disjoint by edge.** The rules read
//! `{witness.lo, demolition.hi, construction.lo}` and write
//! `{construction.hi, demolition.lo}`, so an input taken from the raw bag equals
//! what the narrowed bag would give: one pass is the fixpoint, with no ordering
//! to get right and nothing to iterate. Every rule writes an *affirming* edge,
//! so a narrowing can never manufacture a denial — the deny channel, the
//! refutation yardstick and the presumption gate all read edges nothing here
//! moves.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use chrono::NaiveDate;

use crate::algebra::lattice::JoinSemilattice;
use crate::algebra::semiring::Semiring;
use crate::conflicts::date_for_role;
use crate::date::{Edge, UncertainDate};
use crate::grammar::assertions::FactualAssertion;
use crate::grammar::bookend::{ConstructionFact, DemolitionFact};
use crate::grammar::ids::{FactId, IdScheme};
use crate::submit::{DateRole, StoredFact};

use super::provenance::{DerivationRule, Stamp};

/// ∃-satisfiability of a derived date constraint against what a claim asserts:
/// `derived ⊓ asserted ≠ ⊥`. Some hypothesis of the claim still holds under
/// `derived`.
///
/// The satisfiable face is what an inference producer injects; the unsatisfiable
/// face is a relational conflict. Which reading the predicate is given decides
/// what happens to that face: against a slot's whole envelope it is the
/// contradiction [`temporal_conflicts`](crate::solvers::temporal_conflicts)
/// reports, and against one claim's own date it is the propagation declining to
/// move a claim it would empty. An emptied demolition names no instant at all —
/// it denies nothing and nothing can outlive it — so the contested era and the
/// refutation would go with it.
pub(crate) fn envelope_satisfies(derived: &UncertainDate, asserted: &UncertainDate) -> bool {
    derived.overlaps(asserted)
}

/// The envelope a set of rival claims spans — their join, the value any one of
/// them allows.
///
/// The one reading of "what this slot's rivals say" that the propagation and the
/// lifetime bounds both take, so the two cannot drift on where a demolition
/// ceiling or a construction floor sits.
pub(crate) fn envelope<'a>(claims: impl IntoIterator<Item = &'a UncertainDate>) -> UncertainDate {
    UncertainDate::join_all(claims.into_iter().cloned())
}

/// Which end of a claim's range a propagation reads or writes. [`Edge`] carries
/// the instant an end names and the precision it was claimed at, so `Side` is
/// only the direction: which end to take, and how a bound on it bears on a
/// claim.
#[derive(Clone, Copy)]
enum Side {
    /// The lower edge — the earliest instant the claim allows.
    Lower,
    /// The upper edge — the latest instant the claim allows.
    Upper,
}

impl Side {
    /// The edge a claim offers on this side, absent when it is open there.
    fn edge_of(self, date: &UncertainDate) -> Option<Edge> {
        match self {
            Side::Lower => date.earliest_bound().copied().map(Edge::start),
            Side::Upper => date.latest_bound().copied().map(Edge::end),
        }
    }

    /// Which of two edges this side ranks as its extreme. The edges
    /// [`tied_at`] folds all name one instant, where either selection falls to
    /// the finer precision — the sharpest claim that set the edge.
    fn extreme(self, a: Edge, b: Edge) -> Edge {
        match self {
            Side::Lower => a.earlier(b),
            Side::Upper => a.later(b),
        }
    }

    /// The one-sided date an edge on this side constrains a claim to. One open
    /// end can never invert a range, so the pair is always constructible.
    fn constraint(self, edge: Edge) -> Option<UncertainDate> {
        match self {
            Side::Lower => UncertainDate::bounded(Some(edge.bound()), None).ok(),
            Side::Upper => UncertainDate::bounded(None, Some(edge.bound())).ok(),
        }
    }

    /// Whether `edge` sits strictly inside the edge `date` already has on this
    /// side — whether the constraint tightens the claim.
    ///
    /// A claim open here has no edge to tighten, and a bound would be creating
    /// one rather than moving it. That is the one case where narrowing widens
    /// what the claim affirms, so it is not a narrowing at all.
    fn tightens(self, edge: Edge, date: &UncertainDate) -> bool {
        match self {
            Side::Lower => date.earliest().is_some_and(|lo| lo < edge.resolve()),
            Side::Upper => date.latest().is_some_and(|hi| hi > edge.resolve()),
        }
    }

    /// How `a` ranks against `b` as a constraint on this side: `Greater` when
    /// `a` binds the claim more tightly. Same-side constraints meet to their
    /// strongest, so this selects the one that sets the edge.
    fn binds(self, a: Edge, b: Edge) -> Ordering {
        match self {
            Side::Lower => a.resolve().cmp(&b.resolve()),
            Side::Upper => b.resolve().cmp(&a.resolve()),
        }
    }
}

/// One propagation input: the edge it bounds at, and the evidence tying it.
struct Binding<T> {
    edge: Edge,
    evidence: T,
}

/// The claims whose edge on `side` lands on `instant`, as one binding: the
/// finest precision among them, and their evidence joined.
///
/// Each tied claim sets the edge on its own, so they are alternatives rather
/// than a joint derivation — the tied-binder reading
/// [`inject_derived_bounds`](crate::solvers::inject_derived_bounds) also takes.
fn tied_at<T: Semiring + Clone>(
    claims: &[(UncertainDate, T)],
    side: Side,
    instant: NaiveDate,
) -> Option<Binding<T>> {
    let mut binding: Option<Binding<T>> = None;
    for (date, evidence) in claims {
        let Some(edge) = side.edge_of(date) else {
            continue;
        };
        if edge.resolve() != instant {
            continue;
        }
        binding = Some(match binding {
            None => Binding {
                edge,
                evidence: evidence.clone(),
            },
            Some(current) => Binding {
                edge: side.extreme(current.edge, edge),
                evidence: current.evidence.plus(evidence.clone()),
            },
        });
    }
    binding
}

/// The latest instant any witness certainly reaches — `demolition ≥ this`.
///
/// Sightings are conjunctive: every one of them holds, so the latest binds. One
/// anchoring no lower edge ("seen before Y") places its moment arbitrarily far
/// back and constrains nothing, so it drops out.
fn witness_reach<T: Semiring + Clone>(witnesses: &[(UncertainDate, T)]) -> Option<Binding<T>> {
    let instant = witnesses.iter().filter_map(|(d, _)| d.earliest()).max()?;
    tied_at(witnesses, Side::Lower, instant)
}

/// A rival set's envelope edge on `side`, with the claims that tie it.
///
/// A rival open on this side puts the envelope's edge at ±∞, which bounds
/// nothing, so the whole reading is absent — where a witness open on its side
/// only drops itself, since witnesses hold jointly and rivals do not.
fn envelope_edge<T: Semiring + Clone>(
    rivals: &[(UncertainDate, T)],
    side: Side,
) -> Option<Binding<T>> {
    let spanned = envelope(rivals.iter().map(|(date, _)| date));
    tied_at(rivals, side, side.edge_of(&spanned)?.resolve())
}

/// Where on the ordering chain a bookend claim sits — what the rest of the class
/// can move about it.
#[derive(Clone, Copy)]
enum ChainEnd {
    /// A construction endpoint, capped from above by the removal that follows it.
    ConstructionUpper,
    /// A demolition completion, floored from below by the evidence it has to
    /// outlast and by the construction it follows.
    DemolitionLower,
}

impl ChainEnd {
    /// The side of a claim this end's constraints bound.
    fn side(self) -> Side {
        match self {
            ChainEnd::ConstructionUpper => Side::Upper,
            ChainEnd::DemolitionLower => Side::Lower,
        }
    }
}

/// The class-level inputs the propagations read, computed once from the raw fact
/// bag.
pub(super) struct Chain<T> {
    /// `demolition ≥ this`: the latest instant any witness certainly reaches.
    witnessed: Option<Binding<T>>,
    /// `construction ≤ this`: the demolition envelope's upper edge — the latest
    /// instant any completion is claimed at.
    removed_by: Option<Binding<T>>,
    /// `demolition ≥ this`: the construction envelope's lower edge — the
    /// earliest instant any start could have fallen at.
    built_from: Option<Binding<T>>,
    /// Whether the class's bookends leave the entity any instant to have stood
    /// at — the negation of the born-after-dead ⊥.
    ///
    /// A total overrun is the honest inconsistency signal, rendered as an entity
    /// denied at every instant. Tightening there would shrink the contested band
    /// that says so: a record contradicting itself is not one to reconcile in
    /// the demolition's favour.
    consistent: bool,
}

/// The propagation inputs of one class, read off its raw facts.
///
/// Raw rather than projected, because the narrowing has to land before the merge
/// folds each claim into its slot — by then the per-assertion distinction rivals
/// turn on is gone. One O(F) scan per class projection.
pub(super) fn chain<R: IdScheme, T>(
    facts: &BTreeMap<FactId, StoredFact<R>>,
    reachers: &BTreeMap<R::Event, R::Entity>,
    provenance: &impl Fn(&FactId, &R::Entity, &StoredFact<R>) -> T,
) -> Chain<T>
where
    T: Semiring + Clone,
{
    let mut witnesses: Vec<(UncertainDate, T)> = Vec::new();
    let mut starts: Vec<(UncertainDate, T)> = Vec::new();
    let mut completions: Vec<(UncertainDate, T)> = Vec::new();

    for (fact_id, stored) in facts {
        let StoredFact::Factual(f) = stored else {
            continue;
        };
        match &f.assertion {
            FactualAssertion::Construction { fact } => {
                if let ConstructionFact::Started { bound, .. } = fact {
                    starts.push((bound.clone(), provenance(fact_id, fact.subject(), stored)));
                }
            }
            FactualAssertion::Demolition { fact } => {
                if let DemolitionFact::Completed { bound, .. } = fact {
                    completions.push((bound.clone(), provenance(fact_id, fact.subject(), stored)));
                }
            }
            FactualAssertion::Existence { fact } => {
                witnesses.push((fact.at.clone(), provenance(fact_id, fact.subject(), stored)));
            }
            // An interior event testifies for the one entity whose `HasEvent`
            // owns it, so an unowned event witnesses nothing — the same gate its
            // slot passes through in the merge.
            FactualAssertion::Event { fact } => {
                if let Some(owner) = reachers.get(fact.subject())
                    && let Some(date) =
                        date_for_role(stored, |role| matches!(role, DateRole::EventDate))
                {
                    witnesses.push((date, provenance(fact_id, owner, stored)));
                }
            }
            FactualAssertion::Attribute { .. }
            | FactualAssertion::Gap { .. }
            | FactualAssertion::Image { .. } => {}
        }
    }

    chain_of(&witnesses, &starts, &completions)
}

/// The chain a class's three claim sets induce, separated from the fact scan so
/// the propagation's laws can be stated over claim sets directly.
fn chain_of<T: Semiring + Clone>(
    witnesses: &[(UncertainDate, T)],
    starts: &[(UncertainDate, T)],
    completions: &[(UncertainDate, T)],
) -> Chain<T> {
    // The ⊥ condition reads the deny channel, not the envelopes: the latest
    // start any rival claims, against the earliest completion any rival claims.
    let deny_floor = starts.iter().filter_map(|(d, _)| d.earliest()).max();
    let deny_ceiling = completions.iter().filter_map(|(d, _)| d.latest()).min();

    Chain {
        witnessed: witness_reach(witnesses),
        removed_by: envelope_edge(completions, Side::Upper),
        built_from: envelope_edge(starts, Side::Lower),
        consistent: !matches!((deny_floor, deny_ceiling), (Some(f), Some(c)) if f > c),
    }
}

impl<T: Semiring + Clone + Stamp> Chain<T> {
    /// The constraints bearing on one end of the chain, each with the rule that
    /// carries it.
    fn constraints(&self, end: ChainEnd) -> Vec<(DerivationRule, &Binding<T>)> {
        let candidates = match end {
            ChainEnd::ConstructionUpper => vec![(
                DerivationRule::ConstructionBeforeDemolition,
                self.removed_by.as_ref(),
            )],
            ChainEnd::DemolitionLower => vec![
                (
                    DerivationRule::DemolitionAfterWitness,
                    self.witnessed.as_ref(),
                ),
                (
                    DerivationRule::DemolitionAfterConstruction,
                    self.built_from.as_ref(),
                ),
            ],
        };
        candidates
            .into_iter()
            .filter_map(|(rule, binding)| Some((rule, binding?)))
            .collect()
    }

    /// What `date` narrows to under the constraints bearing on `end`, and the
    /// support the narrowing earns — `None` where every constraint leaves the
    /// claim where it stands.
    ///
    /// Per assertion, never per slot: rivals are alternatives, so a slot-level
    /// rewrite would empty the rival a witness sits past instead of leaving the
    /// disagreement on the record.
    ///
    /// Every constraint on one end pushes the same edge the same way, so their
    /// meet is the strongest of them on its own. Selecting that one first is
    /// what makes the recorded rules the rules that *set* the edge — a reader
    /// phrases the bound from them — and keeps the selection independent of the
    /// order [`constraints`](Self::constraints) lists them in.
    fn tighten(&self, end: ChainEnd, date: &UncertainDate) -> Option<(UncertainDate, T)> {
        if !self.consistent {
            return None;
        }
        let side = end.side();
        let mut binding: Option<(Edge, T)> = None;
        for (rule, candidate) in self.constraints(end) {
            // Each rule is read against the claim as asserted, so which rules
            // fire never depends on the order they run in — and a rule moving
            // nothing records nothing, which is what lets a reader take any rule
            // atom as "this bound goes beyond what a source stated".
            if !side.tightens(candidate.edge, date) {
                continue;
            }
            let Some(constraint) = side.constraint(candidate.edge) else {
                continue;
            };
            // A claim the constraint contradicts stays where it stands: the two
            // cannot both hold, and emptying the claim would take its denial and
            // its refutability off the record along with the disagreement.
            if !envelope_satisfies(&constraint, date) {
                continue;
            }
            let earned = T::stamp(rule, candidate.evidence.clone());
            binding = Some(match binding {
                None => (candidate.edge, earned),
                Some((held, stamp)) => match side.binds(candidate.edge, held) {
                    Ordering::Greater => (candidate.edge, earned),
                    Ordering::Less => (held, stamp),
                    // Two rules landing on one instant each set the edge by
                    // themselves, so they are alternatives — the tied-binder
                    // reading [`tied_at`] takes over evidence.
                    Ordering::Equal => (side.extreme(held, candidate.edge), stamp.plus(earned)),
                },
            });
        }
        let (edge, stamp) = binding?;
        Some((date.meet(&side.constraint(edge)?), stamp))
    }
}

/// A construction claim as the chain leaves it: the fact with its date narrowed,
/// and the support the narrowing earns. `None` where nothing fired.
pub(super) fn narrow_construction<R: IdScheme, T>(
    chain: &Chain<T>,
    fact: &ConstructionFact<R>,
) -> Option<(ConstructionFact<R>, T)>
where
    T: Semiring + Clone + Stamp,
{
    let mut narrowed = fact.clone();
    let bound = match &mut narrowed {
        ConstructionFact::Started { bound, .. } | ConstructionFact::Completed { bound, .. } => {
            bound
        }
        // A location names no instant on the chain.
        ConstructionFact::Location { .. } => return None,
    };
    let (date, stamp) = chain.tighten(ChainEnd::ConstructionUpper, bound)?;
    *bound = date;
    Some((narrowed, stamp))
}

/// A demolition claim as the chain leaves it, mirroring [`narrow_construction`].
/// A started removal is claimed at no instant the chain can bound, so only a
/// completion narrows.
pub(super) fn narrow_demolition<R: IdScheme, T>(
    chain: &Chain<T>,
    fact: &DemolitionFact<R>,
) -> Option<(DemolitionFact<R>, T)>
where
    T: Semiring + Clone + Stamp,
{
    let mut narrowed = fact.clone();
    let bound = match &mut narrowed {
        DemolitionFact::Completed { bound, .. } => bound,
        DemolitionFact::Started { .. } => return None,
    };
    let (date, stamp) = chain.tighten(ChainEnd::DemolitionLower, bound)?;
    *bound = date;
    Some((narrowed, stamp))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use crate::algebra::semiring::{Label, Support as _};
    use crate::date::{DateBound, DatePrecision};
    use crate::lifespan::Lifespan;

    use super::super::provenance::Premise;
    use super::*;

    /// The provenance carrier the laws run over: the smallest thing that is a
    /// semiring and can name a rule, so a claim's evidence is one distinguishable
    /// atom and nothing else has to be built.
    type Lineage = Label<Premise<u8>>;

    // --- Generators ---
    //
    // Every law here turns on *relative* order and precision, so a claim set and
    // the instants it is read at come out of one narrow window, drawn once per
    // case — the discipline `lifespan.rs` established. Sampled independently over
    // three millennia a constraint's edge lands inside a claim's range about
    // never, and the propagation would run on an empty antecedent.
    //
    // Open rays are drawn nearly as often as closed spans, because an open claim
    // is the shape that decides whether a rule tightens an edge or invents one,
    // and it is the one a bag of closed ranges cannot reach.

    /// Half-width of the shared window, in years.
    const WINDOW: i32 = 8;

    /// Map a contiguous integer axis onto the years a [`DateBound`] accepts;
    /// there is no year 0, so the axis skips it rather than leaving a hole.
    fn bound_year(axis: i32) -> i32 {
        if axis >= 0 { axis + 1 } else { axis }
    }

    fn arb_base_axis() -> impl Strategy<Value = i32> {
        prop_oneof![
            2 => -WINDOW..=WINDOW,
            3 => -100i32..=100,
            1 => -2000i32..=2000,
        ]
    }

    /// Month and day, weighted onto the period edges every precision snaps to —
    /// the only days at which a claim's edge can *equal* a constraint's.
    fn arb_month_day() -> BoxedStrategy<(u32, u32)> {
        prop_oneof![
            6 => Just((1u32, 1u32)),
            6 => Just((12u32, 31u32)),
            5 => (1u32..=12, 1u32..=31),
        ]
        .boxed()
    }

    fn arb_precision() -> BoxedStrategy<DatePrecision> {
        prop_oneof![
            8 => Just(DatePrecision::Year),
            5 => Just(DatePrecision::Decade),
            3 => Just(DatePrecision::Month),
            3 => Just(DatePrecision::Day),
            2 => Just(DatePrecision::Century),
        ]
        .boxed()
    }

    fn arb_bound(base: i32) -> BoxedStrategy<DateBound> {
        (
            ((base - WINDOW)..=(base + WINDOW), arb_month_day())
                .prop_filter_map("valid calendar date", |(axis, (m, d))| {
                    NaiveDate::from_ymd_opt(bound_year(axis), m, d)
                }),
            arb_precision(),
        )
            .prop_filter_map("valid bound", |(day, precision)| {
                DateBound::new(day, precision).ok()
            })
            .boxed()
    }

    /// One claim's date. The closed shapes carry an edge on both sides; the rays
    /// carry one; `unknown` carries neither.
    fn arb_udate_in(base: i32) -> BoxedStrategy<UncertainDate> {
        prop_oneof![
            5 => arb_bound(base)
                .prop_filter_map("point", |b| UncertainDate::bounded(Some(b), Some(b)).ok()),
            4 => (arb_bound(base), arb_bound(base))
                .prop_filter_map("closed span", |(a, b)| {
                    let (lo, hi) = if a.period_start() <= b.period_start() { (a, b) } else { (b, a) };
                    UncertainDate::bounded(Some(lo), Some(hi)).ok()
                }),
            3 => arb_bound(base)
                .prop_filter_map("before", |b| UncertainDate::bounded(None, Some(b)).ok()),
            3 => arb_bound(base)
                .prop_filter_map("after", |b| UncertainDate::bounded(Some(b), None).ok()),
            1 => Just(UncertainDate::unknown()),
        ]
        .boxed()
    }

    fn arb_instant_in(base: i32) -> BoxedStrategy<NaiveDate> {
        prop_oneof![
            6 => ((base - WINDOW)..=(base + WINDOW), arb_month_day()),
            1 => ((base - WINDOW * 30)..=(base + WINDOW * 30), arb_month_day()),
        ]
        .prop_filter_map("valid calendar date", |(axis, (m, d))| {
            NaiveDate::from_ymd_opt(axis, m, d)
        })
        .boxed()
    }

    /// Which of the class's date slots a claim landed in — what decides whether
    /// the chain bounds it, and how its date reads as existence.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Slot {
        ConstructionStarted,
        ConstructionCompleted,
        DemolitionStarted,
        DemolitionCompleted,
        Witness,
    }

    impl Slot {
        /// Every slot, so a new one is a compile error here and enrolls in every
        /// law at once.
        const ALL: [Slot; 5] = [
            Slot::ConstructionStarted,
            Slot::ConstructionCompleted,
            Slot::DemolitionStarted,
            Slot::DemolitionCompleted,
            Slot::Witness,
        ];

        /// The end of the chain a claim in this slot sits on, when one bounds it.
        fn end(self) -> Option<ChainEnd> {
            match self {
                Slot::ConstructionStarted | Slot::ConstructionCompleted => {
                    Some(ChainEnd::ConstructionUpper)
                }
                Slot::DemolitionCompleted => Some(ChainEnd::DemolitionLower),
                Slot::DemolitionStarted | Slot::Witness => None,
            }
        }

        /// The existence claim this slot's date makes — the reading the laws
        /// about affirming and denying are stated against.
        fn fold_in(self, date: &UncertainDate) -> Lifespan {
            let date = date.clone();
            match self {
                Slot::ConstructionStarted => Lifespan::construction_started(date),
                Slot::ConstructionCompleted => Lifespan::construction_completed(date),
                Slot::DemolitionStarted => Lifespan::demolition_started(date),
                Slot::DemolitionCompleted => Lifespan::demolition_completed(date),
                Slot::Witness => Lifespan::witness(date),
            }
        }
    }

    /// One source's claim: where it landed, what it said, and an atom naming it.
    #[derive(Debug, Clone)]
    struct Claim {
        slot: Slot,
        date: UncertainDate,
        support: Lineage,
    }

    /// A distinct atom per claim, so a stamp names the evidence it read rather
    /// than collapsing into one indistinguishable mark.
    fn indexed(mut bag: Vec<Claim>) -> Vec<Claim> {
        for (i, claim) in bag.iter_mut().enumerate() {
            let index = u8::try_from(i).unwrap_or(u8::MAX);
            claim.support = Label::premise(Premise::Fact(index));
        }
        bag
    }

    fn arb_bag_in(base: i32) -> BoxedStrategy<Vec<Claim>> {
        let claim =
            (prop::sample::select(&Slot::ALL[..]), arb_udate_in(base)).prop_map(|(slot, date)| {
                Claim {
                    slot,
                    date,
                    support: Label::empty(),
                }
            });
        prop::collection::vec(claim, 0..=5)
            .prop_map(indexed)
            .boxed()
    }

    /// A claim set and a handful of instants to read it at.
    fn arb_bag_and_probes() -> impl Strategy<Value = (Vec<Claim>, Vec<NaiveDate>)> {
        arb_base_axis().prop_flat_map(|base| {
            (
                arb_bag_in(base),
                prop::collection::vec(arb_instant_in(base), 1..=3),
            )
        })
    }

    /// A claim set the record cannot square: a construction start whose earliest
    /// instant outlasts a completion's latest, with an arbitrary class around it.
    ///
    /// Built rather than filtered. Drawn independently the shape turns up in a
    /// small fraction of cases, which spends the reject budget before the law has
    /// been checked; the two forced claims come from windows far enough apart
    /// that they cross, and the rest of the bag is free.
    fn arb_contradictory_bag() -> impl Strategy<Value = Vec<Claim>> {
        arb_base_axis().prop_flat_map(|base| {
            (
                arb_bound(base),
                arb_bound(base + 3 * WINDOW),
                arb_bag_in(base),
            )
                .prop_filter_map(
                    "the start outlasts the completion",
                    |(cap, floor, mut bag)| {
                        if floor.period_start() <= cap.period_end() {
                            return None;
                        }
                        let point = |b| UncertainDate::bounded(Some(b), Some(b)).ok();
                        bag.push(Claim {
                            slot: Slot::DemolitionCompleted,
                            date: point(cap)?,
                            support: Label::empty(),
                        });
                        bag.push(Claim {
                            slot: Slot::ConstructionStarted,
                            date: point(floor)?,
                            support: Label::empty(),
                        });
                        Some(indexed(bag))
                    },
                )
        })
    }

    /// The chain a claim set induces, sorted into the three sets the pre-pass
    /// reads.
    fn chain_from(bag: &[Claim]) -> Chain<Lineage> {
        let of = |slot: Slot| -> Vec<(UncertainDate, Lineage)> {
            bag.iter()
                .filter(|claim| claim.slot == slot)
                .map(|claim| (claim.date.clone(), claim.support.clone()))
                .collect()
        };
        chain_of(
            &of(Slot::Witness),
            &of(Slot::ConstructionStarted),
            &of(Slot::DemolitionCompleted),
        )
    }

    fn narrow(chain: &Chain<Lineage>, claim: &Claim) -> Option<(UncertainDate, Lineage)> {
        chain.tighten(claim.slot.end()?, &claim.date)
    }

    /// Whether the class is born after it died — the model's ⊥ condition, read
    /// straight off the claim set: some start whose earliest instant outlasts
    /// some completion's latest.
    fn born_after_dead(bag: &[Claim]) -> bool {
        let starts = bag.iter().filter(|c| c.slot == Slot::ConstructionStarted);
        starts.clone().any(|start| {
            bag.iter()
                .filter(|c| c.slot == Slot::DemolitionCompleted)
                .any(|end| match (start.date.earliest(), end.date.latest()) {
                    (Some(floor), Some(cap)) => floor > cap,
                    _ => false,
                })
        })
    }

    /// The instants a claim's own edges make interesting, either side of each,
    /// plus the case's drawn probes. A law about an instant sitting exactly on a
    /// moved edge needs the collision to be routine rather than possible.
    fn probe_days(
        raw: &UncertainDate,
        narrowed: &UncertainDate,
        drawn: &[NaiveDate],
    ) -> Vec<NaiveDate> {
        let mut days: Vec<NaiveDate> = drawn.to_vec();
        for date in [raw, narrowed] {
            for edge in [date.earliest(), date.latest()].into_iter().flatten() {
                days.push(edge);
                days.extend(edge.pred_opt());
                days.extend(edge.succ_opt());
            }
        }
        days
    }

    proptest! {
        /// The law the whole unit rests on. A narrowing may only take
        /// possibilities away, so the claim's own existence reading can shrink
        /// and never grow.
        ///
        /// It is what an open claim breaks: "built after 1800" pins its moment
        /// nowhere and affirms nothing, and closing it hands it a century to
        /// affirm on an inference's authority.
        #[test]
        fn a_narrowing_affirms_no_instant_the_claim_did_not(
            (bag, drawn) in arb_bag_and_probes()
        ) {
            let chain = chain_from(&bag);
            for claim in &bag {
                let Some((narrowed, _)) = narrow(&chain, claim) else {
                    continue;
                };
                let raw = claim.slot.fold_in(&claim.date);
                let tightened = claim.slot.fold_in(&narrowed);
                for at in probe_days(&claim.date, &narrowed, &drawn) {
                    prop_assert!(
                        !tightened.affirms(at) || raw.affirms(at),
                        "{:?} narrowed to {:?} affirms {at}, which it did not",
                        claim,
                        narrowed,
                    );
                }
            }
        }

        /// Non-interference, made checkable rather than argued: every rule writes
        /// an affirming edge, so what a claim rules out is exactly what it ruled
        /// out before.
        #[test]
        fn a_narrowing_leaves_the_deny_channel_untouched(
            (bag, drawn) in arb_bag_and_probes()
        ) {
            let chain = chain_from(&bag);
            for claim in &bag {
                let Some((narrowed, _)) = narrow(&chain, claim) else {
                    continue;
                };
                let raw = claim.slot.fold_in(&claim.date);
                let tightened = claim.slot.fold_in(&narrowed);
                for at in probe_days(&claim.date, &narrowed, &drawn) {
                    prop_assert_eq!(
                        raw.denies(at),
                        tightened.denies(at),
                        "{:?} narrowed to {:?} changed what it denies at {}",
                        claim,
                        narrowed,
                        at,
                    );
                }
            }
        }

        /// One pass is the fixpoint — the reads-and-writes disjointness stated
        /// directly. Rebuilding the chain from the narrowed claims and running it
        /// again moves nothing, so there is no ordering to get right inside the
        /// pass and nothing to iterate.
        #[test]
        fn narrowing_an_already_narrowed_class_changes_nothing(bag in arb_bag_and_probes()) {
            let (bag, _) = bag;
            let chain = chain_from(&bag);
            let once: Vec<Claim> = bag
                .iter()
                .map(|claim| Claim {
                    date: narrow(&chain, claim)
                        .map_or_else(|| claim.date.clone(), |(date, _)| date),
                    ..claim.clone()
                })
                .collect();
            let again = chain_from(&once);
            for claim in &once {
                prop_assert!(
                    narrow(&again, claim).is_none(),
                    "a second pass moved {:?}",
                    claim,
                );
            }
        }

        /// A narrowing only removes possibilities: the narrowed claim meets the
        /// one the source made back to itself, so it admits nothing new.
        #[test]
        fn a_narrowing_sits_below_the_claim_it_narrowed(bag in arb_bag_and_probes()) {
            let (bag, _) = bag;
            let chain = chain_from(&bag);
            for claim in &bag {
                let Some((narrowed, _)) = narrow(&chain, claim) else {
                    continue;
                };
                prop_assert_eq!(
                    narrowed.meet(&claim.date),
                    narrowed.clone(),
                    "{:?} narrowed to {:?}, which admits something it did not",
                    claim,
                    narrowed,
                );
            }
        }

        /// The per-assertion guard, over arbitrary input. An emptied claim names
        /// no instant at all — it denies nothing and nothing can outlive it — so
        /// a propagation that empties one takes a disagreement off the record
        /// instead of narrowing it.
        #[test]
        fn a_narrowing_never_empties_a_claim(bag in arb_bag_and_probes()) {
            let (bag, _) = bag;
            let chain = chain_from(&bag);
            for claim in &bag {
                let Some((narrowed, _)) = narrow(&chain, claim) else {
                    continue;
                };
                prop_assert!(
                    !narrowed.is_bottom(),
                    "{:?} was emptied rather than narrowed",
                    claim,
                );
            }
        }

        /// Every rule a claim records bound at the claim's new edge. A reader
        /// phrases a derived bound from the rules it names and the instant the
        /// value carries, so a rule that fired against the raw claim while a
        /// stronger sibling set the edge would put the wrong evidence sentence on
        /// the bound.
        #[test]
        fn a_stamped_rule_bound_at_the_edge_the_claim_now_carries(bag in arb_bag_and_probes()) {
            let (bag, _) = bag;
            let chain = chain_from(&bag);
            for claim in &bag {
                let Some(end) = claim.slot.end() else {
                    continue;
                };
                let Some((narrowed, stamp)) = chain.tighten(end, &claim.date) else {
                    continue;
                };
                let side = end.side();
                let moved_to = side.edge_of(&narrowed).map(Edge::resolve);
                for (rule, candidate) in chain.constraints(end) {
                    let recorded = stamp
                        .atoms()
                        .any(|atom| matches!(atom, Premise::Rule(r) if *r == rule));
                    if recorded {
                        prop_assert_eq!(
                            Some(candidate.edge.resolve()),
                            moved_to,
                            "{:?} records {:?}, which bound elsewhere",
                            claim,
                            rule,
                        );
                    }
                }
            }
        }

        /// Stamping tracks movement, in both directions: a claim carries a rule
        /// marker exactly when its date moved. The forward half is what lets a
        /// reader treat any rule atom as "this bound goes beyond what a source
        /// stated"; the reverse says a declined claim is one no constraint could
        /// tighten without creating an edge or emptying it.
        #[test]
        fn a_claim_is_stamped_exactly_when_its_date_moved(bag in arb_bag_and_probes()) {
            let (bag, _) = bag;
            let chain = chain_from(&bag);
            for claim in &bag {
                let Some(end) = claim.slot.end() else {
                    continue;
                };
                let side = end.side();
                match chain.tighten(end, &claim.date) {
                    Some((narrowed, stamp)) => {
                        prop_assert_ne!(
                            &narrowed,
                            &claim.date,
                            "{:?} was stamped without moving",
                            claim,
                        );
                        prop_assert!(
                            stamp.atoms().any(|atom| matches!(atom, Premise::Rule(_))),
                            "{:?} moved without naming a rule",
                            claim,
                        );
                    }
                    None if born_after_dead(&bag) => {}
                    None => {
                        for (_, candidate) in chain.constraints(end) {
                            let Some(constraint) = side.constraint(candidate.edge) else {
                                continue;
                            };
                            // Instants, not values: a meet can re-tag an edge's
                            // precision while leaving the day it names alone,
                            // which is no movement to account for.
                            let edge_of = |date: &UncertainDate| side.edge_of(date).map(Edge::resolve);
                            let met = claim.date.meet(&constraint);
                            prop_assert!(
                                edge_of(&met) == edge_of(&claim.date)
                                    || met.is_bottom()
                                    || edge_of(&claim.date).is_none(),
                                "{:?} was declined though {:?} would have tightened it",
                                claim,
                                constraint,
                            );
                        }
                    }
                }
            }
        }

        /// A record contradicting itself is not one to reconcile in the
        /// demolition's favour: where the class is born after it died, every
        /// claim comes through exactly as its source made it, so the band that
        /// says so keeps its full width.
        #[test]
        fn a_contradictory_class_narrows_nothing(bag in arb_contradictory_bag()) {
            prop_assert!(born_after_dead(&bag), "the generator built a ⊥ class");
            let chain = chain_from(&bag);
            for claim in &bag {
                prop_assert!(
                    narrow(&chain, claim).is_none(),
                    "{:?} was narrowed inside a class denied at every instant",
                    claim,
                );
            }
        }
    }
}
