//! Wikidata property handlers.
//!
//! Handlers for non-lifecycle Wikidata properties. Lifecycle properties
//! (P571, P576, P625, P793, P1619, P3999, P729, P730) are handled by the
//! `lifecycle` module.

use std::collections::HashMap;
use std::sync::LazyLock;

use anyhow::Result;
use chronoscope_core::{
    AnnotationKind, ExternalLink, ImageSource, LinkTarget, LinkType, OsmElementType, OsmId,
};
use chronoscope_integrations::wikidata::{Claim, CommonsFilename, url_for_filename};
use url::Url;

use crate::wikidata::ingest::{HandlerOutput, PropertyContext};

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
    Box<dyn Fn(&[Claim], &PropertyContext<'_>) -> Result<HandlerOutput> + Send + Sync>;

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
        let h = |f: fn(&[Claim], &PropertyContext<'_>) -> HandlerOutput| -> PropertyHandler {
            Box::new(move |claims, ctx| Ok(f(claims, ctx)))
        };
        HashMap::from([
            // Images
            ("P18", image(AnnotationKind::ExteriorView { region: None })), // image
            (
                "P5775",
                image(AnnotationKind::InteriorView { region: None }),
            ), // image of interior
            (
                "P3451",
                image(AnnotationKind::ExteriorView { region: None }),
            ), // nighttime view
            (
                "P3311",
                image(AnnotationKind::SpatialTrace { geometry: None }),
            ), // image of design plans
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

fn image(annotation_kind: AnnotationKind) -> PropertyHandler {
    Box::new(move |claims, _ctx| {
        let mut out = HandlerOutput::new();
        for claim in claims {
            // Skip claims with explicit "no value" or "unknown value"
            if claim.mainsnak.is_special() {
                continue;
            }
            match claim.mainsnak.string_value() {
                Some(filename) => {
                    let url = url_for_filename(&CommonsFilename(filename.to_string()));
                    out.add_image(
                        ImageSource {
                            url,
                            date: None,
                            location: None,
                        },
                        annotation_kind.clone(),
                    );
                }
                None => out.issue("claim has no string value"),
            }
        }
        Ok(out)
    })
}

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

fn handle_osm_relation(claims: &[Claim], _ctx: &PropertyContext<'_>) -> HandlerOutput {
    let mut out = HandlerOutput::new();
    for claim in claims {
        match claim.mainsnak.string_value() {
            Some(id_str) => match id_str.parse::<i64>() {
                Ok(element_id) => {
                    out.add_link(ExternalLink {
                        target: LinkTarget::OpenStreetMap {
                            element_type: OsmElementType::Relation,
                            element_id: OsmId(element_id),
                        },
                        link_type: LinkType::SameAs,
                    });
                }
                Err(e) => out.issue(format!("invalid OSM relation ID '{id_str}': {e}")),
            },
            None => out.issue("claim has no string value"),
        }
    }
    out
}

fn handle_pleiades(claims: &[Claim], _ctx: &PropertyContext<'_>) -> HandlerOutput {
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
    use chronoscope_integrations::wikidata::{
        DataValue, QuantityAmount, QuantityUnit, QuantityValue, Snak, WikidataId,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn test_ctx() -> PropertyContext<'static> {
        PropertyContext::new("Q12345", 100, "P18")
    }

    fn claim_with_string(value: &str) -> Claim {
        Claim::simple(Snak::Value(DataValue::String(value.to_string())))
    }

    fn claim_novalue() -> Claim {
        Claim::simple(Snak::NoValue)
    }

    fn claim_somevalue() -> Claim {
        Claim::simple(Snak::SomeValue)
    }

    fn claim_non_string() -> Claim {
        Claim::simple(Snak::Value(DataValue::Quantity(QuantityValue {
            amount: QuantityAmount("1".to_string()),
            unit: QuantityUnit::Dimensionless,
        })))
    }

    // =========================================================================
    // image handler (P18, P5775, P3451, P3311)
    // =========================================================================

    #[test]
    fn image_handler_creates_image_from_filename() -> TestResult {
        let handler = PROPERTY_HANDLERS
            .get("P18")
            .ok_or("P18 handler not found")?;
        let claims = vec![claim_with_string("Example.jpg")];
        let ctx = test_ctx();
        let output = handler(&claims, &ctx)?;

        assert_eq!(output.images.len(), 1);
        let (img, kind) = &output.images[0];
        assert!(img.url.as_str().contains("Example.jpg"));
        assert!(matches!(kind, AnnotationKind::ExteriorView { .. }));
        Ok(())
    }

    #[test]
    fn image_handler_interior_view() -> TestResult {
        let handler = PROPERTY_HANDLERS
            .get("P5775")
            .ok_or("P5775 handler not found")?;
        let claims = vec![claim_with_string("Interior.jpg")];
        let ctx = test_ctx();
        let output = handler(&claims, &ctx)?;

        assert_eq!(output.images.len(), 1);
        let (_img, kind) = &output.images[0];
        assert!(matches!(kind, AnnotationKind::InteriorView { .. }));
        Ok(())
    }

    #[test]
    fn image_handler_spatial_trace() -> TestResult {
        let handler = PROPERTY_HANDLERS
            .get("P3311")
            .ok_or("P3311 handler not found")?;
        let claims = vec![claim_with_string("Plans.jpg")];
        let ctx = test_ctx();
        let output = handler(&claims, &ctx)?;

        assert_eq!(output.images.len(), 1);
        let (_img, kind) = &output.images[0];
        assert!(matches!(kind, AnnotationKind::SpatialTrace { .. }));
        Ok(())
    }

    #[test]
    fn image_handler_skips_novalue() -> TestResult {
        let handler = PROPERTY_HANDLERS
            .get("P18")
            .ok_or("P18 handler not found")?;
        let claims = vec![claim_novalue()];
        let ctx = test_ctx();
        let output = handler(&claims, &ctx)?;

        assert!(output.images.is_empty());
        assert!(output.issues.is_empty());
        Ok(())
    }

    #[test]
    fn image_handler_skips_somevalue() -> TestResult {
        let handler = PROPERTY_HANDLERS
            .get("P18")
            .ok_or("P18 handler not found")?;
        let claims = vec![claim_somevalue()];
        let ctx = test_ctx();
        let output = handler(&claims, &ctx)?;

        assert!(output.images.is_empty());
        assert!(output.issues.is_empty());
        Ok(())
    }

    #[test]
    fn image_handler_non_string_value_issues() -> TestResult {
        let handler = PROPERTY_HANDLERS
            .get("P18")
            .ok_or("P18 handler not found")?;
        // A value snak but with a non-string DataValue (e.g., Quantity)
        let claims = vec![claim_non_string()];
        let ctx = test_ctx();
        let output = handler(&claims, &ctx)?;

        assert!(output.images.is_empty());
        assert_eq!(output.issues.len(), 1);
        Ok(())
    }

    #[test]
    fn image_handler_multiple_claims() -> TestResult {
        let handler = PROPERTY_HANDLERS
            .get("P18")
            .ok_or("P18 handler not found")?;
        let claims = vec![
            claim_with_string("Photo1.jpg"),
            claim_with_string("Photo2.jpg"),
        ];
        let ctx = test_ctx();
        let output = handler(&claims, &ctx)?;

        assert_eq!(output.images.len(), 2);
        Ok(())
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
        assert!(matches!(
            &output.links[0].target,
            LinkTarget::OpenStreetMap {
                element_type: OsmElementType::Relation,
                element_id: OsmId(12345),
            }
        ));
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

    // =========================================================================
    // entity_accumulator
    // =========================================================================

    #[test]
    fn entity_accumulator_lifecycle() -> TestResult {
        use crate::wikidata::ingest::EntityAccumulator;

        let wid = WikidataId::try_from("Q100".to_string())?;
        use chronoscope_integrations::wikidata::RevisionId;
        let mut acc = EntityAccumulator::new(0, wid, RevisionId(42));
        assert_eq!(acc.wikidata_id(), "Q100");

        // Add image
        acc.add_image(
            ImageSource {
                url: url::Url::parse("https://example.com/img.jpg")?,
                date: None,
                location: None,
            },
            AnnotationKind::ExteriorView { region: None },
        );

        // Add link
        acc.add_link(ExternalLink {
            target: LinkTarget::Pleiades {
                place_id: "test".to_string(),
            },
            link_type: LinkType::SameAs,
        });

        // Add issue
        acc.add_issue("P18", "test issue");

        // Create property context
        let ctx = acc.property_context("P18");
        assert_eq!(ctx.property(), "P18");
        assert_eq!(ctx.wikidata_id(), "Q100");

        // Merge handler output
        let mut output = HandlerOutput::new();
        output.issue("merged issue");
        acc.merge_output("P856", output);

        // Convert to result
        let result = acc.into_result(vec![]);
        assert_eq!(result.wikidata_id, "Q100");
        assert_eq!(result.revision_id, RevisionId(42));
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.links.len(), 1);
        assert_eq!(result.annotations.len(), 1);
        assert_eq!(result.issues.len(), 2);
        Ok(())
    }

    #[test]
    fn property_context_creates_cited_evidence() -> TestResult {
        let ctx = PropertyContext::new("Q100", 42, "P571");
        let cited = ctx.cited("test_raw", 123);

        assert_eq!(cited.value, 123);
        assert_eq!(cited.evidence.len(), 1);
        if let chronoscope_core::Evidence::Wikidata {
            entity_id,
            property_id,
            property_value,
            revision_id,
        } = &cited.evidence[0]
        {
            assert_eq!(entity_id.0, "Q100");
            assert_eq!(property_id.0, "P571");
            assert_eq!(property_value, "test_raw");
            assert_eq!(*revision_id, 42);
        } else {
            return Err("expected Wikidata evidence".into());
        }
        Ok(())
    }
}
