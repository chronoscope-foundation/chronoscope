//! Date types with uncertainty support.
//!
//! [`UncertainDate`] models epistemic uncertainty about a single instant in time.
//! "Built in the 1920s" means "construction started at some unknown instant within
//! \[1920, 1929\]" — not "construction spanned the entire decade." Duration is modeled
//! structurally by pairing two uncertain instants (`started_at` + `completed_at` in
//! [`EntityTransition`](crate::entity::EntityTransition)), not by widening a single date.
//!
//! # Representation
//!
//! An `UncertainDate` is a pair of optional [`DateBound`] endpoints, where each bound
//! carries a [`NaiveDate`] and a [`DatePrecision`] indicating the granularity of the
//! boundary itself. Precision lives on bounds, not on values: "before 1950" has a
//! year-granularity upper bound, distinct from "before January 1, 1950" which has a
//! day-granularity bound.
//!
//! | Human expression | `earliest` | `latest` |
//! |---|---|---|
//! | "1927" | `DateBound(1927-01-01, Year)` | `DateBound(1927-01-01, Year)` |
//! | "The 1920s" | `DateBound(1920-01-01, Decade)` | `DateBound(1920-01-01, Decade)` |
//! | "Before 1950" | `None` | `DateBound(1950-01-01, Year)` |
//! | "After 1800" | `DateBound(1800-01-01, Year)` | `None` |
//! | Unknown | `None` | `None` |
//!
//! `(None, None)` is the identity element for meet (intersection).
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
//!   in practice it behaves as one. See <https://www.wikidata.org/wiki/Help:Dates>.

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
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

    /// The snapped date (start of the precision period).
    #[must_use]
    pub fn date(&self) -> NaiveDate {
        self.date
    }

    /// The precision level.
    #[must_use]
    pub fn precision(&self) -> DatePrecision {
        self.precision
    }

    /// The last day of this bound's precision period.
    #[must_use]
    pub fn period_end(&self) -> NaiveDate {
        precision_end(self.date, self.precision)
    }
}

/// A date with uncertainty, represented as a pair of optional bounds.
///
/// Each bound carries its own [`DatePrecision`]. `None` means unbounded
/// in that direction (the entire past or future). `(None, None)` represents
/// a completely unknown date.
///
/// # Construction
///
/// - [`UncertainDate::with_precision`] — symmetric bounds (e.g., "1927" or "the 1920s")
/// - [`UncertainDate::bounded`] — asymmetric or one-sided (e.g., "before 1950")
/// - [`UncertainDate::unknown`] — completely unknown `(None, None)`
///
/// Year 0 is rejected in all constructors.
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct UncertainDate {
    earliest: Option<DateBound>,
    latest: Option<DateBound>,
}

impl<'de> Deserialize<'de> for UncertainDate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            earliest: Option<DateBound>,
            latest: Option<DateBound>,
        }
        let raw = Raw::deserialize(deserializer)?;
        UncertainDate::bounded(raw.earliest, raw.latest).map_err(serde::de::Error::custom)
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
        Ok(Self {
            earliest: Some(bound),
            latest: Some(bound),
        })
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
        if let (Some(e), Some(l)) = (&earliest, &latest)
            && e.date() > l.period_end()
        {
            return Err(DateError::InvertedRange);
        }
        Ok(Self { earliest, latest })
    }

    /// Create a completely unknown date `(None, None)`.
    ///
    /// This is the identity element for meet (intersection).
    #[must_use]
    pub fn unknown() -> Self {
        Self {
            earliest: None,
            latest: None,
        }
    }

    /// The earliest possible date, or `None` if unbounded.
    #[must_use]
    pub fn earliest(&self) -> Option<NaiveDate> {
        self.earliest.as_ref().map(DateBound::date)
    }

    /// The latest possible date, or `None` if unbounded.
    #[must_use]
    pub fn latest(&self) -> Option<NaiveDate> {
        self.latest.as_ref().map(DateBound::period_end)
    }

    /// The earliest bound (with precision), if present.
    #[must_use]
    pub fn earliest_bound(&self) -> Option<&DateBound> {
        self.earliest.as_ref()
    }

    /// The latest bound (with precision), if present.
    #[must_use]
    pub fn latest_bound(&self) -> Option<&DateBound> {
        self.latest.as_ref()
    }

    /// Check if this date range overlaps with another.
    ///
    /// Two ranges overlap if there's any point in time that falls within both.
    /// Unbounded endpoints overlap with everything.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.meet(other).is_some()
    }

    /// Intersection of two uncertain dates (meet in the lattice).
    ///
    /// Returns the tightest range contained by both, or `None` if the ranges
    /// are disjoint. Selects bounds from the inputs — never manufactures new
    /// `DateBound` values.
    ///
    /// `unknown().meet(x) == Some(x)` — unknown is the identity.
    #[must_use]
    pub fn meet(&self, other: &Self) -> Option<Self> {
        let earliest = tighten_earliest(&self.earliest, &other.earliest);
        let latest = tighten_latest(&self.latest, &other.latest);

        // Check for disjoint: if both bounds are present and earliest > latest
        if let (Some(e), Some(l)) = (earliest.as_ref(), latest.as_ref())
            && e.date() > l.period_end()
        {
            return None;
        }

        Some(Self { earliest, latest })
    }

    /// Bounding interval of two uncertain dates (join in the lattice).
    ///
    /// Returns the smallest range containing both. Always succeeds.
    /// Selects bounds from the inputs — never manufactures new `DateBound` values.
    ///
    /// `unknown().join(x) == unknown()` — unknown absorbs everything.
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        Self {
            earliest: widen_earliest(&self.earliest, &other.earliest),
            latest: widen_latest(&self.latest, &other.latest),
        }
    }
}

/// For the earliest bound: the effective boundary is `date()` (period start).
/// For the latest bound: the effective boundary is `period_end()` (period end).
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

#[allow(clippy::expect_used)]
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
            #[allow(clippy::expect_used)]
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
    use proptest::prelude::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn d(y: i32, m: u32, d: u32) -> Result<NaiveDate, &'static str> {
        NaiveDate::from_ymd_opt(y, m, d).ok_or("invalid date")
    }

    // --- DateBound tests ---

    #[test]
    fn date_bound_snaps_to_precision() -> TestResult {
        let db = DateBound::new(d(2020, 6, 15)?, DatePrecision::Year)?;
        assert_eq!(db.date(), d(2020, 1, 1)?);
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

    #[test]
    fn meet_disjoint_returns_none() -> TestResult {
        let a = UncertainDate::with_precision(d(1920, 1, 1)?, DatePrecision::Year)?;
        let b = UncertainDate::with_precision(d(1950, 1, 1)?, DatePrecision::Year)?;
        assert_eq!(a.meet(&b), None);
        Ok(())
    }

    #[test]
    fn meet_overlapping_returns_intersection() -> TestResult {
        let decade = UncertainDate::with_precision(d(1920, 1, 1)?, DatePrecision::Decade)?;
        let year = UncertainDate::with_precision(d(1925, 1, 1)?, DatePrecision::Year)?;
        let result = decade.meet(&year);
        assert!(result.is_some());
        let result = result.ok_or("expected Some")?;
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
        let result = before.meet(&after).ok_or("expected overlap")?;
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
        assert_eq!(before.meet(&after), None);
        Ok(())
    }

    #[test]
    fn join_widens_to_bounding_interval() -> TestResult {
        let a = UncertainDate::with_precision(d(1920, 1, 1)?, DatePrecision::Year)?;
        let b = UncertainDate::with_precision(d(1950, 1, 1)?, DatePrecision::Year)?;
        let result = a.join(&b);
        assert_eq!(result.earliest(), Some(d(1920, 1, 1)?));
        assert_eq!(result.latest(), Some(d(1950, 12, 31)?));
        Ok(())
    }

    #[test]
    fn join_unknown_absorbs() -> TestResult {
        let a = UncertainDate::with_precision(d(1920, 1, 1)?, DatePrecision::Year)?;
        let result = a.join(&UncertainDate::unknown());
        assert_eq!(result, UncertainDate::unknown());
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

    /// Generate an arbitrary `UncertainDate`: ~20% unknown, ~20% one-sided, ~60% symmetric.
    fn arb_uncertain_date() -> impl Strategy<Value = UncertainDate> {
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

        // --- Lattice property tests ---

        #[test]
        fn prop_meet_commutative(a in arb_uncertain_date(), b in arb_uncertain_date()) {
            prop_assert_eq!(a.meet(&b), b.meet(&a));
        }

        #[test]
        fn prop_meet_associative(
            a in arb_uncertain_date(),
            b in arb_uncertain_date(),
            c in arb_uncertain_date(),
        ) {
            let ab_c = a.meet(&b).and_then(|ab| ab.meet(&c));
            let a_bc = b.meet(&c).and_then(|bc| a.meet(&bc));
            prop_assert_eq!(ab_c, a_bc);
        }

        #[test]
        fn prop_meet_identity(a in arb_uncertain_date()) {
            let unknown = UncertainDate::unknown();
            prop_assert_eq!(a.meet(&unknown), Some(a.clone()));
            prop_assert_eq!(unknown.meet(&a), Some(a));
        }

        #[test]
        fn prop_join_commutative(a in arb_uncertain_date(), b in arb_uncertain_date()) {
            prop_assert_eq!(a.join(&b), b.join(&a));
        }

        #[test]
        fn prop_join_associative(
            a in arb_uncertain_date(),
            b in arb_uncertain_date(),
            c in arb_uncertain_date(),
        ) {
            prop_assert_eq!(a.join(&b).join(&c), a.join(&b.join(&c)));
        }

        #[test]
        fn prop_meet_idempotent(a in arb_uncertain_date()) {
            prop_assert_eq!(a.meet(&a), Some(a));
        }

        #[test]
        fn prop_join_idempotent(a in arb_uncertain_date()) {
            prop_assert_eq!(a.join(&a), a);
        }

        #[test]
        fn prop_overlaps_consistent_with_meet(
            a in arb_uncertain_date(),
            b in arb_uncertain_date(),
        ) {
            prop_assert_eq!(a.overlaps(&b), a.meet(&b).is_some());
        }
    }
}
