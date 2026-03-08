//! Wikidata type fetching via SPARQL.
//!
//! Fetches architectural structure types and exclusions from the Wikidata SPARQL endpoint.

use anyhow::{Context, Result, bail};
use quick_xml::Reader;
use quick_xml::events::Event;
use reqwest_middleware::ClientWithMiddleware;
use std::collections::HashSet;
use std::io::BufReader;
use std::time::Duration;

use super::build_client;

/// Types to exclude (architectural elements, not structures).
pub const EXCLUDE_ROOT_TYPES: &[&str] = &[
    "Q391414",   // architectural element
    "Q2996394",  // architectural structure type (metaclass)
    "Q811909",   // building part
    "Q19953632", // building component
    "Q702492",   // urban area (cities, towns, etc.)
    // Q254978 (burgh) is a Scottish administrative unit erroneously classified as
    // a subclass of fortification in Wikidata. We exclude it specifically because
    // we can't exclude its parent (fortification) without losing real buildings.
    "Q254978", // burgh (Scottish town type, not a building)
];

/// The root type for architectural structures.
pub const ARCHITECTURAL_STRUCTURE_TYPE: &str = "Q811979";

/// Timeout for SPARQL queries (5 minutes for large queries).
const SPARQL_TIMEOUT: Duration = Duration::from_secs(300);

/// Fetch architectural structure types by querying for includes, then subtracting excludes.
///
/// We fetch includes and excludes separately rather than doing a single SPARQL query with
/// MINUS clauses because the combined query exceeds Wikidata's query timeout limits.
pub async fn fetch_architectural_types(verbose: bool) -> Result<HashSet<String>> {
    let client = build_client(SPARQL_TIMEOUT)?;

    // Fetch all subclasses of Q811979 (architectural structure)
    if verbose {
        eprintln!("  Fetching subclasses of Q811979 (architectural structure)...");
    }
    let mut types = fetch_subclasses(&client, ARCHITECTURAL_STRUCTURE_TYPE).await?;
    if verbose {
        eprintln!("    Found {} types", types.len());
    }

    // Subtract excluded hierarchies
    for exclude_type in EXCLUDE_ROOT_TYPES {
        if verbose {
            eprintln!("  Fetching subclasses of {} to exclude...", exclude_type);
        }
        let exclude_set = fetch_subclasses(&client, exclude_type).await?;
        if verbose {
            eprintln!("    Found {} types to exclude", exclude_set.len());
        }
        for t in exclude_set {
            types.remove(&t);
        }
    }

    Ok(types)
}

/// Fetch all transitive subclasses of a type from Wikidata SPARQL endpoint.
async fn fetch_subclasses(
    client: &ClientWithMiddleware,
    root_type: &str,
) -> Result<HashSet<String>> {
    // Safety: root_type is always a constant Q-ID from our code, no injection risk
    let query = format!(
        "SELECT ?class WHERE {{ ?class wdt:P279* wd:{} . }}",
        root_type
    );

    let url = format!(
        "https://query.wikidata.org/sparql?query={}",
        urlencoding::encode(&query)
    );

    let response = client
        .get(&url)
        .header("Accept", "application/sparql-results+xml")
        .send()
        .await
        .context("Failed to fetch from Wikidata SPARQL endpoint")?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("SPARQL query failed with status {}: {}", status, body);
    }

    // Get the response body as bytes and parse XML
    let body = response
        .bytes()
        .await
        .context("Failed to read response body")?;
    let reader_source = BufReader::new(body.as_ref());
    let mut reader = Reader::from_reader(reader_source);
    let mut types = HashSet::new();
    let mut in_uri = false;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().as_ref() == b"uri" => {
                in_uri = true;
            }
            Ok(Event::Text(e)) if in_uri => {
                let Ok(text) = e.decode() else { continue };
                if let Some(qid) = text.strip_prefix("http://www.wikidata.org/entity/")
                    && qid.starts_with('Q')
                {
                    types.insert(qid.to_string());
                }
            }
            Ok(Event::End(e)) if e.name().as_ref() == b"uri" => {
                in_uri = false;
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                bail!("XML parse error in SPARQL results: {}", e);
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(types)
}
