//! Wikidata property handlers.
//!
//! Handlers for non-lifecycle Wikidata properties. Lifecycle properties
//! (P571, P576, P625, P793, P1619, P3999, P729, P730) are handled by the
//! `lifecycle` module.

use std::collections::HashMap;
use std::sync::LazyLock;

use anyhow::Result;
use chronoscope_core::{ExternalLink, LinkTarget, LinkType, OsmElementType, OsmId};
use chronoscope_integrations::wikidata::Claim;
use url::Url;

use crate::wikidata::{HandlerOutput, PropertyContext};

// =============================================================================
// CONSTANTS
// =============================================================================

const MAX_URL_LENGTH: usize = 2048;

// =============================================================================
// HANDLER TYPES
// =============================================================================

/// Handler for top-level Wikidata properties (P-codes).
///
/// Handlers are pure functions: they receive claims and immutable context,
/// and return what they want to add. Issues are automatically tagged with
/// the property by the caller.
pub type PropertyHandler =
    Box<dyn Fn(&[Claim], &PropertyContext) -> Result<HandlerOutput> + Send + Sync>;

// =============================================================================
// PROPERTY HANDLER REGISTRY
// =============================================================================

/// Property handlers for non-lifecycle properties.
///
/// Lifecycle properties (P571, P576, P625, P793, P1619, P3999, P729, P730)
/// are handled separately by the `lifecycle::build_lifecycles` function.
pub static PROPERTY_HANDLERS: LazyLock<HashMap<&'static str, PropertyHandler>> =
    LazyLock::new(|| {
        // Wrapper for infallible handlers
        let h = |f: fn(&[Claim], &PropertyContext) -> HandlerOutput| -> PropertyHandler {
            Box::new(move |claims, ctx| Ok(f(claims, ctx)))
        };
        HashMap::from([
            // Links
            ("P856", url_link(LinkType::Related)), // official website
            ("P973", url_link(LinkType::FurtherReading)), // described at URL
            ("P402", h(handle_osm_relation)),      // OpenStreetMap relation ID
            ("P1584", h(handle_pleiades)),         // Pleiades ID
        ])
    });

// =============================================================================
// PROPERTY HANDLER FACTORIES
// =============================================================================

fn url_link(link_type: LinkType) -> PropertyHandler {
    Box::new(move |claims, _ctx| {
        let mut out = HandlerOutput::new();
        for claim in claims {
            if let Some(url_str) = claim.mainsnak.string_value() {
                if url_str.len() > MAX_URL_LENGTH {
                    out.issue(format!("URL too long ({} chars)", url_str.len()));
                    continue;
                }
                if !url_str.starts_with("http://") && !url_str.starts_with("https://") {
                    out.issue(format!(
                        "not an HTTP URL: {}",
                        &url_str[..url_str.len().min(50)]
                    ));
                    continue;
                }
                match Url::parse(url_str) {
                    Ok(url) => {
                        out.add_link(ExternalLink {
                            target: LinkTarget::Url { url },
                            link_type,
                        });
                    }
                    Err(e) => out.issue(format!("invalid URL: {e}")),
                }
            }
        }
        Ok(out)
    })
}

// =============================================================================
// CUSTOM HANDLERS
// =============================================================================

fn handle_osm_relation(claims: &[Claim], _ctx: &PropertyContext) -> HandlerOutput {
    let mut out = HandlerOutput::new();
    for claim in claims {
        let Some(id_str) = claim.mainsnak.string_value() else {
            out.issue("claim has no string value");
            continue;
        };
        match id_str.parse::<u64>() {
            Ok(element_id) => out.add_link(ExternalLink {
                target: LinkTarget::OpenStreetMap {
                    element_type: OsmElementType::Relation,
                    element_id: OsmId::new(element_id),
                },
                link_type: LinkType::SameAs,
            }),
            Err(e) => out.issue(format!("invalid OSM relation ID '{id_str}': {e}")),
        }
    }
    out
}

fn handle_pleiades(claims: &[Claim], _ctx: &PropertyContext) -> HandlerOutput {
    let mut out = HandlerOutput::new();
    for claim in claims {
        match claim.mainsnak.string_value() {
            Some(place_id) => {
                out.add_link(ExternalLink {
                    target: LinkTarget::Pleiades {
                        place_id: place_id.to_string(),
                    },
                    link_type: LinkType::SameAs,
                });
            }
            None => out.issue("claim has no string value"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chronoscope_core::{WikidataEntityId, WikidataPropertyId};
    use chronoscope_integrations::wikidata::{
        DataValue, QuantityAmount, QuantityUnit, QuantityValue, Snak,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn test_ctx() -> PropertyContext {
        PropertyContext::with_property(
            WikidataEntityId::new(12345),
            100,
            WikidataPropertyId::new(18),
        )
    }

    fn claim_with_string(value: &str) -> Claim {
        Claim::simple(Snak::Value(DataValue::String(value.to_string())))
    }

    fn claim_non_string() -> Claim {
        Claim::simple(Snak::Value(DataValue::Quantity(QuantityValue {
            amount: QuantityAmount("1".to_string()),
            unit: QuantityUnit::Dimensionless,
        })))
    }

    // =========================================================================
    // url_link handler (P856, P973)
    // =========================================================================

    #[test]
    fn url_link_handler_valid_url() -> TestResult {
        let handler = PROPERTY_HANDLERS
            .get("P856")
            .ok_or("P856 handler not found")?;
        let claims = vec![claim_with_string("https://example.com")];
        let ctx = test_ctx();
        let output = handler(&claims, &ctx)?;

        assert_eq!(output.links.len(), 1);
        assert!(matches!(
            &output.links[0].target,
            LinkTarget::Url { url } if url.as_str() == "https://example.com/"
        ));
        assert_eq!(output.links[0].link_type, LinkType::Related);
        Ok(())
    }

    #[test]
    fn url_link_handler_further_reading() -> TestResult {
        let handler = PROPERTY_HANDLERS
            .get("P973")
            .ok_or("P973 handler not found")?;
        let claims = vec![claim_with_string("https://example.com/article")];
        let ctx = test_ctx();
        let output = handler(&claims, &ctx)?;

        assert_eq!(output.links.len(), 1);
        assert_eq!(output.links[0].link_type, LinkType::FurtherReading);
        Ok(())
    }

    #[test]
    fn url_link_handler_rejects_non_http() -> TestResult {
        let handler = PROPERTY_HANDLERS
            .get("P856")
            .ok_or("P856 handler not found")?;
        let claims = vec![claim_with_string("ftp://example.com/file")];
        let ctx = test_ctx();
        let output = handler(&claims, &ctx)?;

        assert!(output.links.is_empty());
        assert_eq!(output.issues.len(), 1);
        assert!(output.issues[0].contains("not an HTTP URL"));
        Ok(())
    }

    #[test]
    fn url_link_handler_rejects_too_long() -> TestResult {
        let handler = PROPERTY_HANDLERS
            .get("P856")
            .ok_or("P856 handler not found")?;
        let long_url = format!("https://example.com/{}", "x".repeat(MAX_URL_LENGTH));
        let claims = vec![claim_with_string(&long_url)];
        let ctx = test_ctx();
        let output = handler(&claims, &ctx)?;

        assert!(output.links.is_empty());
        assert_eq!(output.issues.len(), 1);
        assert!(output.issues[0].contains("too long"));
        Ok(())
    }

    #[test]
    fn url_link_handler_exactly_at_max_length() -> TestResult {
        let handler = PROPERTY_HANDLERS
            .get("P856")
            .ok_or("P856 handler not found")?;
        // URL exactly at MAX_URL_LENGTH should be accepted
        let padding = MAX_URL_LENGTH - "https://example.com/".len();
        let url = format!("https://example.com/{}", "x".repeat(padding));
        assert_eq!(url.len(), MAX_URL_LENGTH);
        let claims = vec![claim_with_string(&url)];
        let ctx = test_ctx();
        let output = handler(&claims, &ctx)?;

        assert_eq!(output.links.len(), 1);
        assert!(output.issues.is_empty());
        Ok(())
    }

    // =========================================================================
    // OSM handler (P402)
    // =========================================================================

    #[test]
    fn osm_handler_valid_id() -> TestResult {
        let handler = PROPERTY_HANDLERS
            .get("P402")
            .ok_or("P402 handler not found")?;
        let claims = vec![claim_with_string("12345")];
        let ctx = test_ctx();
        let output = handler(&claims, &ctx)?;

        assert_eq!(output.links.len(), 1);
        let LinkTarget::OpenStreetMap {
            element_type,
            element_id,
        } = &output.links[0].target
        else {
            return Err("expected OpenStreetMap target".into());
        };
        assert_eq!(*element_type, OsmElementType::Relation);
        assert_eq!(element_id.get(), 12345);
        assert_eq!(output.links[0].link_type, LinkType::SameAs);
        Ok(())
    }

    #[test]
    fn osm_handler_invalid_id() -> TestResult {
        let handler = PROPERTY_HANDLERS
            .get("P402")
            .ok_or("P402 handler not found")?;
        let claims = vec![claim_with_string("not_a_number")];
        let ctx = test_ctx();
        let output = handler(&claims, &ctx)?;

        assert!(output.links.is_empty());
        assert_eq!(output.issues.len(), 1);
        assert!(output.issues[0].contains("invalid OSM relation ID"));
        Ok(())
    }

    #[test]
    fn osm_handler_non_string_value() -> TestResult {
        let handler = PROPERTY_HANDLERS
            .get("P402")
            .ok_or("P402 handler not found")?;
        let claims = vec![claim_non_string()];
        let ctx = test_ctx();
        let output = handler(&claims, &ctx)?;

        assert!(output.links.is_empty());
        assert_eq!(output.issues.len(), 1);
        Ok(())
    }

    // =========================================================================
    // Pleiades handler (P1584)
    // =========================================================================

    #[test]
    fn pleiades_handler_valid_id() -> TestResult {
        let handler = PROPERTY_HANDLERS
            .get("P1584")
            .ok_or("P1584 handler not found")?;
        let claims = vec![claim_with_string("423025")];
        let ctx = test_ctx();
        let output = handler(&claims, &ctx)?;

        assert_eq!(output.links.len(), 1);
        assert!(matches!(
            &output.links[0].target,
            LinkTarget::Pleiades { place_id } if place_id == "423025"
        ));
        assert_eq!(output.links[0].link_type, LinkType::SameAs);
        Ok(())
    }

    #[test]
    fn pleiades_handler_non_string_value() -> TestResult {
        let handler = PROPERTY_HANDLERS
            .get("P1584")
            .ok_or("P1584 handler not found")?;
        let claims = vec![claim_non_string()];
        let ctx = test_ctx();
        let output = handler(&claims, &ctx)?;

        assert!(output.links.is_empty());
        assert_eq!(output.issues.len(), 1);
        Ok(())
    }
}
