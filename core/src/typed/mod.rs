//! Consumer-facing typed projection.
//!
//! Flattens the lattice- and provenance-rich `projection::Entity` into an
//! `Entity` DTO for read-side consumers: each restrictive field becomes a
//! `Bounded` (the bracket read off as a settled value, a conflict, a pending
//! verdict, or absent), each membership becomes an attributed value, and the
//! interior events parse into a typed timeline.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::algebra::lattice::JoinSemilattice;
use crate::algebra::semiring::Support;
use crate::conflicts::{FactAtom, fact_date, minimal_fighting_sets};
use crate::date::UncertainDate;
use crate::grammar::depiction::Perspective;
use crate::grammar::geometry::ImageGeometry;
use crate::grammar::identity::OrderedDistinctPair;
use crate::grammar::ids::{FactId, IdScheme};
use crate::location::ConflictStatus;
use crate::nonempty::NonEmptyVec;
use crate::projection::Claimed;
use crate::store::schema::EquivClass;

use crate::projection::{Bracket, Citation, Cited, ConsensusConflict, DepictionRecord, Sameness};

mod entity;
mod image;
mod timeline;

pub use entity::*;
pub use image::*;
pub use timeline::*;

/// A value with the citations that attribute it — the additive-field mirror,
/// where membership carries no consensus/extent split.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(bound(deserialize = "V: ::serde::Deserialize<'de>, ImgId: ::serde::de::DeserializeOwned"))]
pub struct Attributed<V, ImgId> {
    pub value: V,
    pub sources: Vec<Citation<ImgId>>,
}

/// Why a value is present when no source directly asserted it. `None` = asserted;
/// `Some(kind)` = derived by that rule. One variant per derivation rule; the
/// witnesses ride in the slot's own `sources`/`facts`/value, so variants stay
/// lean and don't duplicate them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Derivation {
    /// `construction ≤ W`, because the entity was witnessed existing at `W` — a
    /// "built by" bound the solver injected from existence witnesses.
    ExistenceWitness,
}

/// The `T`-flattened mirror of the projection's [`Bracket`], for any lattice
/// `V`: the extent (`possible`), the citations behind it, and the consensus read
/// off the bracket's conflict tri-state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(bound(
    deserialize = "V: ::serde::de::DeserializeOwned, ImgId: ::serde::de::DeserializeOwned"
))]
pub struct Bounded<V, ImgId> {
    /// The extent (join) — what any source allows, in the field's own lattice.
    pub possible: V,
    pub sources: Vec<Citation<ImgId>>,
    /// The fact ids backing the extent — the provenance-by-id complement to
    /// `sources`. A consumer correlates a slot across fields by shared id: a
    /// timeline row matches an entity-level
    /// [`TemporalConflict`](crate::solvers::TemporalConflict) when a fact id here
    /// rides its `facts`. Empty when the support carries no fact ids (a
    /// member-lineage read).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<FactId>,
    pub consensus: Consensus<V, ImgId>,
    /// Set when the value was derived rather than asserted, naming the rule that
    /// derived it. The value, citations, and facts above are the derivation's
    /// witnesses; a reader shows the bound inline and tags it inferred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivation: Option<Derivation>,
}

impl<X: Ord, ImgId> Bounded<Claimed<X>, ImgId> {
    /// The lone settled value: `Some(x)` when consensus reached a
    /// `Claimed::Of` holding exactly one value `x`. Absent, conflicting,
    /// pending, `Any`, an empty set, and a multi-value set each yield `None`.
    pub fn settled(&self) -> Option<&X> {
        let Consensus::Reached {
            value: Claimed::Of { values },
        } = &self.consensus
        else {
            return None;
        };
        let mut it = values.iter();
        let first = it.next()?;
        match it.next() {
            Some(_) => None,
            None => Some(first),
        }
    }
}

/// The consensus side of a flattened bracket: whether a claim settled the slot,
/// over-determined it, was declined this layer, or never touched it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
#[serde(bound(
    deserialize = "V: ::serde::de::DeserializeOwned, ImgId: ::serde::de::DeserializeOwned"
))]
pub enum Consensus<V, ImgId> {
    /// No claim touched this slot.
    Absent,
    /// The meet settled to a value all sources agree on.
    Reached { value: V },
    /// The meet bottomed out; the rival extent lives in [`Bounded::possible`].
    /// `fighting` holds the minimal sets of facts that can't jointly hold, each
    /// rival carrying its value and the citations behind it.
    Conflict {
        fighting: Vec<NonEmptyVec<Attributed<V, ImgId>>>,
    },
    /// This layer declined to decide (circle cap / unresolved reference).
    Pending { reason: PendingReason },
}

/// The distinct rivals across a conflict's minimal fighting sets, in first-
/// appearance order — one entry per fact, so a fact shared by several minimal
/// sets surfaces once. The flat view a single-value display wants.
pub fn distinct_rivals<V: PartialEq, ImgId: PartialEq>(
    fighting: &[NonEmptyVec<Attributed<V, ImgId>>],
) -> Vec<&Attributed<V, ImgId>> {
    let mut out: Vec<&Attributed<V, ImgId>> = Vec::new();
    for rival in fighting.iter().flat_map(|set| set.iter()) {
        if !out.contains(&rival) {
            out.push(rival);
        }
    }
    out
}

/// Why a consensus is [`Pending`](Consensus::Pending). One variant today — the
/// location circle cap; the carrier is uniform across fields so a future
/// undecidable date or discrete bound surfaces the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PendingReason {
    /// The merged region is too complex to run the emptiness check, or an
    /// unresolved reference leaves the verdict open.
    Unresolved,
}

/// One `SameEntity` bridge: the ordered, distinct id pair a judgment unified and
/// the citations behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(bound(
    deserialize = "EntId: ::serde::Deserialize<'de> + Ord + std::fmt::Debug, ImgId: ::serde::de::DeserializeOwned"
))]
pub struct MergeBridge<EntId: Ord, ImgId> {
    pub endpoints: OrderedDistinctPair<EntId>,
    pub judgment: Vec<Citation<ImgId>>,
}

/// How an entity's class was assembled: the mention count (always ≥1, the class
/// includes its own subject) and the bridges that merged its mentions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(bound(
    deserialize = "EntId: ::serde::Deserialize<'de> + Ord + std::fmt::Debug, ImgId: ::serde::de::DeserializeOwned"
))]
pub struct MergeProvenance<EntId: Ord, ImgId> {
    pub mention_count: NonZeroUsize,
    pub bridges: Vec<MergeBridge<EntId, ImgId>>,
}

/// One entity↔image depiction: its far end, where the entity sits in the image,
/// the view classification when a source supplies one, and the citations behind
/// the link.
///
/// The one type serves both reading directions: [`Image::depicts`] keys it by
/// entity, and the entity's image sub-resource
/// ([`project_entity_images`](crate::projection::project_entity_images)) keys it
/// by image. A bare depiction (no localization, no perspective) flattens both
/// bracket fields to `Absent`; `sources` keeps the link's attribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(bound(
    deserialize = "OtherId: ::serde::Deserialize<'de>, ImgId: ::serde::de::DeserializeOwned"
))]
pub struct Depiction<OtherId, ImgId> {
    /// The depiction's far end — the depicted entity in [`Image::depicts`], the
    /// depicting image in the entity's image sub-resource; the one generic type
    /// serves both directions.
    pub other: OtherId,
    pub localization: Bounded<Claimed<ImageGeometry>, ImgId>,
    pub perspective: Bounded<Claimed<Perspective>, ImgId>,
    pub sources: Vec<Citation<ImgId>>,
}

// ----------------------------------------------------------------------------
// Flatteners
// ----------------------------------------------------------------------------

/// A provenance-support atom, yielding its citation when it warrants one. A
/// member-lineage atom carries its citation beside the member id; a fact atom
/// derives it from the stored fact. `None` for an atom that backs no value (a
/// meta fact).
///
/// Public because [`Entity::parse`] is generic over it — a caller flattens
/// either support without naming the atom.
pub trait SupportAtom {
    /// The image id the atom's citation references.
    type Img;
    fn citation(&self) -> Option<Citation<Self::Img>>;

    /// The fact id this atom names, when it carries one. Absent for an atom that
    /// rode only a member id.
    fn fact_id(&self) -> Option<FactId> {
        None
    }

    /// The (fact id, date) this atom contributes to a date slot — the premise a
    /// date conflict is minimized over. Absent for atoms that carry no fact.
    fn date_premise(&self) -> Option<(FactId, UncertainDate)> {
        None
    }

    /// Whether this atom directly asserts a construction start. A present
    /// construction-start slot whose support holds no such atom carries a derived
    /// "built by" bound; this is how the flatten reads asserted-vs-derived off
    /// the support. `false` for atoms that back no construction-start fact.
    fn asserts_construction_start(&self) -> bool {
        false
    }
}

impl<EntId, ImgId: Clone> SupportAtom for (EntId, Citation<ImgId>) {
    type Img = ImgId;
    fn citation(&self) -> Option<Citation<ImgId>> {
        Some(self.1.clone())
    }
}

impl<R: IdScheme> SupportAtom for FactAtom<R> {
    type Img = R::Image;
    fn citation(&self) -> Option<Citation<R::Image>> {
        crate::projection::citation_of(&self.fact)
    }
    fn fact_id(&self) -> Option<FactId> {
        Some(self.id)
    }
    fn date_premise(&self) -> Option<(FactId, UncertainDate)> {
        Some((self.id, fact_date(&self.fact)?))
    }
    fn asserts_construction_start(&self) -> bool {
        crate::conflicts::is_construction_start(&self.fact)
    }
}

/// The citations a support's atoms attribute, deduped. Each atom's own identity
/// (a member id, a fact id) rode along to make the projection computable; the
/// typed surface keeps only the citations.
pub(super) fn sources<S, X>(support: &S) -> Vec<Citation<X::Img>>
where
    S: Support<Atom = X>,
    X: SupportAtom,
    X::Img: Ord,
{
    support
        .atoms()
        .filter_map(SupportAtom::citation)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The fact ids a support's atoms name, deduped and ordered — the
/// provenance-by-id mirror of [`sources`]. Empty for a member-lineage support.
pub(super) fn fact_ids<S, X>(support: &S) -> Vec<FactId>
where
    S: Support<Atom = X>,
    X: SupportAtom,
{
    support
        .atoms()
        .filter_map(SupportAtom::fact_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Flatten a restrictive field. `Absent` when the extent support is the semiring
/// zero (nothing contributed via `plus`); else `possible`/`sources` come from
/// the extent and the consensus reads off `consensus.value.conflict()`.
pub(super) fn bracket<V, S, X>(b: &Bracket<V, S>) -> Bounded<V, X::Img>
where
    V: JoinSemilattice + ConsensusConflict + Clone + PartialEq,
    S: Support<Atom = X>,
    X: SupportAtom,
    X::Img: Ord,
{
    if !touched(b) {
        return Bounded {
            possible: V::bottom(),
            sources: Vec::new(),
            facts: Vec::new(),
            consensus: Consensus::Absent,
            derivation: None,
        };
    }

    let possible = b.extent.value.clone();
    let consensus = match b.consensus.value.conflict() {
        ConflictStatus::Consistent => Consensus::Reached {
            value: b.consensus.value.clone(),
        },
        ConflictStatus::Conflict => Consensus::Conflict {
            fighting: Vec::new(),
        },
        ConflictStatus::Pending => Consensus::Pending {
            reason: PendingReason::Unresolved,
        },
    };

    // Producer-only invariants the bracket types leave loose: a present field
    // cites at least one fact, and a settled consensus sits below the extent.
    let srcs = sources(&b.extent.support);
    debug_assert!(
        !srcs.is_empty(),
        "a present field's extent support must cite a fact"
    );
    if let Consensus::Reached { value } = &consensus {
        debug_assert!(
            value.clone().join(possible.clone()) == possible,
            "the reached consensus must sit below the extent"
        );
    }

    Bounded {
        possible,
        sources: srcs,
        facts: fact_ids(&b.extent.support),
        consensus,
        derivation: None,
    }
}

/// Flatten a date slot, filling a conflict's fighting rivals. Flattens via
/// [`bracket`], then for an over-determined slot reads the fighting facts off the
/// consensus support: each fact's date premise and citations, minimized into the
/// sets whose dates can't jointly hold.
pub(super) fn dated_bracket<S, X>(b: &Bracket<UncertainDate, S>) -> Bounded<UncertainDate, X::Img>
where
    S: Support<Atom = X>,
    X: SupportAtom,
    X::Img: Ord + Clone,
{
    let mut bounded = bracket(b);
    if matches!(bounded.consensus, Consensus::Conflict { .. }) {
        bounded.consensus = Consensus::Conflict {
            fighting: fighting_sets(&b.consensus.support),
        };
    }
    bounded
}

/// One fighting fact's contribution to a date slot: its claimed date and the
/// citations attributing it.
type DatePremise<ImgId> = (UncertainDate, Vec<Citation<ImgId>>);

/// The minimal fighting sets of a date slot's consensus support: group each
/// fact's date premise with the citations attributing it, minimize the premises
/// into the sets whose joint meet is empty, and re-attribute each set's facts as
/// their dates plus citations.
fn fighting_sets<S, X>(support: &S) -> Vec<NonEmptyVec<Attributed<UncertainDate, X::Img>>>
where
    S: Support<Atom = X>,
    X: SupportAtom,
    X::Img: Ord + Clone,
{
    let mut by_fact: BTreeMap<FactId, DatePremise<X::Img>> = BTreeMap::new();
    for atom in support.atoms() {
        let Some((id, date)) = atom.date_premise() else {
            continue;
        };
        let entry = by_fact.entry(id).or_insert_with(|| (date, Vec::new()));
        if let Some(citation) = atom.citation() {
            entry.1.push(citation);
        }
    }

    let premises: Vec<(FactId, UncertainDate)> = by_fact
        .iter()
        .map(|(id, (date, _))| (*id, date.clone()))
        .collect();

    minimal_fighting_sets(&premises)
        .into_iter()
        .filter_map(|set| {
            let rivals: Vec<Attributed<UncertainDate, X::Img>> = set
                .iter()
                .filter_map(|id| {
                    by_fact.get(id).map(|(date, sources)| Attributed {
                        value: date.clone(),
                        sources: sources.clone(),
                    })
                })
                .collect();
            NonEmptyVec::try_from_vec(rivals).ok()
        })
        .collect()
}

/// Flatten an additive entry's value to its attributing citations.
pub(super) fn factset<E, S, X>(entry: &Cited<(), S>, value: E) -> Attributed<E, X::Img>
where
    S: Support<Atom = X>,
    X: SupportAtom,
    X::Img: Ord,
{
    Attributed {
        value,
        sources: sources(&entry.support),
    }
}

/// Flatten one depiction record entry into a [`Depiction`]: its far end `other`,
/// each annotation axis `bracket()`'d, the link's existence citations from the
/// entry's support. Shared by both reading directions — the image side passes the
/// entity as `other`, the entity side the image — so the two views can't drift.
pub(super) fn depiction<OtherId, S, X>(
    other: OtherId,
    entry: &Cited<DepictionRecord<S>, S>,
) -> Depiction<OtherId, X::Img>
where
    S: Support<Atom = X>,
    X: SupportAtom,
    X::Img: Ord,
{
    Depiction {
        other,
        localization: bracket(&entry.value.localization),
        perspective: bracket(&entry.value.perspective),
        sources: sources(&entry.support),
    }
}

/// Whether a flattened slot carries a claim: an `Absent` consensus never touched
/// it, so a date slot is undated. Shared by the moment layer and the timeline's
/// date resolution.
pub(crate) fn has_date<V, ImgId>(bounded: &Bounded<V, ImgId>) -> bool {
    !matches!(bounded.consensus, Consensus::Absent)
}

/// The slot as a present bound, or `None` when it is absent.
pub(crate) fn dated_bound<V, ImgId>(bounded: &Bounded<V, ImgId>) -> Option<&Bounded<V, ImgId>> {
    has_date(bounded).then_some(bounded)
}

/// Whether a bracket carries a claim — its extent support is past the semiring
/// zero (something contributed via `plus`).
pub(super) fn touched<V, S>(b: &Bracket<V, S>) -> bool
where
    S: Support,
{
    !b.extent.support.is_zero()
}

pub(super) fn merge_provenance<EntId, S, X>(
    sameness: &Sameness<EntId, S>,
    class: &EquivClass<EntId>,
) -> MergeProvenance<EntId, X::Img>
where
    EntId: Ord + Clone,
    S: Support<Atom = X>,
    X: SupportAtom,
    X::Img: Ord,
{
    // The class always contains its own subject, so `members` is non-empty.
    let mention_count = NonZeroUsize::new(class.members.len()).unwrap_or(NonZeroUsize::MIN);

    let bridges: Vec<MergeBridge<EntId, X::Img>> = sameness
        .iter()
        .map(|(pair, entry)| MergeBridge {
            endpoints: pair.clone(),
            judgment: sources(&entry.support),
        })
        .collect();

    let endpoints: BTreeSet<&EntId> = bridges
        .iter()
        .flat_map(|b| [b.endpoints.a(), b.endpoints.b()])
        .collect();
    debug_assert!(
        endpoints.len() <= mention_count.get(),
        "bridge endpoints can't exceed the class's mention count"
    );

    MergeProvenance {
        mention_count,
        bridges,
    }
}

#[cfg(test)]
mod test_support {
    use std::hash::{Hash, Hasher};

    use chrono::NaiveDate;
    use url::Url;

    use crate::algebra::semiring::{Label, Semiring};
    use crate::date::{DatePrecision, UncertainDate};
    use crate::grammar::assertions::FactualAssertion;
    use crate::grammar::bookend;
    use crate::grammar::citations::{Excerpt, ExternalSource, FactualCitation};
    use crate::grammar::ids::{FactId, IdScheme};
    use crate::projection::MemberLineage;
    use crate::submit::StoredFact;
    use crate::submit::result::StoredFactualFact;

    use super::*;

    pub(super) type TestResult = Result<(), Box<dyn std::error::Error>>;
    pub(super) type EntId = u64;
    pub(super) type EvtId = u64;
    pub(super) type ImgId = u64;
    pub(super) type Lin = MemberLineage<EntId, ImgId>;

    /// A bare id scheme over `u64`, so the entity typed flatten's fact-atom
    /// support has a concrete scheme to name its stored facts under.
    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub(super) struct TestIds;

    impl IdScheme for TestIds {
        type Entity = u64;
        type Event = u64;
        type Image = u64;
    }

    /// The entity typed flatten's fact-atom support, over the [`TestIds`] scheme.
    pub(super) type FactLin = Label<FactAtom<TestIds>>;

    /// A factual citation distinguished by source url, so distinct sources keep
    /// distinct citations.
    pub(super) fn factual_citation(
        url: &str,
    ) -> Result<FactualCitation, Box<dyn std::error::Error>> {
        Ok(FactualCitation::new(
            ExternalSource::Url {
                url: Url::parse(url)?,
                published: None,
            },
            vec![Excerpt::new("source-text")?],
        )?)
    }

    /// A factual citation wrapped for a member-lineage atom.
    pub(super) fn factual(url: &str) -> Result<Citation<ImgId>, Box<dyn std::error::Error>> {
        Ok(Citation::Factual {
            citation: factual_citation(url)?,
        })
    }

    /// A member-lineage premise for one id citing one source.
    pub(super) fn lin(id: EntId, url: &str) -> Result<Lin, Box<dyn std::error::Error>> {
        Ok(Label::premise((id, factual(url)?)))
    }

    /// A fact-atom lineage citing one source, keyed by a fact id derived from
    /// `(id, url)` so distinct sources stay distinct facts and repeats dedup.
    pub(super) fn fact_lin(id: u64, url: &str) -> Result<FactLin, Box<dyn std::error::Error>> {
        fact_lin_dated(id, url, 1900)
    }

    /// A fact-atom lineage for a construction-start bound at year `y`, so a
    /// date-conflict flatten has distinct fighting dates to recover.
    pub(super) fn fact_lin_dated(
        id: u64,
        url: &str,
        y: i32,
    ) -> Result<FactLin, Box<dyn std::error::Error>> {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (id, url).hash(&mut hasher);
        let fact = StoredFact::Factual(StoredFactualFact {
            assertion: FactualAssertion::Construction {
                fact: bookend::ConstructionFact::Started {
                    entity: 0,
                    bound: year(y)?,
                },
            },
            citation: factual_citation(url)?,
        });
        Ok(Label::premise(FactAtom {
            id: FactId::new(hasher.finish()),
            fact,
        }))
    }

    /// A single claim's bracket: both bounds the value, backed by `support`.
    pub(super) fn claim<V: Clone, S: Clone>(value: V, support: S) -> Bracket<V, S> {
        Bracket::from((value, support))
    }

    /// The identity (untouched) bracket: consensus ⊤ / support one, extent ⊥ /
    /// support zero — the absent slot.
    pub(super) fn untouched<V, S>() -> Bracket<V, S>
    where
        V: crate::algebra::lattice::BoundedLattice,
        S: Semiring,
    {
        use crate::algebra::monoid::CommutativeMonoid;
        Bracket::identity()
    }

    pub(super) fn cited<V, S>(value: V, support: S) -> Cited<V, S> {
        Cited { value, support }
    }

    /// A single claim pinning one value as the settled `Claimed::Of` singleton,
    /// the bracket every restrictive value-mode field carries.
    pub(super) fn claimed_value<V: Ord + Clone, S: Clone>(
        value: V,
        support: S,
    ) -> Bracket<Claimed<V>, S> {
        claim(
            Claimed::Of {
                values: [value].into_iter().collect(),
            },
            support,
        )
    }

    pub(super) fn geometry() -> Result<ImageGeometry, Box<dyn std::error::Error>> {
        Ok(ImageGeometry::bbox(0.1, 0.2, 0.4, 0.5)?)
    }

    pub(super) fn year(y: i32) -> Result<UncertainDate, Box<dyn std::error::Error>> {
        Ok(UncertainDate::with_precision(
            NaiveDate::from_ymd_opt(y, 1, 1).ok_or("date")?,
            DatePrecision::Year,
        )?)
    }

    /// A depiction record with each axis present iff supplied, the shape the
    /// projection's depiction fold produces.
    pub(super) fn depiction_record<S: Semiring + Clone>(
        localization: Option<ImageGeometry>,
        perspective: Option<Perspective>,
        support: S,
    ) -> DepictionRecord<S> {
        DepictionRecord {
            localization: match localization {
                Some(g) => claimed_value(g, support.clone()),
                None => untouched(),
            },
            perspective: match perspective {
                Some(p) => claimed_value(p, support),
                None => untouched(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::date::UncertainDate;
    use crate::location::{LocationReference, UnresolvedLocation};

    use super::test_support::*;
    use super::*;

    // ---- Bounded::settled ----

    #[test]
    fn settled_returns_lone_value_else_none() -> TestResult {
        fn of(values: &[u8]) -> Claimed<u8> {
            Claimed::Of {
                values: values.iter().copied().collect(),
            }
        }
        fn with_consensus(consensus: Consensus<Claimed<u8>, ImgId>) -> Bounded<Claimed<u8>, ImgId> {
            Bounded {
                possible: Claimed::Any,
                sources: Vec::new(),
                facts: Vec::new(),
                consensus,
                derivation: None,
            }
        }

        assert_eq!(
            with_consensus(Consensus::Reached { value: of(&[7]) }).settled(),
            Some(&7),
            "a settled singleton yields its value"
        );
        assert_eq!(with_consensus(Consensus::Absent).settled(), None);
        assert_eq!(
            with_consensus(Consensus::Conflict {
                fighting: Vec::new()
            })
            .settled(),
            None
        );
        assert_eq!(
            with_consensus(Consensus::Pending {
                reason: PendingReason::Unresolved
            })
            .settled(),
            None
        );
        assert_eq!(
            with_consensus(Consensus::Reached { value: of(&[]) }).settled(),
            None,
            "an empty Of is not a settled value"
        );
        assert_eq!(
            with_consensus(Consensus::Reached { value: of(&[1, 2]) }).settled(),
            None,
            "a multi-value Of has no lone value"
        );
        assert_eq!(
            with_consensus(Consensus::Reached {
                value: Claimed::Any
            })
            .settled(),
            None,
            "Any is not a settled value"
        );
        Ok(())
    }

    // ---- bracket flatten arms ----

    #[test]
    fn bracket_settled_claim_reaches() -> TestResult {
        let b = claim(year(1850)?, lin(1, "https://a")?);
        let out = bracket(&b);
        assert_eq!(
            out.consensus,
            Consensus::Reached { value: year(1850)? },
            "a single claim settles its consensus"
        );
        assert_eq!(out.possible, year(1850)?);
        assert_eq!(out.sources.len(), 1);
        Ok(())
    }

    #[test]
    fn bracket_disjoint_claims_conflict() -> TestResult {
        use crate::algebra::monoid::CommutativeMonoid;
        let b = claim(year(1850)?, lin(1, "https://a")?)
            .combine(claim(year(1860)?, lin(2, "https://b")?));
        let out = bracket(&b);
        assert_eq!(
            out.consensus,
            Consensus::Conflict {
                fighting: Vec::new()
            },
            "two disjoint years over-determine the meet; the generic bracket leaves fighting empty"
        );
        assert!(
            out.possible.intervals().len() >= 2,
            "the extent keeps both rival years"
        );
        Ok(())
    }

    #[test]
    fn dated_bracket_surfaces_fighting_rivals() -> TestResult {
        use crate::algebra::monoid::CommutativeMonoid;
        // Two construction-start facts a decade apart over-determine the slot; the
        // date flatten must surface the disjoint pair as one fighting set whose
        // rivals carry the two submitted dates and their citations.
        let b = claim(year(1850)?, fact_lin_dated(1, "https://a", 1850)?)
            .combine(claim(year(1860)?, fact_lin_dated(2, "https://b", 1860)?));
        let out = dated_bracket(&b);
        let Consensus::Conflict { fighting } = &out.consensus else {
            return Err("disjoint start dates must flatten to a Conflict".into());
        };
        assert_eq!(
            fighting.len(),
            1,
            "a single disjoint pair yields one minimal fighting set"
        );
        let set = fighting.first().ok_or("no fighting set")?;
        assert_eq!(set.len().get(), 2, "the set names both rival facts");
        let dates: BTreeSet<UncertainDate> = set.iter().map(|r| r.value.clone()).collect();
        assert_eq!(
            dates,
            [year(1850)?, year(1860)?].into_iter().collect(),
            "the rivals carry the two submitted start dates"
        );
        for rival in set {
            assert_eq!(rival.sources.len(), 1, "each rival cites its own fact");
        }
        Ok(())
    }

    #[test]
    fn dated_bracket_dedupes_three_pairwise_disjoint_rivals() -> TestResult {
        use crate::algebra::monoid::CommutativeMonoid;
        // Three mutually disjoint start dates give the pairs [A,B],[A,C],[B,C] —
        // three minimal fighting sets. The flat rival view collapses each fact to
        // one entry.
        let b = claim(year(1850)?, fact_lin_dated(1, "https://a", 1850)?)
            .combine(claim(year(1860)?, fact_lin_dated(2, "https://b", 1860)?))
            .combine(claim(year(1870)?, fact_lin_dated(3, "https://c", 1870)?));
        let out = dated_bracket(&b);
        let Consensus::Conflict { fighting } = &out.consensus else {
            return Err("three disjoint start dates must flatten to a Conflict".into());
        };
        assert_eq!(
            fighting.len(),
            3,
            "three pairwise-disjoint dates give three minimal fighting pairs"
        );
        let rivals = distinct_rivals(fighting);
        assert_eq!(
            rivals.len(),
            3,
            "each fact surfaces once across the pairs it appears in"
        );
        let dates: BTreeSet<UncertainDate> = rivals.iter().map(|r| r.value.clone()).collect();
        assert_eq!(
            dates,
            [year(1850)?, year(1860)?, year(1870)?]
                .into_iter()
                .collect(),
            "the distinct rivals carry all three submitted start dates"
        );
        Ok(())
    }

    #[test]
    fn bracket_unresolved_reference_is_pending() -> TestResult {
        let reference = UnresolvedLocation::Reference(LocationReference::NamedPlace {
            name: "Springfield".to_owned(),
        });
        let b = claim(reference, lin(1, "https://a")?);
        let out = bracket(&b);
        assert_eq!(
            out.consensus,
            Consensus::Pending {
                reason: PendingReason::Unresolved
            },
            "an unresolved reference leaves the consensus pending"
        );
        Ok(())
    }

    #[test]
    fn bracket_untouched_slot_is_absent() -> TestResult {
        use crate::algebra::lattice::JoinSemilattice;
        let out = bracket(&untouched::<UncertainDate, Lin>());
        assert_eq!(out.consensus, Consensus::Absent);
        assert!(out.sources.is_empty(), "an absent field cites nothing");
        assert_eq!(
            out.possible,
            UncertainDate::bottom(),
            "an absent field's extent is the honest ⊥"
        );
        Ok(())
    }

    // ---- depiction flatten ----

    #[test]
    fn depiction_flattens_present_axes_and_sources() -> TestResult {
        let geom = geometry()?;
        let entry = cited(
            depiction_record(
                Some(geom.clone()),
                Some(Perspective::Exterior),
                lin(1, "https://d")?,
            ),
            lin(1, "https://d")?,
        );
        let out = depiction(7u64, &entry);
        assert_eq!(out.other, 7, "the depiction carries its far end");
        assert_eq!(
            out.localization.consensus,
            Consensus::Reached {
                value: Claimed::Of {
                    values: [geom].into_iter().collect()
                }
            },
            "a localized depiction settles its geometry"
        );
        assert_eq!(
            out.perspective.consensus,
            Consensus::Reached {
                value: Claimed::Of {
                    values: [Perspective::Exterior].into_iter().collect()
                }
            },
            "the perspective settles to the claimed view"
        );
        assert_eq!(out.sources.len(), 1, "the depiction link cites its fact");
        Ok(())
    }

    #[test]
    fn bare_depiction_flattens_to_absent_axes_keeping_sources() -> TestResult {
        // The common P18 case: an entity↔image link with neither axis annotated.
        // Both brackets flatten to Absent, but the link must still surface its
        // citation — the whole reason `sources` is separate from the axes.
        let entry = cited(
            depiction_record(None, None, lin(1, "https://d")?),
            lin(1, "https://d")?,
        );
        let out = depiction(7u64, &entry);
        assert_eq!(out.other, 7, "a bare depiction still carries its far end");
        assert_eq!(
            out.localization.consensus,
            Consensus::Absent,
            "an unlocalized depiction leaves localization absent"
        );
        assert_eq!(
            out.perspective.consensus,
            Consensus::Absent,
            "an unclassified depiction leaves perspective absent"
        );
        assert_eq!(
            out.sources.len(),
            1,
            "a bare depiction still surfaces its link citation"
        );
        Ok(())
    }
}
