//! Wikidata dump filtering.
//!
//! Filters a Wikidata JSON dump for entities that are instances of
//! architectural structures (transitively via P31).

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::http::HttpClient;
use crate::wikidata::{WikidataClient, WikidataId};
use anyhow::{Context, Result};
use futures_util::StreamExt;
use futures_util::stream::Stream;
use serde_json::Value;

use crate::wikidata::stream::{
    is_instance_of, open_compressed, subclass_parents, wikidata_entities,
};

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

/// Root of the architectural type set: the transitive P279 (subclass-of)
/// closure of `Q811979` (architectural structure).
const ARCHITECTURAL_STRUCTURE_ROOT: &str = "Q811979";

/// Subclass roots whose entire subtrees are removed from the architectural
/// set — elements, metaclasses, parts, and settlement types that aren't
/// buildings in their own right.
const EXCLUDE_ROOTS: &[&str] = &[
    "Q391414",   // architectural element
    "Q2996394",  // architectural structure type (metaclass)
    "Q811909",   // building part
    "Q19953632", // building component
    "Q702492",   // urban area (cities, towns, etc.)
    // Burgh: a Scottish administrative unit misclassified in Wikidata as a
    // subclass of fortification. Its parent (fortification) holds real
    // buildings, so we cut burgh's subtree instead — its children are finer
    // administrative types, never buildings.
    "Q254978", // burgh
];

/// Parse a Q-ID constant into a validated [`WikidataId`].
fn qid(s: &str) -> Result<WikidataId> {
    WikidataId::try_from(s.to_string()).map_err(|e| anyhow::anyhow!("{e}"))
}

/// Resolve the architectural type set via live Wikidata SPARQL.
///
/// Fetches the transitive P279 subclasses of the architectural-structure root,
/// then subtracts each exclusion root's subtree. Shares its root lists with
/// [`resolve_types_from_dump`], the offline path, so the two stay in step.
///
/// # Errors
/// Returns an error if any SPARQL query fails.
pub async fn fetch_architectural_types<H: HttpClient>(
    client: &WikidataClient<H>,
) -> Result<HashSet<WikidataId>> {
    let mut types: HashSet<WikidataId> = client
        .fetch_subclasses(&qid(ARCHITECTURAL_STRUCTURE_ROOT)?)
        .await
        .context("Failed to fetch architectural structure subclasses")?;

    let exclude_roots: Vec<WikidataId> = EXCLUDE_ROOTS
        .iter()
        .copied()
        .map(qid)
        .collect::<Result<_>>()?;
    let exclude_futures: Vec<_> = exclude_roots
        .iter()
        .map(|t| client.fetch_subclasses(t))
        .collect();
    let exclude_results = futures_util::future::try_join_all(exclude_futures)
        .await
        .context("Failed to fetch exclusion types")?;

    for exclude_set in exclude_results {
        for t in &exclude_set {
            types.remove(t.as_str());
        }
    }

    Ok(types)
}

/// Resolve the architectural type set purely from the dump's own P279
/// subclass graph, with no network access.
///
/// Streams the dump once, building a reverse subclass map (parent Q-ID ->
/// child Q-IDs) over only the entities that carry P279, then walks it to the
/// same closure [`fetch_architectural_types`] produces from SPARQL.
///
/// # Errors
/// Returns an error if the dump cannot be read or a resolved Q-ID is invalid.
pub async fn resolve_types_from_dump(
    input_path: &Path,
    verbose: bool,
) -> Result<HashSet<WikidataId>> {
    if verbose {
        eprintln!(
            "Building P279 subclass graph from {}...",
            input_path.display()
        );
    }

    let reader = open_compressed(input_path).await?;
    let mut entities = std::pin::pin!(wikidata_entities(reader));

    // parent Q-ID -> child Q-IDs. Only subclass edges land here, so this stays
    // far smaller than the full dump.
    let mut reverse: HashMap<String, Vec<String>> = HashMap::new();
    let mut total: u64 = 0;
    let mut errors: u64 = 0;

    while let Some(result) = entities.next().await {
        match result {
            Ok(entity) => {
                total += 1;
                if verbose && total.is_multiple_of(PROGRESS_INTERVAL) {
                    eprintln!("Scanned {total} entities, {} subclass roots", reverse.len());
                }
                record_subclass_edges(&mut reverse, &entity);
            }
            Err(e) => {
                errors += 1;
                if verbose {
                    eprintln!("Error at entity {total}: {e}");
                }
            }
        }
    }

    let types = architectural_closure(&reverse)?;

    if verbose {
        eprintln!(
            "Scanned {total} entities ({errors} errors); resolved {} architectural types",
            types.len()
        );
    }

    Ok(types)
}

/// Record entity `A`'s P279 edges into the reverse map: for each parent `B`
/// (`A` is a subclass of `B`), append `A` to `B`'s child list. Entities
/// without an `id` or without P279 add nothing, keeping the map to the
/// subclass graph alone.
fn record_subclass_edges(reverse: &mut HashMap<String, Vec<String>>, entity: &Value) {
    let Some(child) = entity.get("id").and_then(|v| v.as_str()) else {
        return;
    };
    for parent in subclass_parents(entity) {
        reverse
            .entry(parent.to_string())
            .or_default()
            .push(child.to_string());
    }
}

/// All transitive subclasses of `root` reachable through the reverse map,
/// including `root` itself.
fn descendants<'a>(reverse: &'a HashMap<String, Vec<String>>, root: &'a str) -> HashSet<&'a str> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if !seen.insert(node) {
            continue;
        }
        if let Some(children) = reverse.get(node) {
            stack.extend(children.iter().map(String::as_str));
        }
    }
    seen
}

/// Collapse a reverse subclass map (parent Q-ID -> child Q-IDs) into the
/// architectural type set: the architectural-structure closure, minus every
/// exclusion root's subtree.
fn architectural_closure(reverse: &HashMap<String, Vec<String>>) -> Result<HashSet<WikidataId>> {
    let mut excluded: HashSet<&str> = HashSet::new();
    for root in EXCLUDE_ROOTS {
        excluded.extend(descendants(reverse, root));
    }

    descendants(reverse, ARCHITECTURAL_STRUCTURE_ROOT)
        .into_iter()
        .filter(|q| !excluded.contains(q))
        .map(qid)
        .collect()
}

/// Serialize a type set as a deterministic sorted JSON array of Q-ID strings.
fn serialize_types(types: &HashSet<WikidataId>) -> Result<String> {
    let mut sorted: Vec<&WikidataId> = types.iter().collect();
    sorted.sort();
    serde_json::to_string_pretty(&sorted).map_err(Into::into)
}

/// Parse a JSON array of Q-ID strings back into a validated type set.
///
/// An empty set is rejected here: it would make `filter_dump` match nothing
/// across the whole dump, a silent no-op that's almost never intended.
fn parse_types(json: &str) -> Result<HashSet<WikidataId>> {
    let ids: Vec<WikidataId> = serde_json::from_str(json)?;
    if ids.is_empty() {
        anyhow::bail!("types file names no Q-IDs");
    }
    Ok(ids.into_iter().collect())
}

/// Write a type set to `path` as a sorted JSON array of Q-ID strings.
///
/// Sorting makes the file stable across runs regardless of set iteration
/// order.
///
/// # Errors
/// Returns an error if serialization or the file write fails.
pub fn write_types(path: &Path, types: &HashSet<WikidataId>) -> Result<()> {
    let json = serialize_types(types)?;
    std::fs::write(path, json).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

/// Read a type set from a JSON array of Q-ID strings.
///
/// # Errors
/// Returns an error if the file cannot be read or contains an invalid Q-ID.
pub fn read_types(path: &Path) -> Result<HashSet<WikidataId>> {
    let json = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    parse_types(&json).with_context(|| format!("Failed to parse types from {}", path.display()))
}

/// Filter a Wikidata dump to extract architectural entities.
///
/// Streams the dump and writes each entity whose P31 (instance-of) names a
/// member of `target_types` as JSONL. If `limit` is provided, stops after
/// processing that many entities. Resolve `target_types` up front with
/// [`resolve_types_from_dump`] or [`fetch_architectural_types`].
///
/// # Errors
/// Returns an error if the input file cannot be read or the output file
/// cannot be written.
pub async fn filter_dump(
    input_path: &Path,
    output_path: &Path,
    target_types: &HashSet<WikidataId>,
    limit: Option<u64>,
    verbose: bool,
) -> Result<FilterStats> {
    if verbose {
        eprintln!("Matching against {} types", target_types.len());
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
        process_entities(base.take(limit), target_types, &mut writer, verbose).await?
    } else {
        process_entities(base, target_types, &mut writer, verbose).await?
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A dump item `qid` subclassing each of `parents` (a P279 claim per parent).
    fn subclass_item(qid: &str, parents: &[&str]) -> Value {
        let claims: Vec<Value> = parents
            .iter()
            .map(|p| json!({ "mainsnak": { "datavalue": { "value": { "id": p } } } }))
            .collect();
        json!({ "id": qid, "type": "item", "claims": { "P279": claims } })
    }

    fn closure_of(entities: &[Value]) -> Result<HashSet<WikidataId>> {
        let mut reverse = HashMap::new();
        for e in entities {
            record_subclass_edges(&mut reverse, e);
        }
        architectural_closure(&reverse)
    }

    #[test]
    fn closure_includes_transitive_subclasses_and_drops_exclusions() -> TestResult {
        // Q1 is a direct subclass of the architectural-structure root; Q2 sits
        // under Q1; both belong. Q3 subclasses an exclusion root and Q4 sits
        // under Q3, so both are cut. Burgh (Q254978) is an exclusion root that
        // Wikidata also files under the include tree; both it and its synthetic
        // child Q6 must be cut along with the rest of its subtree.
        let entities = vec![
            subclass_item("Q1", &[ARCHITECTURAL_STRUCTURE_ROOT]),
            subclass_item("Q2", &["Q1"]),
            subclass_item("Q3", &["Q391414"]), // architectural element (exclusion root)
            subclass_item("Q4", &["Q3"]),
            subclass_item("Q254978", &["Q1"]), // burgh, misfiled under an included type
            subclass_item("Q6", &["Q254978"]), // child administrative type of burgh
        ];

        let types = closure_of(&entities)?;

        let has = |q: &str| types.iter().any(|t| t == q);
        assert!(has(ARCHITECTURAL_STRUCTURE_ROOT), "root itself is a type");
        assert!(has("Q1"), "direct subclass included");
        assert!(has("Q2"), "transitive subclass included");
        assert!(!has("Q3"), "exclusion-root subtree removed");
        assert!(!has("Q4"), "exclusion-root subtree removed transitively");
        assert!(!has("Q254978"), "burgh removed as an exclusion root");
        assert!(!has("Q6"), "burgh's subtree removed transitively");
        // Q391414 lives outside the architectural closure entirely.
        assert!(!has("Q391414"));

        assert_eq!(types.len(), 3, "exactly root, Q1, Q2");
        Ok(())
    }

    #[test]
    fn types_serialize_sorted_and_round_trip() -> TestResult {
        let types: HashSet<WikidataId> = ["Q9", "Q10", "Q2", "Q100"]
            .into_iter()
            .map(qid)
            .collect::<Result<_>>()?;

        let json = serialize_types(&types)?;
        let order: Vec<String> = serde_json::from_str(&json)?;
        assert!(
            order.windows(2).all(|w| w[0] <= w[1]),
            "types serialize in sorted order: {order:?}"
        );
        assert_eq!(parse_types(&json)?, types, "round-trip preserves the set");
        Ok(())
    }

    #[test]
    fn parse_types_rejects_empty_set() {
        assert!(
            parse_types("[]").is_err(),
            "an empty types file is a match-nothing footgun and must be rejected"
        );
    }
}
