//! Wikidata JSON parsing utilities.
//!
//! Functions for extracting data from Wikidata entity JSON structures.
//! Some functions operate on raw `serde_json::Value` (for dump filtering),
//! while others use the typed [`WikidataEntity`] model.

use crate::SourceIdx;
use chrono::NaiveDate;
use chronoscope_core::{
    Cited, DatePrecision, EntityName, Evidence, ExternalLink, LinkTarget, LinkType, NameType,
    UncertainDate, WikidataEntityId, WikidataField, WikidataPropertyId,
};
use chronoscope_integrations::wikidata::{DataValue, Snak, WikidataEntity, WikidataPrecision};
use oxilangtag::LanguageTag;

/// Build an `UncertainDate` at year granularity or coarser.
fn coarse_date(year: i32, precision: DatePrecision) -> Option<UncertainDate> {
    let date = NaiveDate::from_ymd_opt(year, 1, 1)?;
    UncertainDate::with_precision(date, precision).ok()
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
            let date = NaiveDate::from_ymd_opt(year, month, day)?;
            UncertainDate::with_precision(date, DatePrecision::Day).ok()
        }
        WikidataPrecision::Month => {
            let month: u32 = parts.get(1).and_then(|m| m.parse().ok()).unwrap_or(1);
            let date = NaiveDate::from_ymd_opt(year, month, 1)?;
            UncertainDate::with_precision(date, DatePrecision::Month).ok()
        }
        WikidataPrecision::Year => coarse_date(year, DatePrecision::Year),
        WikidataPrecision::Decade => coarse_date(year, DatePrecision::Decade),
        WikidataPrecision::Century => coarse_date(year, DatePrecision::Century),
        WikidataPrecision::Millennium => coarse_date(year, DatePrecision::Millennium),
        // Precisions coarser than millennium not supported
        _ => None,
    }
}

/// Extract names from labels (common names) and P1448 (official name) claims.
pub fn extract_names(
    wd: &WikidataEntity,
    wikidata_id: &str,
    revision_id: u64,
) -> Vec<Cited<EntityName, SourceIdx>> {
    let mut names = Vec::new();

    // The entity id is parsed once here; a non-entity id can't be cited,
    // so we return no names rather than fabricate evidence.
    let Ok(entity_id) = WikidataEntityId::parse(wikidata_id) else {
        return names;
    };

    for (lang, label) in &wd.labels {
        if let Ok(language) = LanguageTag::parse(lang.0.clone()) {
            names.push(Cited::new(
                EntityName {
                    name: label.value.clone(),
                    name_type: NameType::Common,
                    language: language.clone(),
                    valid_from: None,
                    valid_to: None,
                },
                vec![Evidence::Wikidata {
                    entity_id,
                    revision_id,
                    field: WikidataField::Label { language },
                    observed_value: label.value.clone(),
                }],
            ));
        }
    }

    let p1448 = WikidataPropertyId::new(1448);

    // P1448 (official name)
    if let Some(claims) = wd.claims.get("P1448") {
        for claim in claims {
            if let Snak::Value(DataValue::MonolingualText(mono)) = &claim.mainsnak
                && let Ok(language_tag) = LanguageTag::parse(mono.language.0.clone())
            {
                names.push(Cited::new(
                    EntityName {
                        name: mono.text.clone(),
                        name_type: NameType::Official,
                        language: language_tag,
                        valid_from: None,
                        valid_to: None,
                    },
                    vec![Evidence::Wikidata {
                        entity_id,
                        revision_id,
                        field: WikidataField::Statement { property_id: p1448 },
                        observed_value: format!("{}:{}", mono.language, mono.text),
                    }],
                ));
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
    use chrono::Datelike;
    use chronoscope_integrations::wikidata::{
        Claim, Label, LanguageCode, MonolingualTextValue, PropertyId, Rank, RevisionId, Snak,
        WikidataEntityType, WikidataId,
    };
    use std::collections::BTreeMap;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn wikidata_id(s: &str) -> Result<WikidataId, String> {
        WikidataId::try_from(s.to_string())
    }

    fn ymd(y: i32, m: u32, d: u32) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(y, m, d)
    }

    // =============================================================================
    // parse_wikidata_time Unit Tests
    // =============================================================================

    #[test]
    fn test_parse_day_precision() -> TestResult {
        let date = parse_wikidata_time("+1920-06-15T00:00:00Z", WikidataPrecision::Day)
            .ok_or("should parse")?;
        let expected = ymd(1920, 6, 15).ok_or("invalid date")?;
        assert_eq!(date.earliest(), Some(expected));
        assert_eq!(date.latest(), Some(expected));
        Ok(())
    }

    #[test]
    fn test_parse_month_precision() -> TestResult {
        let date = parse_wikidata_time("+1920-06-01T00:00:00Z", WikidataPrecision::Month)
            .ok_or("should parse")?;
        assert_eq!(
            date.earliest(),
            Some(ymd(1920, 6, 1).ok_or("invalid date")?)
        );
        assert_eq!(
            date.latest(),
            Some(NaiveDate::from_ymd_opt(1920, 6, 30).ok_or("invalid date")?)
        );
        Ok(())
    }

    #[test]
    fn test_parse_year_precision() -> TestResult {
        let date = parse_wikidata_time("+1920-01-01T00:00:00Z", WikidataPrecision::Year)
            .ok_or("should parse")?;
        assert_eq!(
            date.earliest(),
            Some(ymd(1920, 1, 1).ok_or("invalid date")?)
        );
        assert_eq!(
            date.latest(),
            Some(NaiveDate::from_ymd_opt(1920, 12, 31).ok_or("invalid date")?)
        );
        Ok(())
    }

    #[test]
    fn test_parse_decade_precision() -> TestResult {
        let date = parse_wikidata_time("+1920-01-01T00:00:00Z", WikidataPrecision::Decade)
            .ok_or("should parse")?;
        assert_eq!(
            date.earliest(),
            Some(ymd(1920, 1, 1).ok_or("invalid date")?)
        );
        assert_eq!(
            date.latest(),
            Some(NaiveDate::from_ymd_opt(1929, 12, 31).ok_or("invalid date")?)
        );
        Ok(())
    }

    #[test]
    fn test_parse_century_precision() -> TestResult {
        let date = parse_wikidata_time("+1850-01-01T00:00:00Z", WikidataPrecision::Century)
            .ok_or("should parse")?;
        // 1850 is in 19th century (1801-1900)
        assert_eq!(
            date.earliest(),
            Some(ymd(1801, 1, 1).ok_or("invalid date")?)
        );
        assert_eq!(
            date.latest(),
            Some(NaiveDate::from_ymd_opt(1900, 12, 31).ok_or("invalid date")?)
        );
        Ok(())
    }

    #[test]
    fn test_parse_millennium_precision() -> TestResult {
        let date = parse_wikidata_time("+1500-01-01T00:00:00Z", WikidataPrecision::Millennium)
            .ok_or("should parse")?;
        // 1500 is in 2nd millennium (1001-2000)
        assert_eq!(
            date.earliest(),
            Some(ymd(1001, 1, 1).ok_or("invalid date")?)
        );
        assert_eq!(
            date.latest(),
            Some(NaiveDate::from_ymd_opt(2000, 12, 31).ok_or("invalid date")?)
        );
        Ok(())
    }

    #[test]
    fn test_parse_bce_date() -> TestResult {
        // BCE dates have negative years — chrono supports years down to ~-262145
        let date = parse_wikidata_time("-0500-01-01T00:00:00Z", WikidataPrecision::Year)
            .ok_or("500 BCE should be within chrono's year range")?;
        assert_eq!(date.earliest().ok_or("expected earliest")?.year(), -500);
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

    #[test]
    fn test_parse_unknown_precision_is_deser_error() {
        // Unknown precision values (e.g., 14) now error at deserialization rather
        // than being silently accepted. Verify the TryFrom<u64> rejects them.
        let result = WikidataPrecision::try_from(14u64);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_year_zero_returns_none() {
        // Wikidata uses astronomical year numbering where year 0 = 1 BCE.
        // chrono uses proleptic Gregorian where year 0 doesn't exist
        // (year -1 = 1 BCE, year 1 = 1 CE). So "+0000" parses to
        // chrono year 0 which chrono rejects.
        let date = parse_wikidata_time("+0000-01-01T00:00:00Z", WikidataPrecision::Year);
        assert!(
            date.is_none(),
            "year 0 is not representable in chrono's proleptic Gregorian"
        );
    }

    #[test]
    fn test_parse_february_leap_year_boundary() -> TestResult {
        // Feb 29 on a leap year should parse at day precision
        let date = parse_wikidata_time("+2000-02-29T00:00:00Z", WikidataPrecision::Day)
            .ok_or("should parse Feb 29 in leap year")?;
        assert_eq!(date.earliest(), Some(ymd(2000, 2, 29).ok_or("invalid")?));
        Ok(())
    }

    #[test]
    fn test_parse_february_non_leap_year() {
        // Feb 29 on a non-leap year should return None
        let date = parse_wikidata_time("+1900-02-29T00:00:00Z", WikidataPrecision::Day);
        assert!(date.is_none(), "Feb 29 in non-leap year should fail");
    }

    #[test]
    fn test_parse_month_february_last_day() -> TestResult {
        // Month precision in February should end on the 28th (or 29th in leap year)
        let date = parse_wikidata_time("+2001-02-01T00:00:00Z", WikidataPrecision::Month)
            .ok_or("should parse")?;
        assert_eq!(
            date.latest(),
            Some(NaiveDate::from_ymd_opt(2001, 2, 28).ok_or("invalid date")?)
        );
        Ok(())
    }

    // =============================================================================
    // extract_names Unit Tests
    // =============================================================================

    #[test]
    fn extract_names_emits_common_names_from_labels() -> TestResult {
        let entity = WikidataEntity {
            id: wikidata_id("Q243")?,
            entity_type: WikidataEntityType::Item,
            lastrevid: RevisionId(100),
            labels: BTreeMap::from([
                (
                    LanguageCode("en".to_string()),
                    Label {
                        language: LanguageCode("en".to_string()),
                        value: "Eiffel Tower".to_string(),
                    },
                ),
                (
                    LanguageCode("fr".to_string()),
                    Label {
                        language: LanguageCode("fr".to_string()),
                        value: "Tour Eiffel".to_string(),
                    },
                ),
            ]),
            claims: BTreeMap::new(),
            sitelinks: BTreeMap::new(),
        };

        let names = extract_names(&entity, "Q243", 100);
        assert_eq!(names.len(), 2);

        let english = names
            .iter()
            .find(|n| n.value.language.as_str() == "en")
            .ok_or("expected an English name")?;
        assert_eq!(english.value.name, "Eiffel Tower");
        assert_eq!(english.value.name_type, NameType::Common);
        assert_eq!(english.evidence.len(), 1);
        let Evidence::Wikidata {
            entity_id,
            revision_id,
            field,
            observed_value,
        } = &english.evidence[0]
        else {
            return Err("expected Wikidata evidence".into());
        };
        assert_eq!(*entity_id, WikidataEntityId::new(243));
        assert_eq!(*revision_id, 100);
        assert_eq!(
            *field,
            WikidataField::Label {
                language: LanguageTag::parse("en".to_string())?,
            }
        );
        assert_eq!(observed_value, "Eiffel Tower");

        let french = names
            .iter()
            .find(|n| n.value.language.as_str() == "fr")
            .ok_or("expected a French name")?;
        assert_eq!(french.value.name, "Tour Eiffel");
        assert_eq!(french.value.name_type, NameType::Common);
        Ok(())
    }

    #[test]
    fn test_extract_names_p1448_official() -> TestResult {
        let entity = WikidataEntity {
            id: wikidata_id("Q243")?,
            entity_type: WikidataEntityType::Item,
            lastrevid: RevisionId(100),
            labels: BTreeMap::new(),
            claims: BTreeMap::from([(
                PropertyId::try_from("P1448".to_string())?,
                vec![Claim {
                    mainsnak: Snak::Value(DataValue::MonolingualText(MonolingualTextValue {
                        text: "Tour Eiffel".to_string(),
                        language: LanguageCode("fr".to_string()),
                    })),
                    qualifiers: BTreeMap::new(),
                    rank: Rank::Normal,
                }],
            )]),
            sitelinks: BTreeMap::new(),
        };

        let names = extract_names(&entity, "Q243", 100);
        assert_eq!(names.len(), 1);
        assert_eq!(names[0].value.name, "Tour Eiffel");
        assert_eq!(names[0].value.name_type, NameType::Official);
        assert_eq!(names[0].evidence.len(), 1);
        let Evidence::Wikidata {
            entity_id,
            revision_id,
            field,
            observed_value,
        } = &names[0].evidence[0]
        else {
            return Err("expected Wikidata evidence".into());
        };
        assert_eq!(*entity_id, WikidataEntityId::new(243));
        assert_eq!(*revision_id, 100);
        assert_eq!(
            *field,
            WikidataField::Statement {
                property_id: WikidataPropertyId::new(1448),
            }
        );
        assert_eq!(observed_value, "fr:Tour Eiffel");
        Ok(())
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
