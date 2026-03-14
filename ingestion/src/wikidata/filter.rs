//! Wikidata dump filtering.
//!
//! Filters a Wikidata JSON dump for entities that are instances of
//! architectural structures (transitively via P31).

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result};
use chronoscope_integrations::http::HttpClient;
use chronoscope_integrations::wikidata::{WikidataClient, WikidataId};
use futures::StreamExt;
use futures::stream::Stream;
use serde_json::Value;

use crate::wikidata::stream::{is_instance_of, open_compressed, wikidata_entities};

/// Statistics from a filter run.
#[derive(Debug, Default)]
pub struct FilterStats {
    pub total_entities: u64,
    pub matches: u64,
    pub errors: u64,
}

/// Progress reporting interval.
const PROGRESS_INTERVAL: u64 = 1_000_000;

/// Buffer size for output file.
const OUTPUT_BUFFER_SIZE: usize = 1024 * 1024;

/// Fetch architectural structure types from Wikidata SPARQL.
///
/// Fetches all transitive subclasses of `Q811979` (architectural structure),
/// then subtracts excluded hierarchies (elements, metaclasses, etc.).
///
/// # Errors
/// Returns an error if any SPARQL query fails.
pub async fn fetch_architectural_types<H: HttpClient>(
    client: &WikidataClient<H>,
) -> Result<HashSet<WikidataId>> {
    let qid = |s: &str| -> Result<WikidataId> {
        WikidataId::try_from(s.to_string()).map_err(|e| anyhow::anyhow!("{e}"))
    };

    let architectural_structure_type = qid("Q811979")?;

    let exclude_root_types = [
        qid("Q391414")?,   // architectural element
        qid("Q2996394")?,  // architectural structure type (metaclass)
        qid("Q811909")?,   // building part
        qid("Q19953632")?, // building component
        qid("Q702492")?,   // urban area (cities, towns, etc.)
        // Q254978 (burgh) is a Scottish administrative unit erroneously classified as
        // a subclass of fortification in Wikidata. We exclude it specifically because
        // we can't exclude its parent (fortification) without losing real buildings.
        qid("Q254978")?, // burgh (Scottish town type, not a building)
    ];

    let mut types: HashSet<WikidataId> = client
        .fetch_subclasses(&architectural_structure_type)
        .await
        .context("Failed to fetch architectural structure subclasses")?;

    // Subtract excluded hierarchies (fetched concurrently)
    let exclude_futures: Vec<_> = exclude_root_types
        .iter()
        .map(|t| client.fetch_subclasses(t))
        .collect();
    let exclude_results = futures::future::try_join_all(exclude_futures)
        .await
        .context("Failed to fetch exclusion types")?;

    for exclude_set in exclude_results {
        for t in &exclude_set {
            types.remove(t.as_str());
        }
    }

    Ok(types)
}

/// Filter a Wikidata dump to extract architectural entities.
///
/// Writes matching entities as JSONL to the output path.
/// If `limit` is provided, stops after processing that many entities.
///
/// # Errors
/// Returns an error if the input file cannot be read, SPARQL query fails,
/// or the output file cannot be written.
pub async fn filter_dump<H: HttpClient>(
    client: &WikidataClient<H>,
    input_path: &Path,
    output_path: &Path,
    limit: Option<u64>,
    verbose: bool,
) -> Result<FilterStats> {
    // Fetch target types from SPARQL
    if verbose {
        eprintln!("Fetching architectural structure types from Wikidata SPARQL...");
    }
    let target_types = fetch_architectural_types(client)
        .await
        .context("Failed to fetch architectural types")?;
    if verbose {
        eprintln!("  Found {} types to match", target_types.len());
        eprintln!("Opening {}...", input_path.display());
    }

    // Open input with automatic decompression
    let reader = open_compressed(input_path).await?;

    // Open output
    let out_file = File::create(output_path).context("Failed to create output file")?;
    let mut writer = BufWriter::with_capacity(OUTPUT_BUFFER_SIZE, out_file);

    // Process entity stream
    let base = wikidata_entities(reader);
    let stats = if let Some(n) = limit {
        let limit = usize::try_from(n).unwrap_or(usize::MAX);
        process_entities(base.take(limit), &target_types, &mut writer, verbose).await?
    } else {
        process_entities(base, &target_types, &mut writer, verbose).await?
    };

    writer.flush()?;

    if verbose {
        eprintln!();
        eprintln!("Done!");
        eprintln!("  Total entities: {}", stats.total_entities);
        eprintln!("  Matches: {}", stats.matches);
        if stats.errors > 0 {
            eprintln!("  Errors: {}", stats.errors);
        }
    }

    Ok(stats)
}

/// Process a stream of entities, filtering and writing matches.
async fn process_entities<S>(
    entities: S,
    target_types: &HashSet<WikidataId>,
    writer: &mut BufWriter<File>,
    verbose: bool,
) -> Result<FilterStats>
where
    S: Stream<Item = Result<Value>>,
{
    let mut entities = std::pin::pin!(entities);
    let mut stats = FilterStats::default();

    while let Some(result) = entities.next().await {
        match result {
            Ok(entity) => {
                stats.total_entities += 1;

                if verbose && stats.total_entities % PROGRESS_INTERVAL == 0 {
                    eprintln!(
                        "Processed {} entities, {} matches, {} errors",
                        stats.total_entities, stats.matches, stats.errors
                    );
                }

                if is_instance_of(&entity, target_types) {
                    writeln!(writer, "{}", serde_json::to_string(&entity)?)?;
                    stats.matches += 1;
                }
            }
            Err(e) => {
                stats.errors += 1;
                if verbose {
                    eprintln!("Error at entity {}: {}", stats.total_entities, e);
                }
            }
        }
    }

    Ok(stats)
}
