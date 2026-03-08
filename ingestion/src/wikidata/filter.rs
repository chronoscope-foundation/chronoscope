//! Wikidata dump filtering.
//!
//! Filters a Wikidata JSON dump for entities that are instances of
//! architectural structures (transitively via P31).

use anyhow::{Context, Result};
use futures::StreamExt;
use futures::stream::Stream;
use serde_json::Value;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::wikidata::stream::{is_instance_of, open_compressed, wikidata_entities};
use crate::wikidata::types::fetch_architectural_types;

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

/// Filter a Wikidata dump to extract architectural entities.
///
/// Writes matching entities as JSONL to the output path.
/// If `limit` is provided, stops after processing that many entities.
///
/// # Errors
/// Returns an error if the input file cannot be read, SPARQL query fails,
/// or the output file cannot be written.
pub async fn filter_dump(
    input_path: &Path,
    output_path: &Path,
    limit: Option<u64>,
    verbose: bool,
) -> Result<FilterStats> {
    // Fetch target types from SPARQL
    if verbose {
        eprintln!("Fetching architectural structure types from Wikidata SPARQL...");
    }
    let target_types = fetch_architectural_types(verbose).await?;
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
    target_types: &HashSet<String>,
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
