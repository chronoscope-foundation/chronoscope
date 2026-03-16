//! SPARQL query execution against Wikidata's query service.

use std::collections::HashSet;
use std::time::Duration;

use url::Url;

use super::WikidataError;
use super::entity::WikidataId;
use crate::http::{ACCEPT, HeaderValue, HttpClient, HttpRequest};

/// Timeout for SPARQL queries (5 minutes for large queries).
const SPARQL_TIMEOUT: Duration = Duration::from_secs(300);

/// Fetch all transitive subclasses of a type from Wikidata SPARQL endpoint.
///
/// # Errors
/// Returns an error if `root_type` is not a valid Q-ID, the HTTP request fails,
/// or the JSON response cannot be parsed.
pub(super) async fn fetch_subclasses<H: HttpClient>(
    http: &H,
    root_type: &WikidataId,
) -> Result<HashSet<WikidataId>, WikidataError> {
    if !root_type.as_str().starts_with('Q') {
        return Err(WikidataError::Api {
            message: format!("expected Q-ID for SPARQL query, got '{root_type}'"),
        });
    }
    let query = format!("SELECT ?class WHERE {{ ?class wdt:P279* wd:{root_type} . }}");

    let url = format!(
        "https://query.wikidata.org/sparql?query={}",
        urlencoding::encode(&query)
    );

    let parsed_url = Url::parse(&url)?;
    let request = HttpRequest::get(parsed_url)
        .header(
            ACCEPT,
            HeaderValue::from_static("application/sparql-results+json"),
        )
        .timeout(SPARQL_TIMEOUT);

    let response = http.execute(request).await?;

    if !response.status.is_success() {
        let body = String::from_utf8_lossy(&response.body);
        return Err(WikidataError::Api {
            message: format!(
                "SPARQL query failed with status {}: {body}",
                response.status
            ),
        });
    }

    parse_sparql_qids(&response.body)
}

/// Parse Q-IDs from a SPARQL JSON response.
///
/// Expects the standard SPARQL Results JSON Format:
/// ```json
/// { "results": { "bindings": [{ "class": { "type": "uri", "value": "http://..." } }] } }
/// ```
fn parse_sparql_qids(body: &[u8]) -> Result<HashSet<WikidataId>, WikidataError> {
    let json: serde_json::Value = serde_json::from_slice(body)?;

    let bindings = json
        .pointer("/results/bindings")
        .and_then(|b| b.as_array())
        .ok_or_else(|| WikidataError::Api {
            message: "SPARQL response missing results.bindings array".to_string(),
        })?;

    let mut types = HashSet::new();
    for binding in bindings {
        if let Some(uri) = binding
            .get("class")
            .and_then(|v| v.get("value"))
            .and_then(|v| v.as_str())
            && let Some(qid) = uri.strip_prefix("http://www.wikidata.org/entity/")
            && qid.starts_with('Q')
            && let Ok(id) = WikidataId::try_from(qid.to_string())
        {
            types.insert(id);
        }
    }

    Ok(types)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sparql_qids() -> Result<(), Box<dyn std::error::Error>> {
        let json = serde_json::json!({
            "results": {
                "bindings": [
                    { "class": { "type": "uri", "value": "http://www.wikidata.org/entity/Q41176" } },
                    { "class": { "type": "uri", "value": "http://www.wikidata.org/entity/Q811979" } },
                    { "class": { "type": "uri", "value": "http://www.wikidata.org/entity/P31" } }
                ]
            }
        });

        let body = serde_json::to_vec(&json)?;
        let qids = parse_sparql_qids(&body)?;
        assert!(qids.contains("Q41176"));
        assert!(qids.contains("Q811979"));
        // P31 is a property, not Q-prefixed, should be excluded
        assert!(!qids.contains("P31"));
        assert_eq!(qids.len(), 2);
        Ok(())
    }

    #[test]
    fn test_parse_sparql_qids_empty() -> Result<(), Box<dyn std::error::Error>> {
        let json = serde_json::json!({
            "results": {
                "bindings": []
            }
        });

        let body = serde_json::to_vec(&json)?;
        let qids = parse_sparql_qids(&body)?;
        assert!(qids.is_empty());
        Ok(())
    }

    #[test]
    fn test_parse_sparql_qids_missing_bindings() -> Result<(), Box<dyn std::error::Error>> {
        let json = serde_json::json!({ "results": {} });
        let body = serde_json::to_vec(&json)?;
        let result = parse_sparql_qids(&body);
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn test_wikidata_id_validation() -> Result<(), Box<dyn std::error::Error>> {
        // Valid IDs
        assert!(WikidataId::try_from("Q42".to_string()).is_ok());
        assert!(WikidataId::try_from("P31".to_string()).is_ok());
        assert!(WikidataId::try_from("L123".to_string()).is_ok());

        // Invalid IDs
        assert!(WikidataId::try_from("".to_string()).is_err());
        assert!(WikidataId::try_from("Q".to_string()).is_err());
        assert!(WikidataId::try_from("42".to_string()).is_err());
        assert!(WikidataId::try_from("X42".to_string()).is_err());
        assert!(WikidataId::try_from("Q1 . } UNION { ?x ?y ?z".to_string()).is_err());

        Ok(())
    }
}
