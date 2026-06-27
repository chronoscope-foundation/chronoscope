//! Date types with uncertainty support.
//!
//! [`UncertainDate`] models epistemic uncertainty about a single instant in
//! time. "Built in the 1920s" means "construction started at some unknown
//! instant within \[1920, 1929\]", not "construction spanned the entire
//! decade." Duration is modeled by pairing two uncertain instants
//! (`started_at` and `completed_at` in
//! [`EntityTransition`](crate::entity::EntityTransition)), not by widening a
//! single date.
//!
//! # Representation
//!
//! An `UncertainDate` is a pair of optional [`DateBound`] endpoints, each
//! carrying a [`NaiveDate`] and a [`DatePrecision`] for the granularity of the
//! boundary itself. Precision lives on bounds, not on values: "before 1950" has
//! a year-granularity upper bound, distinct from "before January 1, 1950" with
//! a day-granularity bound.
//!
//! | Human expression | `earliest` | `latest` |
//! |---|---|---|
//! | "1927" | `DateBound(1927-01-01, Year)` | `DateBound(1927-01-01, Year)` |
//! | "The 1920s" | `DateBound(1920-01-01, Decade)` | `DateBound(1920-01-01, Decade)` |
//! | "Before 1950" | `None` | `DateBound(1950-01-01, Year)` |
//! | "After 1800" | `DateBound(1800-01-01, Year)` | `None` |
//! | Unknown | `None` | `None` |
//!
//! An [`UncertainDate`] is a canonical union of such intervals — usually one,
//! but a disjunction ("1925 or 1935") holds several disjoint ones. The fully
//! unbounded single interval `(None, None)` is `unknown` (⊤); the empty union
//! is `empty` (⊥).
//!
//! # Prior art and references
//!
//! - **EDTF / ISO 8601-2:2019**: Distinguishes unspecified digits (`192X`) from
//!   intervals (`1920/1929`), plus uncertainty (`?`), approximation (`~`), and
//!   combined (`%`). Every implementation collapses to `[lower, upper]` for computation.
//!   See <https://www.loc.gov/standards/datetime/>.
//!
//! - **Dyreson & Snodgrass (TODS 1998)**: The "possible chronons" model. Temporal
//!   granularity and indeterminacy are two sides of the same coin.
//!   See <https://www2.cs.arizona.edu/~rts/pubs/TODS98.pdf>.
//!
//! - **OWL-Time (W3C 2017)**: A `DateTimeDescription` is "strictly always a description
//!   of an interval." Found overly prescriptive for historical data by the `PeriodO`
//!   project. See <https://www.w3.org/TR/owl-time/>.
//!
//! - **CIDOC-CRM**: Four boundary points (begin-of-begin, end-of-begin, begin-of-end,
//!   end-of-end) for fuzzy time-spans. See <https://cidoc-crm.org/taxonomy/term/74>.
//!
//! - **Wikidata**: Numeric precision (0=billion years through 14=seconds). Documentation
//!   says "an indicator of significant parts, not directly specifying an interval" — but
//!   consumers treat it as one. See <https://www.wikidata.org/wiki/Help:Dates>.

use std::fmt;

use chrono::{Datelike, NaiveDate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Errors from date construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DateError {
    /// Year 0 does not exist in historical convention (use -1 for 1 BCE).
    Year0,
    /// Range endpoints are inverted (earliest > latest).
    InvertedRange,
}

impl fmt::Display for DateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Year0 => {
                write!(
                    f,
                    "year 0 does not exist in historical convention (use -1 for 1 BCE)"
                )
            }
            Self::InvertedRange => write!(f, "range: earliest must be <= latest"),
        }
    }
}

impl std::error::Error for DateError {}

/// Precision level for date bounds.
///
/// Determines the granularity of a [`DateBound`]. A bound with `Year` precision
/// represents a boundary at year granularity: "1927" spans Jan 1 – Dec 31.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DatePrecision {
    Day,
    Month,
    Year,
    Decade,
    /// Century using the historical convention: 21st century = 2001-2100 (year 1-based).
    Century,
    /// Millennium using the historical convention: 2nd millennium = 1001-2000 (year 1-based).
    Millennium,
}

/// A date with known precision, used as a bound in [`UncertainDate`].
///
/// The date is always snapped to the start of its precision period.
/// For example, `Year` precision for 2020 stores `2020-01-01`.
///
/// Year 0 is rejected — use negative years for BCE dates (e.g., -1 for 1 BCE).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
pub struct DateBound {
    date: NaiveDate,
    precision: DatePrecision,
}

impl<'de> Deserialize<'de> for DateBound {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            date: NaiveDate,
            precision: DatePrecision,
        }
        let raw = Raw::deserialize(deserializer)?;
        DateBound::new(raw.date, raw.precision).map_err(serde::de::Error::custom)
    }
}

impl DateBound {
    /// Create a new `DateBound`, snapping the date to its precision period start.
    ///
    /// Returns `Err(DateError::Year0)` if the date has year 0.
    pub fn new(date: NaiveDate, precision: DatePrecision) -> Result<Self, DateError> {
        if date.year() == 0 {
            return Err(DateError::Year0);
        }
        let snapped = snap_to_precision_start(date, precision);
        Ok(Self {
            date: snapped,
            precision,
        })
    }

    /// The first day of this bound's precision period.
    pub fn period_start(&self) -> NaiveDate {
        self.date
    }

    /// The precision level.
    pub fn precision(&self) -> DatePrecision {
        self.precision
    }

    /// The last day of this bound's precision period.
    pub fn period_end(&self) -> NaiveDate {
        precision_end(self.date, self.precision)
    }
}

/// A bounded interval over [`DateBound`] endpoints — the primitive interval
/// type. Both endpoints are optional; `None` denotes "open" on that side.
/// `(None, None)` is the empty constraint (the entire timeline).
///
/// [`TimeRange::new`] enforces `earliest.period_start() <= latest.period_end()` when
/// both ends are present, so a constructed `TimeRange` is never inverted.
///
/// Wire shape: `{ "earliest": Option<DateBound>, "latest": Option<DateBound> }`
/// with `null` fields suppressed via [`serde_with::skip_serializing_none`].
/// [`UncertainDate`] wraps `TimeRange` transparently — same serialized form.
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
pub struct TimeRange {
    earliest: Option<DateBound>,
    latest: Option<DateBound>,
}

impl<'de> Deserialize<'de> for TimeRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            earliest: Option<DateBound>,
            latest: Option<DateBound>,
        }
        let raw = Raw::deserialize(deserializer)?;
        TimeRange::new(raw.earliest, raw.latest).map_err(serde::de::Error::custom)
    }
}

/// Errors from [`TimeRange::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeRangeError {
    /// `earliest`'s period start is after `latest`'s period end — endpoints
    /// crossed.
    EndpointsInverted {
        /// The earliest endpoint, as supplied.
        earliest: DateBound,
        /// The latest endpoint, as supplied.
        latest: DateBound,
    },
}

impl fmt::Display for TimeRangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EndpointsInverted { earliest, latest } => write!(
                f,
                "time range earliest ({earliest:?}) must be <= latest ({latest:?})"
            ),
        }
    }
}

impl std::error::Error for TimeRangeError {}

// TimeRange::new and UncertainDate::bounded enforce the same "earliest <=
// latest" predicate; this conversion collapses the TimeRange error onto
// UncertainDate's InvertedRange so UncertainDate's constructors route through
// TimeRange without a new error surface.
impl From<TimeRangeError> for DateError {
    fn from(err: TimeRangeError) -> Self {
        match err {
            TimeRangeError::EndpointsInverted { .. } => Self::InvertedRange,
        }
    }
}

impl TimeRange {
    /// Construct a time range from optional endpoints. When both are
    /// present, `earliest`'s snapped period-start day must be `<=`
    /// `latest`'s period-end day. `None` denotes "open" on that side.
    pub fn new(
        earliest: Option<DateBound>,
        latest: Option<DateBound>,
    ) -> Result<Self, TimeRangeError> {
        if let (Some(e), Some(l)) = (&earliest, &latest)
            && e.period_start() > l.period_end()
        {
            return Err(TimeRangeError::EndpointsInverted {
                earliest: *e,
                latest: *l,
            });
        }
        Ok(Self { earliest, latest })
    }

    /// The empty constraint `(None, None)` — open on both ends.
    pub fn unbounded() -> Self {
        Self {
            earliest: None,
            latest: None,
        }
    }

    /// The earliest-bound endpoint, when set.
    pub fn earliest(&self) -> Option<&DateBound> {
        self.earliest.as_ref()
    }

    /// The latest-bound endpoint, when set.
    pub fn latest(&self) -> Option<&DateBound> {
        self.latest.as_ref()
    }

    /// True if `date` falls within this time range, inclusive of both
    /// endpoint precision-extent corners.
    pub fn contains(&self, date: NaiveDate) -> bool {
        let lower_ok = self
            .earliest
            .as_ref()
            .is_none_or(|b| b.period_start() <= date);
        let upper_ok = self.latest.as_ref().is_none_or(|b| date <= b.period_end());
        lower_ok && upper_ok
    }

    /// The lower endpoint as a day, or `None` for an open (−∞) lower side.
    fn lower_day(&self) -> Option<NaiveDate> {
        self.earliest.as_ref().map(DateBound::period_start)
    }

    /// The upper endpoint as a day, or `None` for an open (+∞) upper side.
    fn upper_day(&self) -> Option<NaiveDate> {
        self.latest.as_ref().map(DateBound::period_end)
    }

    /// Orders intervals by lower endpoint first, then upper — an open lower
    /// sorts first as −∞, an open upper sorts last as +∞. This total order lets
    /// the union canonicalizer ([`UncertainDate::from_ranges`]) coalesce in a
    /// single forward sweep, merging each interval into the running last one.
    fn cmp_by_span(&self, other: &Self) -> std::cmp::Ordering {
        // For the lower side, `None` (−∞) is smallest, matching `Option`'s own
        // `None < Some` ordering.
        let lower = self.lower_day().cmp(&other.lower_day());
        // For the upper side, `None` (+∞) is largest, the reverse of `Option`'s
        // ordering, so compare the flipped key.
        lower.then_with(|| match (self.upper_day(), other.upper_day()) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
            (Some(a), Some(b)) => a.cmp(&b),
        })
    }

    /// The merge predicate the union canonicalizer sweeps with. Two contiguous
    /// ranges' set-union *is* a single range — `{1920s} ∪ {1930s}` with no gap
    /// between them is the unbroken set 1920–1939 — so canonical form must fuse
    /// adjacent-or-overlapping intervals, else one date-set would have two
    /// encodings. Assumes `self` sorts at-or-before `other` by lower endpoint.
    fn adjacent_or_overlapping(&self, other: &Self) -> bool {
        let Some(upper) = self.upper_day() else {
            // Open upper end absorbs everything ordered after it.
            return true;
        };
        match other.lower_day() {
            // Open lower end means `other` starts at −∞, so it overlaps.
            None => true,
            // Overlap up to and including the shared boundary day.
            Some(lower) if lower <= upper => true,
            // Adjacency: the next day after `self`'s upper end is `other`'s start.
            Some(lower) => upper.succ_opt() == Some(lower),
        }
    }

    /// Intersection of two ranges — the tightest range contained by both, or
    /// `None` when they are disjoint. Selects bounds from the inputs — never
    /// manufactures new [`DateBound`] values.
    ///
    /// The single-interval primitive behind [`UncertainDate::meet`].
    fn intersect(&self, other: &Self) -> Option<Self> {
        let earliest = tighten_earliest(&self.earliest, &other.earliest);
        let latest = tighten_latest(&self.latest, &other.latest);

        // Disjoint: both bounds present and the tightened lower exceeds the
        // tightened upper.
        if let (Some(e), Some(l)) = (earliest.as_ref(), latest.as_ref())
            && e.period_start() > l.period_end()
        {
            return None;
        }

        Some(Self { earliest, latest })
    }

    /// Span (bounding interval) of two ranges — the smallest range containing
    /// both. Selects bounds from the inputs; an open side stays open. The
    /// single-interval primitive the union canonicalizer applies to
    /// overlapping/adjacent intervals.
    fn join(&self, other: &Self) -> Self {
        Self {
            earliest: widen_earliest(&self.earliest, &other.earliest),
            latest: widen_latest(&self.latest, &other.latest),
        }
    }
}

/// A date with uncertainty: a canonical union of disjoint [`TimeRange`]
/// intervals, each interpreted as "an unknown instant in time lies somewhere
/// in this interval".
///
/// Most claims are a single interval — the only shape a stored fact may carry
/// (the submit boundary rejects ⊥ and disjunctions). The union exists so that
/// joining genuinely conflicting claims ("1925 or 1935") keeps them disjoint
/// instead of inventing the gap between them, and so meet of contradictory
/// claims is the honest empty union rather than a forced overlap.
///
/// This is why [`UncertainDate`] is a distinct type from [`TimeRange`]:
/// `TimeRange` is a single interval, an `UncertainDate` an epistemic date claim
/// that may be a disjunction. A single-interval value still serializes flat as
/// `{earliest, latest}`, the same bytes the old transparent newtype produced.
///
/// # The bounded lattice
///
/// The carrier is the set of canonical disjoint-interval unions; `meet` is set
/// intersection and `join` is set union. Both are total:
///
/// - [`unknown`](Self::unknown) — the single fully-unbounded interval, ⊤. The
///   meet identity: `a ∧ ⊤ = a`.
/// - [`empty`](Self::empty) — the empty union, ⊥. The join identity:
///   `a ∨ ⊥ = a`.
///
/// # Canonical form
///
/// The internal interval list is sorted by lower endpoint and pairwise
/// disjoint-and-non-adjacent: any two intervals that overlap, or touch across a
/// one-day gap, are merged into one. The merge is confluent (independent of the
/// order intervals arrive in), so one logical date has exactly one
/// representation — the prerequisite for a deterministic `CommitId` and for
/// `BTreeSet<SubmitFact>` dedup.
///
/// # Construction
///
/// - [`UncertainDate::with_precision`] — symmetric bounds (e.g., "1927" or "the 1920s")
/// - [`UncertainDate::bounded`] — asymmetric or one-sided (e.g., "before 1950")
/// - [`UncertainDate::unknown`] — completely unknown `(None, None)`, ⊤
/// - [`UncertainDate::empty`] — the empty union, ⊥
///
/// Year 0 is rejected in all single-interval constructors.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UncertainDate(canonical::CanonicalRanges);

/// The one place a canonical interval list is built. The inner `Vec` is private
/// to this module, reachable only through [`CanonicalRanges::from_ranges`] (the
/// sort-and-coalesce canonicalizer) and [`CanonicalRanges::empty`]. Every other
/// path — `meet`, `join`, deserialize — feeds raw intervals through
/// `from_ranges`, so a non-canonical list cannot be assembled outside these few
/// lines.
mod canonical {
    use super::TimeRange;

    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub(super) struct CanonicalRanges(Vec<TimeRange>);

    impl CanonicalRanges {
        /// Canonicalize an arbitrary collection of intervals into the disjoint,
        /// sorted, adjacency-merged union. The merge is confluent: any input
        /// order yields one canonical form.
        pub(super) fn from_ranges(mut ranges: Vec<TimeRange>) -> Self {
            ranges.sort_by(TimeRange::cmp_by_span);
            let mut merged: Vec<TimeRange> = Vec::with_capacity(ranges.len());
            for next in ranges {
                match merged.last() {
                    Some(acc) if acc.adjacent_or_overlapping(&next) => {
                        let span = acc.join(&next);
                        if let Some(slot) = merged.last_mut() {
                            *slot = span;
                        }
                    }
                    _ => merged.push(next),
                }
            }
            Self(merged)
        }

        /// The empty union, ⊥.
        pub(super) fn empty() -> Self {
            Self(Vec::new())
        }

        pub(super) fn as_slice(&self) -> &[TimeRange] {
            &self.0
        }

        pub(super) fn first(&self) -> Option<&TimeRange> {
            self.0.first()
        }

        pub(super) fn last(&self) -> Option<&TimeRange> {
            self.0.last()
        }
    }
}

impl UncertainDate {
    /// Create an `UncertainDate` with symmetric bounds at the given precision.
    ///
    /// For example, `with_precision(2020-06-15, Year)` produces bounds
    /// where both earliest and latest are `DateBound(2020-01-01, Year)`.
    ///
    /// Returns `Err(DateError::Year0)` if the date has year 0.
    pub fn with_precision(date: NaiveDate, precision: DatePrecision) -> Result<Self, DateError> {
        let bound = DateBound::new(date, precision)?;
        // Symmetric bounds (earliest == latest) can't fail TimeRange::new's
        // ordering check; routing through `?` keeps the construction path
        // uniform.
        Ok(Self::single(TimeRange::new(Some(bound), Some(bound))?))
    }

    /// Create an `UncertainDate` with explicit earliest and latest bounds.
    ///
    /// Either or both bounds may be `None` (unbounded).
    ///
    /// Returns `Err(DateError::InvertedRange)` if both bounds are present and
    /// the earliest bound's period start is after the latest bound's period end.
    pub fn bounded(
        earliest: Option<DateBound>,
        latest: Option<DateBound>,
    ) -> Result<Self, DateError> {
        Ok(Self::single(TimeRange::new(earliest, latest)?))
    }

    /// The fully-unbounded single interval `(None, None)`, ⊤.
    ///
    /// The meet (intersection) identity: `a ∧ unknown() == a`.
    pub fn unknown() -> Self {
        Self::single(TimeRange::unbounded())
    }

    /// The empty union, ⊥ — "no possible date".
    ///
    /// The join (union) identity: `a ∨ empty() == a`.
    pub fn empty() -> Self {
        Self(canonical::CanonicalRanges::empty())
    }

    /// A one-interval union from a single non-empty [`TimeRange`] — already
    /// canonical, but routed through the canonicalizer so it is the sole
    /// build path.
    fn single(range: TimeRange) -> Self {
        Self::from_ranges(vec![range])
    }

    /// Canonicalize an arbitrary collection of intervals into the disjoint,
    /// sorted, adjacency-merged union.
    fn from_ranges(ranges: Vec<TimeRange>) -> Self {
        Self(canonical::CanonicalRanges::from_ranges(ranges))
    }

    /// The disjoint intervals, in canonical order.
    pub fn intervals(&self) -> &[TimeRange] {
        self.0.as_slice()
    }

    /// The single interval this date is, when it is exactly one — the only
    /// shape a stored fact may hold. `None` for ⊥ (empty) or a disjunction.
    pub fn as_single_interval(&self) -> Option<&TimeRange> {
        match self.0.as_slice() {
            [single] => Some(single),
            _ => None,
        }
    }

    /// The earliest possible date across every interval, or `None` if the
    /// earliest interval is open on its lower side (or the union is empty).
    pub fn earliest(&self) -> Option<NaiveDate> {
        self.0.first()?.earliest().map(DateBound::period_start)
    }

    /// The latest possible date across every interval, or `None` if the latest
    /// interval is open on its upper side (or the union is empty).
    pub fn latest(&self) -> Option<NaiveDate> {
        self.0.last()?.latest().map(DateBound::period_end)
    }

    /// The earliest bound (with precision) across every interval, if present.
    pub fn earliest_bound(&self) -> Option<&DateBound> {
        self.0.first()?.earliest()
    }

    /// The latest bound (with precision) across every interval, if present.
    pub fn latest_bound(&self) -> Option<&DateBound> {
        self.0.last()?.latest()
    }

    /// Check whether two dates share any instant — true iff their meet is
    /// non-empty.
    pub fn overlaps(&self, other: &Self) -> bool {
        !self.meet(other).intervals().is_empty()
    }

    /// Whether this is ⊤ ([`unknown`](Self::unknown)) — the single
    /// fully-unbounded interval. The meet identity.
    fn is_unknown(&self) -> bool {
        self.as_single_interval()
            .is_some_and(|r| r.earliest().is_none() && r.latest().is_none())
    }

    /// Whether this is ⊥ ([`empty`](Self::empty)) — the empty union. The join
    /// identity.
    fn is_empty(&self) -> bool {
        self.intervals().is_empty()
    }

    /// Set intersection — meet in the lattice. Total: contradictory claims
    /// intersect to [`empty`](Self::empty) (⊥).
    ///
    /// Intersects every interval of `self` against every interval of `other`,
    /// then canonicalizes. `unknown()` (⊤) is the identity: `a ∧ unknown() == a`.
    /// Selects bounds from the inputs — never manufactures new `DateBound`s.
    pub fn meet(&self, other: &Self) -> Self {
        // ⊤ (`unknown`) is the meet identity: the other operand is already
        // canonical, so return it untouched rather than re-canonicalizing.
        if self.is_unknown() {
            return other.clone();
        }
        if other.is_unknown() {
            return self.clone();
        }
        let mut pieces = Vec::new();
        for a in self.intervals() {
            for b in other.intervals() {
                if let Some(piece) = a.intersect(b) {
                    pieces.push(piece);
                }
            }
        }
        Self::from_ranges(pieces)
    }

    /// Set union — join in the lattice. Total: disjoint claims stay disjoint,
    /// so no gap is invented between conflicting dates.
    ///
    /// Collects both interval lists and canonicalizes; only genuinely
    /// overlapping or adjacent intervals merge. `empty()` (⊥) is the identity:
    /// `a ∨ empty() == a`. Selects bounds from the inputs.
    pub fn join(&self, other: &Self) -> Self {
        // ⊥ (`empty`) is the join identity: the other operand is already
        // canonical, so return it untouched rather than re-canonicalizing.
        if self.is_empty() {
            return other.clone();
        }
        if other.is_empty() {
            return self.clone();
        }
        let mut pieces = self.intervals().to_vec();
        pieces.extend_from_slice(other.intervals());
        Self::from_ranges(pieces)
    }
}

/// The join half of the date lattice. The fold seeds from ⊥
/// ([`empty`](Self::empty)), so a real bound is never poisoned — seeding from
/// ⊤ (`unknown`) would absorb every disjunct.
impl crate::algebra::monoid::CommutativeMonoid for UncertainDate {
    fn identity() -> Self {
        Self::empty()
    }

    fn combine(self, other: Self) -> Self {
        UncertainDate::join(&self, &other)
    }
}

impl crate::algebra::lattice::JoinSemilattice for UncertainDate {}

/// The meet half of the date lattice. ⊤ is `unknown()`, the meet identity;
/// `meet` delegates to the inherent intersection.
impl crate::algebra::lattice::MeetSemilattice for UncertainDate {
    fn top() -> Self {
        Self::unknown()
    }

    fn meet(self, other: Self) -> Self {
        UncertainDate::meet(&self, &other)
    }
}

// Wire shape: a single-interval union serializes flat as the `TimeRange`
// `{earliest, latest}` (byte-identical to the former transparent newtype, so
// stored-fact goldens don't move). A disjunction or the empty union serializes
// as `{one_of: [<flat interval>, …]}`. The submit boundary forbids storing
// anything but a single interval, so `one_of` reaches the wire only on the
// read side, never in a hashed commit.
impl Serialize for UncertainDate {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.as_single_interval() {
            Some(single) => single.serialize(serializer),
            None => DateDisjunction {
                one_of: self.intervals().to_vec(),
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for UncertainDate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // `one_of` and a flat interval have disjoint key sets, so an untagged
        // split picks the right arm. Both route through the canonicalizer, so
        // non-canonical disjunctions normalize rather than error — matching the
        // deserialize-through-constructor pattern `DateBound` / `TimeRange` use.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Disjunction(DateDisjunction),
            Single(TimeRange),
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Disjunction(DateDisjunction { one_of }) => Self::from_ranges(one_of),
            Wire::Single(range) => Self::single(range),
        })
    }
}

/// The disjunction wire form. Its single field's key (`one_of`) is disjoint
/// from a flat interval's, so deserialize tells the two apart untagged.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DateDisjunction {
    one_of: Vec<TimeRange>,
}

impl JsonSchema for UncertainDate {
    fn schema_name() -> String {
        "UncertainDate".to_owned()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("chronoscope_core::date::UncertainDate")
    }

    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        use schemars::schema::{Schema, SchemaObject, SubschemaValidation};
        // A flat single interval OR a `{one_of: [interval, …]}` disjunction.
        let single = generator.subschema_for::<TimeRange>();
        let disjunction = generator.subschema_for::<DateDisjunction>();
        Schema::Object(SchemaObject {
            subschemas: Some(Box::new(SubschemaValidation {
                any_of: Some(vec![single, disjunction]),
                ..Default::default()
            })),
            ..Default::default()
        })
    }
}

/// For the earliest bound: the effective boundary is `period_start()`.
/// For the latest bound: the effective boundary is `period_end()`.
/// When boundaries are equal, precision tiebreaks for a total order.
impl DateBound {
    /// Ordering key for earliest-bound comparisons (lower bound of the interval).
    fn earliest_key(&self) -> (NaiveDate, DatePrecision) {
        (self.date, self.precision)
    }

    /// Ordering key for latest-bound comparisons (upper bound of the interval).
    fn latest_key(&self) -> (NaiveDate, DatePrecision) {
        (self.period_end(), self.precision)
    }
}

/// Tighten (for meet): `Some` wins over `None`. Picks the more restrictive bound.
fn tighten_earliest(a: &Option<DateBound>, b: &Option<DateBound>) -> Option<DateBound> {
    match (a, b) {
        (Some(a), Some(b)) => Some(*std::cmp::max_by_key(a, b, |x| x.earliest_key())),
        (Some(x), None) | (None, Some(x)) => Some(*x),
        (None, None) => None,
    }
}

fn tighten_latest(a: &Option<DateBound>, b: &Option<DateBound>) -> Option<DateBound> {
    match (a, b) {
        (Some(a), Some(b)) => Some(*std::cmp::min_by_key(a, b, |x| x.latest_key())),
        (Some(x), None) | (None, Some(x)) => Some(*x),
        (None, None) => None,
    }
}

/// Widen (for join): `None` wins over `Some`. Picks the less restrictive bound.
fn widen_earliest(a: &Option<DateBound>, b: &Option<DateBound>) -> Option<DateBound> {
    match (a, b) {
        (Some(a), Some(b)) => Some(*std::cmp::min_by_key(a, b, |x| x.earliest_key())),
        _ => None,
    }
}

fn widen_latest(a: &Option<DateBound>, b: &Option<DateBound>) -> Option<DateBound> {
    match (a, b) {
        (Some(a), Some(b)) => Some(*std::cmp::max_by_key(a, b, |x| x.latest_key())),
        _ => None,
    }
}

// --- Precision arithmetic helpers ---
//
// These construct dates from components known to be valid (derived from chrono
// accessors on existing valid dates, or from precision boundary formulas that
// only produce valid year/month/day triples). Using `.expect()` instead of
// `.unwrap_or()` so bugs in the formulas surface loudly instead of silently
// returning the wrong date.

#[expect(
    clippy::expect_used,
    reason = "components come from precision-boundary formulas / chrono accessors that only yield valid year/month/day triples"
)]
fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("precision arithmetic: valid date components")
}

/// Start year of a 1-based historical period (century, millennium).
///
/// Convention: 1st century CE = 1–100, 2nd = 101–200;
///             1st century BCE = −1–−100, 2nd = −101–−200.
fn period_start_year(year: i32, period: i32) -> i32 {
    if year > 0 {
        ((year - 1) / period) * period + 1
    } else {
        let neg_y = -year;
        -(((neg_y - 1) / period + 1) * period)
    }
}

/// End year of a 1-based historical period (century, millennium).
fn period_end_year(year: i32, period: i32) -> i32 {
    if year > 0 {
        ((year - 1) / period + 1) * period
    } else {
        let neg_y = -year;
        -(((neg_y - 1) / period) * period + 1)
    }
}

/// Snap a date to the start of its precision period.
///
/// Century and millennium use the historical convention (1-based).
/// Decades use the common convention (0-based): 1980s = 1980–1989.
fn snap_to_precision_start(date: NaiveDate, precision: DatePrecision) -> NaiveDate {
    match precision {
        DatePrecision::Day => date,
        DatePrecision::Month => ymd(date.year(), date.month(), 1),
        DatePrecision::Year => ymd(date.year(), 1, 1),
        DatePrecision::Decade => {
            let mut decade_start = date.year().div_euclid(10) * 10;
            // Decades are 0-based (1980s = 1980–1989), so years 1-9 CE snap to
            // "decade 0" — but year 0 doesn't exist historically, so clamp to 1.
            if decade_start == 0 {
                decade_start = 1;
            }
            ymd(decade_start, 1, 1)
        }
        DatePrecision::Century => ymd(period_start_year(date.year(), 100), 1, 1),
        DatePrecision::Millennium => ymd(period_start_year(date.year(), 1000), 1, 1),
    }
}

/// Calculate the last day of a precision period.
fn precision_end(date: NaiveDate, precision: DatePrecision) -> NaiveDate {
    match precision {
        DatePrecision::Day => date,
        DatePrecision::Month => {
            let (next_year, next_month) = if date.month() == 12 {
                (date.year() + 1, 1)
            } else {
                (date.year(), date.month() + 1)
            };
            // Last day of current month = day before first of next month.
            #[expect(
                clippy::expect_used,
                reason = "first-of-next-month and its predecessor are valid by construction"
            )]
            NaiveDate::from_ymd_opt(next_year, next_month, 1)
                .expect("precision arithmetic: valid next-month date")
                .pred_opt()
                .expect("precision arithmetic: predecessor of valid date")
        }
        DatePrecision::Year => ymd(date.year(), 12, 31),
        DatePrecision::Decade => {
            let decade_end = date.year().div_euclid(10) * 10 + 9;
            ymd(decade_end, 12, 31)
        }
        DatePrecision::Century => ymd(period_end_year(date.year(), 100), 12, 31),
        DatePrecision::Millennium => ymd(period_end_year(date.year(), 1000), 12, 31),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::lattice::{JoinSemilattice, MeetSemilattice};
    use proptest::prelude::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn d(y: i32, m: u32, d: u32) -> Result<NaiveDate, &'static str> {
        NaiveDate::from_ymd_opt(y, m, d).ok_or("invalid date")
    }

    // --- DateBound tests ---

    #[test]
    fn date_bound_snaps_to_precision() -> TestResult {
        let db = DateBound::new(d(2020, 6, 15)?, DatePrecision::Year)?;
        assert_eq!(db.period_start(), d(2020, 1, 1)?);
        assert_eq!(db.period_end(), d(2020, 12, 31)?);
        assert_eq!(db.precision(), DatePrecision::Year);
        Ok(())
    }

    #[test]
    fn date_bound_rejects_year_0() -> TestResult {
        assert_eq!(
            DateBound::new(d(0, 1, 1)?, DatePrecision::Year),
            Err(DateError::Year0),
        );
        Ok(())
    }

    #[test]
    fn date_bound_serde_roundtrip() -> TestResult {
        let db = DateBound::new(d(1850, 6, 15)?, DatePrecision::Year)?;
        let json = serde_json::to_string(&db)?;
        let deserialized: DateBound = serde_json::from_str(&json)?;
        assert_eq!(db, deserialized);
        Ok(())
    }

    #[test]
    fn date_bound_deserialize_rejects_year_0() {
        let json = r#"{"date":"0000-01-01","precision":"year"}"#;
        assert!(serde_json::from_str::<DateBound>(json).is_err());
    }

    // --- BCE/boundary edge cases ---

    #[test]
    fn bce_decade_boundary() -> TestResult {
        let ud = UncertainDate::with_precision(d(-10, 1, 1)?, DatePrecision::Decade)?;
        assert_eq!(ud.earliest(), Some(d(-10, 1, 1)?));
        assert_eq!(ud.latest(), Some(d(-1, 12, 31)?));
        Ok(())
    }

    #[test]
    fn bce_century_boundary() -> TestResult {
        // Year -100 (100 BCE) is still in the 1st century BCE
        let ud = UncertainDate::with_precision(d(-100, 1, 1)?, DatePrecision::Century)?;
        assert_eq!(ud.earliest(), Some(d(-100, 1, 1)?));
        assert_eq!(ud.latest(), Some(d(-1, 12, 31)?));

        // Year -101 (101 BCE) is in the 2nd century BCE: -200 to -101
        let ud = UncertainDate::with_precision(d(-101, 1, 1)?, DatePrecision::Century)?;
        assert_eq!(ud.earliest(), Some(d(-200, 1, 1)?));
        assert_eq!(ud.latest(), Some(d(-101, 12, 31)?));
        Ok(())
    }

    #[test]
    fn bce_millennium_boundary() -> TestResult {
        // Year -1000 is still in the 1st millennium BCE
        let ud = UncertainDate::with_precision(d(-1000, 1, 1)?, DatePrecision::Millennium)?;
        assert_eq!(ud.earliest(), Some(d(-1000, 1, 1)?));
        assert_eq!(ud.latest(), Some(d(-1, 12, 31)?));

        // Year -1001 is in the 2nd millennium BCE: -2000 to -1001
        let ud = UncertainDate::with_precision(d(-1001, 1, 1)?, DatePrecision::Millennium)?;
        assert_eq!(ud.earliest(), Some(d(-2000, 1, 1)?));
        assert_eq!(ud.latest(), Some(d(-1001, 12, 31)?));
        Ok(())
    }

    #[test]
    fn february_end_of_month() -> TestResult {
        // Non-leap year
        let ud = UncertainDate::with_precision(d(2019, 2, 15)?, DatePrecision::Month)?;
        assert_eq!(ud.latest(), Some(d(2019, 2, 28)?));

        // Leap year
        let ud = UncertainDate::with_precision(d(2020, 2, 15)?, DatePrecision::Month)?;
        assert_eq!(ud.latest(), Some(d(2020, 2, 29)?));
        Ok(())
    }

    #[test]
    fn december_end_of_month() -> TestResult {
        let ud = UncertainDate::with_precision(d(2020, 12, 15)?, DatePrecision::Month)?;
        assert_eq!(ud.earliest(), Some(d(2020, 12, 1)?));
        assert_eq!(ud.latest(), Some(d(2020, 12, 31)?));
        Ok(())
    }

    // --- Range tests ---

    #[test]
    fn bounded_with_independent_precision() -> TestResult {
        // "sometime between the 1880s and 1899"
        let ud = UncertainDate::bounded(
            Some(DateBound::new(d(1880, 1, 1)?, DatePrecision::Decade)?),
            Some(DateBound::new(d(1899, 1, 1)?, DatePrecision::Year)?),
        )?;
        assert_eq!(ud.earliest(), Some(d(1880, 1, 1)?));
        assert_eq!(ud.latest(), Some(d(1899, 12, 31)?));
        Ok(())
    }

    #[test]
    fn one_sided_before() -> TestResult {
        // "before 1950"
        let ud = UncertainDate::bounded(
            None,
            Some(DateBound::new(d(1950, 1, 1)?, DatePrecision::Year)?),
        )?;
        assert_eq!(ud.earliest(), None);
        assert_eq!(ud.latest(), Some(d(1950, 12, 31)?));
        Ok(())
    }

    #[test]
    fn one_sided_after() -> TestResult {
        // "after 1800"
        let ud = UncertainDate::bounded(
            Some(DateBound::new(d(1800, 1, 1)?, DatePrecision::Year)?),
            None,
        )?;
        assert_eq!(ud.earliest(), Some(d(1800, 1, 1)?));
        assert_eq!(ud.latest(), None);
        Ok(())
    }

    #[test]
    fn unknown_date() {
        let ud = UncertainDate::unknown();
        assert_eq!(ud.earliest(), None);
        assert_eq!(ud.latest(), None);
    }

    // --- Overlap tests ---

    #[test]
    fn overlaps_year_contains_day() -> TestResult {
        let day = UncertainDate::with_precision(d(2020, 6, 15)?, DatePrecision::Day)?;
        let year = UncertainDate::with_precision(d(2020, 1, 1)?, DatePrecision::Year)?;
        assert!(day.overlaps(&year));
        assert!(year.overlaps(&day));
        Ok(())
    }

    #[test]
    fn adjacent_years_no_overlap() -> TestResult {
        let y2019 = UncertainDate::with_precision(d(2019, 6, 15)?, DatePrecision::Year)?;
        let y2020 = UncertainDate::with_precision(d(2020, 6, 15)?, DatePrecision::Year)?;
        assert!(!y2019.overlaps(&y2020));
        Ok(())
    }

    #[test]
    fn unknown_overlaps_everything() -> TestResult {
        let unknown = UncertainDate::unknown();
        let year = UncertainDate::with_precision(d(2020, 1, 1)?, DatePrecision::Year)?;
        assert!(unknown.overlaps(&year));
        assert!(year.overlaps(&unknown));
        assert!(unknown.overlaps(&unknown));
        Ok(())
    }

    // --- Validation tests ---

    #[test]
    fn bounded_rejects_inverted() -> TestResult {
        assert_eq!(
            UncertainDate::bounded(
                Some(DateBound::new(d(2000, 1, 1)?, DatePrecision::Year)?),
                Some(DateBound::new(d(1990, 1, 1)?, DatePrecision::Year)?),
            ),
            Err(DateError::InvertedRange),
        );
        Ok(())
    }

    #[test]
    fn with_precision_rejects_year_0() -> TestResult {
        assert_eq!(
            UncertainDate::with_precision(d(0, 1, 1)?, DatePrecision::Year),
            Err(DateError::Year0),
        );
        Ok(())
    }

    // --- Serde round-trip ---

    #[test]
    fn serde_roundtrip_symmetric() -> TestResult {
        let ud = UncertainDate::with_precision(d(2020, 6, 15)?, DatePrecision::Day)?;
        let json = serde_json::to_string(&ud)?;
        let deserialized: UncertainDate = serde_json::from_str(&json)?;
        assert_eq!(ud, deserialized);
        Ok(())
    }

    #[test]
    fn serde_roundtrip_bounded() -> TestResult {
        let ud = UncertainDate::bounded(
            Some(DateBound::new(d(1920, 1, 1)?, DatePrecision::Year)?),
            Some(DateBound::new(d(1925, 1, 1)?, DatePrecision::Year)?),
        )?;
        let json = serde_json::to_string(&ud)?;
        let deserialized: UncertainDate = serde_json::from_str(&json)?;
        assert_eq!(ud, deserialized);
        Ok(())
    }

    #[test]
    fn serde_roundtrip_one_sided() -> TestResult {
        let ud = UncertainDate::bounded(
            None,
            Some(DateBound::new(d(1950, 1, 1)?, DatePrecision::Year)?),
        )?;
        let json = serde_json::to_string(&ud)?;
        let deserialized: UncertainDate = serde_json::from_str(&json)?;
        assert_eq!(ud, deserialized);
        Ok(())
    }

    #[test]
    fn serde_roundtrip_unknown() -> TestResult {
        let ud = UncertainDate::unknown();
        let json = serde_json::to_string(&ud)?;
        let deserialized: UncertainDate = serde_json::from_str(&json)?;
        assert_eq!(ud, deserialized);
        Ok(())
    }

    #[test]
    fn deserialize_snaps_to_precision_start() -> TestResult {
        let json = r#"{"earliest":{"date":"2020-06-15","precision":"year"},"latest":{"date":"2020-06-15","precision":"year"}}"#;
        let ud: UncertainDate = serde_json::from_str(json)?;
        assert_eq!(ud.earliest(), Some(d(2020, 1, 1)?));
        assert_eq!(ud.latest(), Some(d(2020, 12, 31)?));
        Ok(())
    }

    // --- Lattice unit tests ---

    /// A `year`-precision single-interval union spanning the given year.
    fn year(y: i32) -> Result<UncertainDate, Box<dyn std::error::Error>> {
        Ok(UncertainDate::with_precision(
            d(y, 1, 1)?,
            DatePrecision::Year,
        )?)
    }

    /// A `decade`-precision single-interval union covering the given decade.
    fn decade(y: i32) -> Result<UncertainDate, Box<dyn std::error::Error>> {
        Ok(UncertainDate::with_precision(
            d(y, 1, 1)?,
            DatePrecision::Decade,
        )?)
    }

    #[test]
    fn meet_disjoint_is_empty() -> TestResult {
        let result = year(1920)?.meet(year(1950)?);
        assert_eq!(result, UncertainDate::empty());
        assert_eq!(result.earliest(), None);
        assert_eq!(result.latest(), None);
        Ok(())
    }

    #[test]
    fn meet_overlapping_returns_intersection() -> TestResult {
        let result = decade(1920)?.meet(year(1925)?);
        assert_eq!(result.earliest(), Some(d(1925, 1, 1)?));
        assert_eq!(result.latest(), Some(d(1925, 12, 31)?));
        Ok(())
    }

    #[test]
    fn meet_one_sided_overlapping() -> TestResult {
        // "before 1950" ∩ "after 1940" = 1940–1950
        let before = UncertainDate::bounded(
            None,
            Some(DateBound::new(d(1950, 1, 1)?, DatePrecision::Year)?),
        )?;
        let after = UncertainDate::bounded(
            Some(DateBound::new(d(1940, 1, 1)?, DatePrecision::Year)?),
            None,
        )?;
        let result = before.meet(after);
        assert_eq!(result.earliest(), Some(d(1940, 1, 1)?));
        assert_eq!(result.latest(), Some(d(1950, 12, 31)?));
        Ok(())
    }

    #[test]
    fn meet_one_sided_disjoint() -> TestResult {
        // "before 1940" ∩ "after 1950" = empty
        let before = UncertainDate::bounded(
            None,
            Some(DateBound::new(d(1940, 1, 1)?, DatePrecision::Year)?),
        )?;
        let after = UncertainDate::bounded(
            Some(DateBound::new(d(1950, 1, 1)?, DatePrecision::Year)?),
            None,
        )?;
        assert_eq!(before.meet(after), UncertainDate::empty());
        Ok(())
    }

    #[test]
    fn join_disjoint_stays_disjoint() -> TestResult {
        // "the 1920s" ∨ "1935" keeps two intervals — no invented gap. An
        // envelope hull would collapse to [1920, 1935] and swallow 1930-1934.
        let result = decade(1920)?.join(year(1935)?);
        assert_eq!(result.intervals().len(), 2);
        assert_eq!(result.earliest(), Some(d(1920, 1, 1)?));
        assert_eq!(result.latest(), Some(d(1935, 12, 31)?));
        Ok(())
    }

    #[test]
    fn join_adjacent_years_merge() -> TestResult {
        // 1929 and 1930 touch at the 1929-12-31 / 1930-01-01 boundary.
        let result = year(1929)?.join(year(1930)?);
        assert_eq!(result.intervals().len(), 1);
        assert_eq!(result.earliest(), Some(d(1929, 1, 1)?));
        assert_eq!(result.latest(), Some(d(1930, 12, 31)?));
        Ok(())
    }

    #[test]
    fn join_adjacent_decades_merge() -> TestResult {
        // The 1920s end 1929-12-31, the 1930s start 1930-01-01 — contiguous.
        let result = decade(1920)?.join(decade(1930)?);
        assert_eq!(result.intervals().len(), 1);
        assert_eq!(result.earliest(), Some(d(1920, 1, 1)?));
        assert_eq!(result.latest(), Some(d(1939, 12, 31)?));
        Ok(())
    }

    #[test]
    fn join_gapped_decades_stay_disjoint() -> TestResult {
        // A whole decade sits between the 1920s and the 1940s.
        let result = decade(1920)?.join(decade(1940)?);
        assert_eq!(result.intervals().len(), 2);
        Ok(())
    }

    #[test]
    fn join_overlapping_merges() -> TestResult {
        // "the 1920s" ∨ "1925" — 1925 sits inside, so the result is one interval.
        let result = decade(1920)?.join(year(1925)?);
        assert_eq!(result.intervals().len(), 1);
        assert_eq!(result.earliest(), Some(d(1920, 1, 1)?));
        assert_eq!(result.latest(), Some(d(1929, 12, 31)?));
        Ok(())
    }

    #[test]
    fn join_unknown_absorbs() -> TestResult {
        let result = year(1920)?.join(UncertainDate::unknown());
        assert_eq!(result, UncertainDate::unknown());
        Ok(())
    }

    #[test]
    fn meet_of_disjunction_intersects_each_piece() -> TestResult {
        // {1920, 1940} ∧ "the 1920s" keeps only the 1920 piece.
        let disjunction = year(1920)?.join(year(1940)?);
        assert_eq!(disjunction.intervals().len(), 2);
        let result = disjunction.meet(decade(1920)?);
        assert_eq!(result.intervals().len(), 1);
        assert_eq!(result.earliest(), Some(d(1920, 1, 1)?));
        assert_eq!(result.latest(), Some(d(1920, 12, 31)?));
        Ok(())
    }

    // --- Fold identities ---

    #[test]
    fn join_all_empty_is_bottom() -> TestResult {
        assert_eq!(UncertainDate::join_all([]), UncertainDate::empty());
        Ok(())
    }

    #[test]
    fn meet_all_empty_is_top() -> TestResult {
        assert_eq!(UncertainDate::meet_all([]), UncertainDate::unknown());
        Ok(())
    }

    #[test]
    fn join_all_singleton_is_self() -> TestResult {
        let a = year(1920)?;
        assert_eq!(UncertainDate::join_all([a.clone()]), a);
        Ok(())
    }

    #[test]
    fn join_all_real_bounds_not_poisoned_to_unknown() -> TestResult {
        // Seeding the fold from ⊥ (not ⊤) keeps real bounds — a ⊤ seed would
        // absorb every disjunct to `unknown()`.
        let a = year(1920)?;
        let b = year(1930)?;
        let result = UncertainDate::join_all([a, b]);
        assert_ne!(result, UncertainDate::unknown());
        assert_eq!(result.earliest(), Some(d(1920, 1, 1)?));
        assert_eq!(result.latest(), Some(d(1930, 12, 31)?));
        Ok(())
    }

    // --- as_single_interval ---

    #[test]
    fn as_single_interval_distinguishes_storable_from_not() -> TestResult {
        assert!(year(1920)?.as_single_interval().is_some());
        assert!(UncertainDate::unknown().as_single_interval().is_some());
        assert!(UncertainDate::empty().as_single_interval().is_none());
        let disjunction = year(1920)?.join(year(1940)?);
        assert!(disjunction.as_single_interval().is_none());
        Ok(())
    }

    // --- Wire shape ---

    #[test]
    fn single_interval_serializes_flat() -> TestResult {
        // The stored-fact wire shape: flat `{earliest, latest}`, byte-identical
        // to the former transparent newtype (this pins it independent of the
        // wire_goldens suite).
        let date = UncertainDate::with_precision(d(1700, 1, 1)?, DatePrecision::Year)?;
        assert_eq!(
            serde_json::to_string(&date)?,
            r#"{"earliest":{"date":"1700-01-01","precision":"year"},"latest":{"date":"1700-01-01","precision":"year"}}"#
        );
        Ok(())
    }

    #[test]
    fn unknown_serializes_empty_object() -> TestResult {
        assert_eq!(serde_json::to_string(&UncertainDate::unknown())?, "{}");
        Ok(())
    }

    #[test]
    fn disjunction_serializes_one_of() -> TestResult {
        let disjunction = year(1920)?.join(year(1940)?);
        let json = serde_json::to_string(&disjunction)?;
        assert!(json.starts_with(r#"{"one_of":["#), "got {json}");
        let back: UncertainDate = serde_json::from_str(&json)?;
        assert_eq!(back, disjunction);
        Ok(())
    }

    #[test]
    fn empty_serializes_empty_one_of() -> TestResult {
        assert_eq!(
            serde_json::to_string(&UncertainDate::empty())?,
            r#"{"one_of":[]}"#
        );
        Ok(())
    }

    #[test]
    fn deserialize_non_canonical_one_of_normalizes() -> TestResult {
        // Two adjacent years supplied out of order in a `one_of` collapse to the
        // single merged interval on the way in.
        let json = r#"{"one_of":[{"earliest":{"date":"1930-01-01","precision":"year"},"latest":{"date":"1930-01-01","precision":"year"}},{"earliest":{"date":"1929-01-01","precision":"year"},"latest":{"date":"1929-01-01","precision":"year"}}]}"#;
        let date: UncertainDate = serde_json::from_str(json)?;
        assert_eq!(date, year(1929)?.join(year(1930)?));
        assert_eq!(date.intervals().len(), 1);
        Ok(())
    }

    // --- Serde ---

    #[test]
    fn deserialize_rejects_year_0() {
        let json = r#"{"earliest":{"date":"0000-01-01","precision":"year"}}"#;
        assert!(serde_json::from_str::<UncertainDate>(json).is_err());
    }

    #[test]
    fn deserialize_rejects_inverted_range() {
        let json = r#"{"earliest":{"date":"1990-01-01","precision":"year"},"latest":{"date":"1980-01-01","precision":"year"}}"#;
        assert!(serde_json::from_str::<UncertainDate>(json).is_err());
    }

    // --- Property-based tests ---
    //
    // The date generator biases toward edge cases: period boundaries,
    // BCE/CE transition, and year 0 (which should always be rejected).

    fn arb_naive_date() -> impl Strategy<Value = NaiveDate> {
        let edge_years = prop_oneof![
            Just(0),     // year 0 — should always be rejected
            Just(1),     // first CE year
            Just(-1),    // first BCE year
            Just(9),     // near decade boundary (CE)
            Just(10),    // decade boundary
            Just(-9),    // near decade boundary (BCE)
            Just(-10),   // decade boundary (BCE)
            Just(100),   // century boundary
            Just(101),   // century boundary + 1
            Just(-100),  // century boundary (BCE)
            Just(-101),  // century boundary + 1 (BCE)
            Just(1000),  // millennium boundary
            Just(1001),  // millennium boundary + 1
            Just(-1000), // millennium boundary (BCE)
            Just(-1001), // millennium boundary + 1 (BCE)
        ];
        let random_years = -3000i32..=3000;
        // ~30% edge cases, ~70% random exploration
        let years = prop_oneof![3 => edge_years, 7 => random_years];

        (years, 1u32..=12, 1u32..=28)
            .prop_filter_map("valid date", |(y, m, d)| NaiveDate::from_ymd_opt(y, m, d))
    }

    fn arb_precision() -> impl Strategy<Value = DatePrecision> {
        prop_oneof![
            Just(DatePrecision::Day),
            Just(DatePrecision::Month),
            Just(DatePrecision::Year),
            Just(DatePrecision::Decade),
            Just(DatePrecision::Century),
            Just(DatePrecision::Millennium),
        ]
    }

    fn symmetric(opt: Option<NaiveDate>) -> Result<NaiveDate, TestCaseError> {
        opt.ok_or_else(|| TestCaseError::fail("symmetric bounds should always be Some"))
    }

    /// Generate a single-interval `UncertainDate`: unknown (⊤), one-sided, or
    /// symmetric. The building block both extremes and disjunctions are built
    /// from.
    fn arb_single_interval() -> impl Strategy<Value = UncertainDate> {
        let unknown = Just(UncertainDate::unknown());
        let symmetric = (arb_naive_date(), arb_precision())
            .prop_filter_map("valid symmetric date", |(date, prec)| {
                UncertainDate::with_precision(date, prec).ok()
            });
        let one_sided_before = (arb_naive_date(), arb_precision()).prop_filter_map(
            "valid before date",
            |(date, prec)| {
                let bound = DateBound::new(date, prec).ok()?;
                UncertainDate::bounded(None, Some(bound)).ok()
            },
        );
        let one_sided_after = (arb_naive_date(), arb_precision()).prop_filter_map(
            "valid after date",
            |(date, prec)| {
                let bound = DateBound::new(date, prec).ok()?;
                UncertainDate::bounded(Some(bound), None).ok()
            },
        );
        prop_oneof![
            2 => unknown,
            2 => one_sided_before,
            2 => one_sided_after,
            6 => symmetric,
        ]
    }

    /// Generate an arbitrary `UncertainDate` across the whole lattice: single
    /// intervals, the ⊥ extreme, and disjunctions of up to four pieces (joined
    /// so the result is canonical, possibly merging back to fewer intervals).
    fn arb_uncertain_date() -> impl Strategy<Value = UncertainDate> {
        let empty = Just(UncertainDate::empty());
        let disjunction =
            prop::collection::vec(arb_single_interval(), 2..=4).prop_map(UncertainDate::join_all);
        prop_oneof![
            6 => arb_single_interval(),
            1 => empty,
            3 => disjunction,
        ]
    }

    /// The canonical-form invariant: intervals sorted by lower endpoint and
    /// pairwise non-touching (no two could merge). Every lattice op must
    /// preserve it.
    fn is_canonical(date: &UncertainDate) -> bool {
        date.intervals()
            .windows(2)
            .all(|w| w[0].cmp_by_span(&w[1]).is_lt() && !w[0].adjacent_or_overlapping(&w[1]))
    }

    proptest! {
        #[test]
        fn prop_earliest_lte_latest(date in arb_naive_date(), precision in arb_precision()) {
            if date.year() == 0 {
                prop_assert_eq!(
                    UncertainDate::with_precision(date, precision),
                    Err(DateError::Year0),
                );
            } else {
                let ud = UncertainDate::with_precision(date, precision)
                    .map_err(|e| TestCaseError::fail(e.to_string()))?;
                let e = symmetric(ud.earliest())?;
                let l = symmetric(ud.latest())?;
                prop_assert!(e <= l,
                    "earliest ({}) should be <= latest ({}) for {:?}",
                    e, l, precision);
            }
        }

        #[test]
        fn prop_date_within_precision_bounds(date in arb_naive_date(), precision in arb_precision()) {
            if date.year() == 0 {
                prop_assert_eq!(
                    UncertainDate::with_precision(date, precision),
                    Err(DateError::Year0),
                );
            } else {
                let ud = UncertainDate::with_precision(date, precision)
                    .map_err(|e| TestCaseError::fail(e.to_string()))?;
                let e = symmetric(ud.earliest())?;
                let l = symmetric(ud.latest())?;
                prop_assert!(e <= date);
                prop_assert!(date <= l);
            }
        }

        #[test]
        fn prop_with_precision_is_idempotent(date in arb_naive_date(), precision in arb_precision()) {
            if date.year() == 0 {
                prop_assert_eq!(
                    UncertainDate::with_precision(date, precision),
                    Err(DateError::Year0),
                );
            } else {
                let ud1 = UncertainDate::with_precision(date, precision)
                    .map_err(|e| TestCaseError::fail(e.to_string()))?;
                let e1 = symmetric(ud1.earliest())?;
                let ud2 = UncertainDate::with_precision(e1, precision)
                    .map_err(|e| TestCaseError::fail(e.to_string()))?;
                prop_assert_eq!(ud1.earliest(), ud2.earliest());
                prop_assert_eq!(ud1.latest(), ud2.latest());
            }
        }

        /// Closure: meet/join of any two canonical unions is itself canonical.
        #[test]
        fn prop_ops_preserve_canonical_form(
            a in arb_uncertain_date(),
            b in arb_uncertain_date(),
        ) {
            prop_assert!(is_canonical(&UncertainDate::meet(&a, &b)));
            prop_assert!(is_canonical(&UncertainDate::join(&a, &b)));
        }

        #[test]
        fn prop_overlaps_consistent_with_meet(
            a in arb_uncertain_date(),
            b in arb_uncertain_date(),
        ) {
            prop_assert_eq!(
                a.overlaps(&b),
                UncertainDate::meet(&a, &b) != UncertainDate::empty()
            );
        }

        /// Canonicalization is confluent: the same multiset of intervals in any
        /// permutation canonicalizes to one form. Load-bearing for `CommitId`
        /// determinism and `BTreeSet<SubmitFact>` dedup.
        #[test]
        fn prop_canonicalization_confluent(
            pieces in prop::collection::vec(arb_single_interval(), 0..=6),
            perm in prop::collection::vec(any::<prop::sample::Index>(), 0..=6),
        ) {
            // Flatten the generated single-interval unions into raw intervals.
            let ranges: Vec<TimeRange> =
                pieces.iter().flat_map(|p| p.intervals().iter().cloned()).collect();
            let mut shuffled = ranges.clone();
            // Fisher-Yates over the proptest-supplied index stream.
            let n = shuffled.len();
            for (i, idx) in perm.iter().enumerate().take(n) {
                let j = i + idx.index(n - i);
                shuffled.swap(i, j);
            }
            let canon_a = UncertainDate::from_ranges(ranges);
            let canon_b = UncertainDate::from_ranges(shuffled);
            prop_assert_eq!(canon_a, canon_b);
        }

        /// Round-trip stability: a canonical date survives serialize →
        /// deserialize unchanged, across single / disjunction / ⊥ / ⊤.
        #[test]
        fn prop_serde_round_trip(a in arb_uncertain_date()) {
            let json = serde_json::to_string(&a)?;
            let back: UncertainDate = serde_json::from_str(&json)?;
            prop_assert_eq!(back, a);
        }
    }

    /// Two dates denote the same instant-set iff their canonical intervals
    /// cover the same days. Canonicalization sorts and coalesces by denoted day
    /// ([`TimeRange::cmp_by_span`] / [`TimeRange::adjacent_or_overlapping`] both
    /// key on `period_start`/`period_end`), so the `(period_start, period_end)`
    /// day-range sequences are aligned and compare directly. `DatePrecision` is
    /// dropped: the lattice is distributive over denoted days, not over the
    /// precision-bearing structural form, since `meet`/`join` select bounds by
    /// `(boundary_day, precision)` and two bounds can denote the same boundary
    /// day at different precisions.
    ///
    /// The precision-clobbering this papers over is not the desired behavior;
    /// it will be resolved in an upcoming commit.
    fn date_denotes_same(a: &UncertainDate, b: &UncertainDate) -> bool {
        fn day_ranges(d: &UncertainDate) -> Vec<(Option<NaiveDate>, Option<NaiveDate>)> {
            d.intervals()
                .iter()
                .map(|r| {
                    (
                        r.earliest().map(DateBound::period_start),
                        r.latest().map(DateBound::period_end),
                    )
                })
                .collect()
        }
        day_ranges(a) == day_ranges(b)
    }

    #[test]
    fn date_denotes_same_discriminates() -> TestResult {
        // Genuinely different instant-sets must compare unequal, else the laws
        // pass vacuously.
        assert!(!date_denotes_same(&year(1000)?, &year(1001)?));

        // Equi-denotational, structurally different: "1927" at year precision
        // and the same span pinned with day-precision bounds cover the same days
        // but carry different `DatePrecision`, so structural `==` would split
        // them while the denotational oracle treats them as one.
        let year_form = year(1927)?;
        let day_form = UncertainDate::bounded(
            Some(DateBound::new(d(1927, 1, 1)?, DatePrecision::Day)?),
            Some(DateBound::new(d(1927, 12, 31)?, DatePrecision::Day)?),
        )?;
        assert_ne!(year_form, day_form);
        assert!(date_denotes_same(&year_form, &day_form));
        Ok(())
    }

    // Core laws run structurally (`==`), guarding canonical-form confluence that
    // `CommitId` and `BTreeSet<SubmitFact>` dedup depend on. The order/lattice
    // laws run denotationally: `meet`/`join` select bounds by `(day, precision)`,
    // so distributivity rearranges which precision-tagged bound survives while
    // denoting the same days — `date_denotes_same` ignores that tag.
    crate::lattice_laws!(
        lattice_laws,
        UncertainDate,
        arb_uncertain_date(),
        date_denotes_same
    );
}
