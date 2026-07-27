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
//! A demolition switches that forward tail off, and what brings it back depends
//! on the instant the removal is claimed at. Dated completions our own evidence
//! outlives are *refuted*, and the tail resumes — contested rather than absent,
//! since they keep denying. A removal claimed at no nameable instant — a start, a
//! completion dated only "after Y" — is a completion at +∞: it denies nothing,
//! and no evidence reaches past it, so it keeps the tail off however late the
//! record runs. Both travel together, since a source can name one while another
//! names the other. Refuting is deliberately hard to earn. Evidence counts at the
//! instant it *guarantees*, the earliest its date could name, so a vague claim
//! testifies to no moment it merely might describe; it has to clear *every*
//! completion claimed, since a rival account of the removal that fits the record
//! is still an account of it; and a completion never refutes another, because
//! rival completions are alternatives and one must not discredit another.
//!
//! `Lifespan` is an opaque commutative-monoid lattice element: a downstream cache
//! folds it through [`combine`](crate::algebra::monoid::CommutativeMonoid::combine)
//! without reading its internals. The four axes are private; the trait surface is
//! the whole contract. Each axis is its own bounded join, and no value reachable
//! through that surface is incoherent: the sum types carry a date only where one
//! exists, and the two conditions spanning axes — a guarantee inside the hull that
//! affirms it, an ordered cap envelope — are enforced where a byte string enters.

use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::algebra::monoid::CommutativeMonoid;
use crate::date::{DayRange, UncertainDate};

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
    /// The one stretch of instants the sources place the entity at.
    Span(SupportSpan),
}

/// A non-empty supported stretch: from [`SupportSpan::from`] through
/// [`SupportSpan::through`], both inclusive. `through` absent runs forward
/// without bound — the closed-world presumption that a built thing stands while
/// no record of its removal stands unrefuted.
///
/// Ordered by construction. An inverted span would claim support over a stretch
/// holding no instant, and the range predicates below — which read the two edges
/// independently — would answer it as if it held every instant between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SupportSpan {
    from: NaiveDate,
    through: Option<NaiveDate>,
}

impl<'de> Deserialize<'de> for SupportSpan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            from: NaiveDate,
            through: Option<NaiveDate>,
        }
        let raw = Raw::deserialize(deserializer)?;
        SupportSpan::new(raw.from, raw.through).ok_or_else(|| {
            serde::de::Error::custom("support span ends before it starts, so it holds no instant")
        })
    }
}

impl SupportSpan {
    /// The span `[from, through]`, or `None` when the two are inverted.
    fn new(from: NaiveDate, through: Option<NaiveDate>) -> Option<Self> {
        through
            .is_none_or(|t| from <= t)
            .then_some(Self { from, through })
    }

    /// The first supported instant.
    pub fn from(self) -> NaiveDate {
        self.from
    }

    /// The last supported instant, or `None` for the unbounded forward tail.
    pub fn through(self) -> Option<NaiveDate> {
        self.through
    }
}

impl SupportWindow {
    /// Whether the sources place the entity at `at`.
    pub fn contains(self, at: NaiveDate) -> bool {
        match self {
            SupportWindow::Empty => false,
            SupportWindow::Span(span) => span.from <= at && span.through.is_none_or(|t| at <= t),
        }
    }

    /// Whether the entity is placed anywhere within `range` — the existential
    /// reading a bbox+interval query wants.
    pub fn overlaps(self, range: DayRange) -> bool {
        match self {
            SupportWindow::Empty => false,
            SupportWindow::Span(span) => {
                span.from <= range.end() && span.through.is_none_or(|t| range.start() <= t)
            }
        }
    }
}

/// The affirmed hull — the convex span every assertion vouches for. `None` on an
/// edge is that side's fold identity (a bound the fold never anchored), so an
/// un-anchored side yields to any present one under `combine`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

/// The envelope of the dated completions on record — the earliest and the latest
/// instant any of them places the removal at, equal when there is only one.
///
/// The two edges answer different questions because rival completions are
/// *alternatives*, not joint claims. Denying takes any one of them, so the
/// earliest cap rules: it denies the most future. Refuting takes all of them at
/// once, so the latest cap rules: evidence that outlives only the earliest still
/// leaves the later claim standing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct CapEnvelope {
    denies_after: NaiveDate,
    claimed_through: NaiveDate,
}

impl<'de> Deserialize<'de> for CapEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            denies_after: NaiveDate,
            claimed_through: NaiveDate,
        }
        let raw = Raw::deserialize(deserializer)?;
        if raw.denies_after > raw.claimed_through {
            return Err(serde::de::Error::custom(
                "demolition cap envelope inverted: the earliest completion claimed is after the latest",
            ));
        }
        Ok(CapEnvelope {
            denies_after: raw.denies_after,
            claimed_through: raw.claimed_through,
        })
    }
}

impl CapEnvelope {
    /// The envelope one completion claim spans on its own.
    fn at(day: NaiveDate) -> Self {
        Self {
            denies_after: day,
            claimed_through: day,
        }
    }

    /// Join: widen to hold both claims, each edge keeping the rival that rules it.
    fn join(self, other: Self) -> Self {
        Self {
            denies_after: self.denies_after.min(other.denies_after),
            claimed_through: self.claimed_through.max(other.claimed_through),
        }
    }
}

/// The deny-after axis, which also gates the forward presumption: the removals
/// on record, split by whether they name an instant to be measured against.
///
/// A removal named at no instant — a start, a completion dated only "after Y" —
/// is a completion at +∞. That is why the two channels stand side by side
/// instead of in a chain: denying takes the *earliest* completion claimed, where
/// +∞ never wins, and refuting has to clear the *latest*, where +∞ always does.
/// Ranked into one scalar, whichever end lost would take its question's answer
/// with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeathBound {
    /// A removal claimed at +∞, which nothing our evidence guarantees is past.
    unrefutable: bool,
    /// The envelope of the dated completions, absent when none is on record.
    caps: Option<CapEnvelope>,
}

/// The forward edge of *guaranteed* existence: the latest instant an assertion
/// puts the entity beyond doubt at, `None` its −∞ fold identity.
///
/// An assertion's true moment lies somewhere in its date's range, so the range's
/// **earliest** edge is the one it guarantees — everything past that is only where
/// the moment *might* fall. A "before Y" guarantees nothing at all, since its
/// moment may lie arbitrarily far back.
///
/// This is the yardstick a demolition claim is measured against: evidence
/// guaranteed past every instant a completion is claimed at refutes them and stops
/// suppressing the presumption. `demolition.completed` is the one slot held out of
/// it — completions are rivals, and one rival must never discredit another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct LiveEdge(Option<NaiveDate>);

/// The existence accumulator over four independent join axes: the affirmed `Hull`,
/// the guaranteed-existence `LiveEdge`, the deny-before `BirthBound`, and the
/// deny-after / presumption-gate `DeathBound`. Each axis is an idempotent
/// semilattice, so `combine` is a commutative, associative, idempotent monoid
/// operation and the whole value is a state-CRDT a cache can warm-start.
///
/// The axes are the entire private state; the public contract is the trait surface
/// ([`CommutativeMonoid`], `Serialize`/`Deserialize`, `Eq`) plus
/// [`classify`](Self::classify). A caller reads existence through `classify`, not
/// through the axes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Lifespan {
    hull: Hull,
    live: LiveEdge,
    birth: BirthBound,
    death: DeathBound,
}

impl<'de> Deserialize<'de> for Lifespan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            hull: Hull,
            live: LiveEdge,
            birth: BirthBound,
            death: DeathBound,
        }
        let raw = Raw::deserialize(deserializer)?;
        // A guarantee is the earliest instant its date could name, and a
        // construction's floor is the earliest instant of the range that same
        // assertion affirms, so the fold lifts neither past `hull.hi`. Crossing
        // either on the wire hands a denied instant a verdict the record does
        // not carry: a guarantee past the hull reads presumed where a demolition
        // denies, and a floor past it reads the entity's own affirmed years as a
        // source disagreement.
        let hull_hi = raw.hull.hi();
        let within_affirmed_hull = |edge: Option<NaiveDate>| match (edge, hull_hi) {
            (None, _) => true,
            (Some(edge), Some(hi)) => edge <= hi,
            (Some(_), None) => false,
        };
        if !within_affirmed_hull(raw.live.0) {
            return Err(serde::de::Error::custom(
                "lifespan guarantees existence past the hull it affirms",
            ));
        }
        if !within_affirmed_hull(raw.birth.floor()) {
            return Err(serde::de::Error::custom(
                "lifespan floors its construction past the hull it affirms",
            ));
        }
        Ok(Lifespan {
            hull: raw.hull,
            live: raw.live,
            birth: raw.birth,
            death: raw.death,
        })
    }
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

    /// The floored start, absent when no construction anchored one.
    fn floor(self) -> Option<NaiveDate> {
        match self {
            BirthBound::Floored(day) => Some(day),
            BirthBound::Open => None,
        }
    }

    /// A floored start denies every instant strictly before it.
    fn denies(self, at: NaiveDate) -> bool {
        matches!(self, BirthBound::Floored(d) if at < d)
    }
}

impl DeathBound {
    /// No removal on record — the join identity.
    const OPEN: DeathBound = DeathBound {
        unrefutable: false,
        caps: None,
    };

    /// A removal claimed at no nameable instant.
    const UNREFUTABLE: DeathBound = DeathBound {
        unrefutable: true,
        caps: None,
    };

    /// A completion claimed at `day`.
    fn capped(day: NaiveDate) -> Self {
        DeathBound {
            unrefutable: false,
            caps: Some(CapEnvelope::at(day)),
        }
    }

    /// Join each channel on its own, so a dated completion arriving beside an
    /// open-ended one leaves both on the record.
    fn combine(self, other: Self) -> Self {
        DeathBound {
            unrefutable: self.unrefutable || other.unrefutable,
            caps: match (self.caps, other.caps) {
                (None, caps) | (caps, None) => caps,
                (Some(a), Some(b)) => Some(a.join(b)),
            },
        }
    }

    /// A dated completion denies every instant strictly after it — the earliest
    /// claimed, since one source ruling the instant out is enough.
    fn denies(self, at: NaiveDate) -> bool {
        self.caps.is_some_and(|caps| at > caps.denies_after)
    }
}

impl LiveEdge {
    /// The −∞ identity: nothing guarantees existence at any instant.
    const NONE: LiveEdge = LiveEdge(None);

    /// The edge an assertion guarantees — the earliest instant its date could
    /// name. Its moment lies at or after that, so existence there is beyond
    /// doubt, where the later edges of its range are only where the moment might
    /// have fallen. An open-below date ("before Y") guarantees nothing.
    fn guaranteed_by(date: &UncertainDate) -> Self {
        LiveEdge(date.earliest())
    }

    /// Join: keep the later edge, since the furthest guarantee is the one a
    /// demolition has to survive.
    fn combine(self, other: Self) -> Self {
        LiveEdge(max_bound(self.0, other.0))
    }

    /// Whether our evidence guarantees existence past `cap` — a moment the
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
        death: DeathBound::OPEN,
    };

    /// What any assertion contributes before its slot's own powers: it affirms
    /// across its hull, and guarantees existence at that date's earliest edge.
    /// Both read the one date the same way for every slot, so both are anchored
    /// here and each slot below adds only what makes it that slot.
    fn asserting(date: &UncertainDate) -> Self {
        Self {
            hull: Hull::spanning(date),
            live: LiveEdge::guaranteed_by(date),
            birth: BirthBound::Open,
            death: DeathBound::OPEN,
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
            ..Self::asserting(&date)
        }
    }

    /// A `construction.completed` assertion. Affirms across its range and denies
    /// nothing — completion is a pure affirmer, not a deny-before.
    pub fn construction_completed(date: UncertainDate) -> Self {
        Self::asserting(&date)
    }

    /// A `demolition.started` assertion. A removal under way puts its completion
    /// at no nameable instant, so it denies nothing and holds the forward
    /// presumption off for good. It affirms at its own date like any assertion,
    /// which is what makes a completion claimed earlier read as a disagreement
    /// across the years between the two.
    pub fn demolition_started(date: UncertainDate) -> Self {
        Self {
            death: DeathBound::UNREFUTABLE,
            ..Self::asserting(&date)
        }
    }

    /// A `demolition.completed` assertion. Caps the deniable future at its latest
    /// possible completion; a completion dated only "after Y" names no instant to
    /// cap at, and is the removal at +∞ a start also claims.
    ///
    /// The one slot that guarantees nothing: completions are rival hypotheses
    /// about the same terminal event, so letting one guarantee existence at its
    /// own date would let the later claim refute the earlier — two sources
    /// agreeing the entity is gone, read as evidence that it survived.
    pub fn demolition_completed(date: UncertainDate) -> Self {
        let death = match date.latest() {
            Some(end) => DeathBound::capped(end),
            None => DeathBound::UNREFUTABLE,
        };
        Self {
            live: LiveEdge::NONE,
            death,
            ..Self::asserting(&date)
        }
    }

    /// An existence witness — a sighting or an interior-event endpoint. Affirms
    /// across its range and denies nothing.
    pub fn witness(date: UncertainDate) -> Self {
        Self::asserting(&date)
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
        let span = match self.hull.lo() {
            // A hull that contains something: supported from its lower edge, and
            // onward without bound while the presumption runs.
            Some(lo) if lo <= hi => SupportSpan::new(lo, (!presumes).then_some(hi)),
            // The hull affirms nothing — unanchored below, or crossed by a
            // same-direction pair of open claims — so only the forward
            // presumption can support anything, and only past `hi`.
            _ if presumes => hi.succ_opt().and_then(|from| SupportSpan::new(from, None)),
            _ => None,
        };
        span.map_or(SupportWindow::Empty, SupportWindow::Span)
    }

    /// Whether an assertion rules existence at `at` impossible — the deny channel,
    /// which only a construction's earliest start and a demolition's latest
    /// completion feed.
    fn denies(&self, at: NaiveDate) -> bool {
        self.birth.denies(at) || self.death.denies(at)
    }

    /// Whether the forward closed-world presumption runs past the affirmed hull.
    ///
    /// A demolition on record normally withdraws it. Dated completions our own
    /// evidence outlives are refuted, and a refuted claim hands the presumption
    /// back: reading the tail as absent would report the entity definitely gone on
    /// the authority of a claim we hold evidence against. They keep denying, so
    /// the tail reads contested — support and denial both on the record, which is
    /// what a source disagreement looks like.
    ///
    /// Refuting means outliving the *latest* completion claimed, not the earliest.
    /// Rival completions are alternatives, so evidence that clears only the
    /// earliest leaves the later one an intact account of the entity's removal.
    ///
    /// A removal claimed at no nameable instant — a start, a completion dated only
    /// "after Y" — is the latest completion there can be, so a sighting however
    /// late is consistent with it and it suppresses whatever else the record holds.
    fn presumes(&self) -> bool {
        !self.death.unrefutable
            && self
                .death
                .caps
                .is_none_or(|caps| self.live.outlives(caps.claimed_through))
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
    use proptest::test_runner::TestCaseError;
    use strum::IntoEnumIterator;

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

    /// A whole century, e.g. "the 17th century" = `century(1601)` = \[1601, 1700\].
    fn century(y: i32) -> Ud {
        Ok(UncertainDate::with_precision(
            nd(y, 1, 1)?,
            DatePrecision::Century,
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

        // Support runs unbounded for the same reason the verdicts do, so a timed
        // query keeps reaching the entity past the claim its photograph outlived.
        let SupportWindow::Span(span) = ls.support() else {
            return Err("the refuted pair supports the span it affirms".into());
        };
        assert_eq!(span.from(), nd(1885, 1, 1)?);
        assert_eq!(span.through(), None);
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

    /// A sighting between two disputed demolition years refutes neither. It
    /// contradicts the earlier claim, but the later one accounts for it perfectly
    /// well — so one source still has an intact account of the entity's removal,
    /// and the entity is gone after the last year anyone claims.
    #[test]
    fn a_sighting_between_disputed_demolitions_refutes_neither() -> Res {
        let ls = fold([
            Lifespan::demolition_completed(year(1950)?),
            Lifespan::demolition_completed(year(1960)?),
            Lifespan::witness(year(1955)?),
        ]);
        assert_eq!(ls.classify(nd(1950, 6, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(1955, 6, 1)?), Contested);
        assert_eq!(ls.classify(nd(1960, 12, 31)?), Contested);
        assert_eq!(ls.classify(nd(1961, 1, 1)?), Absent);
        assert_eq!(ls.classify(nd(2500, 1, 1)?), Absent);
        Ok(())
    }

    /// A vague date guarantees existence only at its earliest edge. "Built in the
    /// 17th century" is consistent with a building finished in 1610 and gone by
    /// 1650, so it is no evidence against the demolition — reading the claim at
    /// the latest instant it could name would refute a demolition nothing
    /// contradicts, and hand the entity a contested present.
    #[test]
    fn a_vague_construction_does_not_outlive_a_demolition_inside_its_range() -> Res {
        let ls = fold([
            Lifespan::construction_started(century(1601)?),
            Lifespan::demolition_completed(year(1650)?),
        ]);
        assert_eq!(ls.classify(nd(1620, 1, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(1650, 12, 31)?), Uncontested);
        // Denied by the demolition and still inside the construction's hull.
        assert_eq!(ls.classify(nd(1651, 1, 1)?), Contested);
        assert_eq!(ls.classify(nd(1700, 12, 31)?), Contested);
        // Past the hull, with the demolition unrefuted: gone, not contested.
        assert_eq!(ls.classify(nd(1701, 1, 1)?), Absent);
        assert_eq!(ls.classify(nd(2000, 1, 1)?), Absent);
        Ok(())
    }

    /// Removal beginning in 1980 places the entity standing in 1980, which no
    /// completion in 1950 can account for — so the years between the two claims
    /// are a live disagreement, one source affirming where the other denies.
    ///
    /// The tail past 1980 is not: both sources say the entity came down, and the
    /// removal under way is the later account of it, claimed at an instant nothing
    /// we hold reaches past. Adding a second source that agrees on the removal
    /// cannot make the entity read as more present than the lone start does.
    #[test]
    fn a_demolition_beginning_after_a_claimed_completion_disputes_the_years_between() -> Res {
        let ls = fold([
            Lifespan::demolition_completed(year(1950)?),
            Lifespan::demolition_started(year(1980)?),
        ]);
        assert_eq!(ls.classify(nd(1949, 12, 31)?), Unknown);
        assert_eq!(ls.classify(nd(1950, 6, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(1951, 1, 1)?), Contested);
        assert_eq!(ls.classify(nd(1980, 12, 31)?), Contested);
        assert_eq!(ls.classify(nd(1981, 1, 1)?), Absent);
        assert_eq!(ls.classify(nd(2500, 1, 1)?), Absent);

        // Support ends where the start's own year does, as it does for the lone
        // start — a timed query cannot reach the pair any later than that.
        let SupportWindow::Span(span) = ls.support() else {
            return Err("the pair supports the span between the two claims".into());
        };
        assert_eq!(span.through(), Some(nd(1980, 12, 31)?));
        Ok(())
    }

    /// "Demolished sometime after 2000" names no instant to cap the future at, so
    /// nothing our evidence guarantees is past it. A 1950 photograph is exactly
    /// what that claim describes — a building still standing well before the
    /// removal — so it refutes nothing, and the 1940 rival's tail stands.
    #[test]
    fn an_open_ended_completion_survives_evidence_it_accounts_for() -> Res {
        let ls = fold([
            Lifespan::demolition_completed(after(2000)?),
            Lifespan::demolition_completed(year(1940)?),
            Lifespan::witness(year(1950)?),
        ]);
        assert_eq!(ls.classify(nd(1940, 6, 1)?), Uncontested);
        // The 1940 claim denies where the 1950 photograph affirms.
        assert_eq!(ls.classify(nd(1950, 6, 1)?), Contested);
        assert_eq!(ls.classify(nd(2000, 1, 1)?), Contested);
        assert_eq!(ls.classify(nd(2000, 1, 2)?), Absent);
        assert_eq!(ls.classify(nd(2500, 1, 1)?), Absent);
        Ok(())
    }

    /// A later completion claim settles a contested tail — the one place added
    /// evidence takes a disagreement *off* the map rather than putting one on it.
    ///
    /// While 1950 is the only claimed removal, the 1955 sighting contradicts it
    /// and the tail runs contested without bound. A second source dating the
    /// removal to 1960 accounts for that sighting, so nothing we hold outlives
    /// every claim any more: the record stops being self-contradictory after 1960
    /// and reads as a verdict again. The disputed era between the two claimed
    /// years stays contested, which is where the disagreement actually is.
    #[test]
    fn a_later_demolition_claim_settles_a_refuted_tail() -> Res {
        let refuted = fold([
            Lifespan::demolition_completed(year(1950)?),
            Lifespan::witness(year(1955)?),
        ]);
        assert_eq!(refuted.classify(nd(2000, 1, 1)?), Contested);

        let settled = refuted.combine(Lifespan::demolition_completed(year(1960)?));
        assert_eq!(settled.classify(nd(1955, 6, 1)?), Contested);
        assert_eq!(settled.classify(nd(1960, 12, 31)?), Contested);
        assert_eq!(settled.classify(nd(2000, 1, 1)?), Absent);
        Ok(())
    }

    /// A demolition that began before the completion claimed is the ordinary
    /// record of one removal, and nothing about it is contradictory: the start
    /// affirms 1949, the completion caps 1950, and the entity is gone after.
    #[test]
    fn a_demolition_started_before_its_completion_refutes_nothing() -> Res {
        let ls = fold([
            Lifespan::demolition_started(year(1949)?),
            Lifespan::demolition_completed(year(1950)?),
        ]);
        assert_eq!(ls.classify(nd(1949, 6, 1)?), Uncontested);
        assert_eq!(ls.classify(nd(1950, 12, 31)?), Uncontested);
        assert_eq!(ls.classify(nd(1951, 1, 1)?), Absent);
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

    /// Born-after-dead with a second demolition claim the construction *can*
    /// live with. Only part of this record contradicts itself: the 1850 claim
    /// cannot be squared with a 1900 construction, but the 1950 claim squares
    /// with all of it, so the entity is genuinely gone after 1950 and the
    /// contested band stops at the hull rather than running forever.
    #[test]
    fn born_after_dead_with_a_reconcilable_rival_ends_absent() -> Res {
        let ls = fold([
            Lifespan::construction_started(year(1900)?),
            Lifespan::demolition_completed(year(1850)?),
            Lifespan::demolition_completed(year(1950)?),
        ]);
        assert_eq!(ls.classify(nd(1849, 12, 31)?), Absent);
        assert_eq!(ls.classify(nd(1875, 1, 1)?), Contested);
        assert_eq!(ls.classify(nd(1900, 6, 1)?), Contested);
        assert_eq!(ls.classify(nd(1950, 12, 31)?), Contested);
        assert_eq!(ls.classify(nd(1951, 1, 1)?), Absent);
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

    // --- What a byte string may say ---

    /// The wire form round-trips, and cannot say what the fold cannot reach. An
    /// assertion guarantees existence no later than the hull edge it affirms to,
    /// so a value claiming otherwise would answer "presumed" — a clean
    /// soft-extant verdict — at instants a demolition denies.
    #[test]
    fn a_guarantee_past_the_affirmed_hull_is_rejected_on_the_wire() -> Res {
        let ls = fold([
            Lifespan::construction_started(year(1900)?),
            Lifespan::demolition_completed(year(1950)?),
        ]);
        let wire = serde_json::to_value(&ls)?;
        assert_eq!(serde_json::from_value::<Lifespan>(wire.clone())?, ls);

        let mut crossed = wire;
        let fields = crossed
            .as_object_mut()
            .ok_or("a lifespan serializes as an object")?;
        fields.insert("live".into(), serde_json::to_value(nd(2000, 1, 1)?)?);
        assert!(serde_json::from_value::<Lifespan>(crossed).is_err());
        Ok(())
    }

    /// A construction floors the past at the earliest instant of the range it
    /// affirms, so the fold cannot lift the floor past the hull either. A value
    /// claiming otherwise reads the entity's own affirmed years as contested — a
    /// source disagreement with only one source in it.
    #[test]
    fn a_construction_floor_past_the_affirmed_hull_is_rejected_on_the_wire() -> Res {
        let ls = fold([
            Lifespan::construction_started(year(1900)?),
            Lifespan::demolition_completed(year(1950)?),
        ]);
        let wire = serde_json::to_value(&ls)?;
        assert_eq!(serde_json::from_value::<Lifespan>(wire.clone())?, ls);

        let mut crossed = wire;
        let fields = crossed
            .as_object_mut()
            .ok_or("a lifespan serializes as an object")?;
        fields.insert(
            "birth".into(),
            serde_json::to_value(BirthBound::Floored(nd(2000, 1, 1)?))?,
        );
        assert!(serde_json::from_value::<Lifespan>(crossed).is_err());
        Ok(())
    }

    /// A support window that ended before it began would report every instant
    /// between its two edges as supported, since the range predicates read the
    /// edges independently.
    #[test]
    fn an_inverted_support_span_is_rejected_on_the_wire() -> Res {
        let window = fold([
            Lifespan::construction_started(year(1900)?),
            Lifespan::demolition_completed(year(1950)?),
        ])
        .support();
        let wire = serde_json::to_value(window)?;
        assert_eq!(
            wire,
            serde_json::json!({"kind": "span", "from": "1900-01-01", "through": "1950-12-31"}),
            "the span's edges sit beside the tag, not nested under it"
        );
        assert_eq!(
            serde_json::from_value::<SupportWindow>(wire.clone())?,
            window
        );

        let mut inverted = wire;
        let fields = inverted
            .as_object_mut()
            .ok_or("a support window serializes as an object")?;
        fields.insert("from".into(), serde_json::to_value(nd(1980, 1, 1)?)?);
        assert!(serde_json::from_value::<SupportWindow>(inverted).is_err());
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

    /// Which slot an assertion landed in. A folded [`Lifespan`] keeps no record
    /// of it — the axes are the join of every slot's contribution — so a bag of
    /// these is what the denotational oracle reads, and what lets a law say
    /// something about *the slot an assertion arrived in* at all.
    ///
    /// Every slot set below is `Slot::iter()` filtered by one of the predicates,
    /// and each predicate is an exhaustive match: a new slot is a compile error
    /// there, and answering it enrolls the slot in the generator and in every law
    /// at once.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumIter)]
    enum Slot {
        ConstructionStarted,
        ConstructionCompleted,
        DemolitionStarted,
        DemolitionCompleted,
        Witness,
    }

    impl Slot {
        /// Whether the slot vouches for existence at its own date — all of them
        /// but the completion, held out so one rival account of a removal cannot
        /// discredit another.
        fn guarantees_existence(self) -> bool {
            match self {
                Slot::ConstructionStarted
                | Slot::ConstructionCompleted
                | Slot::DemolitionStarted
                | Slot::Witness => true,
                Slot::DemolitionCompleted => false,
            }
        }

        /// Whether the slot puts a removal on the record, which withdraws the
        /// forward presumption.
        fn claims_a_removal(self) -> bool {
            match self {
                Slot::DemolitionStarted | Slot::DemolitionCompleted => true,
                Slot::ConstructionStarted | Slot::ConstructionCompleted | Slot::Witness => false,
            }
        }

        /// Whether the slot floors the deniable past at its own earliest instant.
        fn floors_the_past(self) -> bool {
            match self {
                Slot::ConstructionStarted => true,
                Slot::ConstructionCompleted
                | Slot::DemolitionStarted
                | Slot::DemolitionCompleted
                | Slot::Witness => false,
            }
        }

        /// Whether this slot's guarantee can refute a dated completion. It takes
        /// vouching for existence *and* leaving a presumption for that evidence
        /// to hand back: a started removal vouches while claiming a completion at
        /// no instant, so its own evidence has nothing left to resume.
        fn can_refute(self) -> bool {
            self.guarantees_existence() && !self.claims_a_removal()
        }

        /// How often the chaotic generator draws this slot. `demolition.completed`
        /// is weighted up because it is the only slot that caps the future:
        /// without a cap there is nothing for later evidence to refute, and the
        /// refutation laws run on an empty antecedent.
        fn weight(self) -> u32 {
            match self {
                Slot::ConstructionStarted => 4,
                Slot::ConstructionCompleted => 2,
                Slot::DemolitionStarted => 2,
                Slot::DemolitionCompleted => 4,
                Slot::Witness => 5,
            }
        }
    }

    /// One assertion, as the specification reads it: a slot and a date.
    #[derive(Debug, Clone)]
    struct Assertion {
        slot: Slot,
        date: UncertainDate,
    }

    impl Assertion {
        fn new(slot: Slot, date: UncertainDate) -> Self {
            Self { slot, date }
        }

        fn fold_in(&self) -> Lifespan {
            let date = self.date.clone();
            match self.slot {
                Slot::ConstructionStarted => Lifespan::construction_started(date),
                Slot::ConstructionCompleted => Lifespan::construction_completed(date),
                Slot::DemolitionStarted => Lifespan::demolition_started(date),
                Slot::DemolitionCompleted => Lifespan::demolition_completed(date),
                Slot::Witness => Lifespan::witness(date),
            }
        }
    }

    fn fold_bag(bag: &[Assertion]) -> Lifespan {
        fold(bag.iter().map(Assertion::fold_in))
    }

    /// The slots a predicate admits, drawn from the whole enumeration so a slot
    /// answering the predicate cannot sit out the law that reads it.
    fn slots(predicate: impl Fn(Slot) -> bool) -> Vec<Slot> {
        Slot::iter().filter(|slot| predicate(*slot)).collect()
    }

    /// One assertion in one of the slots, each drawn at [`Slot::weight`].
    fn arb_assertion_in(base: i32) -> BoxedStrategy<Assertion> {
        let slot = prop::strategy::Union::new_weighted(
            Slot::iter()
                .map(|slot| (slot.weight(), Just(slot)))
                .collect(),
        );
        (slot, arb_udate_in(base))
            .prop_map(|(slot, date)| Assertion::new(slot, date))
            .boxed()
    }

    /// An arbitrary bag of assertions — sources that need not agree, or even
    /// make sense together.
    fn arb_chaotic_bag_in(base: i32) -> BoxedStrategy<Vec<Assertion>> {
        prop::collection::vec(arb_assertion_in(base), 0..=6).boxed()
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
    fn arb_coherent_bag_in(base: i32) -> BoxedStrategy<Vec<Assertion>> {
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
                    let mut parts = vec![Assertion::new(
                        Slot::ConstructionStarted,
                        closed_at(start, built, precision)?,
                    )];
                    for day in [seen_once, seen_again].into_iter().take(sightings) {
                        parts.push(Assertion::new(
                            Slot::Witness,
                            closed_at(day, day, precision)?,
                        ));
                    }
                    let last = closed_at(last, last, precision)?;
                    parts.push(Assertion::new(
                        match ending {
                            0 => Slot::DemolitionCompleted,
                            1 => Slot::DemolitionStarted,
                            // Still standing: no demolition on record at all.
                            _ => Slot::Witness,
                        },
                        last,
                    ));
                    Some(parts)
                },
            )
            .boxed()
    }

    fn arb_bag_in(base: i32) -> BoxedStrategy<Vec<Assertion>> {
        prop_oneof![
            2 => arb_chaotic_bag_in(base),
            1 => arb_coherent_bag_in(base),
        ]
        .boxed()
    }

    fn arb_lifespan_in(base: i32) -> BoxedStrategy<Lifespan> {
        arb_bag_in(base).prop_map(|bag| fold_bag(&bag)).boxed()
    }

    fn arb_lifespan() -> impl Strategy<Value = Lifespan> {
        windowed(arb_lifespan_in)
    }

    // --- The denotational oracle ---
    //
    // The five states written straight from the model's set-predicates over a
    // bag of assertions, reaching for nothing the fold builds. An oracle that
    // read the accumulator's axes, or called the predicates it is checking,
    // would agree with a sign error inside them and report the agreement as a
    // pass; this one can only agree with the model.

    /// The hull edges one assertion contributes, direction-aware. A closed claim
    /// vouches for its whole range; an open ray pins its moment nowhere, so it
    /// anchors only the edge its own contiguity guarantees — a "before Y" the
    /// lower, an "after Y" the upper — and a wholly unknown date neither.
    fn affirmed_edges(date: &UncertainDate) -> (Option<NaiveDate>, Option<NaiveDate>) {
        match (date.earliest(), date.latest()) {
            (Some(lo), Some(hi)) => (Some(lo), Some(hi)),
            (None, Some(hi)) => (Some(hi), None),
            (Some(lo), None) => (None, Some(lo)),
            (None, None) => (None, None),
        }
    }

    /// What one assertion says on its own, before anything is joined: the span it
    /// vouches for, the two ways its own date can rule an instant out, whether it
    /// claims a removal at no nameable instant, and the moment it puts existence
    /// beyond doubt at.
    ///
    /// Every question below is answered by joining one of these fields across the
    /// bag, each in the direction its own question wants — which is what keeps a
    /// claim that answers two questions oppositely from collapsing into one.
    struct Contribution {
        /// The edges of the span this vouches for.
        affirms: (Option<NaiveDate>, Option<NaiveDate>),
        /// Existence before this is impossible.
        floor: Option<NaiveDate>,
        /// Existence after this is impossible.
        cap: Option<NaiveDate>,
        /// A removal claimed past every instant, which no evidence can outlive.
        removal_at_no_instant: bool,
        /// Existence here is beyond doubt.
        guarantees: Option<NaiveDate>,
    }

    /// The model read off one assertion. A construction's start floors the past,
    /// a completion caps the future at the latest instant it could name, a removal
    /// under way names no instant at all, and every slot but the completion
    /// vouches for existence at the earliest moment its date could name.
    fn contribution(assertion: &Assertion) -> Contribution {
        let date = &assertion.date;
        // What every slot contributes on the strength of its date alone.
        let affirming = Contribution {
            affirms: affirmed_edges(date),
            floor: None,
            cap: None,
            removal_at_no_instant: false,
            guarantees: date.earliest(),
        };
        match assertion.slot {
            Slot::ConstructionStarted => Contribution {
                floor: date.earliest(),
                ..affirming
            },
            Slot::ConstructionCompleted | Slot::Witness => affirming,
            Slot::DemolitionStarted => Contribution {
                removal_at_no_instant: true,
                ..affirming
            },
            Slot::DemolitionCompleted => Contribution {
                cap: date.latest(),
                removal_at_no_instant: date.latest().is_none(),
                guarantees: None,
                ..affirming
            },
        }
    }

    fn contributions(bag: &[Assertion]) -> Vec<Contribution> {
        bag.iter().map(contribution).collect()
    }

    /// Whether the forward presumption runs: a removal claimed at no instant holds
    /// it off outright, and a dated completion holds it off until some assertion
    /// guarantees existence past every instant one is claimed at — a completion the
    /// evidence does not reach is still an intact account of the removal.
    fn spec_presumes(parts: &[Contribution]) -> bool {
        if parts.iter().any(|p| p.removal_at_no_instant) {
            return false;
        }
        match parts.iter().filter_map(|p| p.cap).max() {
            Some(last) => parts
                .iter()
                .filter_map(|p| p.guarantees)
                .max()
                .is_some_and(|edge| edge > last),
            None => true,
        }
    }

    /// The five states at `at`, in presence-ascending order — absent, unknown,
    /// presumed, contested, uncontested — each straight from its definition.
    fn denotational_states(bag: &[Assertion], at: NaiveDate) -> [bool; 5] {
        let parts = contributions(bag);
        // Deny: ∃ an assertion for which existence at `at` is impossible under
        // every reading of its own date, and any one rival suffices — so the
        // floors keep the latest and the caps the earliest.
        let denied = parts
            .iter()
            .filter_map(|p| p.floor)
            .max()
            .is_some_and(|floor| at < floor)
            || parts
                .iter()
                .filter_map(|p| p.cap)
                .min()
                .is_some_and(|cap| at > cap);

        // Affirm: the convex hull of every assertion's edges.
        let hull_lo = parts.iter().filter_map(|p| p.affirms.0).min();
        let hull_hi = parts.iter().filter_map(|p| p.affirms.1).max();
        let affirmed = matches!((hull_lo, hull_hi), (Some(lo), Some(hi)) if lo <= at && at <= hi);

        let presumed_reach = spec_presumes(&parts) && hull_hi.is_some_and(|hi| at > hi);
        let supported = affirmed || presumed_reach;

        [
            !supported && denied,
            !supported && !denied,
            presumed_reach && !affirmed && !denied,
            supported && denied,
            affirmed && !denied,
        ]
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
        push(ls.birth.floor());
        push(ls.death.caps.map(|c| c.denies_after));
        push(ls.death.caps.map(|c| c.claimed_through));
        days
    }

    /// The boundary instants `ls` both affirms and denies. Every such stretch
    /// begins at a boundary — the hull's lower edge, the day after a cap — so
    /// this is empty exactly when no affirmed instant is contested, which makes
    /// it both the antecedent filter and the instant supply for the absorption
    /// law.
    fn affirmed_contested_days(ls: &Lifespan) -> Vec<NaiveDate> {
        boundary_days(ls)
            .into_iter()
            .filter(|day| ls.hull.contains(*day) && ls.classify(*day) == Contested)
            .collect()
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

    /// A bag of assertions and an instant read off the lifespan it folds to —
    /// the shape every law needing the slot an assertion arrived in takes.
    fn arb_bag_and_instant() -> impl Strategy<Value = (Vec<Assertion>, NaiveDate)> {
        windowed(|base| {
            arb_bag_in(base).prop_flat_map(move |bag| {
                let at = arb_instant_for(base, &fold_bag(&bag));
                (Just(bag), at)
            })
        })
    }

    /// Two instants past the affirmed hull, the later at or after the earlier.
    ///
    /// Both are *built* from the hull's upper edge rather than drawn and
    /// filtered. The presumption begins one day past that edge and nowhere else,
    /// so an independently drawn instant is presumed about one time in twenty —
    /// which exhausts proptest's local-reject budget and aborts the run before
    /// the law has been checked at all. What is left to filter is only whether
    /// this lifespan has a presumed stretch, a property of the bag.
    fn arb_instants_past_the_hull() -> impl Strategy<Value = (Lifespan, NaiveDate, NaiveDate)> {
        let past_the_edge = prop_oneof![
            1 => 1u64..400,
            1 => 1u64..100_000,
        ];
        let ahead = prop_oneof![
            1 => 0u64..400,
            1 => 0u64..100_000,
        ];
        (arb_lifespan(), past_the_edge, ahead).prop_filter_map(
            "two representable instants past the affirmed hull",
            |(ls, past_the_edge, ahead)| {
                let hi = ls.hull.hi()?;
                let earlier = hi.checked_add_days(chrono::Days::new(past_the_edge))?;
                let later = earlier.checked_add_days(chrono::Days::new(ahead))?;
                Some((ls, earlier, later))
            },
        )
    }

    /// Two lifespans and an instant the first one both affirms and denies, taken
    /// from its contested boundaries rather than drawn at large — an instant
    /// drawn at large is contested about one time in seven, and the local-reject
    /// budget runs out before the law does.
    fn arb_contested_instant_and_lifespans()
    -> impl Strategy<Value = (Lifespan, Lifespan, NaiveDate)> {
        windowed(|base| {
            (arb_lifespan_in(base), arb_lifespan_in(base))
                .prop_filter(
                    "the first lifespan contests an affirmed instant",
                    |(a, _)| !affirmed_contested_days(a).is_empty(),
                )
                .prop_flat_map(|(a, b)| {
                    let days = prop::sample::select(affirmed_contested_days(&a));
                    (Just(a), Just(b), days)
                })
        })
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

    /// A dated demolition our own evidence outlives, plus an instant read off
    /// the pair once folded.
    ///
    /// The refuter comes from every slot that can refute: a construction refutes
    /// as well as a sighting and floors the past while doing it, which is what
    /// gives the law its two forms — absence below the affirmed hull where the
    /// refuter floors, and none anywhere where it does not.
    ///
    /// Both dates are laid out along one sorted run of days rather than drawn
    /// independently and filtered for the ordering: independent draws refute one
    /// another about one time in seven, which exhausts the local-reject budget
    /// and aborts the run. What is left to reject is only the pair landing inside
    /// one precision period, where neither can outlive the other.
    fn arb_refuted_demolition_and_instant()
    -> impl Strategy<Value = (UncertainDate, Assertion, NaiveDate)> {
        windowed(|base| {
            (
                prop::array::uniform4(arb_bound_day(base)),
                arb_precision(),
                prop::sample::select(slots(Slot::can_refute)),
                any::<bool>(),
                any::<bool>(),
            )
                .prop_filter_map(
                    "the refuter's period begins past the demolition's",
                    |(mut days, precision, slot, open_below, open_above)| {
                        days.sort_unstable();
                        let [from, capped, guaranteed, until] = days;
                        let cap = DateBound::new(capped, precision).ok()?;
                        let edge = DateBound::new(guaranteed, precision).ok()?;
                        if edge.period_start() <= cap.period_end() {
                            return None;
                        }
                        // A "before Y" demolition caps the future where a closed
                        // one does and an "after Y" refuter guarantees what a
                        // closed one guarantees, so both shapes belong here.
                        let demolition = if open_below {
                            UncertainDate::bounded(None, Some(cap)).ok()?
                        } else {
                            closed_at(from, capped, precision)?
                        };
                        let refuter = if open_above {
                            UncertainDate::bounded(Some(edge), None).ok()?
                        } else {
                            closed_at(guaranteed, until, precision)?
                        };
                        Some((demolition, Assertion::new(slot, refuter)))
                    },
                )
                .prop_flat_map(move |(demolition, refuter)| {
                    let ls = fold([
                        Lifespan::demolition_completed(demolition.clone()),
                        refuter.fold_in(),
                    ]);
                    let at = arb_instant_for(base, &ls);
                    (Just(demolition), Just(refuter), at)
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
        /// The fold agrees with the model: recomputing the five states from the
        /// assertion bag itself gives exactly what `classify` returns off the
        /// folded accumulator, at every instant. The homomorphism a cache rests
        /// on, and the law that keeps the whole derivation — deny channel,
        /// affirmed hull, refutation, the five-way split — pinned to the
        /// denotation rather than to itself.
        ///
        /// The oracle also asserts the partition: exactly one state holds, so
        /// no predicate edit can open a gap or an overlap at the boundaries
        /// (uncontested/presumed at the hull's upper edge, presumed/unknown,
        /// contested/absent).
        #[test]
        fn classify_matches_the_denotational_partition(
            (bag, at) in arb_bag_and_instant(),
        ) {
            let states = denotational_states(&bag, at);
            prop_assert_eq!(states.into_iter().filter(|held| *held).count(), 1);

            let expected = [Absent, Unknown, Presumed, Contested, Uncontested]
                .into_iter()
                .zip(states)
                .find_map(|(state, held)| held.then_some(state));
            prop_assert_eq!(Some(fold_bag(&bag).classify(at)), expected);
        }

        /// A `demolition.completed` never advances the guaranteed edge. Rival
        /// completions are alternative accounts of one removal, so letting a
        /// completion vouch for existence at its own date would let the later
        /// claim refute the earlier — two sources agreeing the entity is gone,
        /// read as evidence that it survived.
        #[test]
        fn a_claimed_completion_never_advances_the_guaranteed_edge(
            (ls, date, _) in arb_lifespan_date_and_instant(),
        ) {
            let folded = ls.clone().combine(Lifespan::demolition_completed(date));
            prop_assert_eq!(folded.live, ls.live);
        }

        /// Every other slot advances the guaranteed edge to exactly the earliest
        /// instant its date could name — the one instant the assertion puts
        /// existence beyond doubt at or after. Reading a later edge would have a
        /// claim vouch for a moment it only *might* describe: "built in the 17th
        /// century" would testify to the year 1700.
        #[test]
        fn a_standing_assertion_guarantees_its_earliest_instant(
            (ls, date, _) in arb_lifespan_date_and_instant(),
        ) {
            let expected = match (ls.live.0, date.earliest()) {
                (edge, None) | (None, edge) => edge,
                (Some(held), Some(arriving)) => Some(held.max(arriving)),
            };
            for slot in slots(Slot::guarantees_existence) {
                let folded = ls
                    .clone()
                    .combine(Assertion::new(slot, date.clone()).fold_in());
                prop_assert_eq!(folded.live.0, expected, "{:?}", slot);
            }
        }

        /// A `demolition.started` guarantees existence at its own date, and no
        /// verdict can tell. The guaranteed edge is read in one place — against
        /// the completions a record dates — and a start in the bag has already
        /// withdrawn the presumption before that reading happens, so the two
        /// folds, one where a started removal vouches for its date and one where
        /// it vouches for nothing, classify and support every instant alike.
        ///
        /// The edge is kept because it is what the slot means: a removal beginning
        /// in 1980 places the entity standing in 1980. That reading is what
        /// `a_standing_assertion_guarantees_its_earliest_instant` pins, and the two
        /// folds are distinguishable by `==` and on the wire — this law is about
        /// the verdicts alone.
        #[test]
        fn a_started_removals_guarantee_reaches_no_verdict(
            (bag, at) in arb_bag_and_instant(),
        ) {
            let muted = fold(bag.iter().map(|assertion| match assertion.slot {
                Slot::DemolitionStarted => Lifespan {
                    live: LiveEdge::NONE,
                    ..assertion.fold_in()
                },
                _ => assertion.fold_in(),
            }));
            let folded = fold_bag(&bag);
            prop_assert_eq!(folded.classify(at), muted.classify(at));
            prop_assert_eq!(folded.support(), muted.support());
        }

        /// The presumption survives a demolition exactly when every removal on
        /// record names an instant and some assertion outside the completion slot
        /// guarantees existence strictly past all of them. Each clause answers a
        /// way the rule goes wrong: measured against the *earliest* cap, a sighting
        /// between two disputed demolition years strips suppression the later claim
        /// still accounts for; read off an assertion's *latest* edge, a vague
        /// construction refutes a demolition falling inside its own range; ranked
        /// against the dated caps, a removal claimed at no instant is refuted by
        /// the very evidence it describes.
        #[test]
        fn refutation_takes_a_guarantee_past_every_claimed_completion(
            (bag, _) in arb_bag_and_instant(),
        ) {
            prop_assert_eq!(fold_bag(&bag).presumes(), spec_presumes(&contributions(&bag)));
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
            prop_assert_eq!(window.overlaps(DayRange::at(at)), window.contains(at));
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
            let range = DayRange::new(start, end)
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            let window = ls.support();
            let any = start
                .iter_days()
                .take_while(|d| *d <= end)
                .any(|d| window.contains(d));
            prop_assert_eq!(window.overlaps(range), any);
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

        /// Contested absorbs wherever evidence affirms: once a source denies an
        /// instant the hull vouches for, no later fold quiets the disagreement,
        /// since neither channel ever withdraws.
        ///
        /// The forward tail is the exception, and by design — see
        /// `a_later_demolition_claim_settles_a_refuted_tail`. A contested instant
        /// past the hull rests on a *refuted* demolition, and refutation is not
        /// monotone: a completion claimed later than anything our evidence
        /// guarantees accounts for the whole record instead of contradicting it,
        /// and the tail becomes a verdict again.
        #[test]
        fn contested_is_absorbing_where_the_hull_affirms(
            (a, b, at) in arb_contested_instant_and_lifespans(),
        ) {
            prop_assert_eq!(a.classify(at), Contested, "the case is the one the law is about");
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
            (ls, _, later) in arb_instants_past_the_hull()
                .prop_filter("presumed at the earlier instant", |(ls, earlier, _)| {
                    ls.classify(*earlier) == Presumed
                }),
        ) {
            prop_assert_eq!(ls.classify(later), Presumed);
        }

        /// A refuted demolition carries no absence forward. From the earliest
        /// instant anything affirms, nothing is absent: the affirmed hull is
        /// supported and the resumed presumption runs past it without bound, so
        /// the whole tail is a live disagreement rather than a definite absence
        /// resting on the claim our own evidence contradicts.
        ///
        /// Where absence survives is the refuter's own doing, so the law reads it
        /// off the slot. A construction floors the past at its earliest instant,
        /// and absence fills exactly the stretch below the affirmed hull; a
        /// completion or a sighting floors nothing, and the whole timeline is free
        /// of it. Stated as an equality, so the strong form keeps being asserted
        /// for the slots that hold it.
        #[test]
        fn a_refuted_demolition_is_contested_from_the_first_affirmation(
            (demolition, refuter, at) in arb_refuted_demolition_and_instant(),
        ) {
            let ls = fold([
                Lifespan::demolition_completed(demolition),
                refuter.fold_in(),
            ]);
            let below_the_refuters_floor =
                refuter.slot.floors_the_past() && ls.hull.lo().is_some_and(|lo| at < lo);
            prop_assert_eq!(ls.classify(at) == Absent, below_the_refuters_floor);
            if ls.hull.hi().is_some_and(|hi| at > hi) {
                prop_assert_eq!(ls.classify(at), Contested);
            }
        }

        /// Order- and duplicate-independence: the fold is the same under any
        /// permutation and any duplication of the assertion bag.
        #[test]
        fn fold_is_order_and_duplicate_insensitive(
            mut bag in windowed(arb_bag_in),
        ) {
            let straight = fold_bag(&bag);
            let doubled = fold(bag.iter().chain(bag.iter()).map(Assertion::fold_in));
            prop_assert_eq!(&straight, &doubled);

            bag.reverse();
            prop_assert_eq!(&straight, &fold_bag(&bag));
        }
    }
}
