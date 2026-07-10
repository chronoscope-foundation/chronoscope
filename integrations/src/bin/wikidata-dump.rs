//! Wikidata dump tooling CLI.
//!
//! Core-free front end to the dump-filter pipeline:
//! - `fetch`: fetch entities at a timestamp (resolves revisions, produces JSONL)
//! - `resolve-types`: resolve the architectural type set (from a dump, or SPARQL)
//! - `filter`: filter a Wikidata dump for architectural entities
//!
//! Loading the filtered output into a fact store is `chronoscope-ingestion`'s
//! `ingest build-db`, which pulls in core/db; this binary deliberately does not.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chronoscope_integrations::wikidata::{
    ApiTimestamp, RevisionId, WikidataClient, WikidataId, filter,
};
use chronoscope_integrations::{ReqwestClient, ReqwestConfig};

/// Wikidata dump tooling
#[derive(clap::Parser)]
#[command(name = "wikidata-dump", version, about)]
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

    /// Resolve the architectural structure type set.
    ///
    /// By default derives the set offline from the dump's own P279
    /// (subclass-of) graph. With `--sparql`, queries Wikidata's live SPARQL
    /// endpoint instead. Writes a sorted JSON array of Q-IDs.
    ResolveTypes {
        /// Input Wikidata dump (JSON, .gz, or .bz2); required unless --sparql
        #[arg(short, long)]
        input: Option<PathBuf>,

        /// Resolve via live Wikidata SPARQL instead of the dump
        #[arg(long, conflicts_with = "input")]
        sparql: bool,

        /// Output types JSON file
        #[arg(short, long)]
        output: PathBuf,

        /// Print progress information
        #[arg(short, long)]
        verbose: bool,
    },

    /// Filter a Wikidata dump for architectural entities.
    ///
    /// Loads a resolved type set (see `resolve-types`), then streams the dump
    /// and writes matching entities as JSONL.
    Filter {
        /// Input Wikidata dump file (JSON, .gz, or .bz2)
        #[arg(short, long)]
        input: PathBuf,

        /// Resolved types JSON produced by `resolve-types`
        #[arg(short, long)]
        types: PathBuf,

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
        Command::ResolveTypes {
            input,
            sparql,
            output,
            verbose,
        } => cmd_resolve_types(input.as_deref(), sparql, &output, verbose).await,
        Command::Filter {
            input,
            types,
            output,
            limit,
            verbose,
        } => cmd_filter(&input, &types, &output, limit, verbose).await,
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
// RESOLVE TYPES
// =============================================================================

async fn cmd_resolve_types(
    input: Option<&Path>,
    sparql: bool,
    output: &Path,
    verbose: bool,
) -> Result<()> {
    let types = if sparql {
        let client = wikidata_client(300)?;
        if verbose {
            eprintln!("Fetching architectural structure types from Wikidata SPARQL...");
        }
        filter::fetch_architectural_types(&client).await?
    } else {
        let input =
            input.ok_or_else(|| anyhow::anyhow!("--input is required unless --sparql is set"))?;
        filter::resolve_types_from_dump(input, verbose).await?
    };

    filter::write_types(output, &types)?;
    eprintln!(
        "Wrote {} architectural types to {}",
        types.len(),
        output.display()
    );
    Ok(())
}

// =============================================================================
// FILTER
// =============================================================================

async fn cmd_filter(
    input: &Path,
    types: &Path,
    output: &Path,
    limit: Option<u64>,
    verbose: bool,
) -> Result<()> {
    let target_types = filter::read_types(types)?;
    filter::filter_dump(input, output, &target_types, limit, verbose).await?;
    Ok(())
}
