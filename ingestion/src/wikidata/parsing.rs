//! Wikidata JSON parsing utilities.
//!
//! Functions for extracting fact-grammar values from the typed
//! [`WikidataEntity`] model: names with their citations, sitelinks as external
//! references, and Wikidata's quirky time format as [`UncertainDate`]s.

use chrono::NaiveDate;
use chronoscope_core::date::{DatePrecision, UncertainDate};
use chronoscope_core::external_ids::WikidataPropertyId;
use chronoscope_core::grammar::attribute::{NameText, NameType};
use chronoscope_core::grammar::citations::{
    ExternalReference, FactualCitation, Language, WikidataField, WikimediaCategoryName,
};
use chronoscope_integrations::wikidata::{DataValue, Snak, WikidataEntity, WikidataPrecision};
use url::Url;

use crate::wikidata::{ItemContext, asserted_claims};

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

/// A name read off an item, with the citation naming where it was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedName {
    pub name: NameText,
    pub language: Language,
    pub name_type: NameType,
    pub citation: FactualCitation,
}

/// Extract names from labels (common names) and P1448 (official name) claims.
///
/// A label or claim whose language tag fails BCP-47 parsing, or whose value
/// can't be quoted as an excerpt (empty), is skipped — an uncitable name is
/// dropped rather than fabricated. Each skip records a warning naming the field
/// and reason.
pub fn extract_names(
    entity: &WikidataEntity,
    ctx: &ItemContext,
    warnings: &mut Vec<String>,
) -> Vec<ExtractedName> {
    let mut names = Vec::new();

    for (lang, label) in &entity.labels {
        let Ok(language) = Language::new(lang.0.as_str()) else {
            warnings.push(format!("label {}: unparseable language tag", lang.0));
            continue;
        };
        let citation = match ctx.citation(
            WikidataField::Label {
                language: language.clone(),
            },
            label.value.clone(),
        ) {
            Ok(citation) => citation,
            Err(e) => {
                warnings.push(format!("label {}: {e}", lang.0));
                continue;
            }
        };
        names.push(ExtractedName {
            name: NameText::new(&label.value),
            language,
            name_type: NameType::Common,
            citation,
        });
    }

    let p1448 = WikidataPropertyId::new(1448);
    if let Some(claims) = entity.claims.get("P1448") {
        for claim in asserted_claims(claims) {
            let Snak::Value(DataValue::MonolingualText(mono)) = &claim.mainsnak else {
                warnings.push("P1448: expected monolingual text value".to_owned());
                continue;
            };
            if mono.text.is_empty() {
                warnings.push(format!("P1448: empty official name ({})", mono.language));
                continue;
            }
            let Ok(language) = Language::new(mono.language.0.as_str()) else {
                warnings.push(format!("P1448: unparseable language tag {}", mono.language));
                continue;
            };
            let citation =
                match ctx.statement_citation(p1448, format!("{}:{}", mono.language, mono.text)) {
                    Ok(citation) => citation,
                    Err(e) => {
                        warnings.push(format!("P1448: {e}"));
                        continue;
                    }
                };
            names.push(ExtractedName {
                name: NameText::new(&mono.text),
                language,
                name_type: NameType::Official,
                citation,
            });
        }
    }

    names
}

/// Parse a sitelink into an [`ExternalReference`].
///
/// Wikidata sitelinks use suffixes like "enwiki", "dewiki", "commonswiki".
/// The `*wiki` pattern catches Wikipedia sites (extracting the language
/// prefix); "commonswiki" maps `Category:` pages to the structured category
/// reference and other Commons pages to their URL. Other Wikimedia projects
/// ("enwikiquote" doesn't end in "wiki") return `None`.
#[must_use]
pub fn parse_sitelink(site: &str, title: &str) -> Option<ExternalReference> {
    if title.is_empty() {
        return None;
    }
    if site == "commonswiki" {
        match title.strip_prefix("Category:") {
            Some(category) => Some(ExternalReference::WikimediaCommonsCategory {
                category: WikimediaCategoryName::new(category.replace('_', " ")).ok()?,
            }),
            // Non-category Commons pages (galleries, File: pages) keep their
            // URL form; file pages are ingested as images elsewhere. Commons
            // page URLs are underscore-form, so normalize spaces to match a
            // P973 `described at URL` pointing at the same page.
            None => {
                let mut url = Url::parse("https://commons.wikimedia.org").ok()?;
                url.set_path(&format!("/wiki/{}", title.replace(' ', "_")));
                Some(ExternalReference::UnmodeledUrl { url })
            }
        }
    } else if site.ends_with("wiki") {
        // Wikidata sitelinks are exclusively Wikimedia projects (enwiki,
        // dewiki, etc.). The BCP-47 parse acts as a secondary filter on the
        // language prefix.
        let language = Language::new(site.strip_suffix("wiki")?).ok()?;
        Some(ExternalReference::Wikipedia {
            language,
            title: title.to_owned(),
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;
    use chronoscope_core::external_ids::WikidataEntityId;
    use chronoscope_core::grammar::citations::ExternalSource;
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

    fn ctx() -> Result<ItemContext, Box<dyn std::error::Error>> {
        Ok(ItemContext::new(WikidataEntityId::new(243), 100)?)
    }

    fn p1448_claim(language: &str, text: &str) -> Claim {
        Claim::simple(Snak::Value(DataValue::MonolingualText(
            MonolingualTextValue {
                text: text.to_string(),
                language: LanguageCode(language.to_string()),
            },
        )))
    }

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

        let names = extract_names(&entity, &ctx()?, &mut Vec::new());
        assert_eq!(names.len(), 2);

        let english = names
            .iter()
            .find(|n| n.language.as_str() == "en")
            .ok_or("expected an English name")?;
        assert_eq!(english.name.as_str(), "Eiffel Tower");
        assert_eq!(english.name_type, NameType::Common);
        let ExternalSource::Wikidata {
            entity_id,
            revision_id,
            field,
            value,
        } = &english.citation.source
        else {
            return Err("expected Wikidata source".into());
        };
        assert_eq!(*entity_id, WikidataEntityId::new(243));
        assert_eq!(*revision_id, 100);
        assert_eq!(
            *field,
            WikidataField::Label {
                language: Language::new("en")?,
            }
        );
        assert_eq!(value, "Eiffel Tower");

        let french = names
            .iter()
            .find(|n| n.language.as_str() == "fr")
            .ok_or("expected a French name")?;
        assert_eq!(french.name.as_str(), "Tour Eiffel");
        assert_eq!(french.name_type, NameType::Common);
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
                vec![p1448_claim("fr", "Tour Eiffel")],
            )]),
            sitelinks: BTreeMap::new(),
        };

        let names = extract_names(&entity, &ctx()?, &mut Vec::new());
        assert_eq!(names.len(), 1);
        assert_eq!(names[0].name.as_str(), "Tour Eiffel");
        assert_eq!(names[0].name_type, NameType::Official);
        let ExternalSource::Wikidata {
            entity_id,
            revision_id,
            field,
            value,
        } = &names[0].citation.source
        else {
            return Err("expected Wikidata source".into());
        };
        assert_eq!(*entity_id, WikidataEntityId::new(243));
        assert_eq!(*revision_id, 100);
        assert_eq!(
            *field,
            WikidataField::Statement {
                property_id: WikidataPropertyId::new(1448),
            }
        );
        assert_eq!(value, "fr:Tour Eiffel");
        Ok(())
    }

    #[test]
    fn extract_names_drops_deprecated_p1448_claim() -> TestResult {
        let mut deprecated = p1448_claim("fr", "Ancien nom");
        deprecated.rank = Rank::Deprecated;
        let entity = WikidataEntity {
            id: wikidata_id("Q243")?,
            entity_type: WikidataEntityType::Item,
            lastrevid: RevisionId(100),
            labels: BTreeMap::new(),
            claims: BTreeMap::from([(
                PropertyId::try_from("P1448".to_string())?,
                vec![deprecated, p1448_claim("fr", "Tour Eiffel")],
            )]),
            sitelinks: BTreeMap::new(),
        };

        let names = extract_names(&entity, &ctx()?, &mut Vec::new());
        assert_eq!(names.len(), 1, "only the non-deprecated claim survives");
        assert_eq!(names[0].name.as_str(), "Tour Eiffel");
        Ok(())
    }

    #[test]
    fn extract_names_skips_empty_p1448_and_warns() -> TestResult {
        let entity = WikidataEntity {
            id: wikidata_id("Q243")?,
            entity_type: WikidataEntityType::Item,
            lastrevid: RevisionId(100),
            labels: BTreeMap::new(),
            claims: BTreeMap::from([(
                PropertyId::try_from("P1448".to_string())?,
                vec![p1448_claim("fr", "")],
            )]),
            sitelinks: BTreeMap::new(),
        };

        let mut warnings = Vec::new();
        let names = extract_names(&entity, &ctx()?, &mut warnings);
        assert!(names.is_empty(), "an empty official name is not emitted");
        assert!(
            warnings.iter().any(|w| w.contains("P1448")),
            "the skip records a P1448 warning, got: {warnings:?}"
        );
        Ok(())
    }

    // =============================================================================
    // parse_sitelink Unit Tests
    // =============================================================================

    #[test]
    fn test_sitelink_english_wikipedia() -> TestResult {
        let reference = parse_sitelink("enwiki", "Empire State Building").ok_or("should parse")?;
        match reference {
            ExternalReference::Wikipedia { language, title } => {
                assert_eq!(language.as_str(), "en");
                assert_eq!(title, "Empire State Building");
            }
            other => return Err(format!("expected Wikipedia, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn test_sitelink_german_wikipedia() -> TestResult {
        let reference = parse_sitelink("dewiki", "Berliner Dom").ok_or("should parse")?;
        match reference {
            ExternalReference::Wikipedia { language, title } => {
                assert_eq!(language.as_str(), "de");
                assert_eq!(title, "Berliner Dom");
            }
            other => return Err(format!("expected Wikipedia, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn test_sitelink_commons_category() -> TestResult {
        let reference =
            parse_sitelink("commonswiki", "Category:Buildings").ok_or("should parse")?;
        match reference {
            ExternalReference::WikimediaCommonsCategory { category } => {
                assert_eq!(category.as_str(), "Buildings");
            }
            other => return Err(format!("expected category, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn commons_gallery_sitelink_matches_p973_url_for_same_page() -> TestResult {
        // The sitelink carries the space-form title; a P973 `described at URL`
        // carries the underscore-form page URL. Both must resolve to the same
        // reference so a lookup joins them.
        let from_sitelink =
            parse_sitelink("commonswiki", "Notre-Dame de Paris").ok_or("should parse")?;
        let from_url = ExternalReference::from_url(&Url::parse(
            "https://commons.wikimedia.org/wiki/Notre-Dame_de_Paris",
        )?);
        assert_eq!(from_sitelink, from_url);
        Ok(())
    }

    #[test]
    fn empty_sitelink_title_is_skipped() {
        assert!(parse_sitelink("enwiki", "").is_none());
        assert!(parse_sitelink("commonswiki", "").is_none());
    }

    #[test]
    fn test_sitelink_commons_gallery_page_keeps_url_form() -> TestResult {
        let reference = parse_sitelink("commonswiki", "Rome").ok_or("should parse")?;
        match reference {
            ExternalReference::UnmodeledUrl { url } => {
                assert_eq!(url.as_str(), "https://commons.wikimedia.org/wiki/Rome");
            }
            other => return Err(format!("expected UnmodeledUrl, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn test_sitelink_unknown_site() {
        let reference = parse_sitelink("unknownsite", "Test");
        assert!(reference.is_none());
    }

    #[test]
    fn test_sitelink_wikiquote_not_supported() {
        // We only support wikipedia and commons
        // "enwikiquote" ends with "e", not "wiki", so naturally filtered out
        let reference = parse_sitelink("enwikiquote", "Test");
        assert!(reference.is_none());
    }
}
