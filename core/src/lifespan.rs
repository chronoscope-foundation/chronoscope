//! Existence `Lifespan` — the map-slider's five-state existence classifier.
//!
//! For one entity, folds post-inference facts — construction and demolition
//! bookends, sightings — and answers, at any instant, one of five states:
//! uncontested (supported, no source denies it), contested (a source
//! disagreement), presumed (soft forward persistence), unknown (no evidence),
//! absent (denied and unsupported). Each state is what our *assertions* say, not
//! a claim about reality.
//!
//! The model is assertion-∀/∃. An assertion *denies* an instant only when its own
//! date makes existence there impossible — a construction's earliest start floors
//! the past, a demolition's latest completion caps the future. It *affirms* across
//! the convex hull of every assertion, direction-aware: an open sighting anchors
//! only the hull edge its contiguity guarantees, so a lone or same-direction pair
//! of open claims inverts the hull to empty. [`classify`](Lifespan::classify)
//! crosses the deny channel (∃ a denier) with the support channel (the hull plus a
//! forward closed-world tail).
//!
//! `Lifespan` is an opaque commutative-monoid lattice element: a downstream cache
//! folds it through [`combine`](crate::algebra::monoid::CommutativeMonoid::combine)
//! without reading its internals. The three axes are private; the trait surface is
//! the whole contract. Each axis is its own bounded join, and every reachable value
//! is coherent by construction — no deserialized byte string can pair a "no bound"
//! marker with a live date, because the sum types carry the date only when it
//! exists.

use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::algebra::monoid::CommutativeMonoid;
use crate::date::UncertainDate;

/// The existence verdict our assertions support at one instant — total and
/// mutually exclusive over the timeline.
///
/// The variant order is *presence-ascending* and load-bearing: `Ord` ranks a
/// weaker claim below a stronger one, so a co-located group of entities shows
/// its most-present member with `.max()`. Reordering the variants silently
/// changes what a shared map pin renders.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ExistenceState {
    /// Denied and unsupported — our sources place it gone or not yet built.
    Absent,
    /// Nothing affirms and nothing denies — grey "no data".
    Unknown,
    /// Soft forward closed-world tail: presumed still extant, defeasible.
    Presumed,
    /// A genuine source disagreement — some evidence supports existence here
    /// while a source denies it (a sighting past a demolition, a disputed era).
    Contested,
    /// Supported by evidence, and no source denies it.
    Uncontested,
}

/// The span of instants an entity's sources place it at — the whole of
/// [`classify`](Lifespan::classify)'s non-`Unknown`, non-`Absent` verdict, as one
/// interval.
///
/// It is always contiguous, which is what makes an existence filter indexable:
/// the affirmed hull and the forward presumption are adjacent, never disjoint, so
/// their union never splits. A caller filtering by time needs only this, not the
/// whole classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SupportWindow {
    /// No instant is supported. Nothing dates the entity, or its only evidence is
    /// an open ray anchoring no forward edge — either way it can answer no
    /// question about a moment, so it matches no timed query.
    Empty,
    /// Supported from `from` through `through`, both inclusive. `through` absent
    /// runs forward without bound: the closed-world presumption that a built
    /// thing stands until something records its removal.
    Span {
        from: NaiveDate,
        through: Option<NaiveDate>,
    },
}

impl SupportWindow {
    /// Whether the sources place the entity at `at`.
    pub fn contains(self, at: NaiveDate) -> bool {
        match self {
            SupportWindow::Empty => false,
            SupportWindow::Span { from, through } => from <= at && through.is_none_or(|t| at <= t),
        }
    }

    /// Whether the entity is placed anywhere within `[start, end]` — the
    /// existential reading a bbox+interval query wants, with an instant its
    /// degenerate `[T, T]` case.
    pub fn overlaps(self, start: NaiveDate, end: NaiveDate) -> bool {
        match self {
            SupportWindow::Empty => false,
            SupportWindow::Span { from, through } => {
                from <= end && through.is_none_or(|t| start <= t)
            }
        }
    }
}

/// The affirmed hull — the convex span every assertion vouches for. `None` on an
/// edge is that side's fold identity (a bound the fold never anchored), so an
/// un-anchored side yields to any present one under `combine`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Hull {
    lo: Option<NaiveDate>,
    hi: Option<NaiveDate>,
}

/// The deny-before axis: a construction's earliest possible start. `Floored(d)`
/// denies every instant before `d`; `Open` (an un-anchored or absent start) denies
/// nothing. `Open ⊑ Floored`, and joining two floors keeps the later — the rival
/// claiming the latest start denies the most past.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum BirthBound {
    Open,
    Floored(NaiveDate),
}

/// The deny-after axis, which also gates the forward presumption. `Capped(d)`
/// denies every instant after `d`; `Ongoing` records a demolition that denies no
/// instant yet still suppresses the presumption; `Open` means no demolition on
/// record. The chain is `Open ⊑ Ongoing ⊑ Capped(later) ⊑ Capped(earlier)`: any
/// demolition beats none, and among completions the earliest caps the most future.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum DeathBound {
    Open,
    Ongoing,
    Capped(NaiveDate),
}

/// The existence accumulator over three independent join axes: the affirmed
/// `Hull`, the deny-before `BirthBound`, and the deny-after / presumption-gate
/// `DeathBound`. Each axis is an idempotent semilattice, so `combine` is a
/// commutative, associative, idempotent monoid operation and the whole value is a
/// state-CRDT a cache can warm-start.
///
/// The axes are the entire private state; the public contract is the trait surface
/// ([`CommutativeMonoid`], `Serialize`/`Deserialize`, `Eq`) plus
/// [`classify`](Self::classify). A caller reads existence through `classify`, not
/// through the axes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lifespan {
    hull: Hull,
    birth: BirthBound,
    death: DeathBound,
}

impl Hull {
    /// The empty hull — both edges at their fold identity, so it defers to every
    /// assertion and contains no instant on its own.
    const EMPTY: Hull = Hull { lo: None, hi: None };

    /// The affirmed hull an assertion contributes, direction-aware.
    fn spanning(date: &UncertainDate) -> Hull {
        let (lo, hi) = hull_edges(date);
        Hull { lo, hi }
    }

    /// Join two hulls edge-wise. `None` is the fold identity on each side, but the
    /// sides read it oppositely: `lo`'s `None` is +∞ (a min-fold), `hi`'s is −∞ (a
    /// max-fold), so an unseen edge always yields to a present one.
    fn combine(self, other: Self) -> Self {
        Hull {
            lo: min_bound(self.lo, other.lo),
            hi: max_bound(self.hi, other.hi),
        }
    }

    /// Whether `at` lies inside the affirmed span. A hull crossed by `combine`
    /// (`lo > hi`) contains nothing, which is how a same-direction pair of open
    /// claims reads as empty.
    fn contains(self, at: NaiveDate) -> bool {
        matches!((self.lo, self.hi), (Some(l), Some(h)) if l <= at && at <= h)
    }

    /// The upper edge — the forward-presumption anchor.
    fn hi(self) -> Option<NaiveDate> {
        self.hi
    }

    /// The lower edge — the earliest instant any assertion vouches for.
    fn lo(self) -> Option<NaiveDate> {
        self.lo
    }
}

impl BirthBound {
    /// Join: `Open` is the identity, and two floors keep the later start, which
    /// denies the most past.
    fn combine(self, other: Self) -> Self {
        match (self, other) {
            (BirthBound::Open, b) | (b, BirthBound::Open) => b,
            (BirthBound::Floored(a), BirthBound::Floored(b)) => BirthBound::Floored(a.max(b)),
        }
    }

    /// A floored start denies every instant strictly before it.
    fn denies(self, at: NaiveDate) -> bool {
        matches!(self, BirthBound::Floored(d) if at < d)
    }
}

impl DeathBound {
    /// Join along `Open ⊑ Ongoing ⊑ Capped(later) ⊑ Capped(earlier)`: any
    /// demolition beats none, and the earliest completion caps the most future.
    fn combine(self, other: Self) -> Self {
        match (self, other) {
            (DeathBound::Open, d) | (d, DeathBound::Open) => d,
            (DeathBound::Ongoing, d) | (d, DeathBound::Ongoing) => d,
            (DeathBound::Capped(a), DeathBound::Capped(b)) => DeathBound::Capped(a.min(b)),
        }
    }

    /// A capped completion denies every instant strictly after it.
    fn denies(self, at: NaiveDate) -> bool {
        matches!(self, DeathBound::Capped(d) if at > d)
    }
}

impl Lifespan {
    /// The empty accumulator: empty hull, no start floor, no demolition. Also the
    /// monoid identity.
    const EMPTY: Lifespan = Lifespan {
        hull: Hull::EMPTY,
        birth: BirthBound::Open,
        death: DeathBound::Open,
    };

    /// A `construction.started` assertion. Floors the deniable past at its earliest
    /// possible start; an open-lower start ("built before Y") floors nothing.
    pub fn construction_started(date: UncertainDate) -> Self {
        let birth = match date.earliest() {
            Some(start) => BirthBound::Floored(start),
            None => BirthBound::Open,
        };
        Self {
            hull: Hull::spanning(&date),
            birth,
            death: DeathBound::Open,
        }
    }

    /// A `construction.completed` assertion. Affirms across its range and denies
    /// nothing — completion is a pure affirmer, not a deny-before.
    pub fn construction_completed(date: UncertainDate) -> Self {
        Self {
            hull: Hull::spanning(&date),
            birth: BirthBound::Open,
            death: DeathBound::Open,
        }
    }

    /// A `demolition.started` assertion. Affirms across its range and suppresses
    /// the forward presumption — removal in progress — while denying no instant.
    pub fn demolition_started(date: UncertainDate) -> Self {
        Self {
            hull: Hull::spanning(&date),
            birth: BirthBound::Open,
            death: DeathBound::Ongoing,
        }
    }

    /// A `demolition.completed` assertion. Caps the deniable future at its latest
    /// possible completion. An undated completion is `Ongoing`, never a cap at
    /// +∞ — that keeps one value with one representation, so structural `==` holds.
    pub fn demolition_completed(date: UncertainDate) -> Self {
        let death = match date.latest() {
            Some(end) => DeathBound::Capped(end),
            None => DeathBound::Ongoing,
        };
        Self {
            hull: Hull::spanning(&date),
            birth: BirthBound::Open,
            death,
        }
    }

    /// An existence witness — a sighting or an interior-event endpoint. Affirms
    /// across its range and denies nothing.
    pub fn witness(date: UncertainDate) -> Self {
        Self {
            hull: Hull::spanning(&date),
            birth: BirthBound::Open,
            death: DeathBound::Open,
        }
    }

    /// The span of instants this lifespan's assertions support — the interval
    /// form of [`classify`](Self::classify)'s supported verdicts, for a caller
    /// that needs to *filter* by time rather than explain a single moment.
    ///
    /// Contiguous by construction: the affirmed hull runs to `hull.hi` and the
    /// presumption picks up immediately after it, so the two never leave a gap.
    /// An entity with no forward anchor supports nothing at all — that is the
    /// undated entity, and the reason a timed query cannot return one.
    pub fn support(&self) -> SupportWindow {
        // No forward anchor: neither the hull nor the presumption reaches an
        // instant, whatever the lower edge says.
        let Some(hi) = self.hull.hi() else {
            return SupportWindow::Empty;
        };
        let presumes = self.death == DeathBound::Open;
        match self.hull.lo() {
            // A hull that contains something: supported from its lower edge, and
            // onward without bound when no demolition closes it.
            Some(lo) if lo <= hi => SupportWindow::Span {
                from: lo,
                through: if presumes { None } else { Some(hi) },
            },
            // The hull affirms nothing — unanchored below, or crossed by a
            // same-direction pair of open claims — so only the forward
            // presumption can support anything, and only past `hi`.
            _ if presumes => match hi.succ_opt() {
                Some(from) => SupportWindow::Span {
                    from,
                    through: None,
                },
                None => SupportWindow::Empty,
            },
            _ => SupportWindow::Empty,
        }
    }

    /// The existence verdict at `at`, derived from the three axes.
    pub fn classify(&self, at: NaiveDate) -> ExistenceState {
        let denied = self.birth.denies(at) || self.death.denies(at);
        let affirmed = self.hull.contains(at);
        // Forward closed-world tail: with no demolition on record, existence
        // persists past the last forward-anchoring evidence. The `Some(h)` guard
        // withholds a tail from a before-only or empty hull, which has no finite
        // forward anchor.
        let presumed_reach =
            self.death == DeathBound::Open && self.hull.hi().is_some_and(|h| at > h);
        let supported = affirmed || presumed_reach;
        // Partition on (denied, supported), then split the supported side by
        // whether the instant sits inside the affirmed hull.
        match (denied, supported, affirmed) {
            (true, true, _) => ExistenceState::Contested,
            (true, false, _) => ExistenceState::Absent,
            (false, false, _) => ExistenceState::Unknown,
            (false, true, true) => ExistenceState::Uncontested,
            (false, true, false) => ExistenceState::Presumed,
        }
    }
}

/// The commutative-monoid fold the cache rides on: join each axis independently.
impl CommutativeMonoid for Lifespan {
    fn identity() -> Self {
        Self::EMPTY
    }

    fn combine(self, other: Self) -> Self {
        Self {
            hull: self.hull.combine(other.hull),
            birth: self.birth.combine(other.birth),
            death: self.death.combine(other.death),
        }
    }
}

/// The affirmed-hull edges an assertion contributes, direction-aware. A closed
/// claim anchors both edges; a backward-open "before Y" anchors only the lower edge
/// (its closed edge `Y`); a forward-open "after Y" only the upper. An un-anchored
/// side stays `None` (its fold identity), so a lone or same-direction open leaves
/// the hull empty for free.
fn hull_edges(date: &UncertainDate) -> (Option<NaiveDate>, Option<NaiveDate>) {
    let lo = date.earliest();
    let hi = date.latest();
    let hull_lo = match (lo, hi) {
        // Forward-open or fully open: no lower anchor.
        (_, None) => None,
        (Some(l), Some(_)) => Some(l),
        // "before Y": the closed upper edge anchors the lower hull.
        (None, Some(h)) => Some(h),
    };
    let hull_hi = match (lo, hi) {
        // Backward-open or fully open: no upper anchor.
        (None, _) => None,
        (Some(_), Some(h)) => Some(h),
        // "after Y": the closed lower edge anchors the upper hull.
        (Some(l), None) => Some(l),
    };
    (hull_lo, hull_hi)
}

/// Min of two lower bounds, `None` (+∞) the identity — the lower-hull join.
fn min_bound(a: Option<NaiveDate>, b: Option<NaiveDate>) -> Option<NaiveDate> {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(a), Some(b)) => Some(a.min(b)),
    }
}

/// Max of two upper bounds, `None` (−∞) the identity — the upper-hull join.
fn max_bound(a: Option<NaiveDate>, b: Option<NaiveDate>) -> Option<NaiveDate> {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(a), Some(b)) => Some(a.max(b)),
    }
}

#[cfg(test)]
mod tests {
    use super::ExistenceState::{Absent, Contested, Presumed, Uncontested, Unknown};
    use super::*;
    use crate::date::{DateBound, DatePrecision};
    use proptest::prelude::*;

    type Res = Result<(), Box<dyn std::error::Error>>;
    type Ud = Result<UncertainDate, Box<dyn std::error::Error>>;

    fn nd(y: i32, m: u32, d: u32) -> Result<NaiveDate, &'static str> {
        NaiveDate::from_ymd_opt(y, m, d).ok_or("invalid date")
    }

    /// A single year, e.g. "1900".
    fn year(y: i32) -> Ud {
        Ok(UncertainDate::with_precision(
            nd(y, 1, 1)?,
            DatePrecision::Year,
        )?)
    }

    /// A whole decade, e.g. "the 1890s".
    fn decade(y: i32) -> Ud {
        Ok(UncertainDate::with_precision(
            nd(y, 1, 1)?,
            DatePrecision::Decade,
        )?)
    }

    /// A closed year span `[lo, hi]`, e.g. a circa-tightened "~1945" as
    /// \[1943, 1947\].
    fn span(lo: i32, hi: i32) -> Ud {
        Ok(UncertainDate::bounded(
            Some(DateBound::new(nd(lo, 1, 1)?, DatePrecision::Year)?),
            Some(DateBound::new(nd(hi, 1, 1)?, DatePrecision::Year)?),
        )?)
    }

    /// A backward-open "before Y".
    fn before(y: i32) -> Ud {
        Ok(UncertainDate::bounded(
            None,
            Some(DateBound::new(nd(y, 1, 1)?, DatePrecision::Year)?),
        )?)
    }

    /// A forward-open "after Y".
    fn after(y: i32) -> Ud {
        Ok(UncertainDate::bounded(
            Some(DateBound::new(nd(y, 1, 1)?, DatePrecision::Year)?),
            None,
        )?)
    }

    fn fold(assertions: impl IntoIterator<Item = Lifespan>) -> Lifespan {
        assertions
            .into_iter()
            .fold(Lifespan::identity(), Lifespan::combine)
    }

    // --- Matrix rows: the model's oracle across the state space ---

    #[test]
    fn construction_and_demolition_bracket_a_solid_green_span() -> Res {
        // CS 1900, DC 1950 — the whole bracket is one green band, edge to edge.
        let ls = fold([
            Lifespan::construction_started(year(1900)?),
            Lifespan::demolition_completed(year(1950)?),
        ]);
        assert_eq!(ls.classify(nd(1900, 1, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(1925, 6, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(1950, 12, 31)?), Uncontested);
        assert_eq!(ls.classify(nd(1899, 12, 31)?), Absent);
        assert_eq!(ls.classify(nd(1951, 1, 1)?), Absent);
        Ok(())
    }

    #[test]
    fn disputed_demolition_paints_an_orange_era() -> Res {
        // CS 1900, DC {1950, 1960} disjoint. Agreed alive to the earliest
        // demolition, disputed death era orange, gone after the latest.
        let ls = fold([
            Lifespan::construction_started(year(1900)?),
            Lifespan::demolition_completed(year(1950)?),
            Lifespan::demolition_completed(year(1960)?),
        ]);
        assert_eq!(ls.classify(nd(1925, 1, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(1950, 12, 31)?), Uncontested);
        assert_eq!(ls.classify(nd(1951, 1, 1)?), Contested);
        assert_eq!(ls.classify(nd(1960, 12, 31)?), Contested);
        assert_eq!(ls.classify(nd(1961, 1, 1)?), Absent);
        assert_eq!(ls.classify(nd(1895, 1, 1)?), Absent);
        Ok(())
    }

    #[test]
    fn disputed_construction_bookend_is_orange_then_green() -> Res {
        // CS {1900, 2000} disjoint, W 1950. The disputed era is one legible orange
        // band; the later claim's year is the green onset.
        let ls = fold([
            Lifespan::construction_started(year(1900)?),
            Lifespan::construction_started(year(2000)?),
            Lifespan::witness(year(1950)?),
        ]);
        assert_eq!(ls.classify(nd(1895, 1, 1)?), Absent);
        assert_eq!(ls.classify(nd(1950, 1, 1)?), Contested);
        assert_eq!(ls.classify(nd(1999, 12, 31)?), Contested);
        assert_eq!(ls.classify(nd(2000, 6, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(2001, 1, 1)?), Presumed);
        Ok(())
    }

    #[test]
    fn disputed_construction_without_a_witness_is_still_orange() -> Res {
        // Lone CS {1900, 2000} renders like the witnessed rivals — the hull spans
        // both claims whether or not a witness bridges them.
        let ls = fold([
            Lifespan::construction_started(year(1900)?),
            Lifespan::construction_started(year(2000)?),
        ]);
        assert_eq!(ls.classify(nd(1895, 1, 1)?), Absent);
        assert_eq!(ls.classify(nd(1950, 1, 1)?), Contested);
        assert_eq!(ls.classify(nd(1999, 12, 31)?), Contested);
        assert_eq!(ls.classify(nd(2000, 6, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(2001, 1, 1)?), Presumed);
        Ok(())
    }

    #[test]
    fn single_vague_demolition_bound_stays_green_through_its_range() -> Res {
        // "demolished 1890s" reads green across the entire decade, edge to edge —
        // the whole range is solid, with a clean cutover at 1900.
        let ls = fold([Lifespan::demolition_completed(decade(1890)?)]);
        assert_eq!(ls.classify(nd(1890, 1, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(1895, 6, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(1899, 12, 31)?), Uncontested);
        assert_eq!(ls.classify(nd(1900, 1, 1)?), Absent);
        // No construction to deny the past, so before the demolition is unknown.
        assert_eq!(ls.classify(nd(1889, 12, 31)?), Unknown);
        Ok(())
    }

    #[test]
    fn demolition_started_alone_is_a_single_green_blip() -> Res {
        // DS 1973 affirms its year, suppresses the forward presumption, and denies
        // nothing — unknown on both sides.
        let ls = fold([Lifespan::demolition_started(year(1973)?)]);
        assert_eq!(ls.classify(nd(1973, 6, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(1972, 12, 31)?), Unknown);
        assert_eq!(ls.classify(nd(1974, 1, 1)?), Unknown);
        Ok(())
    }

    #[test]
    fn born_after_dead_renders_all_orange() -> Res {
        // CS 1900, DC 1850 — the start floors the past above where the completion
        // caps the future, so every instant is denied. The affirmed hull reads all
        // orange, everything else absent.
        let ls = fold([
            Lifespan::construction_started(year(1900)?),
            Lifespan::demolition_completed(year(1850)?),
        ]);
        assert_eq!(ls.classify(nd(1860, 1, 1)?), Contested);
        assert_eq!(ls.classify(nd(1875, 1, 1)?), Contested);
        assert_eq!(ls.classify(nd(1900, 12, 31)?), Contested);
        assert_eq!(ls.classify(nd(1849, 12, 31)?), Absent);
        assert_eq!(ls.classify(nd(1920, 1, 1)?), Absent);
        Ok(())
    }

    #[test]
    fn a_lone_witness_is_green_then_presumed_forward() -> Res {
        // A photo's year is green, the future presumed (no demolition on record),
        // the past unknown (no construction on record).
        let ls = fold([Lifespan::witness(year(1950)?)]);
        assert_eq!(ls.classify(nd(1950, 6, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(1949, 12, 31)?), Unknown);
        assert_eq!(ls.classify(nd(2000, 1, 1)?), Presumed);
        Ok(())
    }

    #[test]
    fn a_lone_before_witness_is_unknown_everywhere() -> Res {
        // A backward-open "before Y" sighting anchors only the hull's lower edge
        // (lo=Some, hi=None), so the hull contains no instant, and with no forward
        // anchor there is nothing to presume from. Grey at every instant.
        let ls = fold([Lifespan::witness(before(1900)?)]);
        assert_eq!(ls.classify(nd(1000, 1, 1)?), Unknown);
        assert_eq!(ls.classify(nd(1899, 12, 31)?), Unknown);
        assert_eq!(ls.classify(nd(1900, 6, 1)?), Unknown);
        assert_eq!(ls.classify(nd(2500, 1, 1)?), Unknown);
        Ok(())
    }

    #[test]
    fn a_sighting_before_construction_is_an_orange_fringe() -> Res {
        // CS 1900, W 1850. The pre-construction sighting is supported by the hull
        // yet denied by the construction — a legible orange "photographed before
        // its recorded construction" era.
        let ls = fold([
            Lifespan::construction_started(year(1900)?),
            Lifespan::witness(year(1850)?),
        ]);
        assert_eq!(ls.classify(nd(1849, 12, 31)?), Absent);
        assert_eq!(ls.classify(nd(1850, 6, 1)?), Contested);
        assert_eq!(ls.classify(nd(1875, 1, 1)?), Contested);
        assert_eq!(ls.classify(nd(1900, 6, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(1950, 1, 1)?), Presumed);
        Ok(())
    }

    #[test]
    fn lone_construction_is_green_then_presumed() -> Res {
        // CS 1900 alone — construction year green, forward presumed, backward
        // absent.
        let ls = fold([Lifespan::construction_started(year(1900)?)]);
        assert_eq!(ls.classify(nd(1900, 6, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(1899, 12, 31)?), Absent);
        assert_eq!(ls.classify(nd(2000, 1, 1)?), Presumed);
        Ok(())
    }

    #[test]
    fn circa_construction_and_demolition_overlap_stays_solid_green() -> Res {
        // Post-inference CS [1940,1947] (capped at the demolition), DC ~1945 =
        // [1943,1947]. Overlapping circa dates stay a lawful span — the whole hull
        // reads solid green, edge to edge.
        let ls = fold([
            Lifespan::construction_started(span(1940, 1947)?),
            Lifespan::demolition_completed(span(1943, 1947)?),
        ]);
        assert_eq!(ls.classify(nd(1940, 1, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(1945, 6, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(1947, 12, 31)?), Uncontested);
        assert_eq!(ls.classify(nd(1939, 12, 31)?), Absent);
        assert_eq!(ls.classify(nd(1948, 1, 1)?), Absent);
        Ok(())
    }

    // --- Open bookends, each alone ---

    #[test]
    fn open_construction_bookend_presumes_forward_without_green() -> Res {
        // CS "after 1800" pins no existence instant: absent before, unknown at the
        // boundary, presumed after, green nowhere.
        let ls = fold([Lifespan::construction_started(after(1800)?)]);
        assert_eq!(ls.classify(nd(1799, 12, 31)?), Absent);
        assert_eq!(ls.classify(nd(1800, 1, 1)?), Unknown);
        assert_eq!(ls.classify(nd(1801, 1, 1)?), Presumed);
        assert_eq!(ls.classify(nd(2000, 1, 1)?), Presumed);
        Ok(())
    }

    #[test]
    fn open_demolition_bookend_is_unknown_then_absent() -> Res {
        // DC "before 1900" — built when? no evidence — so unknown up to the
        // demolition, absent after. A demolition suppresses the presumption.
        let ls = fold([Lifespan::demolition_completed(before(1900)?)]);
        assert_eq!(ls.classify(nd(1850, 1, 1)?), Unknown);
        assert_eq!(ls.classify(nd(1900, 12, 31)?), Unknown);
        assert_eq!(ls.classify(nd(1901, 1, 1)?), Absent);
        Ok(())
    }

    // --- Direction-aware open witnesses: the false-green guards ---

    #[test]
    fn a_before_and_after_sighting_bracket_a_green_span() -> Res {
        // "before 1900" + "after 2000" bracket a green [1900, 2000]; unknown before
        // (a backward ray affirms only up to its edge), presumed after.
        let ls = fold([
            Lifespan::witness(before(1900)?),
            Lifespan::witness(after(2000)?),
        ]);
        assert_eq!(ls.classify(nd(1000, 1, 1)?), Unknown);
        assert_eq!(ls.classify(nd(1899, 6, 1)?), Unknown);
        assert_eq!(ls.classify(nd(1950, 1, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(2000, 1, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(2500, 1, 1)?), Presumed);
        Ok(())
    }

    #[test]
    fn lone_forward_open_witness_pins_no_green() -> Res {
        // "after 1906" alone affirms no instant: unknown up to its anchor, presumed
        // forward, green nowhere.
        let ls = fold([Lifespan::witness(after(1906)?)]);
        assert_eq!(ls.classify(nd(1905, 1, 1)?), Unknown);
        assert_eq!(ls.classify(nd(1906, 1, 1)?), Unknown);
        assert_eq!(ls.classify(nd(1907, 1, 1)?), Presumed);
        assert_eq!(ls.classify(nd(2000, 1, 1)?), Presumed);
        Ok(())
    }

    #[test]
    fn two_after_sightings_bracket_no_green() -> Res {
        // "after 1906" + "after 1950" leave the hull's lower edge unanchored (+∞),
        // so the hull is empty — two forward-open sightings pin no instant. Uncontested
        // nowhere.
        let ls = fold([
            Lifespan::witness(after(1906)?),
            Lifespan::witness(after(1950)?),
        ]);
        for &(y, m, d) in &[(1800, 1, 1), (1906, 1, 1), (1950, 1, 1), (3000, 1, 1)] {
            assert_ne!(ls.classify(nd(y, m, d)?), Uncontested, "at {y}-{m}-{d}");
        }
        assert_eq!(ls.classify(nd(1950, 1, 1)?), Unknown);
        assert_eq!(ls.classify(nd(1951, 1, 1)?), Presumed);
        Ok(())
    }

    #[test]
    fn an_after_below_a_before_brackets_no_green() -> Res {
        // "after 1906" (anchors the hull's upper edge) with "before 2000" (anchors
        // the lower) invert the hull — lower 2000 > upper 1906 — so it is empty.
        // Only a before *earlier* than an after brackets green.
        let ls = fold([
            Lifespan::witness(after(1906)?),
            Lifespan::witness(before(2000)?),
        ]);
        for &(y, m, d) in &[(1906, 1, 1), (1950, 1, 1), (1999, 1, 1), (2000, 6, 1)] {
            assert_ne!(ls.classify(nd(y, m, d)?), Uncontested, "at {y}-{m}-{d}");
        }
        Ok(())
    }

    /// The presence order a shared map pin resolves by: co-located entities
    /// collapse to one marker showing its most-present member (`.max()`), so this
    /// chain decides what the user sees. Pinned because it lives in the variant
    /// declaration order — a reorder would silently change the render.
    #[test]
    fn presence_order_ranks_uncontested_over_contested_over_presumed_over_unknown_over_absent() {
        assert!(Absent < Unknown);
        assert!(Unknown < Presumed);
        assert!(Presumed < Contested);
        assert!(Contested < Uncontested);
    }

    // --- The monoid contract (associativity / commutativity / idempotence /
    //     identity, structurally) and the five-state partition ---

    fn arb_udate() -> impl Strategy<Value = UncertainDate> {
        (1i32..=3000, 1i32..=3000, 0u8..4).prop_filter_map(
            "valid uncertain date",
            |(a, b, shape)| {
                let lo = DateBound::new(
                    NaiveDate::from_ymd_opt(a.min(b), 1, 1)?,
                    DatePrecision::Year,
                )
                .ok()?;
                let hi = DateBound::new(
                    NaiveDate::from_ymd_opt(a.max(b), 1, 1)?,
                    DatePrecision::Year,
                )
                .ok()?;
                match shape {
                    0 => UncertainDate::bounded(Some(lo), Some(hi)).ok(),
                    1 => UncertainDate::bounded(None, Some(hi)).ok(),
                    2 => UncertainDate::bounded(Some(lo), None).ok(),
                    _ => Some(UncertainDate::unknown()),
                }
            },
        )
    }

    fn arb_assertion() -> impl Strategy<Value = Lifespan> {
        (0u8..5, arb_udate()).prop_map(|(slot, ud)| match slot {
            0 => Lifespan::construction_started(ud),
            1 => Lifespan::construction_completed(ud),
            2 => Lifespan::demolition_started(ud),
            3 => Lifespan::demolition_completed(ud),
            _ => Lifespan::witness(ud),
        })
    }

    fn arb_lifespan() -> impl Strategy<Value = Lifespan> {
        prop::collection::vec(arb_assertion(), 0..=6).prop_map(fold)
    }

    fn arb_instant() -> impl Strategy<Value = NaiveDate> {
        (1i32..=3000, 1u32..=12, 1u32..=28).prop_filter_map("valid instant", |(y, m, d)| {
            NaiveDate::from_ymd_opt(y, m, d)
        })
    }

    // Structural associativity / commutativity / identity / idempotence of the
    // per-axis join fold — the cache-coherence prerequisite.
    crate::join_semilattice_laws!(
        monoid_laws,
        Lifespan,
        arb_lifespan(),
        |a: &Lifespan, b: &Lifespan| a == b
    );

    proptest! {
        /// The five states, written straight from their predicate definitions,
        /// partition the timeline: exactly one holds at every instant, and it is
        /// the one `classify` returns. Guards the match arms and the predicate
        /// boundaries (green/presumed at the hull's upper edge, presumed/unknown,
        /// orange/absent) against drift.
        #[test]
        fn classify_matches_the_denotational_partition(ls in arb_lifespan(), at in arb_instant()) {
            let denied = ls.birth.denies(at) || ls.death.denies(at);
            let affirmed = ls.hull.contains(at);
            let presumed_reach =
                ls.death == DeathBound::Open && ls.hull.hi().is_some_and(|h| at > h);
            let supported = affirmed || presumed_reach;

            let green = affirmed && !denied;
            let orange = supported && denied;
            let presumed = presumed_reach && !affirmed && !denied;
            let unknown = !supported && !denied;
            let absent = !supported && denied;

            let held = [green, orange, presumed, unknown, absent]
                .into_iter()
                .filter(|b| *b)
                .count();
            prop_assert_eq!(held, 1);

            let expected = if green {
                Uncontested
            } else if orange {
                Contested
            } else if presumed {
                Presumed
            } else if unknown {
                Unknown
            } else {
                Absent
            };
            prop_assert_eq!(ls.classify(at), expected);
        }

        /// The support window is exactly the supported verdicts, at every
        /// instant. This is the law that lets a filter use the interval instead
        /// of the classification: an index may prune on `support()` and still
        /// agree with `classify` about which entities a timed query returns.
        ///
        /// It also pins contiguity — a single interval could not track `classify`
        /// everywhere if the supported set were ever split.
        #[test]
        fn support_window_holds_exactly_the_supported_instants(
            ls in arb_lifespan(),
            at in arb_instant(),
        ) {
            let supported = matches!(ls.classify(at), Uncontested | Contested | Presumed);
            prop_assert_eq!(ls.support().contains(at), supported);
        }

        /// An instant is the degenerate interval, so the two reads agree there.
        /// This is the contract a timed query rests on: asking about a moment is
        /// asking about `[T, T]`.
        #[test]
        fn overlaps_at_a_degenerate_interval_is_contains(
            ls in arb_lifespan(),
            at in arb_instant(),
        ) {
            let window = ls.support();
            prop_assert_eq!(window.overlaps(at, at), window.contains(at));
        }

        /// Over a range, `overlaps` is the existential reading: it matches iff
        /// some instant within is supported. Checked against a day-by-day scan —
        /// the honest oracle — over a bounded span, since an unbounded one walks
        /// millions of days whenever the answer is false.
        #[test]
        fn overlaps_is_some_supported_instant_in_range(
            ls in arb_lifespan(),
            start in arb_instant(),
            span in 0u64..400,
        ) {
            let Some(end) = start.checked_add_days(chrono::Days::new(span)) else {
                return Ok(());
            };
            let window = ls.support();
            let any = start
                .iter_days()
                .take_while(|d| *d <= end)
                .any(|d| window.contains(d));
            prop_assert_eq!(window.overlaps(start, end), any);
        }

        /// Order- and duplicate-independence: the fold is the same under any
        /// permutation and any duplication of the assertion bag.
        #[test]
        fn fold_is_order_and_duplicate_insensitive(
            mut assertions in prop::collection::vec(arb_assertion(), 0..=6),
        ) {
            let straight = fold(assertions.clone());
            let doubled = fold(assertions.iter().cloned().chain(assertions.iter().cloned()));
            prop_assert_eq!(&straight, &doubled);

            assertions.reverse();
            prop_assert_eq!(&straight, &fold(assertions));
        }
    }
}
