//! Consumer-facing typed projection.
//!
//! Flattens the lattice- and provenance-rich `projection::Entity` into an
//! `Entity` DTO for read-side consumers: each restrictive field becomes a
//! `Bounded` (the bracket read off as a settled value, a conflict, a pending
//! verdict, or absent), each membership becomes an attributed value, and the
//! interior events parse into a typed timeline.

use std::collections::BTreeSet;
use std::num::NonZeroUsize;

use serde::Serialize;

use crate::algebra::lattice::JoinSemilattice;
use crate::algebra::semiring::Semiring;
use crate::claimed::Claimed;
use crate::facts::depiction::Perspective;
use crate::facts::geometry::ImageGeometry;
use crate::facts::identity::OrderedDistinctPair;
use crate::facts::schema::EquivClass;
use crate::location::ConflictStatus;

use crate::facts::projection::{
    Bracket, Citation, Cited, ConsensusConflict, DepictionRecord, MemberLineage, Sameness,
};

mod entity;
mod image;

pub use entity::*;
pub use image::*;

/// A value with the citations that attribute it — the additive-field mirror,
/// where membership carries no consensus/extent split.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Attributed<V, ImgId> {
    pub value: V,
    pub sources: Vec<Citation<ImgId>>,
}

/// The `T`-flattened mirror of the projection's [`Bracket`], for any lattice
/// `V`: the extent (`possible`), the citations behind it, and the consensus read
/// off the bracket's conflict tri-state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Bounded<V, ImgId> {
    /// The extent (join) — what any source allows, in the field's own lattice.
    pub possible: V,
    pub sources: Vec<Citation<ImgId>>,
    pub consensus: Consensus<V>,
}

/// The consensus side of a flattened bracket: whether a claim settled the slot,
/// over-determined it, was declined this layer, or never touched it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Consensus<V> {
    /// No claim touched this slot.
    Absent,
    /// The meet settled to a value all sources agree on.
    Reached { value: V },
    /// The meet bottomed out; the rival extent lives in [`Bounded::possible`].
    Conflict,
    /// This layer declined to decide (circle cap / unresolved reference).
    Pending { reason: PendingReason },
}

/// Why a consensus is [`Pending`](Consensus::Pending). One variant today — the
/// location circle cap; the carrier is uniform across fields so a future
/// undecidable date or discrete bound surfaces the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingReason {
    /// The merged region is too complex to run the emptiness check, or an
    /// unresolved reference leaves the verdict open.
    Unresolved,
}

/// One `SameEntity` bridge: the ordered, distinct id pair a judgment unified and
/// the citations behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MergeBridge<EntId: Ord, ImgId> {
    pub endpoints: OrderedDistinctPair<EntId>,
    pub judgment: Vec<Citation<ImgId>>,
}

/// How an entity's class was assembled: the mention count (always ≥1, the class
/// includes its own subject) and the bridges that merged its mentions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MergeProvenance<EntId: Ord, ImgId> {
    pub mention_count: NonZeroUsize,
    pub bridges: Vec<MergeBridge<EntId, ImgId>>,
}

/// One entity↔image depiction: its far end, where the entity sits in the image,
/// the view classification when a source supplies one, and the citations behind
/// the link.
///
/// Reached from both directions — [`Image::depicts`] reads it by entity,
/// [`Entity::depictions`] by image — so the one type carries both views. A bare
/// depiction (no localization, no perspective) flattens both bracket fields to
/// `Absent`; `sources` keeps the link's attribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Depiction<OtherId, ImgId> {
    /// The depiction's far end — the depicted entity in [`Image::depicts`], the
    /// depicting image in [`Entity::depictions`]; the one generic type serves
    /// both directions.
    pub other: OtherId,
    pub localization: Bounded<Claimed<ImageGeometry>, ImgId>,
    pub perspective: Bounded<Claimed<Perspective>, ImgId>,
    pub sources: Vec<Citation<ImgId>>,
}

// ----------------------------------------------------------------------------
// Flatteners
// ----------------------------------------------------------------------------

/// Iterate a lineage's `(id, citation)` atoms, keep the citations, dedup. The
/// id rode along to make cross-id glue computable in the projection; the typed
/// surface drops it.
pub(super) fn sources<EntId, ImgId>(support: &MemberLineage<EntId, ImgId>) -> Vec<Citation<ImgId>>
where
    EntId: Ord + Clone,
    ImgId: Ord + Clone,
{
    support
        .iter()
        .map(|(_, citation)| citation.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Flatten a restrictive field. `Absent` when the extent support is the semiring
/// zero (nothing contributed via `plus`); else `possible`/`sources` come from
/// the extent and the consensus reads off `consensus.value.conflict()`.
pub(super) fn bracket<V, EntId, ImgId>(
    b: &Bracket<V, MemberLineage<EntId, ImgId>>,
) -> Bounded<V, ImgId>
where
    V: JoinSemilattice + ConsensusConflict + Clone + PartialEq,
    EntId: Ord + Clone,
    ImgId: Ord + Clone,
{
    if !touched(b) {
        return Bounded {
            possible: V::bottom(),
            sources: Vec::new(),
            consensus: Consensus::Absent,
        };
    }

    let possible = b.extent.value.clone();
    let consensus = match b.consensus.value.conflict() {
        ConflictStatus::Consistent => Consensus::Reached {
            value: b.consensus.value.clone(),
        },
        ConflictStatus::Conflict => Consensus::Conflict,
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
        consensus,
    }
}

/// Flatten an additive entry's value to its attributing citations.
pub(super) fn factset<E, EntId, ImgId>(
    entry: &Cited<(), MemberLineage<EntId, ImgId>>,
    value: E,
) -> Attributed<E, ImgId>
where
    EntId: Ord + Clone,
    ImgId: Ord + Clone,
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
pub(super) fn depiction<OtherId, SrcId, ImgId>(
    other: OtherId,
    entry: &Cited<DepictionRecord<MemberLineage<SrcId, ImgId>>, MemberLineage<SrcId, ImgId>>,
) -> Depiction<OtherId, ImgId>
where
    SrcId: Ord + Clone,
    ImgId: Ord + Clone,
{
    Depiction {
        other,
        localization: bracket(&entry.value.localization),
        perspective: bracket(&entry.value.perspective),
        sources: sources(&entry.support),
    }
}

/// Whether a bracket carries a claim — its extent support is past the semiring
/// zero (something contributed via `plus`).
pub(super) fn touched<V, EntId, ImgId>(b: &Bracket<V, MemberLineage<EntId, ImgId>>) -> bool
where
    EntId: Ord,
    ImgId: Ord,
{
    b.extent.support != MemberLineage::zero()
}

pub(super) fn merge_provenance<EntId, ImgId>(
    sameness: &Sameness<EntId, MemberLineage<EntId, ImgId>>,
    class: &EquivClass<EntId>,
) -> MergeProvenance<EntId, ImgId>
where
    EntId: Ord + Clone,
    ImgId: Ord + Clone,
{
    // The class always contains its own subject, so `members` is non-empty.
    let mention_count = NonZeroUsize::new(class.members.len()).unwrap_or(NonZeroUsize::MIN);

    let bridges: Vec<MergeBridge<EntId, ImgId>> = sameness
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
    use chrono::NaiveDate;
    use url::Url;

    use crate::date::{DatePrecision, UncertainDate};
    use crate::facts::citations::{Excerpt, ExternalSource, FactualCitation};

    use super::*;

    pub(super) type TestResult = Result<(), Box<dyn std::error::Error>>;
    pub(super) type EntId = u64;
    pub(super) type EvtId = u64;
    pub(super) type ImgId = u64;
    pub(super) type Lin = MemberLineage<EntId, ImgId>;

    /// A factual citation, distinguished by source url so distinct claims keep
    /// distinct lineage atoms.
    pub(super) fn factual(url: &str) -> Result<Citation<ImgId>, Box<dyn std::error::Error>> {
        Ok(Citation::Factual {
            citation: FactualCitation::new(
                ExternalSource::Url {
                    url: Url::parse(url)?,
                    published: None,
                },
                vec![Excerpt::new("source-text")?],
            )?,
        })
    }

    /// A lineage atom for one id citing one source.
    pub(super) fn lin(id: EntId, url: &str) -> Result<Lin, Box<dyn std::error::Error>> {
        Ok(MemberLineage::Of(
            [(id, factual(url)?)].into_iter().collect(),
        ))
    }

    /// A single claim's bracket: both bounds the value, backed by `support`.
    pub(super) fn claim<V: Clone>(value: V, support: Lin) -> Bracket<V, Lin> {
        Bracket::from((value, support))
    }

    /// The identity (untouched) bracket: consensus ⊤ / support one, extent ⊥ /
    /// support zero — the absent slot.
    pub(super) fn untouched<V>() -> Bracket<V, Lin>
    where
        V: crate::algebra::lattice::BoundedLattice,
    {
        use crate::algebra::monoid::CommutativeMonoid;
        Bracket::identity()
    }

    pub(super) fn cited<V>(value: V, support: Lin) -> Cited<V, Lin> {
        Cited { value, support }
    }

    /// A single claim pinning one value as the settled `Claimed::Of` singleton,
    /// the bracket every restrictive value-mode field carries.
    pub(super) fn claimed_value<V: Ord + Clone>(
        value: V,
        support: Lin,
    ) -> Bracket<Claimed<V>, Lin> {
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
    pub(super) fn depiction_record(
        localization: Option<ImageGeometry>,
        perspective: Option<Perspective>,
        support: Lin,
    ) -> DepictionRecord<Lin> {
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
            Consensus::Conflict,
            "two disjoint years over-determine the meet"
        );
        assert!(
            out.possible.intervals().len() >= 2,
            "the extent keeps both rival years"
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
        let out = bracket(&untouched::<UncertainDate>());
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
