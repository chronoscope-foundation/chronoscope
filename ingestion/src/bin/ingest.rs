//! Chronoscope ingestion CLI.
//!
//! `build-db`: load a filtered Wikidata entities JSONL dump into a SQLite fact
//! store. The dump-filter tooling (`fetch`, `resolve-types`, `filter`) lives in
//! `chronoscope-integrations`' `wikidata-dump` binary, which stays free of
//! core/db.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chronoscope_ingestion::wikidata::commits::IngestStats;

/// Chronoscope ingestion pipeline
#[derive(clap::Parser)]
#[command(name = "ingest", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

fn parse_recorded_at(s: &str) -> std::result::Result<chrono::DateTime<chrono::Utc>, String> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .map_err(|e| format!("invalid RFC 3339 timestamp: {e}"))
}

#[derive(clap::Subcommand)]
enum Command {
    /// Load a Wikidata entities JSONL dump into a SQLite fact store.
    ///
    /// Streams the input in batches, submitting one commit per entity to the
    /// on-disk store under a single ingester run.
    BuildDb {
        /// Input entities JSONL, one entity per line
        #[arg(short, long)]
        input: PathBuf,

        /// Fact-store database URL for the persistent artifact (e.g.
        /// `sqlite:facts.db`)
        #[arg(long)]
        database_url: String,

        /// Snapshot timestamp (RFC 3339) recorded on every commit
        #[arg(long, value_parser = parse_recorded_at)]
        recorded_at: chrono::DateTime<chrono::Utc>,

        /// Ingest only the first N entities
        #[arg(short, long)]
        limit: Option<u64>,
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
        Command::BuildDb {
            input,
            database_url,
            recorded_at,
            limit,
        } => cmd_build_db(&input, &database_url, recorded_at, limit).await,
    }
}

// =============================================================================
// BUILD DB
// =============================================================================

/// Stream a Wikidata entities JSONL dump into a SQLite fact store.
///
/// The dump is millions of entities, so it's read line by line and submitted
/// in batches rather than materialized into one `Vec`. Every commit carries the
/// same `recorded_at`, so re-loading the same snapshot content-addresses to the
/// same `CommitId`s.
async fn cmd_build_db(
    input: &Path,
    database_url: &str,
    recorded_at: chrono::DateTime<chrono::Utc>,
    limit: Option<u64>,
) -> Result<()> {
    use chronoscope_core::grammar::ids::IngesterRunId;
    use chronoscope_db::FactStoreLocations;

    // `open` creates+migrates the facts file (the artifact), then attaches it
    // as `ovl` for the ingest writes — the two-file layout, with a throwaway
    // in-memory `main` since the ingest touches no app tables. The codec
    // stamp comes AFTER a successful ingest (`build_and_stamp`), so an
    // interrupted build leaves the file unstamped and consumers reject it.
    let store = chronoscope_db::SqliteFactStore::open(FactStoreLocations::standalone(database_url))
        .await
        .with_context(|| format!("opening fact store at {database_url}"))?;
    let run = IngesterRunId::new("wikidata-dump");

    // Tear the pool down inside the runtime whatever the outcome, so
    // SpatiaLite's dlclose stays off the process-exit path even on error.
    let result = build_and_stamp(&store, input, &run, recorded_at, limit).await;
    store.close().await;
    let stats = result?;

    eprintln!(
        "Ingested {} entities across {} commits ({} facts, {} skipped, {} issues)",
        stats.entities, stats.commits, stats.facts, stats.skipped, stats.issues
    );
    Ok(())
}

/// Ingest the dump, then stamp the finished artifact with the facts codec
/// version — the certificate of a completed build that `validate_facts_file`
/// and the dev mount require. Stamping only on success means an interrupted
/// or failed ingest leaves an unstamped partial DB that both reject.
async fn build_and_stamp(
    store: &chronoscope_db::SqliteFactStore,
    input: &Path,
    run: &chronoscope_core::grammar::ids::IngesterRunId,
    recorded_at: chrono::DateTime<chrono::Utc>,
    limit: Option<u64>,
) -> Result<IngestStats> {
    let stats = ingest_jsonl(store, input, run, recorded_at, limit).await?;
    store
        .stamp_codec_version()
        .await
        .context("stamping facts codec version")?;
    Ok(stats)
}

/// Stream the JSONL dump into `store` in batches, stopping at `limit` entities.
/// Split out from `cmd_build_db` so the caller can close the store on any
/// error path, not just success.
async fn ingest_jsonl(
    store: &chronoscope_db::SqliteFactStore,
    input: &Path,
    run: &chronoscope_core::grammar::ids::IngesterRunId,
    recorded_at: chrono::DateTime<chrono::Utc>,
    limit: Option<u64>,
) -> Result<IngestStats> {
    use std::io::BufRead;

    use chronoscope_integrations::wikidata::WikidataEntity;

    const BATCH_SIZE: usize = 10_000;

    let file =
        std::fs::File::open(input).with_context(|| format!("opening {}", input.display()))?;
    let reader = std::io::BufReader::new(file);

    let mut stats = IngestStats::default();
    let mut batch: Vec<WikidataEntity> = Vec::new();
    let mut parsed: u64 = 0;

    for (i, line) in reader.lines().enumerate() {
        if limit.is_some_and(|n| parsed >= n) {
            break;
        }
        let line = line.with_context(|| format!("reading {} line {}", input.display(), i + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let entity: WikidataEntity = serde_json::from_str(&line)
            .with_context(|| format!("parsing {} line {}", input.display(), i + 1))?;
        batch.push(entity);
        parsed += 1;

        if batch.len() >= BATCH_SIZE {
            ingest_batch(store, run, recorded_at, &mut batch, &mut stats).await?;
        }
    }
    ingest_batch(store, run, recorded_at, &mut batch, &mut stats).await?;
    Ok(stats)
}

/// Submit one batch of parsed entities, draining `batch` and folding the
/// per-batch tally into the running total.
async fn ingest_batch(
    store: &chronoscope_db::SqliteFactStore,
    run: &chronoscope_core::grammar::ids::IngesterRunId,
    recorded_at: chrono::DateTime<chrono::Utc>,
    batch: &mut Vec<chronoscope_integrations::wikidata::WikidataEntity>,
    stats: &mut chronoscope_ingestion::wikidata::commits::IngestStats,
) -> Result<()> {
    use chronoscope_ingestion::wikidata::commits::ingest_entities;

    if batch.is_empty() {
        return Ok(());
    }
    let batch_stats = ingest_entities(store, std::mem::take(batch), run, recorded_at)
        .await
        .context("submitting entity batch")?;
    *stats += batch_stats;
    Ok(())
}
