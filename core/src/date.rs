//! Date types with uncertainty support.
//!
//! Provides types for representing dates with various levels of precision,
//! from exact seconds to millennia.

use std::fmt;

use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};
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

/// Precision level for dates
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DatePrecision {
    Second,
    Minute,
    Hour,
    Day,
    Month,
    Year,
    Decade,
    /// Century using the historical convention: 21st century = 2001-2100 (year 1-based).
    Century,
    /// Millennium using the historical convention: 2nd millennium = 1001-2000 (year 1-based).
    Millennium,
}

/// A date/time with known precision.
///
/// The datetime is always snapped to the start of its precision period.
/// For example, `Year` precision for 2020 stores `2020-01-01T00:00:00`.
///
/// Year 0 is rejected — use negative years for BCE dates (e.g., -1 for 1 BCE).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PreciseDate {
    datetime: NaiveDateTime,
    precision: DatePrecision,
}

impl<'de> Deserialize<'de> for PreciseDate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            datetime: NaiveDateTime,
            precision: DatePrecision,
        }
        let raw = Raw::deserialize(deserializer)?;
        PreciseDate::new(raw.datetime, raw.precision).map_err(serde::de::Error::custom)
    }
}

impl PreciseDate {
    /// Create a new `PreciseDate`, snapping the datetime to its precision period start.
    ///
    /// Returns `Err(DateError::Year0)` if the datetime has year 0.
    pub fn new(datetime: NaiveDateTime, precision: DatePrecision) -> Result<Self, DateError> {
        if datetime.year() == 0 {
            return Err(DateError::Year0);
        }
        let snapped = snap_to_precision_start(datetime, precision);
        Ok(Self {
            datetime: snapped,
            precision,
        })
    }

    /// The snapped datetime (start of the precision period).
    #[must_use]
    pub fn datetime(&self) -> NaiveDateTime {
        self.datetime
    }

    /// The precision level.
    #[must_use]
    pub fn precision(&self) -> DatePrecision {
        self.precision
    }

    /// The earliest possible moment (same as `datetime()`).
    #[must_use]
    pub fn earliest(&self) -> NaiveDateTime {
        self.datetime
    }

    /// The latest possible moment within this precision period.
    #[must_use]
    pub fn latest(&self) -> NaiveDateTime {
        precision_end(self.datetime, self.precision)
    }
}

/// Represents a date/time with uncertainty.
///
/// Uses `chrono::NaiveDateTime` (proleptic Gregorian calendar) which supports dates
/// from ±262K years and preserves sub-day precision for EXIF timestamps and similar sources.
///
/// Construct via `exact()`, `with_precision()`, or `range()`. The `Precise` representation
/// enforces that the datetime is always snapped to the start of its precision period — this
/// invariant is maintained by construction and cannot be violated by external code.
///
/// **Year 0 is rejected** in all constructors — use negative years for BCE dates
/// (e.g., -1 for 1 BCE). There is no year 0 in historical convention.
///
/// **Note**: There is no "Unknown" representation. Use `Option<UncertainDate>` when a date
/// may be completely unknown. This allows `Cited<UncertainDate>` to always have a meaningful value.
///
/// **Convention**: Date ranges use half-open semantics conceptually — `earliest()` is the
/// *terminus post quem* (earliest possible moment) and `latest()` is the *terminus ante quem*
/// (latest possible moment, inclusive at second resolution).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct UncertainDate(UncertainDateInner);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
enum UncertainDateInner {
    /// A date with known precision (second through millennium).
    Precise(PreciseDate),
    /// A date range with independently precise endpoints.
    ///
    /// Represents source-asserted uncertainty about a single point in time,
    /// e.g., "between 1850 and 1854" from a single source. Each endpoint
    /// carries its own precision level.
    Range {
        earliest: PreciseDate,
        latest: PreciseDate,
    },
}

impl JsonSchema for UncertainDate {
    fn schema_name() -> String {
        "UncertainDate".to_string()
    }

    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        UncertainDateInner::json_schema(generator)
    }
}

// Custom deserializer: PreciseDate's Deserialize handles year 0 rejection and snapping;
// Range validation (earliest <= latest) is checked by UncertainDate::range().
impl<'de> Deserialize<'de> for UncertainDate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum UncertainDateRaw {
            Precise(PreciseDate),
            Range {
                earliest: PreciseDate,
                latest: PreciseDate,
            },
        }

        let raw = UncertainDateRaw::deserialize(deserializer)?;
        match raw {
            UncertainDateRaw::Precise(pd) => Ok(Self(UncertainDateInner::Precise(pd))),
            UncertainDateRaw::Range { earliest, latest } => {
                UncertainDate::range(earliest, latest).map_err(serde::de::Error::custom)
            }
        }
    }
}

impl UncertainDate {
    /// Create an `UncertainDate` for an exact known date/time (second precision).
    ///
    /// Returns `Err(DateError::Year0)` if the datetime has year 0.
    pub fn exact(datetime: NaiveDateTime) -> Result<Self, DateError> {
        Ok(Self(UncertainDateInner::Precise(PreciseDate::new(
            datetime,
            DatePrecision::Second,
        )?)))
    }

    /// Create an `UncertainDate` with specific precision.
    /// The datetime is snapped to the start of the precision period for canonical representation.
    ///
    /// Returns `Err(DateError::Year0)` if the datetime has year 0.
    pub fn with_precision(
        datetime: NaiveDateTime,
        precision: DatePrecision,
    ) -> Result<Self, DateError> {
        Ok(Self(UncertainDateInner::Precise(PreciseDate::new(
            datetime, precision,
        )?)))
    }

    /// Create an `UncertainDate` for a date range with independently precise endpoints.
    ///
    /// Each endpoint is a `PreciseDate` with its own precision level. For example,
    /// "between the 1880s and 1899" would use decade precision for the earliest
    /// endpoint and year precision for the latest.
    ///
    /// Returns `Err(DateError::InvertedRange)` if `earliest.earliest() > latest.latest()`.
    pub fn range(earliest: PreciseDate, latest: PreciseDate) -> Result<Self, DateError> {
        if earliest.earliest() > latest.latest() {
            return Err(DateError::InvertedRange);
        }
        Ok(Self(UncertainDateInner::Range { earliest, latest }))
    }

    /// Returns the precision level if this is a precise date, or `None` for ranges.
    #[must_use]
    pub fn precision(&self) -> Option<DatePrecision> {
        match &self.0 {
            UncertainDateInner::Precise(pd) => Some(pd.precision()),
            UncertainDateInner::Range { .. } => None,
        }
    }

    /// Get the earliest possible datetime for this uncertain date.
    #[must_use]
    pub fn earliest(&self) -> NaiveDateTime {
        match &self.0 {
            UncertainDateInner::Precise(pd) => pd.earliest(),
            UncertainDateInner::Range { earliest, .. } => earliest.earliest(),
        }
    }

    /// Get the latest possible datetime for this uncertain date.
    #[must_use]
    pub fn latest(&self) -> NaiveDateTime {
        match &self.0 {
            UncertainDateInner::Precise(pd) => pd.latest(),
            UncertainDateInner::Range { latest, .. } => latest.latest(),
        }
    }

    /// Check if this date range overlaps with another.
    ///
    /// Two ranges overlap if there's any point in time that falls within both.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.earliest() <= other.latest() && other.earliest() <= self.latest()
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
fn ymd_midnight(y: i32, m: u32, d: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, m, d)
        .expect("precision arithmetic: valid date components")
        .and_hms_opt(0, 0, 0)
        .expect("midnight is always valid")
}

#[allow(clippy::expect_used)]
fn ymd_end_of_day(y: i32, m: u32, d: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, m, d)
        .expect("precision arithmetic: valid date components")
        .and_hms_opt(23, 59, 59)
        .expect("23:59:59 is always valid")
}

#[allow(clippy::expect_used)]
fn date_hms(d: NaiveDate, h: u32, m: u32, s: u32) -> NaiveDateTime {
    d.and_hms_opt(h, m, s)
        .expect("precision arithmetic: valid time components")
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

/// Calculate the start of a precision period.
///
/// Century and millennium use the historical convention (1-based).
/// Decades use the common convention (0-based): 1980s = 1980–1989.
fn snap_to_precision_start(dt: NaiveDateTime, precision: DatePrecision) -> NaiveDateTime {
    match precision {
        DatePrecision::Second => dt,
        DatePrecision::Minute => date_hms(dt.date(), dt.hour(), dt.minute(), 0),
        DatePrecision::Hour => date_hms(dt.date(), dt.hour(), 0, 0),
        DatePrecision::Day => date_hms(dt.date(), 0, 0, 0),
        DatePrecision::Month => ymd_midnight(dt.year(), dt.month(), 1),
        DatePrecision::Year => ymd_midnight(dt.year(), 1, 1),
        DatePrecision::Decade => {
            let mut decade_start = dt.year().div_euclid(10) * 10;
            // Decades are 0-based (1980s = 1980–1989), so years 1-9 CE snap to
            // "decade 0" — but year 0 doesn't exist historically, so clamp to 1.
            // Century/millennium don't need this because their 1-based convention
            // naturally avoids year 0.
            if decade_start == 0 {
                decade_start = 1;
            }
            ymd_midnight(decade_start, 1, 1)
        }
        DatePrecision::Century => ymd_midnight(period_start_year(dt.year(), 100), 1, 1),
        DatePrecision::Millennium => ymd_midnight(period_start_year(dt.year(), 1000), 1, 1),
    }
}

/// Calculate the end of a precision period.
///
/// See [`snap_to_precision_start`] for the century/millennium convention.
fn precision_end(dt: NaiveDateTime, precision: DatePrecision) -> NaiveDateTime {
    match precision {
        DatePrecision::Second => dt,
        DatePrecision::Minute => date_hms(dt.date(), dt.hour(), dt.minute(), 59),
        DatePrecision::Hour => date_hms(dt.date(), dt.hour(), 59, 59),
        DatePrecision::Day => date_hms(dt.date(), 23, 59, 59),
        DatePrecision::Month => {
            let (next_year, next_month) = if dt.month() == 12 {
                (dt.year() + 1, 1)
            } else {
                (dt.year(), dt.month() + 1)
            };
            // Last day of current month = day before first of next month.
            // Note: chrono supports year 0 internally, so December of year -1
            // correctly rolls to Jan 1 of year 0 (chrono) for the subtraction.
            #[allow(clippy::expect_used)]
            let last_day = NaiveDate::from_ymd_opt(next_year, next_month, 1)
                .expect("precision arithmetic: valid next-month date")
                .pred_opt()
                .expect("precision arithmetic: predecessor of valid date");
            date_hms(last_day, 23, 59, 59)
        }
        DatePrecision::Year => ymd_end_of_day(dt.year(), 12, 31),
        DatePrecision::Decade => {
            let decade_end = dt.year().div_euclid(10) * 10 + 9;
            ymd_end_of_day(decade_end, 12, 31)
        }
        DatePrecision::Century => ymd_end_of_day(period_end_year(dt.year(), 100), 12, 31),
        DatePrecision::Millennium => ymd_end_of_day(period_end_year(dt.year(), 1000), 12, 31),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn dt(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> Result<NaiveDateTime, &'static str> {
        NaiveDate::from_ymd_opt(y, m, d)
            .ok_or("invalid date")?
            .and_hms_opt(h, min, s)
            .ok_or("invalid time")
    }

    fn midnight(y: i32, m: u32, d: u32) -> Result<NaiveDateTime, &'static str> {
        dt(y, m, d, 0, 0, 0)
    }

    fn end_of_day(y: i32, m: u32, d: u32) -> Result<NaiveDateTime, &'static str> {
        dt(y, m, d, 23, 59, 59)
    }

    // --- PreciseDate tests ---

    #[test]
    fn precise_date_snaps_to_precision() -> TestResult {
        let pd = PreciseDate::new(dt(2020, 6, 15, 14, 30, 45)?, DatePrecision::Year)?;
        assert_eq!(pd.datetime(), midnight(2020, 1, 1)?);
        assert_eq!(pd.earliest(), midnight(2020, 1, 1)?);
        assert_eq!(pd.latest(), end_of_day(2020, 12, 31)?);
        assert_eq!(pd.precision(), DatePrecision::Year);
        Ok(())
    }

    #[test]
    fn precise_date_rejects_year_0() -> TestResult {
        assert_eq!(
            PreciseDate::new(midnight(0, 1, 1)?, DatePrecision::Year),
            Err(DateError::Year0),
        );
        Ok(())
    }

    #[test]
    fn precise_date_serde_roundtrip() -> TestResult {
        let pd = PreciseDate::new(midnight(1850, 6, 15)?, DatePrecision::Year)?;
        let json = serde_json::to_string(&pd)?;
        let deserialized: PreciseDate = serde_json::from_str(&json)?;
        assert_eq!(pd, deserialized);
        Ok(())
    }

    #[test]
    fn precise_date_deserialize_rejects_year_0() {
        let json = r#"{"datetime":"0000-01-01T00:00:00","precision":"year"}"#;
        assert!(serde_json::from_str::<PreciseDate>(json).is_err());
    }

    // --- BCE/boundary edge cases ---
    //
    // These verify specific expected values at period boundaries where the
    // 0-based (decade) and 1-based (century, millennium) conventions interact
    // with the nonexistent year 0. The general properties (earliest <= input
    // <= latest, idempotence, etc.) are covered by proptests below.

    #[test]
    fn bce_decade_boundary() -> TestResult {
        // Year -10 should be in the [-10, -1] decade
        let ud = UncertainDate::with_precision(midnight(-10, 1, 1)?, DatePrecision::Decade)?;
        assert_eq!(ud.earliest(), midnight(-10, 1, 1)?);
        assert_eq!(ud.latest(), end_of_day(-1, 12, 31)?);
        Ok(())
    }

    #[test]
    fn bce_century_boundary() -> TestResult {
        // Year -100 (100 BCE) is still in the 1st century BCE
        let ud = UncertainDate::with_precision(midnight(-100, 1, 1)?, DatePrecision::Century)?;
        assert_eq!(ud.earliest(), midnight(-100, 1, 1)?);
        assert_eq!(ud.latest(), end_of_day(-1, 12, 31)?);

        // Year -101 (101 BCE) is in the 2nd century BCE: -200 to -101
        let ud = UncertainDate::with_precision(midnight(-101, 1, 1)?, DatePrecision::Century)?;
        assert_eq!(ud.earliest(), midnight(-200, 1, 1)?);
        assert_eq!(ud.latest(), end_of_day(-101, 12, 31)?);
        Ok(())
    }

    #[test]
    fn bce_millennium_boundary() -> TestResult {
        // Year -1000 is still in the 1st millennium BCE
        let ud = UncertainDate::with_precision(midnight(-1000, 1, 1)?, DatePrecision::Millennium)?;
        assert_eq!(ud.earliest(), midnight(-1000, 1, 1)?);
        assert_eq!(ud.latest(), end_of_day(-1, 12, 31)?);

        // Year -1001 is in the 2nd millennium BCE: -2000 to -1001
        let ud = UncertainDate::with_precision(midnight(-1001, 1, 1)?, DatePrecision::Millennium)?;
        assert_eq!(ud.earliest(), midnight(-2000, 1, 1)?);
        assert_eq!(ud.latest(), end_of_day(-1001, 12, 31)?);
        Ok(())
    }

    #[test]
    fn february_end_of_month() -> TestResult {
        // Non-leap year
        let ud = UncertainDate::with_precision(midnight(2019, 2, 15)?, DatePrecision::Month)?;
        assert_eq!(ud.latest(), end_of_day(2019, 2, 28)?);

        // Leap year
        let ud = UncertainDate::with_precision(midnight(2020, 2, 15)?, DatePrecision::Month)?;
        assert_eq!(ud.latest(), end_of_day(2020, 2, 29)?);
        Ok(())
    }

    #[test]
    fn december_end_of_month() -> TestResult {
        let ud = UncertainDate::with_precision(midnight(2020, 12, 15)?, DatePrecision::Month)?;
        assert_eq!(ud.earliest(), midnight(2020, 12, 1)?);
        assert_eq!(ud.latest(), end_of_day(2020, 12, 31)?);
        Ok(())
    }

    // --- Range tests ---

    #[test]
    fn range_with_independent_precision() -> TestResult {
        // "sometime between the 1880s and 1899"
        let ud = UncertainDate::range(
            PreciseDate::new(midnight(1880, 1, 1)?, DatePrecision::Decade)?,
            PreciseDate::new(midnight(1899, 1, 1)?, DatePrecision::Year)?,
        )?;
        assert_eq!(ud.earliest(), midnight(1880, 1, 1)?);
        assert_eq!(ud.latest(), end_of_day(1899, 12, 31)?);
        Ok(())
    }

    // --- Overlap tests ---

    #[test]
    fn overlaps_year_contains_day() -> TestResult {
        let day = UncertainDate::with_precision(midnight(2020, 6, 15)?, DatePrecision::Day)?;
        let year = UncertainDate::with_precision(midnight(2020, 1, 1)?, DatePrecision::Year)?;
        assert!(day.overlaps(&year));
        assert!(year.overlaps(&day));
        Ok(())
    }

    #[test]
    fn adjacent_years_no_overlap() -> TestResult {
        let y2019 = UncertainDate::with_precision(midnight(2019, 6, 15)?, DatePrecision::Year)?;
        let y2020 = UncertainDate::with_precision(midnight(2020, 6, 15)?, DatePrecision::Year)?;
        assert!(!y2019.overlaps(&y2020));
        Ok(())
    }

    // --- Validation tests ---

    #[test]
    fn range_rejects_inverted() -> TestResult {
        assert_eq!(
            UncertainDate::range(
                PreciseDate::new(midnight(2000, 1, 1)?, DatePrecision::Year)?,
                PreciseDate::new(midnight(1990, 1, 1)?, DatePrecision::Year)?,
            ),
            Err(DateError::InvertedRange),
        );
        Ok(())
    }

    #[test]
    fn exact_rejects_year_0() -> TestResult {
        assert_eq!(
            UncertainDate::exact(midnight(0, 1, 1)?),
            Err(DateError::Year0),
        );
        Ok(())
    }

    #[test]
    fn deserialize_rejects_year_0_precise() {
        let json = r#"{"type":"precise","datetime":"0000-01-01T00:00:00","precision":"year"}"#;
        assert!(serde_json::from_str::<UncertainDate>(json).is_err());
    }

    #[test]
    fn deserialize_rejects_inverted_range() {
        let json = r#"{"type":"range","earliest":{"datetime":"1990-01-01T00:00:00","precision":"year"},"latest":{"datetime":"1980-01-01T00:00:00","precision":"year"}}"#;
        assert!(serde_json::from_str::<UncertainDate>(json).is_err());
    }

    // --- Serde round-trip ---

    #[test]
    fn serde_roundtrip_precise() -> TestResult {
        let ud = UncertainDate::with_precision(dt(2020, 6, 15, 14, 30, 0)?, DatePrecision::Day)?;
        let json = serde_json::to_string(&ud)?;
        let deserialized: UncertainDate = serde_json::from_str(&json)?;
        assert_eq!(ud, deserialized);
        Ok(())
    }

    #[test]
    fn serde_roundtrip_range() -> TestResult {
        let ud = UncertainDate::range(
            PreciseDate::new(midnight(1920, 1, 1)?, DatePrecision::Year)?,
            PreciseDate::new(midnight(1925, 1, 1)?, DatePrecision::Year)?,
        )?;
        let json = serde_json::to_string(&ud)?;
        let deserialized: UncertainDate = serde_json::from_str(&json)?;
        assert_eq!(ud, deserialized);
        Ok(())
    }

    #[test]
    fn deserialize_snaps_precise_to_precision_start() -> TestResult {
        let json = r#"{"type":"precise","datetime":"2020-06-15T14:30:00","precision":"year"}"#;
        let ud: UncertainDate = serde_json::from_str(json)?;
        assert_eq!(ud.earliest(), midnight(2020, 1, 1)?);
        assert_eq!(ud.latest(), end_of_day(2020, 12, 31)?);
        Ok(())
    }

    // --- Property-based tests ---
    //
    // The datetime generator biases toward edge cases: period boundaries,
    // BCE/CE transition, and year 0 (which should always be rejected).

    fn arb_naive_datetime() -> impl Strategy<Value = NaiveDateTime> {
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

        (years, 1u32..=12, 1u32..=28, 0u32..=23, 0u32..=59, 0u32..=59)
            .prop_filter_map("valid datetime", |(y, m, d, h, min, s)| {
                NaiveDate::from_ymd_opt(y, m, d)?.and_hms_opt(h, min, s)
            })
    }

    fn arb_precision() -> impl Strategy<Value = DatePrecision> {
        prop_oneof![
            Just(DatePrecision::Second),
            Just(DatePrecision::Minute),
            Just(DatePrecision::Hour),
            Just(DatePrecision::Day),
            Just(DatePrecision::Month),
            Just(DatePrecision::Year),
            Just(DatePrecision::Decade),
            Just(DatePrecision::Century),
            Just(DatePrecision::Millennium),
        ]
    }

    proptest! {
        #[test]
        fn prop_earliest_lte_latest(datetime in arb_naive_datetime(), precision in arb_precision()) {
            if datetime.year() == 0 {
                prop_assert_eq!(
                    UncertainDate::with_precision(datetime, precision),
                    Err(DateError::Year0),
                );
            } else {
                let ud = UncertainDate::with_precision(datetime, precision)
                    .map_err(|e| TestCaseError::fail(e.to_string()))?;
                prop_assert!(ud.earliest() <= ud.latest(),
                    "earliest ({}) should be <= latest ({}) for {:?}",
                    ud.earliest(), ud.latest(), precision);
            }
        }

        #[test]
        fn prop_exact_date_earliest_equals_latest(datetime in arb_naive_datetime()) {
            if datetime.year() == 0 {
                prop_assert_eq!(
                    UncertainDate::exact(datetime),
                    Err(DateError::Year0),
                );
            } else {
                let ud = UncertainDate::exact(datetime)
                    .map_err(|e| TestCaseError::fail(e.to_string()))?;
                prop_assert_eq!(ud.earliest(), ud.latest());
            }
        }

        #[test]
        fn prop_date_within_precision_bounds(datetime in arb_naive_datetime(), precision in arb_precision()) {
            if datetime.year() == 0 {
                prop_assert_eq!(
                    UncertainDate::with_precision(datetime, precision),
                    Err(DateError::Year0),
                );
            } else {
                let ud = UncertainDate::with_precision(datetime, precision)
                    .map_err(|e| TestCaseError::fail(e.to_string()))?;
                prop_assert!(ud.earliest() <= datetime);
                prop_assert!(datetime <= ud.latest());
            }
        }

        #[test]
        fn prop_with_precision_is_idempotent(datetime in arb_naive_datetime(), precision in arb_precision()) {
            if datetime.year() == 0 {
                prop_assert_eq!(
                    UncertainDate::with_precision(datetime, precision),
                    Err(DateError::Year0),
                );
            } else {
                let ud1 = UncertainDate::with_precision(datetime, precision)
                    .map_err(|e| TestCaseError::fail(e.to_string()))?;
                let ud2 = UncertainDate::with_precision(ud1.earliest(), precision)
                    .map_err(|e| TestCaseError::fail(e.to_string()))?;
                prop_assert_eq!(ud1.earliest(), ud2.earliest());
                prop_assert_eq!(ud1.latest(), ud2.latest());
            }
        }
    }
}
