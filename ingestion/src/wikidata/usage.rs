//! Usage inference from Wikidata properties.
//!
//! Maps Wikidata Q-IDs to Chronoscope [`Usage`] categories — the
//! classification kernel for usage state observations (what a building is
//! used for, as distinct from the dated usage-change events the lifecycle
//! path emits, which carry only source-stated uses).

use chronoscope_core::facts::lifecycle::Usage;
use chronoscope_integrations::wikidata::WikidataEntity;
use std::collections::{BTreeSet, HashMap};
use std::sync::LazyLock;

/// P366 (has use) -> Usage mapping.
/// These are direct "has use" values from Wikidata.
pub static P366_USAGE_MAP: LazyLock<HashMap<&'static str, Usage>> = LazyLock::new(|| {
    HashMap::from([
        // Transportation
        ("Q1571929", Usage::Transportation), // general aviation
        ("Q55488", Usage::Transportation),   // railway station
        // Religious
        ("Q29044875", Usage::Religious), // church with cemetery
        ("Q16970", Usage::Religious),    // church building
        ("Q108325", Usage::Religious),   // chapel
        ("Q9174", Usage::Religious),     // religion
        // Residential
        ("Q13402009", Usage::Residential), // apartment building
        ("Q1307276", Usage::Residential),  // single-family detached home
        ("Q3947", Usage::Residential),     // house
        ("Q27686", Usage::Residential),    // hotel (as use = lodging)
        ("Q11755880", Usage::Residential), // residential building
        ("Q1371789", Usage::Residential),  // vacation home
        ("Q699405", Usage::Residential),   // dwelling
        ("Q7419624", Usage::Residential),  // semi-detached house
        ("Q12001587", Usage::Residential), // small house
        ("Q879050", Usage::Residential),   // manor house
        // Commercial
        ("Q182060", Usage::Commercial),  // office
        ("Q1021645", Usage::Commercial), // office building
        ("Q11707", Usage::Commercial),   // restaurant
        ("Q22687", Usage::Commercial),   // bank
        ("Q133215", Usage::Commercial),  // casino
        // Cultural
        ("Q33506", Usage::Cultural),   // museum
        ("Q24354", Usage::Cultural),   // theatre building
        ("Q2087181", Usage::Cultural), // historic house museum
        // Institutional
        ("Q16917", Usage::Institutional),  // hospital
        ("Q3914", Usage::Institutional),   // school
        ("Q481289", Usage::Institutional), // official residence
        ("Q543654", Usage::Institutional), // Rathaus (town hall)
        // Agricultural
        ("Q11453", Usage::Agricultural),  // agricultural irrigation
        ("Q188989", Usage::Agricultural), // aquaculture
        ("Q44494", Usage::Agricultural),  // mill
        // Infrastructure
        ("Q12280", Usage::Infrastructure),   // bridge
        ("Q1501619", Usage::Infrastructure), // water management
    ])
});

/// P31 (instance of) -> Usage mapping.
/// These infer usage from the building type when P366 is not available.
pub static P31_USAGE_MAP: LazyLock<HashMap<&'static str, Usage>> = LazyLock::new(|| {
    HashMap::from([
        // Transportation
        ("Q79007", Usage::Transportation),   // street
        ("Q34442", Usage::Transportation),   // road
        ("Q55488", Usage::Transportation),   // railway station
        ("Q928830", Usage::Transportation),  // metro station
        ("Q1248784", Usage::Transportation), // airport
        ("Q62447", Usage::Transportation),   // aerodrome
        ("Q12284", Usage::Transportation),   // canal
        ("Q949819", Usage::Transportation),  // ship canal
        ("Q728937", Usage::Transportation),  // railway line
        // Religious
        ("Q16970", Usage::Religious),   // church building
        ("Q108325", Usage::Religious),  // chapel
        ("Q39614", Usage::Religious),   // cemetery
        ("Q842402", Usage::Religious),  // Hindu temple
        ("Q44613", Usage::Religious),   // monastery
        ("Q2309609", Usage::Religious), // wayside cross
        // Residential
        ("Q3947", Usage::Residential),     // house
        ("Q11755880", Usage::Residential), // residential building
        ("Q489357", Usage::Residential),   // farmhouse
        ("Q5783996", Usage::Residential),  // cottage
        ("Q3950", Usage::Residential),     // villa
        ("Q751876", Usage::Residential),   // château
        // Commercial
        ("Q27686", Usage::Commercial),  // hotel
        ("Q212198", Usage::Commercial), // pub
        // Cultural
        ("Q24354", Usage::Cultural),   // theatre building
        ("Q41253", Usage::Cultural),   // movie theater
        ("Q2065736", Usage::Cultural), // cultural property
        ("Q2319498", Usage::Cultural), // architectural landmark
        // Institutional
        ("Q16917", Usage::Institutional),   // hospital
        ("Q16560", Usage::Institutional),   // palace
        ("Q1577547", Usage::Institutional), // revenue house
        ("Q2116450", Usage::Institutional), // manor estate
        // Military
        ("Q23413", Usage::Military), // castle
        // Agricultural
        ("Q1303167", Usage::Agricultural), // barn
        // Infrastructure
        ("Q12280", Usage::Infrastructure),  // bridge
        ("Q12323", Usage::Infrastructure),  // dam
        ("Q131681", Usage::Infrastructure), // reservoir
        ("Q820477", Usage::Infrastructure), // mine
        ("Q459297", Usage::Infrastructure), // qanat
    ])
});

/// Infer usage from Wikidata entity claims.
/// Priority: P366 (has use) > P31 (instance of) > Unknown
pub fn infer(wd_entity: &WikidataEntity) -> BTreeSet<Usage> {
    let mut usages = BTreeSet::new();

    // First try P366 (has use)
    if let Some(claims) = wd_entity.claims.get("P366") {
        for claim in claims {
            if let Some(qid) = claim.mainsnak.entity_id()
                && let Some(usage) = P366_USAGE_MAP.get(qid.as_str())
            {
                usages.insert(usage.clone());
            }
        }
    }

    // If no P366 matches, try P31 (instance of)
    if usages.is_empty()
        && let Some(claims) = wd_entity.claims.get("P31")
    {
        for claim in claims {
            if let Some(qid) = claim.mainsnak.entity_id()
                && let Some(usage) = P31_USAGE_MAP.get(qid.as_str())
            {
                usages.insert(usage.clone());
            }
        }
    }

    // Fall back to Unknown if nothing matched
    if usages.is_empty() {
        usages.insert(Usage::Unknown);
    }

    usages
}

#[cfg(test)]
mod tests {
    use super::*;

    use chronoscope_integrations::wikidata::{
        Claim, DataValue, EntityRefValue, PropertyId, RevisionId, Snak, WikidataEntityType,
        WikidataId,
    };
    use std::collections::BTreeMap;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn wikidata_id(s: &str) -> Result<WikidataId, String> {
        WikidataId::try_from(s.to_string())
    }

    fn property_id(s: &str) -> Result<PropertyId, String> {
        PropertyId::try_from(s.to_string())
    }

    fn entity_with_claims(
        claims: BTreeMap<PropertyId, Vec<Claim>>,
    ) -> Result<WikidataEntity, String> {
        Ok(WikidataEntity {
            id: wikidata_id("Q1")?,
            entity_type: WikidataEntityType::Item,
            lastrevid: RevisionId(1),
            labels: BTreeMap::new(),
            claims,
            sitelinks: BTreeMap::new(),
        })
    }

    fn entity_id_claim(qid: &str) -> Result<Claim, String> {
        Ok(Claim::simple(Snak::Value(DataValue::WikibaseEntityId(
            EntityRefValue {
                id: wikidata_id(qid)?,
            },
        ))))
    }

    // =========================================================================
    // infer() tests
    // =========================================================================

    #[test]
    fn infer_p366_has_use_transportation() -> TestResult {
        // Q1571929 = general aviation -> Transportation
        let entity = entity_with_claims(BTreeMap::from([(
            property_id("P366")?,
            vec![entity_id_claim("Q1571929")?],
        )]))?;
        let result = infer(&entity);
        assert_eq!(result.len(), 1);
        assert!(result.contains(&Usage::Transportation));
        Ok(())
    }

    #[test]
    fn infer_p366_multiple_usages() -> TestResult {
        // Q1571929 = Transportation, Q33506 = Cultural (museum)
        let entity = entity_with_claims(BTreeMap::from([(
            property_id("P366")?,
            vec![entity_id_claim("Q1571929")?, entity_id_claim("Q33506")?],
        )]))?;
        let result = infer(&entity);
        assert_eq!(result.len(), 2);
        assert!(result.contains(&Usage::Transportation));
        assert!(result.contains(&Usage::Cultural));
        Ok(())
    }

    #[test]
    fn infer_p31_fallback_when_no_p366() -> TestResult {
        // Q55488 = railway station -> Transportation (via P31)
        let entity = entity_with_claims(BTreeMap::from([(
            property_id("P31")?,
            vec![entity_id_claim("Q55488")?],
        )]))?;
        let result = infer(&entity);
        assert_eq!(result.len(), 1);
        assert!(result.contains(&Usage::Transportation));
        Ok(())
    }

    #[test]
    fn infer_p366_takes_priority_over_p31() -> TestResult {
        // P366 gives Transportation, P31 gives Religious — P366 should win
        let entity = entity_with_claims(BTreeMap::from([
            (property_id("P366")?, vec![entity_id_claim("Q1571929")?]),
            (property_id("P31")?, vec![entity_id_claim("Q16970")?]),
        ]))?;
        let result = infer(&entity);
        assert_eq!(result.len(), 1);
        assert!(result.contains(&Usage::Transportation));
        // Should not contain Religious from P31
        assert!(!result.contains(&Usage::Religious));
        Ok(())
    }

    #[test]
    fn infer_unknown_when_no_match() -> TestResult {
        let entity = entity_with_claims(BTreeMap::from([(
            property_id("P366")?,
            vec![entity_id_claim("Q999999999")?],
        )]))?;
        let result = infer(&entity);
        assert_eq!(result.len(), 1);
        assert!(result.contains(&Usage::Unknown));
        Ok(())
    }

    #[test]
    fn infer_unknown_when_no_claims() -> TestResult {
        let entity = entity_with_claims(BTreeMap::new())?;
        let result = infer(&entity);
        assert_eq!(result.len(), 1);
        assert!(result.contains(&Usage::Unknown));
        Ok(())
    }

    #[test]
    fn infer_all_p366_categories() -> TestResult {
        // Test a representative from each P366 category
        let test_cases = [
            ("Q55488", Usage::Transportation),
            ("Q29044875", Usage::Religious),
            ("Q13402009", Usage::Residential),
            ("Q182060", Usage::Commercial),
            ("Q33506", Usage::Cultural),
            ("Q16917", Usage::Institutional),
            ("Q11453", Usage::Agricultural),
            ("Q12280", Usage::Infrastructure),
        ];
        for (qid, expected_usage) in test_cases {
            let entity = entity_with_claims(BTreeMap::from([(
                property_id("P366")?,
                vec![entity_id_claim(qid)?],
            )]))?;
            let result = infer(&entity);
            assert!(
                result.contains(&expected_usage),
                "P366 QID {qid} should map to {expected_usage:?}, got {result:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn infer_all_p31_categories() -> TestResult {
        // Test a representative from each P31 category
        let test_cases = [
            ("Q79007", Usage::Transportation),
            ("Q16970", Usage::Religious),
            ("Q3947", Usage::Residential),
            ("Q27686", Usage::Commercial),
            ("Q24354", Usage::Cultural),
            ("Q16917", Usage::Institutional),
            ("Q23413", Usage::Military),
            ("Q1303167", Usage::Agricultural),
            ("Q12280", Usage::Infrastructure),
        ];
        for (qid, expected_usage) in test_cases {
            let entity = entity_with_claims(BTreeMap::from([(
                property_id("P31")?,
                vec![entity_id_claim(qid)?],
            )]))?;
            let result = infer(&entity);
            assert!(
                result.contains(&expected_usage),
                "P31 QID {qid} should map to {expected_usage:?}, got {result:?}"
            );
        }
        Ok(())
    }
}
