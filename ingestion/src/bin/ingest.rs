//! Chronoscope ingestion CLI.
//!
//! Subcommands for the Wikidata ingestion pipeline:
//! - `fetch`: Fetch entities at a timestamp (resolves revisions, produces JSONL)
//! - `filter`: Filter a Wikidata dump for architectural entities
//! - `bundle`: Transform JSONL into an `IngestionBundle`
//! - `check`: Analyze an `IngestionBundle` for consistency
//! - `load`: Load an `IngestionBundle` into a SQLite database

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chronoscope_integrations::wikidata::{ApiTimestamp, RevisionId, WikidataClient, WikidataId};
use chronoscope_integrations::{ReqwestClient, ReqwestConfig};

/// Chronoscope ingestion pipeline
#[derive(clap::Parser)]
#[command(name = "ingest", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Output target: file path or stdout.
#[derive(Clone)]
enum OutputTarget {
    Stdout,
    File(PathBuf),
}

impl OutputTarget {
    fn open(&self) -> Result<std::io::BufWriter<Box<dyn std::io::Write>>> {
        let writer: Box<dyn std::io::Write> = match self {
            Self::Stdout => Box::new(std::io::stdout().lock()),
            Self::File(path) => {
                let file = std::fs::File::create(path)
                    .with_context(|| format!("Failed to create {}", path.display()))?;
                Box::new(file)
            }
        };
        Ok(std::io::BufWriter::new(writer))
    }
}

fn parse_output_target(s: &str) -> std::result::Result<OutputTarget, String> {
    if s == "-" {
        Ok(OutputTarget::Stdout)
    } else {
        Ok(OutputTarget::File(PathBuf::from(s)))
    }
}

fn parse_entity_arg(s: &str) -> std::result::Result<(WikidataId, String), String> {
    let (qid, name) = s
        .split_once('=')
        .ok_or_else(|| format!("entity must be QID=Name, got: {s}"))?;
    let id = WikidataId::try_from(qid.to_string())?;
    Ok((id, name.to_string()))
}

#[derive(clap::Subcommand)]
enum Command {
    /// Fetch entities from Wikidata at a specific timestamp.
    ///
    /// Resolves revision IDs at the given timestamp, fetches entity data,
    /// and validates that each entity has a label matching the provided name.
    /// Produces deterministic sorted JSONL.
    Fetch {
        /// Timestamp to pin revisions to (e.g., "2022-01-03T00:00:00Z")
        #[arg(short, long)]
        timestamp: ApiTimestamp,

        /// Entities to fetch as QID=Name pairs (e.g., Q243="Eiffel Tower")
        #[arg(short, long = "entity", required = true, value_parser = parse_entity_arg)]
        entities: Vec<(WikidataId, String)>,

        /// Output JSONL file path (or - for stdout)
        #[arg(short, long, default_value = "-", value_parser = parse_output_target)]
        output: OutputTarget,
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
    Bundle {
        /// Input JSONL file path
        #[arg(short, long)]
        input: PathBuf,

        /// Output `IngestionBundle` JSON file path
        #[arg(short, long)]
        output: PathBuf,

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

    /// Load an `IngestionBundle` into a SQLite database.
    ///
    /// Creates or opens the database, runs migrations, and loads all
    /// entities, images, relations, and annotations from the bundle.
    Load {
        /// Path to SQLite database file (created if missing)
        #[arg(short, long)]
        db: PathBuf,

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
        Command::Fetch {
            timestamp,
            entities,
            output,
        } => cmd_fetch(&timestamp, &entities, &output).await,
        Command::Filter {
            input,
            output,
            limit,
            verbose,
        } => cmd_filter(&input, &output, limit, verbose).await,
        Command::Bundle {
            input,
            output,
            verbose,
        } => cmd_bundle(&input, &output, verbose).await,
        Command::Check { input } => cmd_check(&input),
        Command::Load { db, input } => cmd_load(&db, &input).await,
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

// =============================================================================
// FETCH
// =============================================================================

async fn cmd_fetch(
    timestamp: &ApiTimestamp,
    entities: &[(WikidataId, String)],
    output: &OutputTarget,
) -> Result<()> {
    let client = wikidata_client(60)?;

    eprintln!(
        "Fetching {} entities at timestamp {timestamp}",
        entities.len()
    );

    // Step 1: Resolve revision IDs at the timestamp
    let ids: Vec<&str> = entities.iter().map(|(id, _)| id.as_str()).collect();
    let revisions = client
        .resolve_revisions(&ids, timestamp)
        .await
        .context("Failed to resolve entity revisions")?;

    for (entity_id, rev_id) in &revisions {
        eprintln!("  {entity_id} -> rev {rev_id}");
    }

    // Step 2: Fetch entities at those revisions
    let revision_pairs: Vec<(&str, RevisionId)> = revisions
        .iter()
        .map(|(id, rev)| (id.as_str(), *rev))
        .collect();
    let fetched = client
        .get_entities_at_revisions(&revision_pairs)
        .await
        .context("Failed to fetch entities")?;

    eprintln!("  Fetched {} entities", fetched.len());

    // Step 3: Validate names against provided names
    for (qid, expected_name) in entities {
        let entity = fetched
            .get(qid)
            .ok_or_else(|| anyhow::anyhow!("entity {qid} not found in fetched results"))?;
        let has_matching_label = entity
            .labels
            .values()
            .any(|label| label.value == *expected_name);
        if !has_matching_label {
            let actual_labels: Vec<&str> = entity
                .labels
                .values()
                .map(|l| l.value.as_str())
                .take(5)
                .collect();
            anyhow::bail!(
                "name mismatch for {qid}: expected '{expected_name}' but labels are {actual_labels:?}",
            );
        }
    }

    // Step 4: Write sorted JSONL
    let mut writer = output.open()?;
    let mut sorted_keys: Vec<&WikidataId> = fetched.keys().collect();
    sorted_keys.sort_by_key(|k| k.as_str());
    for key in sorted_keys {
        serde_json::to_writer(&mut writer, &fetched[key])?;
        writeln!(writer)?;
    }

    eprintln!("Done! Wrote {} entities", fetched.len());
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
// BUNDLE
// =============================================================================

async fn cmd_bundle(input: &Path, output: &Path, verbose: bool) -> Result<()> {
    let config = chronoscope_ingestion::wikidata::ingest::Config {
        input_path: input.to_path_buf(),
        output_path: output.to_path_buf(),
        verbose,
    };

    // Gallery media is not resolved at bundle time — gallery images will be
    // handled by a dedicated Commons worker in a future commit.
    let gallery_media = HashMap::new();
    chronoscope_ingestion::wikidata::ingest::run(&config, &gallery_media).await
}

// =============================================================================
// CHECK
// =============================================================================

fn cmd_check(input: &Path) -> Result<()> {
    let data = std::fs::read_to_string(input)
        .with_context(|| format!("Failed to read {}", input.display()))?;
    let bundle: chronoscope_ingestion::IngestionOutput =
        serde_json::from_str(&data).context("Failed to parse IngestionBundle")?;

    let report = chronoscope_ingestion::check::analyze(&bundle);
    let json = serde_json::to_string_pretty(&report).context("Failed to serialize report")?;
    println!("{json}");
    Ok(())
}

// =============================================================================
// LOAD
// =============================================================================

async fn cmd_load(db_path: &Path, input: &Path) -> Result<()> {
    let data = std::fs::read_to_string(input)
        .with_context(|| format!("Failed to read {}", input.display()))?;
    let bundle: chronoscope_ingestion::IngestionOutput =
        serde_json::from_str(&data).context("Failed to parse IngestionBundle")?;

    eprintln!(
        "Loading bundle: {} entities, {} images, {} relations, {} annotations",
        bundle.entities.len(),
        bundle.images.len(),
        bundle.entity_relations.len(),
        bundle.annotations.len(),
    );

    let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
    let db = chronoscope_db::Database::new(&db_url)
        .await
        .context("Failed to open database")?;

    let result = chronoscope_db::ingestion::load_bundle(db.pool_ref(), &bundle)
        .await
        .context("Failed to load bundle")?;

    eprintln!(
        "Done! Created {} entities, {} images, {} annotations, {} relations",
        result.entities_created,
        result.images_created,
        result.annotations_created,
        result.relations_created,
    );

    Ok(())
}
