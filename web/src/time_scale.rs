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

const ANCIENT_YEARS_PER_UNIT: u32 = 20;
const EARLY_MODERN_YEARS_PER_UNIT: u32 = 2;

/// Units in the ancient band, and so the unit [`EARLY_MODERN_YEAR`] sits on.
/// Both bands' formulas agree there, which is what makes the seam invisible.
const ANCIENT_UNITS: u32 = (EARLY_MODERN_YEAR - FLOOR_YEAR).unsigned_abs() / ANCIENT_YEARS_PER_UNIT;

/// The unit [`PHOTOGRAPHIC_YEAR`] sits on. 3500/20 and 350/2 both land on 175,
/// so the two closed bands take equal thirds without a fudge.
const PHOTOGRAPHIC_START: u32 = ANCIENT_UNITS
    + (PHOTOGRAPHIC_YEAR - EARLY_MODERN_YEAR).unsigned_abs() / EARLY_MODERN_YEARS_PER_UNIT;

/// The years the bands meet at, left to right. Each lands on an exact unit, so a
/// divider sits where the thumb reads that year, and each carries that year as a
/// label, which is what makes the compression legible: three named regions of
/// equal width holding 3500, 350, and 176 years.
const BOUNDARY_YEARS: [i32; 2] = [EARLY_MODERN_YEAR, PHOTOGRAPHIC_YEAR];

/// A year as the slider says it: `"1750"`, `"500 CE"`, `"2000 BCE"`.
///
/// Four digits read as a year unaided; fewer need the era spelled out, and
/// every BCE year does.
pub fn era_label(year: i32) -> String {
    if year < 0 {
        format!("{} BCE", year.unsigned_abs())
    } else if year < 1000 {
        format!("{year} CE")
    } else {
        year.to_string()
    }
}

/// Where two bands meet, drawn as a labelled divider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandBoundary {
    pub year: i32,
    pub unit: TrackUnit,
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

    /// The band boundaries, left to right. Both always fall inside the axis,
    /// since it runs to 1850 even for a clock set earlier.
    pub fn boundaries(&self) -> [BandBoundary; 2] {
        BOUNDARY_YEARS.map(|year| BandBoundary {
            year,
            unit: self.position(year),
        })
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

    #[test]
    fn era_labels_name_the_era_where_the_digits_cannot() {
        assert_eq!(era_label(1), "1 CE");
        assert_eq!(era_label(-1), "1 BCE");
        assert_eq!(era_label(FLOOR_YEAR), "2000 BCE");
        assert_eq!(era_label(1750), "1750");
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

    /// A divider drawn off the year it names would misreport the axis it
    /// explains, which is the one thing the label buys.
    #[test]
    fn each_band_boundary_sits_where_the_thumb_reads_its_year() {
        for max_year in MAX_YEARS {
            let scale = TimeScale::new(max_year);
            let [early_modern, photographic] = scale.boundaries();
            assert_eq!(
                (early_modern.year, photographic.year),
                (EARLY_MODERN_YEAR, PHOTOGRAPHIC_YEAR),
                "the axis divides where the evidence regimes change"
            );
            for boundary in scale.boundaries() {
                assert_eq!(
                    scale.year(boundary.unit),
                    boundary.year,
                    "the {} divider sits on unit {:?}, which the thumb reads as {}",
                    boundary.year,
                    boundary.unit,
                    scale.year(boundary.unit)
                );
            }
        }
    }
}
