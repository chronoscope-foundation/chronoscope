//! Chronoscope ingestion CLI.
//!
//! Subcommands for the Wikidata ingestion pipeline:
//! - `resolve`: Resolve entity IDs + timestamp to a versioned manifest
//! - `fetch`: Fetch entities at pinned revisions (produces JSONL)
//! - `filter`: Filter a Wikidata dump for architectural entities
//! - `ingest`: Transform JSONL into an `IngestionBundle`
//! - `check`: Analyze an `IngestionBundle` for consistency

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chronoscope_integrations::wikidata::{
    ApiTimestamp, PageId, RevisionId, WikidataClient, WikidataEntity, WikidataId,
};
use chronoscope_integrations::{ReqwestClient, ReqwestConfig};

/// Chronoscope ingestion pipeline
#[derive(clap::Parser)]
#[command(name = "ingest", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Resolve entity IDs at a timestamp to produce a versioned manifest.
    ///
    /// Takes a list of entity IDs and a timestamp, resolves revision IDs
    /// for each entity (and any Commons gallery pages), and outputs a
    /// manifest with pinned revisions suitable for deterministic fetching.
    Resolve {
        /// Timestamp to pin revisions to (e.g., "2022-01-03T00:00:00Z")
        #[arg(short, long)]
        timestamp: String,

        /// Entity IDs to resolve (e.g., Q243 Q2981)
        #[arg(required = true)]
        entities: Vec<String>,

        /// Output manifest JSON file path (or - for stdout)
        #[arg(short, long, default_value = "-")]
        output: String,
    },

    /// Fetch entities at pinned revisions, producing JSONL.
    ///
    /// Reads a versioned manifest (from `resolve`) and fetches entity data
    /// at the exact revisions specified.
    Fetch {
        /// Path to manifest JSON file (or - for stdin)
        #[arg(short, long)]
        manifest: String,

        /// Output JSONL file path (or - for stdout)
        #[arg(short, long, default_value = "-")]
        output: String,
    },

    /// Filter a Wikidata dump for architectural entities.
    ///
    /// Queries Wikidata SPARQL for architectural structure types, then
    /// streams the dump and writes matching entities as JSONL.
    Filter {
        /// Input Wikidata dump file (JSON, .gz, or .bz2)
        #[arg(short, long)]
        input: PathBuf,

        /// Output JSONL file path
        #[arg(short, long)]
        output: PathBuf,

        /// Maximum number of entities to process
        #[arg(short, long)]
        limit: Option<u64>,

        /// Print progress information
        #[arg(short, long)]
        verbose: bool,
    },

    /// Transform JSONL into an `IngestionBundle`.
    ///
    /// Reads pre-filtered JSONL (from `fetch` or `filter`), processes each
    /// entity concurrently, and writes a complete `IngestionBundle` as JSON.
    Ingest {
        /// Input JSONL file path
        #[arg(short, long)]
        input: String,

        /// Output `IngestionBundle` JSON file path
        #[arg(short, long)]
        output: String,

        /// Print progress information
        #[arg(short, long)]
        verbose: bool,
    },

    /// Analyze an `IngestionBundle` for consistency and distributions.
    ///
    /// Reads an `IngestionBundle` JSON file and prints an analysis report
    /// including summary statistics, distribution histograms, consistency
    /// warnings, and interesting entities.
    Check {
        /// Input `IngestionBundle` JSON file path
        #[arg(short, long)]
        input: PathBuf,
    },
}

fn main() -> Result<()> {
    use clap::Parser;
    let cli = Cli::parse();

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(cli))
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Resolve {
            timestamp,
            entities,
            output,
        } => {
            let ts = ApiTimestamp::try_from(timestamp).map_err(|e| anyhow::anyhow!("{e}"))?;
            cmd_resolve(&ts, &entities, &output).await
        }
        Command::Fetch { manifest, output } => cmd_fetch(&manifest, &output).await,
        Command::Filter {
            input,
            output,
            limit,
            verbose,
        } => cmd_filter(&input, &output, limit, verbose).await,
        Command::Ingest {
            input,
            output,
            verbose,
        } => cmd_ingest(&input, &output, verbose).await,
        Command::Check { input } => cmd_check(&input),
    }
}

/// Create a `WikidataClient` with standard configuration.
fn wikidata_client(timeout_secs: u64) -> Result<WikidataClient<ReqwestClient>> {
    let http = ReqwestClient::with_config(&ReqwestConfig {
        timeout: std::time::Duration::from_secs(timeout_secs),
        ..ReqwestConfig::default()
    })?;
    Ok(WikidataClient::new(http))
}

/// Open a buffered writer to stdout (if path is "-") or a file.
fn open_output(path: &str) -> Result<std::io::BufWriter<Box<dyn std::io::Write>>> {
    let writer: Box<dyn std::io::Write> = if path == "-" {
        Box::new(std::io::stdout().lock())
    } else {
        let file = std::fs::File::create(path)
            .with_context(|| format!("Failed to create output file {path}"))?;
        Box::new(file)
    };
    Ok(std::io::BufWriter::new(writer))
}

/// Write entities as JSONL to stdout or a file, sorted by entity ID for determinism.
fn write_jsonl(output_path: &str, entities: &HashMap<WikidataId, WikidataEntity>) -> Result<()> {
    use std::io::Write;
    let mut writer = open_output(output_path)?;
    let mut sorted_keys: Vec<&WikidataId> = entities.keys().collect();
    sorted_keys.sort_by_key(|k| k.as_str());
    for key in sorted_keys {
        serde_json::to_writer(&mut writer, &entities[key])?;
        writeln!(writer)?;
    }
    Ok(())
}

/// Write JSON to stdout or a file.
fn write_json(output_path: &str, value: &impl serde::Serialize) -> Result<()> {
    use std::io::Write;
    let mut writer = open_output(output_path)?;
    serde_json::to_writer_pretty(&mut writer, value)?;
    writeln!(writer)?;
    Ok(())
}

// =============================================================================
// MANIFEST
// =============================================================================

/// Versioned manifest: entity IDs and gallery pages pinned to specific revisions.
///
/// Produced by `resolve`, consumed by `fetch`. All revision IDs are explicit,
/// making fetches fully deterministic. Entity names are validated against the
/// fetched data — they serve as both documentation and a correctness check.
#[derive(serde::Serialize, serde::Deserialize)]
struct Manifest {
    /// Entity ID -> pinned revision with human-readable name.
    versioned_entities: BTreeMap<WikidataId, VersionedEntity>,

    /// Gallery title -> pinned page/revision.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    gallery_pages: BTreeMap<String, GalleryRevision>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct VersionedEntity {
    /// Human-readable name, validated against the entity's labels on fetch.
    name: String,
    /// Pinned revision ID.
    revision: RevisionId,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct GalleryRevision {
    page_id: PageId,
    revision: RevisionId,
}

// =============================================================================
// RESOLVE
// =============================================================================

async fn cmd_resolve(
    timestamp: &ApiTimestamp,
    entity_ids: &[String],
    output_path: &str,
) -> Result<()> {
    let client = wikidata_client(60)?;

    eprintln!(
        "Resolving {} entities at timestamp {timestamp}",
        entity_ids.len()
    );

    // Resolve and fetch everything at once (entities + gallery media)
    let ids: Vec<&str> = entity_ids.iter().map(|s| s.as_str()).collect();
    let revisions = client
        .resolve_revisions(&ids, timestamp)
        .await
        .context("Failed to resolve entity revisions")?;

    for (entity_id, rev_id) in &revisions {
        eprintln!("  {entity_id} -> rev {rev_id}");
    }

    let revision_pairs: Vec<(&str, RevisionId)> = revisions
        .iter()
        .map(|(id, rev)| (id.as_str(), *rev))
        .collect();
    let entities = client
        .get_entities_at_revisions(&revision_pairs)
        .await
        .context("Failed to fetch entities")?;

    // Build versioned_entities with English label as name
    let mut versioned_entities = BTreeMap::new();
    for (entity_id, rev_id) in &revisions {
        let name = entities
            .get(entity_id.as_str())
            .and_then(|e| e.labels.get("en"))
            .map(|l| l.value.clone())
            .unwrap_or_else(|| format!("({entity_id})"));
        eprintln!("  {entity_id}: {name}");
        versioned_entities.insert(
            entity_id.clone(),
            VersionedEntity {
                name,
                revision: *rev_id,
            },
        );
    }

    // Resolve gallery revisions using the shared helper
    eprintln!("Resolving gallery pages...");
    let gallery_resolutions = client
        .resolve_gallery_revisions_from_entities(&entities, timestamp)
        .await
        .context("Failed to resolve gallery revisions")?;

    let mut gallery_pages = BTreeMap::new();
    for (gallery_name, resolution) in &gallery_resolutions {
        match resolution {
            Some((page_id, rev_id)) => {
                eprintln!("  {gallery_name} -> page {page_id}, rev {rev_id}");
                gallery_pages.insert(
                    gallery_name.clone(),
                    GalleryRevision {
                        page_id: *page_id,
                        revision: *rev_id,
                    },
                );
            }
            None => {
                eprintln!("  {gallery_name}: page not found");
            }
        }
    }

    let manifest = Manifest {
        versioned_entities,
        gallery_pages,
    };

    write_json(output_path, &manifest)?;
    eprintln!("Done!");
    Ok(())
}

// =============================================================================
// FETCH
// =============================================================================

async fn cmd_fetch(manifest_path: &str, output_path: &str) -> Result<()> {
    // Read manifest
    let manifest_json = if manifest_path == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("Failed to read manifest from stdin")?;
        buf
    } else {
        std::fs::read_to_string(manifest_path)
            .with_context(|| format!("Failed to read manifest from {manifest_path}"))?
    };
    let manifest: Manifest =
        serde_json::from_str(&manifest_json).context("Failed to parse manifest")?;

    eprintln!(
        "Fetching {} entities at pinned revisions",
        manifest.versioned_entities.len()
    );

    let client = wikidata_client(60)?;

    // Fetch entities at their pinned revisions
    let revision_pairs: Vec<(&str, RevisionId)> = manifest
        .versioned_entities
        .iter()
        .map(|(id, ve)| (id.as_str(), ve.revision))
        .collect();

    let entities = client
        .get_entities_at_revisions(&revision_pairs)
        .await
        .context("Failed to fetch entities")?;

    eprintln!("  Fetched {} entities", entities.len());

    // Validate names: each entity must have a label matching the manifest name
    for (entity_id, entity) in &entities {
        if let Some(ve) = manifest.versioned_entities.get(entity_id) {
            let has_matching_label = entity.labels.values().any(|label| label.value == ve.name);
            if !has_matching_label {
                let actual_labels: Vec<&str> = entity
                    .labels
                    .values()
                    .map(|l| l.value.as_str())
                    .take(5)
                    .collect();
                anyhow::bail!(
                    "name mismatch for {}: manifest says '{}' but entity labels are {:?}",
                    entity_id,
                    ve.name,
                    actual_labels
                );
            }
        }
    }

    write_jsonl(output_path, &entities)?;

    eprintln!("Done! Wrote {} entities", entities.len());
    Ok(())
}

// =============================================================================
// FILTER
// =============================================================================

async fn cmd_filter(input: &Path, output: &Path, limit: Option<u64>, verbose: bool) -> Result<()> {
    let client = wikidata_client(300)?;

    chronoscope_ingestion::wikidata::filter::filter_dump(&client, input, output, limit, verbose)
        .await?;
    Ok(())
}

// =============================================================================
// INGEST
// =============================================================================

async fn cmd_ingest(input: &str, output: &str, verbose: bool) -> Result<()> {
    let config = chronoscope_ingestion::wikidata::ingest::Config {
        input_path: input.to_string(),
        output_path: output.to_string(),
        verbose,
    };

    // TODO: Load gallery media from a pre-resolved file when available.
    // For now, pass an empty map (galleries will be skipped during ingestion).
    let gallery_media = HashMap::new();
    chronoscope_ingestion::wikidata::ingest::run(&config, &gallery_media).await
}

// =============================================================================
// CHECK
// =============================================================================

fn cmd_check(input: &Path) -> Result<()> {
    let data = std::fs::read_to_string(input)
        .with_context(|| format!("Failed to read {}", input.display()))?;
    let bundle: chronoscope_core::IngestionOutput =
        serde_json::from_str(&data).context("Failed to parse IngestionBundle")?;

    let report = chronoscope_ingestion::check::analyze(&bundle);
    let json = serde_json::to_string_pretty(&report).context("Failed to serialize report")?;
    println!("{json}");
    Ok(())
}
