//! Wikidata JSON parsing utilities.
//!
//! Functions for extracting data from Wikidata entity JSON structures.

use chrono::NaiveDate;
use chronoscope_core::{
    Cited, DatePrecision, EntityName, Evidence, ExternalLink, LinkTarget, LinkType, NameType,
    UncertainDate, WikidataEntityId, WikidataPropertyId,
};
use oxilangtag::LanguageTag;
use serde_json::Value;

/// Wikidata time precision values.
///
/// See <https://www.wikidata.org/wiki/Help:Dates#Precision>
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WikidataPrecision {
    BillionYears = 0,
    HundredMillionYears = 1,
    TenMillionYears = 2,
    MillionYears = 3,
    HundredThousandYears = 4,
    TenThousandYears = 5,
    Millennium = 6,
    Century = 7,
    Decade = 8,
    Year = 9,
    Month = 10,
    Day = 11,
}

impl WikidataPrecision {
    /// Convert from Wikidata's numeric precision value.
    ///
    /// Returns `None` for unknown precision values (12+). Wikidata only supports
    /// precisions 0-11 in practice; values 12-14 (hour/minute/second) are defined
    /// but not actually usable on wikidata.org.
    #[must_use]
    pub fn from_u64(value: u64) -> Option<Self> {
        match value {
            0 => Some(Self::BillionYears),
            1 => Some(Self::HundredMillionYears),
            2 => Some(Self::TenMillionYears),
            3 => Some(Self::MillionYears),
            4 => Some(Self::HundredThousandYears),
            5 => Some(Self::TenThousandYears),
            6 => Some(Self::Millennium),
            7 => Some(Self::Century),
            8 => Some(Self::Decade),
            9 => Some(Self::Year),
            10 => Some(Self::Month),
            11 => Some(Self::Day),
            _ => None,
        }
    }
}

/// Check if a claim has a special snaktype (novalue/somevalue) instead of actual data.
///
/// Wikidata uses "novalue" for explicitly absent values and "somevalue" for
/// values known to exist but whose content is unknown.
pub fn is_special_snaktype(claim: &Value) -> bool {
    claim
        .get("mainsnak")
        .and_then(|m| m.get("snaktype"))
        .and_then(|s| s.as_str())
        .is_some_and(|t| t == "novalue" || t == "somevalue")
}

/// Get the inner value from a datavalue wrapper.
///
/// Wikidata wraps values in `{ "datavalue": { "value": ... } }`.
/// This extracts the inner value. Used for qualifiers and snaks.
pub fn get_datavalue(container: &Value) -> Option<&Value> {
    container.get("datavalue").and_then(|d| d.get("value"))
}

/// Get the mainsnak value from a claim.
///
/// Claims have structure `{ "mainsnak": { "datavalue": { "value": ... } } }`.
pub fn get_claim_value(claim: &Value) -> Option<&Value> {
    claim.get("mainsnak").and_then(get_datavalue)
}

/// Get a string value from a claim's mainsnak.
pub fn get_claim_str(claim: &Value) -> Option<&str> {
    get_claim_value(claim).and_then(|v| v.as_str())
}

/// Get a Q-ID reference from a claim's mainsnak (for wikibase-entityid type).
pub fn get_claim_qid(claim: &Value) -> Option<&str> {
    get_claim_value(claim)
        .and_then(|v| v.get("id"))
        .and_then(|i| i.as_str())
}

/// Get claims array for a property.
pub fn get_claims<'a>(wd: &'a Value, property: &str) -> Option<&'a Vec<Value>> {
    wd.get("claims")
        .and_then(|c| c.get(property))
        .and_then(|p| p.as_array())
}

/// Extract time from a qualifier value.
pub fn extract_time_from_qualifier(qualifier: &Value) -> Option<(UncertainDate, String)> {
    let value = get_datavalue(qualifier)?;
    let time_str = value.get("time").and_then(|t| t.as_str())?;
    let precision = value
        .get("precision")
        .and_then(|p| p.as_u64())
        .and_then(WikidataPrecision::from_u64)?;
    let date = parse_wikidata_time(time_str, precision)?;
    Some((date, time_str.to_string()))
}

/// Extract time from top-level claims array.
pub fn extract_time_from_claims(claims: &[Value]) -> Option<(UncertainDate, String)> {
    let claim = claims.first()?;
    let value = get_claim_value(claim)?;
    let time_str = value.get("time").and_then(|t| t.as_str())?;
    let precision = value
        .get("precision")
        .and_then(|p| p.as_u64())
        .and_then(WikidataPrecision::from_u64)?;
    let date = parse_wikidata_time(time_str, precision)?;
    Some((date, time_str.to_string()))
}

/// Extract all date values for a specific qualifier property (P580, P582, or P585).
pub fn extract_qualifier_dates(claim: &Value, property: &str) -> Vec<(UncertainDate, String)> {
    claim
        .get("qualifiers")
        .and_then(|q| q.get(property))
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(extract_time_from_qualifier).collect())
        .unwrap_or_default()
}

/// Parse Wikidata time format to `UncertainDate`.
///
/// Wikidata uses ISO 8601-like format with quirks: `+`/`-` prefix for CE/BCE,
/// always includes `T00:00:00Z`, and precision is stored separately.
pub fn parse_wikidata_time(time_str: &str, precision: WikidataPrecision) -> Option<UncertainDate> {
    let date_part = time_str.trim_start_matches(['+', '-']);
    let date_part = date_part.split('T').next()?;

    let parts: Vec<&str> = date_part.split('-').collect();
    if parts.is_empty() {
        return None;
    }

    let year: i32 = parts[0].parse().ok()?;
    let is_bce = time_str.starts_with('-');
    let year = if is_bce { -year } else { year };

    match precision {
        WikidataPrecision::Day => {
            let month: u32 = parts.get(1).and_then(|m| m.parse().ok()).unwrap_or(1);
            let day: u32 = parts.get(2).and_then(|d| d.parse().ok()).unwrap_or(1);
            let datetime = NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(0, 0, 0)?;
            UncertainDate::with_precision(datetime, DatePrecision::Day).ok()
        }
        WikidataPrecision::Month => {
            let month: u32 = parts.get(1).and_then(|m| m.parse().ok()).unwrap_or(1);
            let datetime = NaiveDate::from_ymd_opt(year, month, 1)?.and_hms_opt(0, 0, 0)?;
            UncertainDate::with_precision(datetime, DatePrecision::Month).ok()
        }
        WikidataPrecision::Year
        | WikidataPrecision::Decade
        | WikidataPrecision::Century
        | WikidataPrecision::Millennium => {
            let date_precision = match precision {
                WikidataPrecision::Year => DatePrecision::Year,
                WikidataPrecision::Decade => DatePrecision::Decade,
                WikidataPrecision::Century => DatePrecision::Century,
                WikidataPrecision::Millennium => DatePrecision::Millennium,
                _ => return None,
            };
            let datetime = NaiveDate::from_ymd_opt(year, 1, 1)?.and_hms_opt(0, 0, 0)?;
            UncertainDate::with_precision(datetime, date_precision).ok()
        }
        // Precisions coarser than millennium not supported
        _ => None,
    }
}

/// Extract names from labels and P1448 (official name).
pub fn extract_names(wd: &Value, wikidata_id: &str, revision_id: u64) -> Vec<Cited<EntityName>> {
    let mut names = Vec::new();

    if let Some(labels) = wd.get("labels").and_then(|l| l.as_object()) {
        for (lang, label_obj) in labels {
            if let Some(value) = label_obj.get("value").and_then(|v| v.as_str())
                && let Ok(language_tag) = LanguageTag::parse(lang.clone())
            {
                names.push(Cited::new(
                    EntityName {
                        name: value.to_string(),
                        name_type: NameType::Common,
                        language: language_tag,
                        valid_from: None,
                        valid_to: None,
                    },
                    vec![Evidence::Wikidata {
                        entity_id: WikidataEntityId(wikidata_id.to_string()),
                        property_id: WikidataPropertyId("label".to_string()),
                        property_value: format!("{}:{}", lang, value),
                        revision_id,
                    }],
                ));
            }
        }
    }

    // P1448 (official name)
    if let Some(claims) = get_claims(wd, "P1448") {
        for claim in claims {
            if let Some(value) = get_claim_value(claim) {
                let Some(text) = value.get("text").and_then(|t| t.as_str()) else {
                    continue;
                };
                let lang = value
                    .get("language")
                    .and_then(|l| l.as_str())
                    .unwrap_or("und");

                if let Ok(language_tag) = LanguageTag::parse(lang.to_string()) {
                    names.push(Cited::new(
                        EntityName {
                            name: text.to_string(),
                            name_type: NameType::Official,
                            language: language_tag,
                            valid_from: None,
                            valid_to: None,
                        },
                        vec![Evidence::Wikidata {
                            entity_id: WikidataEntityId(wikidata_id.to_string()),
                            property_id: WikidataPropertyId("P1448".to_string()),
                            property_value: format!("{}:{}", lang, text),
                            revision_id,
                        }],
                    ));
                }
            }
        }
    }

    names
}

/// Parse a sitelink into a structured `ExternalLink`.
///
/// Returns Wikipedia or `WikimediaCommons` variants for known sites.
/// Returns None for unsupported sitelink types.
///
/// Wikidata sitelinks use suffixes like "enwiki", "dewiki", "commonswiki".
/// We match `*wiki` pattern to catch Wikipedia sites (extracting the language prefix),
/// but exclude "commonswiki" which uses a different URL structure. Other wikis
/// like "enwikiquote" don't end in "wiki" so they're naturally filtered out.
#[must_use]
pub fn parse_sitelink(site: &str, title: &str) -> Option<ExternalLink> {
    // Wikidata sitelinks are exclusively Wikimedia projects (enwiki, dewiki, etc.),
    // so non-Wikimedia wikis like RationalWiki never appear here. The language tag
    // parse acts as a secondary filter — only valid BCP 47 tags pass through.
    if site.ends_with("wiki") && site != "commonswiki" {
        let language = site.strip_suffix("wiki")?;
        let language_tag = LanguageTag::parse(language.to_string()).ok()?;
        Some(ExternalLink {
            target: LinkTarget::Wikipedia {
                language: language_tag,
                title: title.to_string(),
            },
            link_type: LinkType::SameAs,
        })
    } else if site == "commonswiki" {
        Some(ExternalLink {
            target: LinkTarget::WikimediaCommons {
                title: title.to_string(),
            },
            link_type: LinkType::SameAs,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, NaiveDateTime};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn midnight(y: i32, m: u32, d: u32) -> Option<NaiveDateTime> {
        NaiveDate::from_ymd_opt(y, m, d)?.and_hms_opt(0, 0, 0)
    }

    // =============================================================================
    // parse_wikidata_time Unit Tests
    // =============================================================================

    #[test]
    fn test_parse_day_precision() -> TestResult {
        let date = parse_wikidata_time("+1920-06-15T00:00:00Z", WikidataPrecision::Day)
            .ok_or("should parse")?;
        let expected = midnight(1920, 6, 15).ok_or("invalid date")?;
        assert_eq!(date.earliest(), expected);
        assert_eq!(
            date.latest(),
            expected
                .date()
                .and_hms_opt(23, 59, 59)
                .ok_or("invalid time")?
        );
        Ok(())
    }

    #[test]
    fn test_parse_month_precision() -> TestResult {
        let date = parse_wikidata_time("+1920-06-01T00:00:00Z", WikidataPrecision::Month)
            .ok_or("should parse")?;
        assert_eq!(date.earliest(), midnight(1920, 6, 1).ok_or("invalid date")?);
        assert_eq!(
            date.latest(),
            NaiveDate::from_ymd_opt(1920, 6, 30)
                .ok_or("invalid date")?
                .and_hms_opt(23, 59, 59)
                .ok_or("invalid time")?
        );
        Ok(())
    }

    #[test]
    fn test_parse_year_precision() -> TestResult {
        let date = parse_wikidata_time("+1920-01-01T00:00:00Z", WikidataPrecision::Year)
            .ok_or("should parse")?;
        assert_eq!(date.earliest(), midnight(1920, 1, 1).ok_or("invalid date")?);
        assert_eq!(
            date.latest(),
            NaiveDate::from_ymd_opt(1920, 12, 31)
                .ok_or("invalid date")?
                .and_hms_opt(23, 59, 59)
                .ok_or("invalid time")?
        );
        Ok(())
    }

    #[test]
    fn test_parse_decade_precision() -> TestResult {
        let date = parse_wikidata_time("+1920-01-01T00:00:00Z", WikidataPrecision::Decade)
            .ok_or("should parse")?;
        assert_eq!(date.earliest(), midnight(1920, 1, 1).ok_or("invalid date")?);
        assert_eq!(
            date.latest(),
            NaiveDate::from_ymd_opt(1929, 12, 31)
                .ok_or("invalid date")?
                .and_hms_opt(23, 59, 59)
                .ok_or("invalid time")?
        );
        Ok(())
    }

    #[test]
    fn test_parse_century_precision() -> TestResult {
        let date = parse_wikidata_time("+1850-01-01T00:00:00Z", WikidataPrecision::Century)
            .ok_or("should parse")?;
        // 1850 is in 19th century (1801-1900)
        assert_eq!(date.earliest(), midnight(1801, 1, 1).ok_or("invalid date")?);
        assert_eq!(
            date.latest(),
            NaiveDate::from_ymd_opt(1900, 12, 31)
                .ok_or("invalid date")?
                .and_hms_opt(23, 59, 59)
                .ok_or("invalid time")?
        );
        Ok(())
    }

    #[test]
    fn test_parse_millennium_precision() -> TestResult {
        let date = parse_wikidata_time("+1500-01-01T00:00:00Z", WikidataPrecision::Millennium)
            .ok_or("should parse")?;
        // 1500 is in 2nd millennium (1001-2000)
        assert_eq!(date.earliest(), midnight(1001, 1, 1).ok_or("invalid date")?);
        assert_eq!(
            date.latest(),
            NaiveDate::from_ymd_opt(2000, 12, 31)
                .ok_or("invalid date")?
                .and_hms_opt(23, 59, 59)
                .ok_or("invalid time")?
        );
        Ok(())
    }

    #[test]
    fn test_parse_bce_date() -> TestResult {
        // BCE dates have negative years — chrono supports years down to ~-262145
        let date = parse_wikidata_time("-0500-01-01T00:00:00Z", WikidataPrecision::Year)
            .ok_or("500 BCE should be within chrono's year range")?;
        assert_eq!(date.earliest().year(), -500);
        Ok(())
    }

    #[test]
    fn test_parse_very_old_bce_date_returns_none() {
        // Years beyond chrono's range (~-262145) should return None, not panic
        let date = parse_wikidata_time("-9999999-01-01T00:00:00Z", WikidataPrecision::Year);
        assert!(
            date.is_none(),
            "extremely old BCE dates should return None gracefully"
        );
    }

    #[test]
    fn test_parse_unsupported_precision() {
        // Precisions coarser than millennium (0-5) return None
        let date =
            parse_wikidata_time("+1920-01-01T00:00:00Z", WikidataPrecision::TenThousandYears);
        assert!(date.is_none());
    }

    // =============================================================================
    // parse_sitelink Unit Tests
    // =============================================================================

    #[test]
    fn test_sitelink_english_wikipedia() -> TestResult {
        let link = parse_sitelink("enwiki", "Empire_State_Building").ok_or("should parse")?;
        assert_eq!(
            link.to_url().as_str(),
            "https://en.wikipedia.org/wiki/Empire_State_Building"
        );
        Ok(())
    }

    #[test]
    fn test_sitelink_german_wikipedia() -> TestResult {
        let link = parse_sitelink("dewiki", "Berliner_Dom").ok_or("should parse")?;
        assert_eq!(
            link.to_url().as_str(),
            "https://de.wikipedia.org/wiki/Berliner_Dom"
        );
        Ok(())
    }

    #[test]
    fn test_sitelink_commons() -> TestResult {
        let link = parse_sitelink("commonswiki", "Category:Buildings").ok_or("should parse")?;
        assert_eq!(
            link.to_url().as_str(),
            "https://commons.wikimedia.org/wiki/Category%3ABuildings"
        );
        Ok(())
    }

    #[test]
    fn test_sitelink_with_spaces() -> TestResult {
        let link = parse_sitelink("enwiki", "Empire State Building").ok_or("should parse")?;
        assert!(link.to_url().as_str().contains("Empire%20State%20Building"));
        Ok(())
    }

    #[test]
    fn test_sitelink_unknown_site() {
        let link = parse_sitelink("unknownsite", "Test");
        assert!(link.is_none());
    }

    #[test]
    fn test_sitelink_wikiquote_not_supported() {
        // We only support wikipedia and commons
        // "enwikiquote" ends with "e", not "wiki", so naturally filtered out
        let link = parse_sitelink("enwikiquote", "Test");
        assert!(link.is_none());
    }
}
