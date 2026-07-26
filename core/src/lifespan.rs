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
//! A demolition switches that forward tail off, but only while it stands: a dated
//! completion our own construction or sighting evidence outlives is *refuted*, and
//! the tail resumes — contested rather than absent, since the demolition keeps
//! denying. Only non-demolition evidence refutes, which is what keeps two sources
//! disagreeing on the demolition year from discrediting each other.
//!
//! `Lifespan` is an opaque commutative-monoid lattice element: a downstream cache
//! folds it through [`combine`](crate::algebra::monoid::CommutativeMonoid::combine)
//! without reading its internals. The four axes are private; the trait surface is
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
    /// thing stands until a record of its removal stands unrefuted.
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

/// The forward edge of the *non-demolition* evidence: the latest instant a
/// construction or a sighting vouches for, `None` its −∞ fold identity. Direction-
/// aware like the hull's upper edge, so a "before Y" anchors nothing here.
///
/// This is the yardstick a demolition claim is measured against — a completion our
/// own later evidence outlives is refuted and stops suppressing the presumption.
/// Demolitions are held out of it because a bookend affirms at its own date: read
/// off the whole hull instead, two sources disagreeing on the demolition year would
/// each pass as evidence against the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct LiveEdge(Option<NaiveDate>);

/// The existence accumulator over four independent join axes: the affirmed `Hull`,
/// the non-demolition `LiveEdge`, the deny-before `BirthBound`, and the deny-after
/// / presumption-gate `DeathBound`. Each axis is an idempotent semilattice, so
/// `combine` is a commutative, associative, idempotent monoid operation and the
/// whole value is a state-CRDT a cache can warm-start.
///
/// The axes are the entire private state; the public contract is the trait surface
/// ([`CommutativeMonoid`], `Serialize`/`Deserialize`, `Eq`) plus
/// [`classify`](Self::classify). A caller reads existence through `classify`, not
/// through the axes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lifespan {
    hull: Hull,
    live: LiveEdge,
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

impl LiveEdge {
    /// The −∞ identity: no non-demolition assertion anchors a forward edge.
    const NONE: LiveEdge = LiveEdge(None);

    /// Join: keep the later edge, since the latest sighting is the one a
    /// demolition has to survive.
    fn combine(self, other: Self) -> Self {
        LiveEdge(max_bound(self.0, other.0))
    }

    /// Whether our evidence reaches past `cap` — a sighting or a construction the
    /// demolition claiming to end at `cap` cannot account for.
    fn outlives(self, cap: NaiveDate) -> bool {
        self.0.is_some_and(|edge| edge > cap)
    }
}

impl Lifespan {
    /// The empty accumulator: empty hull, no evidence edge, no start floor, no
    /// demolition. Also the monoid identity.
    const EMPTY: Lifespan = Lifespan {
        hull: Hull::EMPTY,
        live: LiveEdge::NONE,
        birth: BirthBound::Open,
        death: DeathBound::Open,
    };

    /// What a non-demolition assertion contributes: it affirms across its hull, and
    /// that hull's upper edge is also evidence a demolition claim has to survive.
    /// The two move together for every such slot, so they are anchored in one place.
    fn affirming(date: &UncertainDate) -> Self {
        let (lo, hi) = hull_edges(date);
        Self {
            hull: Hull { lo, hi },
            live: LiveEdge(hi),
            birth: BirthBound::Open,
            death: DeathBound::Open,
        }
    }

    /// What a demolition assertion contributes: it affirms across its hull like any
    /// other, and anchors no evidence edge — a demolition is measured against our
    /// other evidence, so two sources disagreeing on the year leave each other
    /// standing rather than one refuting the other.
    fn demolishing(date: &UncertainDate, death: DeathBound) -> Self {
        Self {
            hull: Hull::spanning(date),
            live: LiveEdge::NONE,
            birth: BirthBound::Open,
            death,
        }
    }

    /// A `construction.started` assertion. Floors the deniable past at its earliest
    /// possible start; an open-lower start ("built before Y") floors nothing.
    pub fn construction_started(date: UncertainDate) -> Self {
        let birth = match date.earliest() {
            Some(start) => BirthBound::Floored(start),
            None => BirthBound::Open,
        };
        Self {
            birth,
            ..Self::affirming(&date)
        }
    }

    /// A `construction.completed` assertion. Affirms across its range and denies
    /// nothing — completion is a pure affirmer, not a deny-before.
    pub fn construction_completed(date: UncertainDate) -> Self {
        Self::affirming(&date)
    }

    /// A `demolition.started` assertion. Affirms across its range and suppresses
    /// the forward presumption — removal in progress — while denying no instant.
    pub fn demolition_started(date: UncertainDate) -> Self {
        Self::demolishing(&date, DeathBound::Ongoing)
    }

    /// A `demolition.completed` assertion. Caps the deniable future at its latest
    /// possible completion. An undated completion is `Ongoing`, never a cap at
    /// +∞ — that keeps one value with one representation, so structural `==` holds.
    pub fn demolition_completed(date: UncertainDate) -> Self {
        let death = match date.latest() {
            Some(end) => DeathBound::Capped(end),
            None => DeathBound::Ongoing,
        };
        Self::demolishing(&date, death)
    }

    /// An existence witness — a sighting or an interior-event endpoint. Affirms
    /// across its range and denies nothing.
    pub fn witness(date: UncertainDate) -> Self {
        Self::affirming(&date)
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
        let presumes = self.presumes();
        match self.hull.lo() {
            // A hull that contains something: supported from its lower edge, and
            // onward without bound while the presumption runs.
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

    /// Whether an assertion rules existence at `at` impossible — the deny channel,
    /// which only a construction's earliest start and a demolition's latest
    /// completion feed.
    fn denies(&self, at: NaiveDate) -> bool {
        self.birth.denies(at) || self.death.denies(at)
    }

    /// Whether the forward closed-world presumption runs past the affirmed hull.
    ///
    /// A demolition on record normally withdraws it. A dated completion our own
    /// construction or sighting evidence outlives is refuted, and a refuted claim
    /// hands the presumption back: reading the tail as absent would report the
    /// entity definitely gone on the authority of a claim we hold evidence
    /// against. It keeps denying, so the tail reads contested — support and denial
    /// both on the record, which is what a source disagreement looks like.
    ///
    /// `Ongoing` — a demolition started, or a completion with no date — asserts no
    /// instant of removal, so a later sighting is consistent with it and it
    /// suppresses unconditionally.
    fn presumes(&self) -> bool {
        match self.death {
            DeathBound::Open => true,
            DeathBound::Ongoing => false,
            DeathBound::Capped(cap) => self.live.outlives(cap),
        }
    }

    /// The existence verdict at `at`, derived from the four axes.
    pub fn classify(&self, at: NaiveDate) -> ExistenceState {
        let denied = self.denies(at);
        let affirmed = self.hull.contains(at);
        // Forward closed-world tail: with no standing demolition, existence
        // persists past the last forward-anchoring evidence. The `Some(h)` guard
        // withholds a tail from a before-only or empty hull, which has no finite
        // forward anchor.
        let presumed_reach = self.presumes() && self.hull.hi().is_some_and(|h| at > h);
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
            live: self.live.combine(other.live),
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

    /// Two demolitions cannot refute each other. Every bookend affirms at its own
    /// date, so measuring the earlier claim against the whole affirmed hull would
    /// read two sources *agreeing* the entity is gone as evidence it survived, and
    /// flip the tail from absent to contested. The 1961 `Absent` is what holds the
    /// refutation yardstick to non-demolition evidence.
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

    /// A photograph taken after the claimed demolition is evidence against it. The
    /// claim keeps denying — so its own era stays contested — but it no longer
    /// carries the future: reporting the entity definitely gone in 1888 would state
    /// as fact what the 1887 photo contradicts.
    #[test]
    fn a_sighting_past_a_demolition_lifts_its_suppression() -> Res {
        let ls = fold([
            Lifespan::demolition_completed(year(1885)?),
            Lifespan::witness(year(1887)?),
        ]);
        assert_eq!(ls.classify(nd(1885, 6, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(1886, 1, 1)?), Contested);
        assert_eq!(ls.classify(nd(1887, 6, 1)?), Contested);
        assert_eq!(ls.classify(nd(1888, 1, 1)?), Contested);
        assert_eq!(ls.classify(nd(2000, 1, 1)?), Contested);
        // No construction on record, so nothing denies the past.
        assert_eq!(ls.classify(nd(1884, 12, 31)?), Unknown);
        Ok(())
    }

    /// A refuted demolition leaves no absent tail at all: everything from the
    /// claimed removal onward is a live disagreement between the demolition and the
    /// sighting, and nothing in the record settles it.
    #[test]
    fn a_witness_long_after_a_demolition_contests_the_whole_tail() -> Res {
        // CS 1900, DC 1950, W 2019.
        let ls = fold([
            Lifespan::construction_started(year(1900)?),
            Lifespan::demolition_completed(year(1950)?),
            Lifespan::witness(year(2019)?),
        ]);
        assert_eq!(ls.classify(nd(1925, 1, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(1950, 12, 31)?), Uncontested);
        assert_eq!(ls.classify(nd(1951, 1, 1)?), Contested);
        assert_eq!(ls.classify(nd(2019, 6, 1)?), Contested);
        assert_eq!(ls.classify(nd(2020, 1, 1)?), Contested);
        assert_eq!(ls.classify(nd(2500, 1, 1)?), Contested);
        // The construction still denies its own past.
        assert_eq!(ls.classify(nd(1899, 12, 31)?), Absent);
        Ok(())
    }

    /// "Removal began in 1973" names no instant of removal, so a photograph from
    /// 1990 is consistent with it rather than evidence against it. An ongoing
    /// demolition therefore keeps suppressing the presumption however late the
    /// sighting: the entity's fate stays unknown, not presumed.
    #[test]
    fn an_ongoing_demolition_survives_a_later_sighting() -> Res {
        let ls = fold([
            Lifespan::demolition_started(year(1973)?),
            Lifespan::witness(year(1990)?),
        ]);
        assert_eq!(ls.classify(nd(1980, 1, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(1990, 6, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(1991, 1, 1)?), Unknown);
        assert_eq!(ls.classify(nd(2500, 1, 1)?), Unknown);
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

    /// A born-after-dead entity refutes its own demolition: the construction claims
    /// a start the completion cannot account for, which is exactly the evidence that
    /// lifts suppression. So the contested band runs forward without bound, and the
    /// only absent region is the past the construction denies before anything
    /// affirms. Calling the tail absent would rest on a demolition the entity's own
    /// construction record contradicts.
    #[test]
    fn born_after_dead_renders_all_orange() -> Res {
        // CS 1900, DC 1850 — the start floors the past above where the completion
        // caps the future, so every instant is denied.
        let ls = fold([
            Lifespan::construction_started(year(1900)?),
            Lifespan::demolition_completed(year(1850)?),
        ]);
        assert_eq!(ls.classify(nd(1860, 1, 1)?), Contested);
        assert_eq!(ls.classify(nd(1875, 1, 1)?), Contested);
        assert_eq!(ls.classify(nd(1900, 12, 31)?), Contested);
        assert_eq!(ls.classify(nd(1920, 1, 1)?), Contested);
        assert_eq!(ls.classify(nd(1849, 12, 31)?), Absent);
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

    // --- Generators ---
    //
    // Every law below turns on *relative* order and precision; absolute years
    // carry none of it. So a lifespan's dates and the instant it is classified
    // at come out of one narrow window, drawn once per case. Sampled
    // independently over three millennia the instant sits far outside every
    // hull, where each law holds for the trivial reason and the edges the laws
    // are actually about — `at == hull.hi`, `at == cap`, one day either side —
    // are never reached at all. The window makes those collisions possible;
    // `arb_instant_for`, which reads candidate days off the lifespan itself,
    // makes them routine.
    //
    // The composed generators are `BoxedStrategy` rather than `impl Strategy`:
    // the nested `prop_oneof!`/`prop_flat_map` types are deep enough that an
    // unboxed `new_tree` chain overflows a test thread's stack in a debug build.

    /// Half-width of the shared window, in years: wide enough to order several
    /// assertions against each other, narrow enough that an instant collides
    /// with a hull edge instead of missing it by centuries.
    const WINDOW: i32 = 10;

    /// Map a contiguous integer axis onto the years a [`DateBound`] accepts.
    /// There is no year 0 — the convention here is −1 for 1 BCE — and
    /// `DateBound::new` rejects it, so the axis skips it on the way out. That
    /// keeps a window straddling the era seam one solid run of valid years;
    /// filtering year 0 afterwards would punch a hole in the middle of the
    /// window and pull its two halves apart.
    fn bound_year(axis: i32) -> i32 {
        if axis >= 0 { axis + 1 } else { axis }
    }

    /// The window's center. The wide band keeps extreme years in play; the
    /// narrow ones make windows drawn independently overlap each other, and put
    /// the BCE/CE seam — where the year axis skips and the decade and century
    /// arithmetic changes sign — inside the window often rather than once in a
    /// hundred runs.
    fn arb_base_axis() -> impl Strategy<Value = i32> {
        prop_oneof![
            2 => -WINDOW..=WINDOW,
            3 => -100i32..=100,
            2 => -3000i32..=3000,
        ]
    }

    /// Draw one window, then everything a law compares from inside it.
    fn windowed<S: Strategy>(f: impl Fn(i32) -> S) -> impl Strategy<Value = S::Value> {
        arb_base_axis().prop_flat_map(f)
    }

    /// Month and day, weighted onto the precision-period edges. Every year,
    /// decade, century and millennium period starts Jan 1 and ends Dec 31, so
    /// those two days are the only ones at which an instant can *equal* a hull
    /// edge or a demolition cap — a uniform draw reaches either about once in
    /// 370, which is to say never within a case budget.
    fn arb_month_day() -> BoxedStrategy<(u32, u32)> {
        prop_oneof![
            6 => Just((1u32, 1u32)),
            6 => Just((12u32, 31u32)),
            1 => Just((1u32, 2u32)),
            1 => Just((12u32, 30u32)),
            6 => (1u32..=12, 1u32..=31),
        ]
        .boxed()
    }

    /// [`DateBound::new`] snaps to the period start and `latest()` reads the
    /// period end, so mixing precisions is where the edge arithmetic actually
    /// runs. Year leads because it is what facts mostly carry; decade is the
    /// model's flagship vague case ("demolished in the 1890s").
    fn arb_precision() -> BoxedStrategy<DatePrecision> {
        prop_oneof![
            8 => Just(DatePrecision::Year),
            5 => Just(DatePrecision::Decade),
            3 => Just(DatePrecision::Month),
            3 => Just(DatePrecision::Day),
            2 => Just(DatePrecision::Century),
            1 => Just(DatePrecision::Millennium),
        ]
        .boxed()
    }

    /// A raw day inside the window, on the bound year axis.
    fn arb_bound_day(base: i32) -> BoxedStrategy<NaiveDate> {
        ((base - WINDOW)..=(base + WINDOW), arb_month_day())
            .prop_filter_map("valid calendar date", |(axis, (m, d))| {
                NaiveDate::from_ymd_opt(bound_year(axis), m, d)
            })
            .boxed()
    }

    /// An instant. `NaiveDate` accepts year 0, and it is a legitimate moment to
    /// classify at — the era seam itself, which no bound can name — so instants
    /// read the axis without the skip.
    ///
    /// Mostly from the window, where every boundary is; occasionally far
    /// outside it, because the forward presumption runs to +∞ and the past
    /// before any evidence runs to −∞, and a window the assertions fill can
    /// reach neither.
    fn arb_instant_in(base: i32) -> BoxedStrategy<NaiveDate> {
        fn axis_span(base: i32, half_width: i32) -> BoxedStrategy<NaiveDate> {
            ((base - half_width)..=(base + half_width), arb_month_day())
                .prop_filter_map("valid calendar date", |(axis, (m, d))| {
                    NaiveDate::from_ymd_opt(axis, m, d)
                })
                .boxed()
        }
        prop_oneof![
            6 => axis_span(base, WINDOW),
            1 => axis_span(base, WINDOW * 20),
        ]
        .boxed()
    }

    fn arb_bound(base: i32) -> BoxedStrategy<DateBound> {
        (arb_bound_day(base), arb_precision())
            .prop_filter_map("valid bound", |(day, precision)| {
                DateBound::new(day, precision).ok()
            })
            .boxed()
    }

    /// A closed `[lo, hi]` at one precision. `None` only for a year-0 bound,
    /// which the year axis already excludes.
    fn closed_at(lo: NaiveDate, hi: NaiveDate, precision: DatePrecision) -> Option<UncertainDate> {
        let lo = DateBound::new(lo, precision).ok()?;
        let hi = DateBound::new(hi, precision).ok()?;
        UncertainDate::bounded(Some(lo), Some(hi)).ok()
    }

    /// One assertion's date, weighted toward the closed shapes. An open ray
    /// anchors a single hull edge and a fully-unknown date anchors none, so a
    /// bag of them exercises the identity path over and over and the hull
    /// arithmetic never.
    fn arb_udate_in(base: i32) -> BoxedStrategy<UncertainDate> {
        prop_oneof![
            // "1927", "the 1890s" — one bound at one precision, the shape a
            // fact usually carries.
            6 => (arb_bound_day(base), arb_precision())
                .prop_filter_map("point date", |(day, precision)| closed_at(day, day, precision)),
            // A circa-tightened span, its two ends free to differ in precision.
            5 => (arb_bound(base), arb_bound(base))
                .prop_filter_map("closed span", |(a, b)| {
                    // Ordering by period start is enough: the lower bound's
                    // start is then <= the upper bound's start <= its end.
                    let (lo, hi) = if a.period_start() <= b.period_start() {
                        (a, b)
                    } else {
                        (b, a)
                    };
                    UncertainDate::bounded(Some(lo), Some(hi)).ok()
                }),
            2 => arb_bound(base)
                .prop_filter_map("before", |b| UncertainDate::bounded(None, Some(b)).ok()),
            2 => arb_bound(base)
                .prop_filter_map("after", |b| UncertainDate::bounded(Some(b), None).ok()),
            1 => Just(UncertainDate::unknown()),
        ]
        .boxed()
    }

    /// One assertion in one of the five slots. `demolition.completed` is
    /// weighted up because it is the only slot that caps the future: without a
    /// cap there is nothing for later evidence to refute, and the refutation
    /// laws run on an empty antecedent.
    fn arb_assertion_in(base: i32) -> BoxedStrategy<Lifespan> {
        let slot = prop_oneof![
            4 => Just(0u8),
            2 => Just(1u8),
            2 => Just(2u8),
            4 => Just(3u8),
            5 => Just(4u8),
        ];
        (slot, arb_udate_in(base))
            .prop_map(|(slot, ud)| match slot {
                0 => Lifespan::construction_started(ud),
                1 => Lifespan::construction_completed(ud),
                2 => Lifespan::demolition_started(ud),
                3 => Lifespan::demolition_completed(ud),
                _ => Lifespan::witness(ud),
            })
            .boxed()
    }

    /// An arbitrary bag of assertions — sources that need not agree, or even
    /// make sense together.
    fn arb_chaotic_lifespan_in(base: i32) -> BoxedStrategy<Lifespan> {
        prop::collection::vec(arb_assertion_in(base), 0..=6)
            .prop_map(fold)
            .boxed()
    }

    /// A coherent record: built, then seen, then removed, in that order. Drawn
    /// independently a born-after-dead entity is as likely as an ordinary
    /// building, which leaves the case the map mostly renders rarer than the
    /// pathological one.
    ///
    /// One precision across the whole record, because `period_start` and
    /// `period_end` are monotone in the raw day only within a single precision.
    /// Mix them and a century-precision sighting outlives a year-precision
    /// demolition — a genuine shape, and one the chaotic generator already
    /// reaches, but not a coherent one.
    fn arb_coherent_lifespan_in(base: i32) -> BoxedStrategy<Lifespan> {
        (
            prop::array::uniform5(arb_bound_day(base)),
            arb_precision(),
            0usize..=2,
            0u8..3,
        )
            .prop_filter_map(
                "coherent record",
                |(mut days, precision, sightings, ending)| {
                    days.sort_unstable();
                    let [start, built, seen_once, seen_again, last] = days;
                    let mut parts = vec![Lifespan::construction_started(closed_at(
                        start, built, precision,
                    )?)];
                    for day in [seen_once, seen_again].into_iter().take(sightings) {
                        parts.push(Lifespan::witness(closed_at(day, day, precision)?));
                    }
                    let last = closed_at(last, last, precision)?;
                    parts.push(match ending {
                        0 => Lifespan::demolition_completed(last),
                        1 => Lifespan::demolition_started(last),
                        // Still standing: no demolition on record at all.
                        _ => Lifespan::witness(last),
                    });
                    Some(fold(parts))
                },
            )
            .boxed()
    }

    fn arb_lifespan_in(base: i32) -> BoxedStrategy<Lifespan> {
        prop_oneof![
            2 => arb_chaotic_lifespan_in(base),
            1 => arb_coherent_lifespan_in(base),
        ]
        .boxed()
    }

    fn arb_lifespan() -> impl Strategy<Value = Lifespan> {
        windowed(arb_lifespan_in)
    }

    /// The four days `classify` compares an instant against — the hull's two
    /// edges, the construction floor, the demolition cap — and the day either
    /// side of each. Between them they are every boundary the model has.
    fn boundary_days(ls: &Lifespan) -> Vec<NaiveDate> {
        let mut days = Vec::new();
        let mut push = |day: Option<NaiveDate>| {
            if let Some(day) = day {
                days.push(day);
                days.extend(day.pred_opt());
                days.extend(day.succ_opt());
            }
        };
        push(ls.hull.lo());
        push(ls.hull.hi());
        push(match ls.birth {
            BirthBound::Floored(day) => Some(day),
            BirthBound::Open => None,
        });
        push(match ls.death {
            DeathBound::Capped(day) => Some(day),
            DeathBound::Ongoing | DeathBound::Open => None,
        });
        days
    }

    /// An instant to read `ls` at: half from the shared window, half off `ls`'s
    /// own boundaries. The window alone makes a boundary collision *possible* —
    /// a bound's period end is one of ~20 years × 365 days — where reading the
    /// day off the value makes it routine, which is what an assertion about
    /// `at == hull.hi` needs to be worth writing.
    fn arb_instant_for(base: i32, ls: &Lifespan) -> BoxedStrategy<NaiveDate> {
        let days = boundary_days(ls);
        if days.is_empty() {
            return arb_instant_in(base);
        }
        prop_oneof![
            1 => arb_instant_in(base),
            1 => prop::sample::select(days),
        ]
        .boxed()
    }

    fn arb_lifespan_and_instant() -> impl Strategy<Value = (Lifespan, NaiveDate)> {
        windowed(|base| {
            arb_lifespan_in(base).prop_flat_map(move |ls| {
                let at = arb_instant_for(base, &ls);
                (Just(ls), at)
            })
        })
    }

    /// A lifespan, an instant, and a second instant at or after it. The later
    /// one is a forward offset rather than the larger of two independent draws:
    /// sorting would make the antecedent "the *earlier* of two is presumed",
    /// which is far rarer than "an instant is presumed" and starves every
    /// upward-closure law of cases.
    fn arb_lifespan_and_forward_pair() -> impl Strategy<Value = (Lifespan, NaiveDate, NaiveDate)> {
        let ahead = prop_oneof![
            1 => 0u64..400,
            1 => 0u64..100_000,
        ];
        (arb_lifespan_and_instant(), ahead).prop_filter_map(
            "the later instant is a representable date",
            |((ls, earlier), ahead)| {
                let later = earlier.checked_add_days(chrono::Days::new(ahead))?;
                Some((ls, earlier, later))
            },
        )
    }

    fn arb_lifespan_and_three_instants() -> impl Strategy<Value = (Lifespan, [NaiveDate; 3])> {
        windowed(|base| {
            arb_lifespan_in(base).prop_flat_map(move |ls| {
                let days = prop::array::uniform3(arb_instant_for(base, &ls));
                (Just(ls), days)
            })
        })
    }

    /// Two lifespans and an instant. The instant comes off the *first*, since
    /// every law shaped this way states its antecedent about that one and folds
    /// the second in afterwards.
    fn arb_two_lifespans_and_instant() -> impl Strategy<Value = (Lifespan, Lifespan, NaiveDate)> {
        windowed(|base| {
            (arb_lifespan_in(base), arb_lifespan_in(base)).prop_flat_map(move |(a, b)| {
                let at = arb_instant_for(base, &a);
                (Just(a), Just(b), at)
            })
        })
    }

    fn arb_lifespan_date_and_instant() -> impl Strategy<Value = (Lifespan, UncertainDate, NaiveDate)>
    {
        windowed(|base| {
            (arb_lifespan_in(base), arb_udate_in(base)).prop_flat_map(move |(ls, date)| {
                let at = arb_instant_for(base, &ls);
                (Just(ls), Just(date), at)
            })
        })
    }

    /// A demolition our own sighting outlives, plus an instant read off the
    /// pair once folded.
    fn arb_refuted_demolition_and_instant()
    -> impl Strategy<Value = (UncertainDate, UncertainDate, NaiveDate)> {
        windowed(|base| {
            (arb_udate_in(base), arb_udate_in(base))
                .prop_filter(
                    "the sighting outlives the demolition's cap",
                    |(demolition, sighting)| {
                        matches!(
                            (demolition.latest(), hull_edges(sighting).1),
                            (Some(cap), Some(edge)) if edge > cap
                        )
                    },
                )
                .prop_flat_map(move |(demolition, sighting)| {
                    let ls = fold([
                        Lifespan::demolition_completed(demolition.clone()),
                        Lifespan::witness(sighting.clone()),
                    ]);
                    let at = arb_instant_for(base, &ls);
                    (Just(demolition), Just(sighting), at)
                })
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
        fn classify_matches_the_denotational_partition(
            (ls, at) in arb_lifespan_and_instant(),
        ) {
            let denied = ls.birth.denies(at) || ls.death.denies(at);
            let affirmed = ls.hull.contains(at);
            let presumes = match ls.death {
                DeathBound::Open => true,
                DeathBound::Ongoing => false,
                DeathBound::Capped(cap) => matches!(ls.live, LiveEdge(Some(e)) if e > cap),
            };
            let presumed_reach = presumes && ls.hull.hi().is_some_and(|h| at > h);
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
            (ls, at) in arb_lifespan_and_instant(),
        ) {
            let supported = matches!(ls.classify(at), Uncontested | Contested | Presumed);
            prop_assert_eq!(ls.support().contains(at), supported);
        }

        /// An instant is the degenerate interval, so the two reads agree there.
        /// This is the contract a timed query rests on: asking about a moment is
        /// asking about `[T, T]`.
        #[test]
        fn overlaps_at_a_degenerate_interval_is_contains(
            (ls, at) in arb_lifespan_and_instant(),
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
            (ls, start) in arb_lifespan_and_instant(),
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

        /// Evidence never un-affirms: whatever the hull vouches for survives every
        /// later fold. It is what lets a cache combine partial folds in any order
        /// and read the result as a sound lower bound on support — an affirm
        /// channel that ever narrowed (an intersection, a latest-wins) would make a
        /// warm-started fold disagree with a cold one about what is confirmed.
        #[test]
        fn affirmation_is_monotone_under_combine(
            (a, b, at) in arb_two_lifespans_and_instant()
                .prop_filter("the hull affirms the instant", |(a, _, at)| a.hull.contains(*at)),
        ) {
            prop_assert!(a.combine(b).hull.contains(at));
        }

        /// A denial never withdraws: a source that rules an instant out keeps
        /// ruling it out however much evidence arrives afterwards. Contradicting
        /// evidence turns the instant contested instead, and the deny channel is
        /// where the disagreement is recorded — so a `combine` letting a later
        /// affirmation erase a denial would drop a source conflict on the floor.
        #[test]
        fn denial_is_monotone_under_combine(
            (a, b, at) in arb_two_lifespans_and_instant()
                .prop_filter("the instant is denied", |(a, _, at)| a.denies(*at)),
        ) {
            prop_assert!(a.combine(b).denies(at));
        }

        /// Contested absorbs: once an instant is disputed, no later evidence quiets
        /// it. This does not follow from the two monotonicity laws — contested is
        /// `supported ∧ denied`, and `supported` includes the presumption, which a
        /// demolition withdraws. It survives because the only supported-and-denied
        /// route through the presumption is a *refuted* demolition, and refutation
        /// is monotone: the evidence edge only advances and the cap only retreats.
        /// An edit letting some assertion withdraw a presumption while leaving the
        /// instant unaffirmed would turn a live disagreement back into a verdict.
        #[test]
        fn contested_is_absorbing(
            (a, b, at) in arb_two_lifespans_and_instant()
                .prop_filter("contested at the instant", |(a, _, at)| a.classify(*at) == Contested),
        ) {
            prop_assert_eq!(a.combine(b).classify(at), Contested);
        }

        /// The admitted instants are one interval: denial is exactly "outside
        /// \[earliest start, latest completion\]". A hole in the middle would mean
        /// something denies an interior instant — a power the model grants nothing —
        /// and would break the single-interval shape of [`SupportWindow`], which an
        /// index prunes on.
        #[test]
        fn the_admitted_instants_are_contiguous(
            (ls, days) in arb_lifespan_and_three_instants()
                .prop_map(|(ls, mut days)| {
                    days.sort_unstable();
                    (ls, days)
                })
                .prop_filter("the outer instants are admitted", |(ls, days)| {
                    !ls.denies(days[0]) && !ls.denies(days[2])
                }),
        ) {
            prop_assert!(!ls.denies(days[1]));
        }

        /// Only a construction's start and a demolition's completion deny. A
        /// completion, a removal in progress, and a sighting affirm and nothing
        /// more. This is the line between *suppressing* the presumption and
        /// *denying* existence: a `demolition.started` that slipped into the deny
        /// channel would paint the future absent — "definitely gone" — where the
        /// record only says removal had begun.
        #[test]
        fn only_the_two_bookend_ends_deny(
            (ls, date, at) in arb_lifespan_date_and_instant(),
        ) {
            let denied = ls.denies(at);
            for silent in [
                Lifespan::construction_completed(date.clone()),
                Lifespan::demolition_started(date.clone()),
                Lifespan::witness(date.clone()),
            ] {
                prop_assert_eq!(ls.clone().combine(silent).denies(at), denied);
            }
        }

        /// The presumption is upward-closed, and all the way to `Presumed` — not
        /// merely to "still supported". Nothing can start denying later than a
        /// presumed instant: a construction denies only the past, and a demolition
        /// able to deny the future has either suppressed the presumption outright or
        /// been refuted, in which case it already denies at the presumed instant
        /// itself. So the forward tail neither decays back to unknown nor breaks
        /// into contested partway along.
        #[test]
        fn the_presumption_is_upward_closed_in_time(
            (ls, _, later) in arb_lifespan_and_forward_pair()
                .prop_filter("presumed at the earlier instant", |(ls, earlier, _)| {
                    ls.classify(*earlier) == Presumed
                }),
        ) {
            prop_assert_eq!(ls.classify(later), Presumed);
        }

        /// A demolition our own evidence outlives paints no absent instant
        /// anywhere. The honest reading of "we hold no undisputed record that it
        /// ever came down": the sighting and the demolition disagree, and a
        /// disagreement renders contested — never as a definite absence resting on
        /// the claim the sighting contradicts.
        #[test]
        fn a_refuted_demolition_leaves_no_absent_instant(
            (demolition, sighting, at) in arb_refuted_demolition_and_instant(),
        ) {
            let ls = fold([
                Lifespan::demolition_completed(demolition),
                Lifespan::witness(sighting),
            ]);
            prop_assert_ne!(ls.classify(at), Absent);
        }

        /// Order- and duplicate-independence: the fold is the same under any
        /// permutation and any duplication of the assertion bag.
        #[test]
        fn fold_is_order_and_duplicate_insensitive(
            mut assertions in windowed(|base| prop::collection::vec(arb_assertion_in(base), 0..=6)),
        ) {
            let straight = fold(assertions.clone());
            let doubled = fold(assertions.iter().cloned().chain(assertions.iter().cloned()));
            prop_assert_eq!(&straight, &doubled);

            assertions.reverse();
            prop_assert_eq!(&straight, &fold(assertions));
        }
    }
}
