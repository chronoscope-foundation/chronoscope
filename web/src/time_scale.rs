//! The time slider's piecewise-linear year axis.
//!
//! Three bands share the track, one per evidence regime, each at its own
//! resolution: 20 years to the unit back to 2000 BCE, 2 years from 1500 where
//! depiction becomes reliably perspectival, and one year from 1850 where
//! photography starts. Resolution therefore tracks how much evidence exists.
//!
//! The grid is pinned to those years, so a unit keeps its year for good and the
//! axis grows at its right edge as the years pass. Every band's span divides its
//! step exactly, which makes the two closed bands equal thirds of the track and
//! makes `position(year(u)) == u` hold exactly, the law that stops the thumb
//! jumping out from under a drag.
//!
//! Years are historical, matching every stored date: 1 CE follows 1 BCE with no
//! year 0 between them, so no unit names one.

/// A position on the slider's track: the range input's own value space.
///
/// Years and units are both integers over the same axis, and mistaking one for
/// the other is the bug this newtype exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TrackUnit(pub u32);

/// The earliest year the slider reaches, chosen for where scrubbing stops being
/// a good way to find things: further back, dating is coarse enough that
/// scrubbing to a year means nothing, and search is the affordance that reaches
/// it.
const FLOOR_YEAR: i32 = -2000;

/// Where depiction becomes reliably perspectival.
const EARLY_MODERN_YEAR: i32 = 1500;

/// Roughly when photography starts.
const PHOTOGRAPHIC_YEAR: i32 = 1850;

/// A round year deep in the photographic band, so the third of the track that
/// band takes carries a scale of its own.
const RECENT_YEAR: i32 = 2000;

const ANCIENT_YEARS_PER_UNIT: u32 = 20;
const EARLY_MODERN_YEARS_PER_UNIT: u32 = 2;

/// Units in the ancient band, and so the unit [`EARLY_MODERN_YEAR`] sits on.
/// Both bands' formulas agree there, which is what makes the seam invisible.
const ANCIENT_UNITS: u32 = (EARLY_MODERN_YEAR - FLOOR_YEAR).unsigned_abs() / ANCIENT_YEARS_PER_UNIT;

/// The unit [`PHOTOGRAPHIC_YEAR`] sits on. 3500/20 and 350/2 both land on 175,
/// so the two closed bands take equal thirds without a fudge.
const PHOTOGRAPHIC_START: u32 = ANCIENT_UNITS
    + (PHOTOGRAPHIC_YEAR - EARLY_MODERN_YEAR).unsigned_abs() / EARLY_MODERN_YEARS_PER_UNIT;

/// The seam between the eras. Historical numbering runs 1 BCE to 1 CE, so this
/// is the first year on the far side of it.
const ERA_SEAM_YEAR: i32 = 1;

/// How much track a named year's label needs.
///
/// The labels share one line, so two whose marks stand close together have only
/// the track between them to divide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelRoom {
    /// Fits at every width the card takes.
    Any,
    /// Fits once the track is wide. 1 CE stands a fifth along, within reach of
    /// the floor's own "2000 BCE" on a small phone, so its year waits for the
    /// width while its mark stays.
    WideTrack,
}

/// The years the axis names outright, left to right: the floor, the era seam,
/// the two band seams, and a round recent year. Together they are what makes
/// the compression legible. 1 CE sits about a fifth along and 1850 at two
/// thirds, so a reader can see that the 1499 years up to 1500 take a seventh of
/// the track while the years since 1850 take a third of it.
const NAMED_YEARS: [(i32, LabelRoom); 5] = [
    (FLOOR_YEAR, LabelRoom::Any),
    (ERA_SEAM_YEAR, LabelRoom::WideTrack),
    (EARLY_MODERN_YEAR, LabelRoom::Any),
    (PHOTOGRAPHIC_YEAR, LabelRoom::Any),
    (RECENT_YEAR, LabelRoom::Any),
];

/// Each band's own granularity: the year it starts at, and the years between its
/// unnamed marks. Every step divides its band's years-per-unit, so each mark
/// lands on an exact unit rather than between two.
///
/// Each interval is the finest round one its band's evidence resolves:
/// millennia where dating is millennial, decades where photography dates a
/// building to the year. The marks close up toward the present as the years
/// themselves do, which is the compression made visible.
const MINOR_STEPS: [(i32, u32); 3] = [
    (FLOOR_YEAR, 1000),
    (EARLY_MODERN_YEAR, 50),
    (PHOTOGRAPHIC_YEAR, 10),
];

/// A year as the slider says it: `"2000 BCE"`, `"1 CE"`, `"1500"`, `"2026"`.
///
/// The ancient band holds both eras, so its years name theirs. Past it the axis
/// is CE the whole way, and the bare number takes a third less width, which is
/// the room the labels need to share one line.
pub fn era_label(year: i32) -> String {
    if year < 0 {
        format!("{} BCE", year.unsigned_abs())
    } else if year < EARLY_MODERN_YEAR {
        format!("{year} CE")
    } else {
        year.to_string()
    }
}

/// How prominently a mark is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickKind {
    /// Carries its year, where the track has room for it.
    Named(LabelRoom),
    /// A band's own subdivision, drawn without one. Legible because the named
    /// marks beside it say what a step is worth.
    Minor,
}

/// A mark on the axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AxisTick {
    pub year: i32,
    /// Where it sits. Exact, so the mark stands where the thumb reads its year.
    pub unit: TrackUnit,
    pub kind: TickKind,
}

/// The mapping between track positions and years.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeScale {
    /// The year the right edge names.
    end_year: i32,
}

impl TimeScale {
    /// The axis running to `max_year`, the present year.
    ///
    /// A clock set before 1850 leaves the photographic band with no units and
    /// the axis ending at 1850.
    pub fn new(max_year: i32) -> Self {
        Self {
            end_year: max_year.max(PHOTOGRAPHIC_YEAR),
        }
    }

    /// The unit at the right edge, and so the input's `max`.
    pub fn total_units(&self) -> TrackUnit {
        TrackUnit(PHOTOGRAPHIC_START + (self.end_year - PHOTOGRAPHIC_YEAR).unsigned_abs())
    }

    /// The track position a year sits at. Years off either end of the axis land
    /// on that end.
    pub fn position(&self, year: i32) -> TrackUnit {
        if year <= FLOOR_YEAR {
            return TrackUnit(0);
        }
        if year >= self.end_year {
            return self.total_units();
        }
        // Half a step added before each truncating divide, so a year between two
        // units takes the nearer one. Every numerator here is positive, which is
        // what makes that truncation round half up.
        let unit = if year <= EARLY_MODERN_YEAR {
            let into_band = (year - FLOOR_YEAR).unsigned_abs();
            (into_band + ANCIENT_YEARS_PER_UNIT / 2) / ANCIENT_YEARS_PER_UNIT
        } else if year <= PHOTOGRAPHIC_YEAR {
            let into_band = (year - EARLY_MODERN_YEAR).unsigned_abs();
            ANCIENT_UNITS
                + (into_band + EARLY_MODERN_YEARS_PER_UNIT / 2) / EARLY_MODERN_YEARS_PER_UNIT
        } else {
            PHOTOGRAPHIC_START + (year - PHOTOGRAPHIC_YEAR).unsigned_abs()
        };
        TrackUnit(unit)
    }

    /// The year a track position names. Positions past the right edge name the
    /// year at it.
    pub fn year(&self, unit: TrackUnit) -> i32 {
        let unit = unit.0.min(self.total_units().0);
        let year = if unit <= ANCIENT_UNITS {
            FLOOR_YEAR.saturating_add_unsigned(unit * ANCIENT_YEARS_PER_UNIT)
        } else if unit <= PHOTOGRAPHIC_START {
            EARLY_MODERN_YEAR
                .saturating_add_unsigned((unit - ANCIENT_UNITS) * EARLY_MODERN_YEARS_PER_UNIT)
        } else {
            PHOTOGRAPHIC_YEAR.saturating_add_unsigned(unit - PHOTOGRAPHIC_START)
        };
        // The era seam has no historical year name, so the readout would have to
        // invent one. Naming 1 CE costs nothing: whatever is supported at the
        // seam is supported there too.
        if year == 0 { 1 } else { year }
    }

    /// Every mark the axis draws, left to right.
    ///
    /// The floor, the era seam and both band seams are on the axis whatever the
    /// present is, since it runs to 1850 even for a clock set earlier. The axis
    /// names a year once it reaches it, so 2000 joins them for a clock past it.
    pub fn ticks(&self) -> Vec<AxisTick> {
        let mut ticks: Vec<AxisTick> = NAMED_YEARS
            .into_iter()
            .filter(|(year, _)| *year <= self.end_year)
            .map(|(year, room)| AxisTick {
                year,
                unit: self.position(year),
                kind: TickKind::Named(room),
            })
            .collect();

        let band_ends = [EARLY_MODERN_YEAR, PHOTOGRAPHIC_YEAR, self.end_year];
        for ((start, step), end) in MINOR_STEPS.into_iter().zip(band_ends) {
            let mut year = start;
            while year < end {
                let unit = self.position(year);
                // Subdivisions fill the space between the named marks: the
                // track's own ends, and any unit a named mark stands on, are
                // spoken for already.
                let free = unit > TrackUnit(0)
                    && unit < self.total_units()
                    && !ticks.iter().any(|tick| tick.unit == unit);
                if free {
                    ticks.push(AxisTick {
                        // From the unit rather than the walk, so a step landing
                        // on the era seam names 1 CE instead of a year 0 that
                        // historical numbering has no name for.
                        year: self.year(unit),
                        unit,
                        kind: TickKind::Minor,
                    });
                }
                year = year.saturating_add_unsigned(step);
            }
        }

        ticks.sort_by_key(|tick| tick.unit);
        ticks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every case runs over pinned present years rather than the clock: a swept
    /// `max_year` covers the empty and one-unit photographic bands that no
    /// single year exercises, and a test that reads the clock changes its own
    /// meaning over New Year.
    const MAX_YEARS: [i32; 7] = [1849, 1850, 1851, 1900, 2026, 2100, 3000];

    /// The year at the right edge, which trails `max_year` only for a clock set
    /// before 1850.
    fn end_year(scale: &TimeScale) -> i32 {
        scale.year(scale.total_units())
    }

    /// The years the axis writes out, left to right.
    fn named_years(scale: &TimeScale) -> Vec<i32> {
        scale
            .ticks()
            .into_iter()
            .filter(|tick| matches!(tick.kind, TickKind::Named(_)))
            .map(|tick| tick.year)
            .collect()
    }

    /// Years between two historical years. One less than the arithmetic
    /// difference across the era seam, since there is no year 0 to count: 10 BCE
    /// is ten years before 1 CE. The bands' quantization is a claim about years,
    /// so it is counted in years.
    fn years_apart(a: i32, b: i32) -> i32 {
        let gap = (a - b).abs();
        if (a < 0) == (b < 0) { gap } else { gap - 1 }
    }

    #[test]
    fn every_unit_survives_the_round_trip_through_its_year() {
        for max_year in MAX_YEARS {
            let scale = TimeScale::new(max_year);
            for unit in 0..=scale.total_units().0 {
                let unit = TrackUnit(unit);
                assert_eq!(
                    scale.position(scale.year(unit)),
                    unit,
                    "unit {unit:?} at max_year {max_year} must survive year and back, \
                     or the thumb jumps mid-drag"
                );
            }
        }
    }

    #[test]
    fn photographic_years_map_to_themselves() {
        for max_year in MAX_YEARS {
            let scale = TimeScale::new(max_year);
            for year in PHOTOGRAPHIC_YEAR..=end_year(&scale) {
                assert_eq!(
                    scale.year(scale.position(year)),
                    year,
                    "the photographic band is one unit per year, at max_year {max_year}"
                );
            }
        }
    }

    #[test]
    fn quantization_stays_within_each_bands_step() {
        for max_year in MAX_YEARS {
            let scale = TimeScale::new(max_year);
            for year in FLOOR_YEAR..EARLY_MODERN_YEAR {
                let landed = scale.year(scale.position(year));
                assert!(
                    years_apart(landed, year) <= 10,
                    "ancient year {year} landed on {landed}, further than half a 20-year step"
                );
            }
            for year in EARLY_MODERN_YEAR..=PHOTOGRAPHIC_YEAR {
                let landed = scale.year(scale.position(year));
                assert!(
                    years_apart(landed, year) <= 1,
                    "early modern year {year} landed on {landed}, further than half a 2-year step"
                );
            }
        }
    }

    #[test]
    fn position_never_goes_backwards_and_year_always_forwards() {
        for max_year in MAX_YEARS {
            let scale = TimeScale::new(max_year);
            for year in (FLOOR_YEAR - 10)..=(end_year(&scale) + 10) {
                assert!(
                    scale.position(year) <= scale.position(year + 1),
                    "position fell back between {year} and {} at max_year {max_year}",
                    year + 1
                );
            }
            for unit in 0..scale.total_units().0 {
                assert!(
                    scale.year(TrackUnit(unit)) < scale.year(TrackUnit(unit + 1)),
                    "unit {unit} and its successor name the same year at max_year {max_year}"
                );
            }
        }
    }

    #[test]
    fn the_anchors_land_on_their_pinned_units() {
        for max_year in MAX_YEARS {
            let scale = TimeScale::new(max_year);
            assert_eq!(scale.position(FLOOR_YEAR), TrackUnit(0));
            assert_eq!(scale.position(EARLY_MODERN_YEAR), TrackUnit(175));
            assert_eq!(scale.position(PHOTOGRAPHIC_YEAR), TrackUnit(350));
            assert_eq!(
                scale.position(max_year),
                scale.total_units(),
                "the present year is the right edge"
            );
        }
    }

    #[test]
    fn the_era_seam_is_named_1_ce_and_no_unit_names_year_0() {
        for max_year in MAX_YEARS {
            let scale = TimeScale::new(max_year);
            for unit in 0..=scale.total_units().0 {
                assert_ne!(
                    scale.year(TrackUnit(unit)),
                    0,
                    "unit {unit} names year 0, which historical numbering has no name for"
                );
            }
            assert_eq!(scale.year(TrackUnit(100)), 1, "the seam's unit names 1 CE");
        }
    }

    /// The era rides along exactly where the axis holds both of them, which is
    /// the ancient band. Everything past it is CE, so the number stands alone.
    #[test]
    fn the_era_rides_along_only_through_the_ancient_band() {
        assert_eq!(era_label(FLOOR_YEAR), "2000 BCE");
        assert_eq!(era_label(-1), "1 BCE");
        assert_eq!(era_label(1), "1 CE");
        assert_eq!(era_label(EARLY_MODERN_YEAR - 1), "1499 CE");
        assert_eq!(era_label(EARLY_MODERN_YEAR), "1500");
        assert_eq!(era_label(PHOTOGRAPHIC_YEAR), "1850");
        assert_eq!(era_label(2026), "2026");
    }

    #[test]
    fn a_unit_names_the_same_year_whatever_the_present_is() {
        for earlier in MAX_YEARS {
            for later in MAX_YEARS {
                let (a, b) = (TimeScale::new(earlier), TimeScale::new(later));
                let shared = a.total_units().0.min(b.total_units().0);
                for unit in 0..=shared {
                    assert_eq!(
                        a.year(TrackUnit(unit)),
                        b.year(TrackUnit(unit)),
                        "unit {unit} names different years at max_year {earlier} and {later}, \
                         so the axis is re-derived rather than pinned"
                    );
                }
            }
        }
    }

    /// A mark drawn off the year it stands on would misreport the axis it
    /// explains, which is the one thing a mark buys.
    #[test]
    fn each_tick_sits_where_the_thumb_reads_its_year() {
        for max_year in MAX_YEARS {
            let scale = TimeScale::new(max_year);
            for tick in scale.ticks() {
                assert_eq!(
                    scale.year(tick.unit),
                    tick.year,
                    "the {} mark sits on unit {:?}, which the thumb reads as {}",
                    tick.year,
                    tick.unit,
                    scale.year(tick.unit)
                );
            }
        }
    }

    /// The years the axis names are the ones that explain its shape: where it
    /// stops, the era seam, the two evidence regimes the bands divide at, and a
    /// recent year that says what the photographic third is worth.
    ///
    /// 2000 is the one that has to wait for the clock to reach it. Placed on an
    /// axis that ends earlier it would pile onto the right edge and name a year
    /// the thumb reads as something else.
    #[test]
    fn the_axis_names_the_floor_the_seams_and_a_recent_year() {
        assert_eq!(
            named_years(&TimeScale::new(1900)),
            [FLOOR_YEAR, 1, 1500, 1850]
        );
        assert_eq!(
            named_years(&TimeScale::new(2026)),
            [FLOOR_YEAR, 1, 1500, 1850, 2000]
        );
    }

    /// Every named year but one has room at any width. The era seam's stands a
    /// fifth along, close enough to the floor's own label that a narrow track
    /// drops its year and keeps its mark.
    #[test]
    fn only_the_era_seams_label_waits_for_a_wide_track() {
        let scale = TimeScale::new(2026);
        let waiting: Vec<i32> = scale
            .ticks()
            .into_iter()
            .filter(|tick| tick.kind == TickKind::Named(LabelRoom::WideTrack))
            .map(|tick| tick.year)
            .collect();
        assert_eq!(waiting, vec![ERA_SEAM_YEAR]);
    }

    /// Each band subdivides at its own granularity, so a step's width says how
    /// much of the axis that band's evidence regime is worth.
    #[test]
    fn minor_marks_step_at_each_bands_granularity() {
        let scale = TimeScale::new(2026);
        let minor: Vec<i32> = scale
            .ticks()
            .into_iter()
            .filter(|tick| tick.kind == TickKind::Minor)
            .map(|tick| tick.year)
            .collect();
        assert_eq!(
            minor,
            vec![
                -1000, 1000, 1550, 1600, 1650, 1700, 1750, 1800, 1860, 1870, 1880, 1890, 1900,
                1910, 1920, 1930, 1940, 1950, 1960, 1970, 1980, 1990, 2010, 2020,
            ],
            "millennia to 1500, half-centuries to 1850, decades since, with 2000 named outright"
        );
    }

    /// Two marks on one unit would draw over each other. Subdivisions keep off
    /// the track's ends too, where the years the thumb parks on are named
    /// already.
    #[test]
    fn no_two_marks_share_a_unit_and_no_subdivision_sits_on_a_track_end() {
        for max_year in MAX_YEARS {
            let scale = TimeScale::new(max_year);
            let ticks = scale.ticks();
            for pair in ticks.windows(2) {
                assert!(
                    pair[0].unit < pair[1].unit,
                    "marks {:?} and {:?} are out of order or share a unit at max_year {max_year}",
                    pair[0],
                    pair[1]
                );
            }
            for tick in ticks.iter().filter(|tick| tick.kind == TickKind::Minor) {
                assert!(
                    tick.unit > TrackUnit(0) && tick.unit < scale.total_units(),
                    "the {} mark sits on a track end at max_year {max_year}",
                    tick.year
                );
            }
        }
    }
}
