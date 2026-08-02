//! OpenHistoricalMap style rewrites.
//!
//! OHM publishes one time-agnostic vector tileset whose features carry
//! `start_decdate` / `end_decdate` decimal years, and the instant is applied
//! client-side by rewriting each style layer's filter. Two pieces make that up:
//! the decimal-year encoding the tiles are written in, and the filter built
//! from it.
//!
//! Its labels are rewritten once at style load in the same spirit: the tiles
//! carry a name per language beside the local one, and the style draws the local
//! one.

use chrono::{Datelike, NaiveDate};
use serde_json::{Value, json};

/// The astronomical year a historical one names. 1 BCE is astronomical 0, so
/// every BCE year shifts by one.
///
/// Year 0 has no historical name and `NaiveDate` accepts it anyway, so it reads
/// as 1 CE here, matching what the slider's own axis names it
/// (`TimeScale::year`).
fn astronomical_year(historical: i32) -> i32 {
    match historical {
        0 => 1,
        year if year < 0 => year + 1,
        year => year,
    }
}

/// The proleptic Gregorian leap rule, which is what OHM's decimal dates count
/// against.
fn is_leap(astronomical: i32) -> bool {
    astronomical % 4 == 0 && (astronomical % 100 != 0 || astronomical % 400 == 0)
}

fn month_lengths(leap: bool) -> [u32; 12] {
    let february = if leap { 29 } else { 28 };
    [31, february, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
}

/// A date in the terms the encoding is built from: the astronomical year, the
/// day's zero-based ordinal *in that year's* calendar, and the year's length.
///
/// The ordinal is counted from the month and day rather than read off
/// `NaiveDate::ordinal`, because chrono numbers the ordinal against the stored
/// year while the encoding counts against the astronomical one, and the two
/// disagree about leap years across most of the BCE range. July 1 of historical
/// -753 is day 182 of 366, where chrono reads it as day 181 of 365.
fn decompose(date: NaiveDate) -> (i32, u32, u32) {
    let year = astronomical_year(date.year());
    let leap = is_leap(year);
    let days_before_month: u32 = month_lengths(leap)
        .into_iter()
        .take(date.month0() as usize)
        .sum();
    // A February 29 chrono accepted can land in an astronomical year that has no
    // such day: chrono year -4 is a leap year, its astronomical -3 is not.
    let day = if leap || date.month() != 2 {
        date.day()
    } else {
        date.day().min(28)
    };
    let days_in_year = if leap { 366 } else { 365 };
    (year, days_before_month + day - 1, days_in_year)
}

/// Where OHM stores a date on its decimal-year axis: the astronomical year plus
/// the fraction of it elapsed, with the day placed at its midpoint.
///
/// Says the encoding once, in the form the served tiles were measured in, which
/// is what the day bounds below are derived from. The map queries by the day
/// (see [`ohm_day_bounds`]), so this is where the tile values are pinned.
#[cfg(test)]
pub fn ohm_midpoint(date: NaiveDate) -> f64 {
    let (year, ordinal0, days_in_year) = decompose(date);
    f64::from(year) + (f64::from(ordinal0) + 0.5) / f64::from(days_in_year)
}

/// The half-open decimal-year interval a day occupies.
///
/// A day is the finest thing the slider emits, and a feature tagged with that
/// very day sits at its midpoint, exactly where a single-point comparison would
/// hide it on its own start day. The interval puts the boundary at the day's
/// edges instead.
pub fn ohm_day_bounds(date: NaiveDate) -> (f64, f64) {
    let (year, ordinal0, days_in_year) = decompose(date);
    let year = f64::from(year);
    let days_in_year = f64::from(days_in_year);
    (
        year + f64::from(ordinal0) / days_in_year,
        year + f64::from(ordinal0 + 1) / days_in_year,
    )
}

/// A style layer's filter as the day `bounds` name it: began before this day
/// ended, had not ended before this day began, and still satisfies whatever the
/// style already asked for.
///
/// A feature carrying neither date draws at every instant. 17% of OHM's features
/// record no start and 29% no end, and the `osm_land` and `ne` source-layers
/// carry no date fields at all, so that branch is what keeps the continents and
/// coastlines on the map.
pub fn layer_filter(bounds: (f64, f64), original: Option<&Value>) -> Value {
    let (lo, hi) = bounds;
    // Filter shape and comparisons follow @openhistoricalmap/maplibre-gl-dates
    // 1.3.0. The decimal year is OHM's own tile encoding: astronomical year plus
    // the fraction elapsed, with a day placed at its midpoint. Measured against
    // start_date/start_decdate pairs in the served tiles.
    let mut clauses = vec![
        json!("all"),
        json!([
            "any",
            ["!", ["has", "start_decdate"]],
            ["<", ["get", "start_decdate"], hi]
        ]),
        json!([
            "any",
            ["!", ["has", "end_decdate"]],
            [">=", ["get", "end_decdate"], lo]
        ]),
    ];
    if let Some(original) = original {
        clauses.push(original.clone());
    }
    Value::Array(clauses)
}

/// Whether a style's own filter can ride along as [`layer_filter`]'s third
/// clause.
///
/// `MapLibre` reads a filter in one of two syntaxes, and a legacy one spliced
/// beside the decimal-date clauses evaluates as neither. This mirrors the
/// classifier `MapLibre` sorts them with, `isExpressionFilter` in
/// `src/style-spec/feature_filter/index.ts`, read out of the 5.1.0 bundle we
/// ship. That function's answer is the one the renderer acts on, so agreeing
/// with it is the whole job.
///
/// The map assigns filters with `{validate: false}`, which takes a composed
/// legacy filter as given, and the style is third-party, so the syntax it is
/// written in is its own to change. Where the two answers can only be guessed
/// apart, reading a filter as legacy is the conservative direction: it costs
/// that layer the style's own predicate, and the layer stays on the map,
/// filtered by time.
pub fn splices_as_expression(filter: &Value) -> bool {
    if filter.is_boolean() {
        return true;
    }
    let Some(parts) = filter.as_array() else {
        return false;
    };
    let Some((head, operands)) = parts.split_first() else {
        return false;
    };
    // A head that is not one of legacy's operator names, its own array included,
    // belongs to an expression.
    match head.as_str() {
        // Legacy reserves two keys for a feature's own identity and geometry;
        // `["has", name]` otherwise reads the same in both.
        Some("has") => operands
            .first()
            .is_some_and(|name| !matches!(name.as_str(), Some("$id" | "$type"))),
        // Legacy's `in` names its property first and lists the values it admits
        // after, so a non-string there, or an array where the first value goes,
        // is an expression addressing a set.
        Some("in") => {
            parts.len() >= 3
                && (operands.first().is_some_and(|v| !v.is_string())
                    || operands.get(1).is_some_and(Value::is_array))
        }
        Some("!in" | "!has" | "none") => false,
        // Legacy's comparisons are exactly three long with the property named
        // directly, so either operand being an array, or any other arity, marks
        // an expression.
        Some("==" | "!=" | ">" | ">=" | "<" | "<=") => {
            parts.len() != 3
                || operands.first().is_some_and(Value::is_array)
                || operands.get(1).is_some_and(Value::is_array)
        }
        // Where one filter nests another whole one, and the one place a bare
        // boolean stands as an operand.
        Some("any" | "all") => operands
            .iter()
            .all(|operand| operand.is_boolean() || splices_as_expression(operand)),
        _ => true,
    }
}

/// A label expression naming the tile's own `language` name, falling back to the
/// local one the style ships with.
///
/// OHM keys its localized names with an underscore (`name_de`) rather than
/// OSM's colon, and carries 515 of them. `coalesce` takes the second branch
/// wherever a tile has no name in the reader's language, which is what the map
/// reads as today.
pub fn localized_text_field(language: &str) -> Value {
    json!([
        "coalesce",
        ["get", format!("name_{language}")],
        ["get", "name"]
    ])
}

/// The primary subtag a locale names, lowercased: `en-US` reads as `en`.
///
/// Both ends of the map agree at this granularity. The tiles key their names by
/// primary subtag, and the API server reduces the request's `Accept-Language` to
/// one before negotiating an entity's display name.
fn primary_subtag(locale: &str) -> Option<String> {
    let primary = locale.split('-').next()?.trim();
    (!primary.is_empty()).then(|| primary.to_ascii_lowercase())
}

/// The label expression for the language the browser reads in, `None` where it
/// names none we can key on.
pub fn reader_text_field() -> Option<Value> {
    let locale = web_sys::window()?.navigator().language()?;
    Some(localized_text_field(&primary_subtag(&locale)?))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// The tile values carry five decimal digits, so agreement is asserted just
    /// inside the last of them. Every mistake these cases are for shows far
    /// wider: a missing half-day at 0.0014, a day of ordinal drift at 0.0027.
    const TOLERANCE: f64 = 1e-5;

    /// A date as our code holds one, in historical years.
    fn date(year: i32, month: u32, day: u32) -> Result<NaiveDate, String> {
        NaiveDate::from_ymd_opt(year, month, day)
            .ok_or_else(|| format!("{year}-{month}-{day} is not a date chrono accepts"))
    }

    #[test]
    fn the_encoding_reproduces_ohms_own_tile_values() -> Result<(), String> {
        // Read off `start_date`/`start_decdate` and `end_date`/`end_decdate`
        // pairs in the served tiles. The founding of Rome carries day precision;
        // the other two carry year precision, where a start resolves to January
        // 1 and an end to December 31.
        for (year, month, day, decdate) in [
            (-753, 4, 21, -751.69536),
            (-5001, 1, 1, -4999.99863),
            (-801, 12, 31, -799.00137),
        ] {
            let midpoint = ohm_midpoint(date(year, month, day)?);
            assert!(
                (midpoint - decdate).abs() < TOLERANCE,
                "{year}-{month}-{day} is {decdate} in OHM's tiles, encoded as {midpoint}"
            );
        }
        Ok(())
    }

    #[test]
    fn the_ordinal_counts_against_the_astronomical_year() -> Result<(), String> {
        // Historical -753 is astronomical -752, a leap year, so July 1 is day 182
        // of 366; historical -752 is astronomical -751, day 181 of 365. Counting
        // against the stored year swaps the two, which moves each value by about
        // 0.0027 rather than by a whole day.
        for (year, decdate) in [(-753, -751.50137), (-752, -750.50274)] {
            let midpoint = ohm_midpoint(date(year, 7, 1)?);
            assert!(
                (midpoint - decdate).abs() < TOLERANCE,
                "July 1 of {year} is {decdate} on OHM's scale, encoded as {midpoint}"
            );
        }
        Ok(())
    }

    #[test]
    fn february_29_falls_back_to_the_28th_where_the_astronomical_year_has_no_29th()
    -> Result<(), String> {
        // Chrono year -4 is a leap year, so it accepts the date; its astronomical
        // year -3 is not, so the day it names has to be one that exists.
        let leap_day = ohm_midpoint(date(-4, 2, 29)?);
        let twenty_eighth = ohm_midpoint(date(-4, 2, 28)?);
        assert!(
            (leap_day - twenty_eighth).abs() < f64::EPSILON,
            "February 29 of historical -4 has to encode as its 28th ({twenty_eighth}), got {leap_day}"
        );
        Ok(())
    }

    #[test]
    fn a_days_bounds_bracket_the_midpoint_ohm_stores() -> Result<(), String> {
        for (year, month, day) in [
            (-753, 4, 21),
            (-5001, 1, 1),
            (-801, 12, 31),
            (-753, 7, 1),
            (-752, 7, 1),
            (1850, 7, 1),
        ] {
            let date = date(year, month, day)?;
            let (lo, hi) = ohm_day_bounds(date);
            let midpoint = ohm_midpoint(date);
            assert!(
                lo < midpoint && midpoint < hi,
                "{year}-{month}-{day} stores at {midpoint}, outside the day it is queried by, \
                 [{lo}, {hi})"
            );
        }
        Ok(())
    }

    #[test]
    fn the_filter_pins_both_decdate_clauses() {
        assert_eq!(
            layer_filter((1850.25, 1850.75), None),
            json!([
                "all",
                [
                    "any",
                    ["!", ["has", "start_decdate"]],
                    ["<", ["get", "start_decdate"], 1850.75]
                ],
                [
                    "any",
                    ["!", ["has", "end_decdate"]],
                    [">=", ["get", "end_decdate"], 1850.25]
                ]
            ])
        );
    }

    #[test]
    fn the_filter_draws_the_undated_and_bounds_the_dated() -> Result<(), String> {
        let filter = layer_filter((1850.0, 1851.0), None);
        let cases = [
            (
                "undated",
                BTreeMap::new(),
                true,
                "an undated feature draws at every instant, which is what keeps \
                 the coastlines and land on the map",
            ),
            (
                "still standing",
                BTreeMap::from([("start_decdate", 1700.0)]),
                true,
                "a feature with no recorded end is still there",
            ),
            (
                "not yet built",
                BTreeMap::from([("start_decdate", 1900.0)]),
                false,
                "a feature that starts after the day ends is not there yet",
            ),
            (
                "already gone",
                BTreeMap::from([("end_decdate", 1800.0)]),
                false,
                "a feature that ended before the day began is gone",
            ),
            (
                "standing through",
                BTreeMap::from([("start_decdate", 1700.0), ("end_decdate", 1900.0)]),
                true,
                "a feature whose life spans the day is there",
            ),
        ];
        for (name, feature, expected, why) in cases {
            assert_eq!(evaluate(&filter, &feature)?, expected, "{name}: {why}");
        }
        Ok(())
    }

    #[test]
    fn a_layers_own_filter_rides_along_as_the_third_clause() {
        let original = json!(["==", ["get", "class"], "city"]);
        let with_original = layer_filter((1850.0, 1851.0), Some(&original));
        let without = layer_filter((1850.0, 1851.0), None);

        assert_eq!(
            with_original.as_array().map(Vec::len),
            Some(4),
            "the style's own filter joins the two date clauses"
        );
        assert_eq!(
            with_original.as_array().and_then(|clauses| clauses.get(3)),
            Some(&original),
            "and it arrives unchanged"
        );
        assert_eq!(
            without.as_array().map(Vec::len),
            Some(3),
            "a layer that had no filter gets the two date clauses alone; a third \
             null clause is rejected by MapLibre outright"
        );
    }

    #[test]
    fn an_expression_filter_splices_and_a_legacy_one_does_not() {
        for filter in [
            json!(["==", ["get", "type"], "city"]),
            json!(["!=", ["coalesce", ["get", "bridge"], 0], 1]),
            json!([">=", ["get", "area"], 100_000]),
            json!(["in", ["get", "type"], ["literal", ["cliff"]]]),
            json!(["has", "name"]),
            json!(["!", ["has", "name"]]),
            json!(["match", ["get", "type"], ["city"], true, false]),
            // The literal comes first and the expression second, which legacy
            // syntax has no form for.
            json!(["==", "Point", ["geometry-type"]]),
            // `in` reading a set out of the feature, where legacy would be
            // naming a property and listing values after it.
            json!(["in", "parking", ["get", "amenity"]]),
            // A bare boolean is a filter in its own right, and stands as an
            // operand of `all`/`any`.
            json!(true),
            json!(false),
            json!(["all", true, ["==", ["get", "type"], "city"]]),
        ] {
            assert!(
                splices_as_expression(&filter),
                "{filter} is an expression, so it rides along as the third clause"
            );
        }

        for filter in [
            json!(["==", "type", "city"]),
            json!(["!=", "type", "city"]),
            json!([">=", "area", 100_000]),
            json!(["in", "type", "city", "town"]),
            json!(["!in", "type", "city"]),
            json!(["!has", "name"]),
            json!(["none", ["==", "type", "city"]]),
            json!(["has", "$type"]),
        ] {
            assert!(
                !splices_as_expression(&filter),
                "{filter} is legacy syntax, and a layer carrying it is filtered by time alone"
            );
        }

        assert!(
            !splices_as_expression(&json!("name")),
            "a filter that is no operator over operands names nothing to splice"
        );
    }

    #[test]
    fn a_legacy_comparison_nested_in_an_all_carries_the_whole_filter_with_it() {
        assert!(
            splices_as_expression(&json!([
                "all",
                ["==", ["get", "type"], "city"],
                ["any", ["has", "name"], [">", ["get", "area"], 1]]
            ])),
            "an `all` over expressions is one, however deep"
        );
        assert!(
            !splices_as_expression(&json!([
                "all",
                ["==", ["get", "type"], "city"],
                ["any", ["has", "name"], [">", "area", 1]]
            ])),
            "one legacy comparison inside an `any` inside an `all` makes the filter legacy"
        );
    }

    #[test]
    fn a_label_reads_the_tiles_own_name_for_the_language() {
        assert_eq!(
            localized_text_field("de"),
            json!(["coalesce", ["get", "name_de"], ["get", "name"]])
        );
    }

    #[test]
    fn a_locale_reduces_to_the_subtag_the_tiles_key_on() {
        assert_eq!(primary_subtag("en-US").as_deref(), Some("en"));
        assert_eq!(primary_subtag("PT").as_deref(), Some("pt"));
        assert_eq!(primary_subtag(""), None);
    }

    /// Evaluate the expression subset the builder emits against a feature's
    /// decimal dates, so a case reads as "this feature draws at that instant"
    /// rather than as JSON.
    fn evaluate(expr: &Value, feature: &BTreeMap<&str, f64>) -> Result<bool, String> {
        let parts = expr
            .as_array()
            .ok_or_else(|| format!("not an expression: {expr}"))?;
        let (head, args) = parts
            .split_first()
            .ok_or_else(|| format!("empty expression: {expr}"))?;
        let arg = |index: usize| {
            args.get(index)
                .ok_or_else(|| format!("{expr} is missing argument {index}"))
        };
        match head.as_str() {
            Some("all") => args
                .iter()
                .try_fold(true, |acc, clause| Ok(acc && evaluate(clause, feature)?)),
            Some("any") => args
                .iter()
                .try_fold(false, |acc, clause| Ok(acc || evaluate(clause, feature)?)),
            Some("!") => Ok(!evaluate(arg(0)?, feature)?),
            Some("has") => {
                let name = field(arg(0)?).ok_or_else(|| format!("{expr} names no field"))?;
                Ok(feature.contains_key(name))
            }
            Some(op @ ("<" | ">=")) => {
                let name = field(arg(0)?).ok_or_else(|| format!("{expr} names no field"))?;
                let bound = arg(1)?
                    .as_f64()
                    .ok_or_else(|| format!("{expr} compares against no number"))?;
                // A feature without the field takes the `!has` branch beside this
                // one, so the comparison only has to agree that it is not there.
                let Some(value) = feature.get(name) else {
                    return Ok(false);
                };
                Ok(match op {
                    "<" => *value < bound,
                    _ => *value >= bound,
                })
            }
            _ => Err(format!("unsupported expression: {expr}")),
        }
    }

    /// The property name a `["has", name]` or `["get", name]` operand reads.
    fn field(operand: &Value) -> Option<&str> {
        match operand {
            Value::String(name) => Some(name),
            Value::Array(parts) => match parts.split_first() {
                Some((head, rest)) if head == "get" => rest.first().and_then(Value::as_str),
                _ => None,
            },
            _ => None,
        }
    }
}
