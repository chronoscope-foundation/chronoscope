//! Link-property extraction.
//!
//! Extracts [`ExternalReference`]s from the non-lifecycle link properties
//! (P856, P973, P402, P1584). Lifecycle properties (P571, P576, P625, P793,
//! P1619, P3999, P729, P730) are handled by the `lifecycle` module.

use std::collections::BTreeMap;

use chronoscope_core::external_ids::{OsmElementType, OsmId, PleiadesPlaceId, WikidataPropertyId};
use chronoscope_core::grammar::citations::ExternalReference;
use chronoscope_integrations::wikidata::{Claim, PropertyId};
use url::Url;

use crate::wikidata::asserted_claims;

const MAX_URL_LENGTH: usize = 2048;

/// A link-property extraction: the typed reference plus the raw claim value
/// its citation quotes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRef {
    /// The property the reference was read from.
    pub property_id: WikidataPropertyId,
    pub reference: ExternalReference,
    /// The claim value as observed, for the citation excerpt.
    pub raw: String,
}

/// Extract external references from every link property present.
///
/// Returns the references alongside issue strings for claims the boundary
/// rejected (malformed URLs, non-numeric ids).
pub fn extract_link_references(
    claims: &BTreeMap<PropertyId, Vec<Claim>>,
) -> (Vec<LinkRef>, Vec<String>) {
    let mut refs = Vec::new();
    let mut issues = Vec::new();

    let mut extract = |prop: &str, f: &dyn Fn(&str) -> Result<ExternalReference, String>| {
        let Some(prop_claims) = claims.get(prop) else {
            return;
        };
        let Ok(property_id) = WikidataPropertyId::parse(prop) else {
            return;
        };
        for claim in asserted_claims(prop_claims) {
            let Some(value) = claim.mainsnak.string_value() else {
                issues.push(format!("{prop}: claim has no string value"));
                continue;
            };
            match f(value) {
                Ok(reference) => refs.push(LinkRef {
                    property_id,
                    reference,
                    raw: value.to_owned(),
                }),
                Err(issue) => issues.push(format!("{prop}: {issue}")),
            }
        }
    };

    extract("P856", &url_reference); // official website
    extract("P973", &url_reference); // described at URL
    extract("P402", &osm_relation_reference); // OpenStreetMap relation ID
    extract("P1584", &pleiades_reference); // Pleiades ID

    (refs, issues)
}

fn url_reference(value: &str) -> Result<ExternalReference, String> {
    if value.len() > MAX_URL_LENGTH {
        return Err(format!("URL too long ({} chars)", value.len()));
    }
    if !value.starts_with("http://") && !value.starts_with("https://") {
        let head: String = value.chars().take(50).collect();
        return Err(format!("not an HTTP URL: {head}"));
    }
    let url = Url::parse(value).map_err(|e| format!("invalid URL: {e}"))?;
    Ok(ExternalReference::from_url(&url))
}

fn osm_relation_reference(value: &str) -> Result<ExternalReference, String> {
    let element_id: u64 = value
        .parse()
        .map_err(|e| format!("invalid OSM relation ID '{value}': {e}"))?;
    Ok(ExternalReference::OpenStreetMap {
        element_type: OsmElementType::Relation,
        id: OsmId::new(element_id),
    })
}

fn pleiades_reference(value: &str) -> Result<ExternalReference, String> {
    let place_id: u64 = value
        .parse()
        .map_err(|e| format!("invalid Pleiades place ID '{value}': {e}"))?;
    Ok(ExternalReference::Pleiades {
        place_id: PleiadesPlaceId::new(place_id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chronoscope_integrations::wikidata::{
        DataValue, QuantityAmount, QuantityUnit, QuantityValue, Rank, Snak,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn claim_with_string(value: &str) -> Claim {
        Claim::simple(Snak::Value(DataValue::String(value.to_string())))
    }

    fn claim_non_string() -> Claim {
        Claim::simple(Snak::Value(DataValue::Quantity(QuantityValue {
            amount: QuantityAmount("1".to_string()),
            unit: QuantityUnit::Dimensionless,
        })))
    }

    fn claims_for(
        prop: &str,
        claims: Vec<Claim>,
    ) -> Result<BTreeMap<PropertyId, Vec<Claim>>, String> {
        Ok(BTreeMap::from([(
            PropertyId::try_from(prop.to_owned())?,
            claims,
        )]))
    }

    // =========================================================================
    // URL properties (P856, P973)
    // =========================================================================

    #[test]
    fn url_property_yields_reference_with_raw_value() -> TestResult {
        let claims = claims_for("P856", vec![claim_with_string("https://example.com")])?;
        let (refs, issues) = extract_link_references(&claims);

        assert!(issues.is_empty());
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].property_id, WikidataPropertyId::new(856));
        assert_eq!(refs[0].raw, "https://example.com");
        assert!(matches!(
            &refs[0].reference,
            ExternalReference::UnmodeledUrl { url } if url.as_str() == "https://example.com/"
        ));
        Ok(())
    }

    #[test]
    fn described_at_url_dispatches_recognized_hosts() -> TestResult {
        let claims = claims_for(
            "P973",
            vec![claim_with_string("https://en.wikipedia.org/wiki/Pantheon")],
        )?;
        let (refs, issues) = extract_link_references(&claims);

        assert!(issues.is_empty());
        assert_eq!(refs.len(), 1);
        assert!(matches!(
            &refs[0].reference,
            ExternalReference::Wikipedia { title, .. } if title.as_str() == "Pantheon"
        ));
        Ok(())
    }

    #[test]
    fn url_property_rejects_non_http() -> TestResult {
        let claims = claims_for("P856", vec![claim_with_string("ftp://example.com/file")])?;
        let (refs, issues) = extract_link_references(&claims);

        assert!(refs.is_empty());
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("not an HTTP URL"));
        Ok(())
    }

    #[test]
    fn url_property_rejects_too_long() -> TestResult {
        let long_url = format!("https://example.com/{}", "x".repeat(MAX_URL_LENGTH));
        let claims = claims_for("P856", vec![claim_with_string(&long_url)])?;
        let (refs, issues) = extract_link_references(&claims);

        assert!(refs.is_empty());
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("too long"));
        Ok(())
    }

    #[test]
    fn url_property_accepts_exactly_max_length() -> TestResult {
        let padding = MAX_URL_LENGTH - "https://example.com/".len();
        let url = format!("https://example.com/{}", "x".repeat(padding));
        assert_eq!(url.len(), MAX_URL_LENGTH);
        let claims = claims_for("P856", vec![claim_with_string(&url)])?;
        let (refs, issues) = extract_link_references(&claims);

        assert_eq!(refs.len(), 1);
        assert!(issues.is_empty());
        Ok(())
    }

    #[test]
    fn deprecated_link_claim_is_dropped() -> TestResult {
        let mut deprecated = claim_with_string("https://old.example.com");
        deprecated.rank = Rank::Deprecated;
        let claims = claims_for(
            "P856",
            vec![deprecated, claim_with_string("https://example.com")],
        )?;
        let (refs, issues) = extract_link_references(&claims);

        assert!(issues.is_empty());
        assert_eq!(refs.len(), 1, "the deprecated claim contributes nothing");
        assert_eq!(refs[0].raw, "https://example.com");
        Ok(())
    }

    // =========================================================================
    // OSM relations (P402)
    // =========================================================================

    #[test]
    fn osm_relation_id_yields_structured_reference() -> TestResult {
        let claims = claims_for("P402", vec![claim_with_string("12345")])?;
        let (refs, issues) = extract_link_references(&claims);

        assert!(issues.is_empty());
        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs[0].reference,
            ExternalReference::OpenStreetMap {
                element_type: OsmElementType::Relation,
                id: OsmId::new(12345),
            }
        );
        assert_eq!(refs[0].raw, "12345");
        Ok(())
    }

    #[test]
    fn osm_relation_rejects_non_numeric_id() -> TestResult {
        let claims = claims_for("P402", vec![claim_with_string("not_a_number")])?;
        let (refs, issues) = extract_link_references(&claims);

        assert!(refs.is_empty());
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("invalid OSM relation ID"));
        Ok(())
    }

    #[test]
    fn osm_relation_rejects_non_string_value() -> TestResult {
        let claims = claims_for("P402", vec![claim_non_string()])?;
        let (refs, issues) = extract_link_references(&claims);

        assert!(refs.is_empty());
        assert_eq!(issues.len(), 1);
        Ok(())
    }

    // =========================================================================
    // Pleiades (P1584)
    // =========================================================================

    #[test]
    fn pleiades_id_yields_structured_reference() -> TestResult {
        let claims = claims_for("P1584", vec![claim_with_string("423025")])?;
        let (refs, issues) = extract_link_references(&claims);

        assert!(issues.is_empty());
        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs[0].reference,
            ExternalReference::Pleiades {
                place_id: PleiadesPlaceId::new(423025),
            }
        );
        Ok(())
    }

    #[test]
    fn pleiades_rejects_non_numeric_id() -> TestResult {
        let claims = claims_for("P1584", vec![claim_with_string("not-numeric")])?;
        let (refs, issues) = extract_link_references(&claims);

        assert!(refs.is_empty());
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("invalid Pleiades place ID"));
        Ok(())
    }
}
