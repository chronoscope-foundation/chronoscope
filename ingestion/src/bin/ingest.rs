//! Chronoscope ingestion CLI.
//!
//! `build-db`: load a filtered Wikidata entities JSONL dump into a SQLite fact
//! store. The dump-filter tooling (`fetch`, `resolve-types`, `filter`) lives in
//! `chronoscope-integrations`' `wikidata-dump` binary, which stays free of
//! core/db.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chronoscope_core::store::FactStore;
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

    // Per-record failures are reported through `tracing`, and a bulk build runs
    // unattended inside a Nix derivation: the build log is the operator's only
    // list of what to fix.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

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
    let run = IngesterRunId::new("wikidata-dump")?;

    // Tear the pool down inside the runtime whatever the outcome, so
    // SpatiaLite's dlclose stays off the process-exit path even on error.
    let result = build_and_stamp(&store, input, &run, recorded_at, limit).await;
    store.close().await;
    let stats = result?;

    eprintln!(
        "Ingested {} entities across {} commits ({} facts, {} skipped, {} failed, {} issues)",
        stats.entities, stats.commits, stats.facts, stats.skipped, stats.failed, stats.issues
    );
    Ok(())
}

/// Ingest the dump, then stamp the finished artifact with the facts codec
/// version — the certificate of a completed build that `validate_facts_file`
/// and the dev mount require. Stamping only on success means an interrupted
/// or failed ingest leaves an unstamped partial DB that both reject.
///
/// A run that dropped records is such a failure: [`DumpTally::into_stats`]
/// refuses before the stamp, so an under-ingested DB never passes for a
/// finished one.
async fn build_and_stamp(
    store: &chronoscope_db::SqliteFactStore,
    input: &Path,
    run: &chronoscope_core::grammar::ids::IngesterRunId,
    recorded_at: chrono::DateTime<chrono::Utc>,
    limit: Option<u64>,
) -> Result<IngestStats> {
    let stats = ingest_jsonl(store, input, run, recorded_at, limit)
        .await?
        .into_stats()?;
    store
        .stamp_codec_version()
        .await
        .context("stamping facts codec version")?;
    Ok(stats)
}

/// What one pass over the dump produced: the entity-level tally, plus the lines
/// that never became an entity.
#[derive(Debug, Default)]
struct DumpTally {
    entities: IngestStats,
    /// Lines that never became a `WikidataEntity` — not UTF-8, or not the JSON
    /// an entity deserializes from.
    malformed_lines: usize,
}

impl DumpTally {
    /// The tally of a build worth keeping, or an error naming what was lost.
    ///
    /// The only way to the stats, so a run that dropped records can't reach the
    /// codec stamp: downstream, an under-ingested DB is indistinguishable from
    /// a complete one, and these builds run where nobody is watching stdout.
    ///
    /// What it gates on is dump integrity. A value the fact store can't hold
    /// costs its own image or reference and is tallied as an issue, so it
    /// leaves the build valid; a line that isn't an entity, or an entity whose
    /// own scaffolding won't build, means records went missing.
    fn into_stats(self) -> Result<IngestStats> {
        if self.malformed_lines > 0 || self.entities.failed > 0 {
            bail!(
                "run completed but the build is not valid — unparseable dump lines: {}, \
                 entities that failed to build: {}. Each was logged above with its line \
                 number or QID; the artifact is left unstamped, so fix them and rebuild.",
                self.malformed_lines,
                self.entities.failed,
            );
        }
        Ok(self.entities)
    }
}

/// Stream the JSONL dump into `store` in batches, stopping at `limit` entities.
/// Split out from `cmd_build_db` so the caller can close the store on any
/// error path, not just success.
///
/// Per-record failures accumulate into the returned tally instead of ending the
/// pass: aborting on the first bad line means an hour of ingest per record
/// fixed, so one pass has to name every one of them. I/O errors stay fatal —
/// nothing after them in the stream is trustworthy.
async fn ingest_jsonl<S: FactStore>(
    store: &S,
    input: &Path,
    run: &chronoscope_core::grammar::ids::IngesterRunId,
    recorded_at: chrono::DateTime<chrono::Utc>,
    limit: Option<u64>,
) -> Result<DumpTally> {
    use std::io::BufRead;

    use chronoscope_integrations::wikidata::WikidataEntity;

    const BATCH_SIZE: usize = 10_000;

    let file =
        std::fs::File::open(input).with_context(|| format!("opening {}", input.display()))?;
    let reader = std::io::BufReader::new(file);

    let mut tally = DumpTally::default();
    let mut batch: Vec<WikidataEntity> = Vec::new();
    let mut parsed: u64 = 0;

    for (i, line) in reader.lines().enumerate() {
        if limit.is_some_and(|n| parsed >= n) {
            break;
        }
        let line = match line {
            Ok(line) => line,
            // `read_line` reports `InvalidData` for exactly one thing — bytes
            // that aren't UTF-8 — and has already consumed through the newline,
            // so the rest of the dump still reads. A genuine I/O error arrives
            // under its own kind and stays fatal: nothing after it in the
            // stream is trustworthy.
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                tracing::warn!(line = i + 1, "ingestion: dump line dropped: {e}");
                tally.malformed_lines += 1;
                continue;
            }
            Err(e) => {
                return Err(anyhow::Error::new(e).context(format!(
                    "reading {} line {}",
                    input.display(),
                    i + 1
                )));
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let entity: WikidataEntity = match serde_json::from_str(&line) {
            Ok(entity) => entity,
            Err(e) => {
                tracing::warn!(line = i + 1, "ingestion: dump line dropped: {e}");
                tally.malformed_lines += 1;
                continue;
            }
        };
        batch.push(entity);
        parsed += 1;

        if batch.len() >= BATCH_SIZE {
            ingest_batch(store, run, recorded_at, &mut batch, &mut tally.entities).await?;
        }
    }
    ingest_batch(store, run, recorded_at, &mut batch, &mut tally.entities).await?;
    Ok(tally)
}

/// Submit one batch of parsed entities, draining `batch` and folding the
/// per-batch tally into the running total.
async fn ingest_batch<S: FactStore>(
    store: &S,
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    use chrono::TimeZone;
    use chronoscope_core::grammar::ids::{IngesterRunId, ValidatedStringError};
    use chronoscope_core::store::memory::MemoryFactStore;
    use chronoscope_integrations::wikidata::{
        RevisionId, SiteId, Sitelink, WikidataEntity, WikidataEntityType, WikidataId,
    };

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn run_id() -> Result<IngesterRunId, ValidatedStringError> {
        IngesterRunId::new("wikidata-test")
    }

    fn recorded_at()
    -> std::result::Result<chrono::DateTime<chrono::Utc>, Box<dyn std::error::Error>> {
        Ok(chrono::Utc
            .with_ymd_and_hms(2024, 1, 1, 12, 0, 0)
            .single()
            .ok_or("unambiguous timestamp")?)
    }

    /// A bare Q-item. It carries no claims, but its own QID is an external
    /// reference, so it still builds a commit.
    fn item(qid: &str) -> std::result::Result<WikidataEntity, Box<dyn std::error::Error>> {
        Ok(WikidataEntity {
            id: WikidataId::try_from(qid.to_owned())?,
            entity_type: WikidataEntityType::Item,
            lastrevid: RevisionId(100),
            labels: BTreeMap::new(),
            claims: BTreeMap::new(),
            sitelinks: BTreeMap::new(),
        })
    }

    /// An item carrying a sitelink title the fact store can't hold — the NUL
    /// costs that one reference, not the entity.
    fn item_with_an_unstorable_sitelink(
        qid: &str,
    ) -> std::result::Result<WikidataEntity, Box<dyn std::error::Error>> {
        let mut entity = item(qid)?;
        entity.sitelinks.insert(
            SiteId("enwiki".to_owned()),
            Sitelink {
                title: "Pan\u{0}theon".to_owned(),
            },
        );
        Ok(entity)
    }

    fn line(entity: &WikidataEntity) -> std::result::Result<String, Box<dyn std::error::Error>> {
        Ok(serde_json::to_string(entity)?)
    }

    /// Write a JSONL dump in a fresh temp dir. The dir goes back to the caller
    /// because dropping it deletes the file.
    fn dump(
        lines: &[&str],
    ) -> std::result::Result<(tempfile::TempDir, PathBuf), Box<dyn std::error::Error>> {
        dump_bytes(&lines.join("\n").into_bytes())
    }

    /// The same, for a dump whose bytes aren't all valid UTF-8.
    fn dump_bytes(
        bytes: &[u8],
    ) -> std::result::Result<(tempfile::TempDir, PathBuf), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("entities.jsonl");
        std::fs::write(&path, bytes)?;
        Ok((dir, path))
    }

    /// A line that isn't an entity costs that line and nothing else, and the
    /// run it happened in is not a build: an unattended derivation would
    /// otherwise hand downstream a DB that silently lost records.
    #[tokio::test]
    async fn a_malformed_line_spares_the_lines_after_it_and_fails_the_run_at_the_end() -> TestResult
    {
        let before = line(&item("Q1")?)?;
        let after = line(&item("Q2")?)?;
        let (_dir, path) = dump(&[&before, "{\"id\": truncated", &after])?;

        let store = MemoryFactStore::new();
        let tally = ingest_jsonl(&store, &path, &run_id()?, recorded_at()?, None).await?;

        assert_eq!(tally.malformed_lines, 1, "only the bad line is lost");
        assert_eq!(
            tally.entities.commits, 2,
            "the entities on either side of it commit"
        );

        let message = tally
            .into_stats()
            .err()
            .ok_or("a dropped line invalidates the build")?
            .to_string();
        assert!(
            message.contains("unparseable dump lines: 1"),
            "the failure counts the malformed lines, got: {message}"
        );
        Ok(())
    }

    /// A line of bytes that aren't UTF-8 never reaches the JSON parser, so it
    /// needs the same treatment one layer up: `read_line` has consumed through
    /// the newline, the lines after it are still readable, and the pass names
    /// every casualty before refusing the build.
    #[tokio::test]
    async fn a_non_utf8_line_spares_the_lines_after_it_and_fails_the_run_at_the_end() -> TestResult
    {
        let before = line(&item("Q1")?)?;
        let after = line(&item("Q2")?)?;
        let mut bytes = before.into_bytes();
        bytes.extend_from_slice(b"\n\xff\xfe not utf-8 \n");
        bytes.extend_from_slice(after.as_bytes());
        let (_dir, path) = dump_bytes(&bytes)?;

        let store = MemoryFactStore::new();
        let tally = ingest_jsonl(&store, &path, &run_id()?, recorded_at()?, None).await?;

        assert_eq!(tally.malformed_lines, 1, "only the bad line is lost");
        assert_eq!(
            tally.entities.commits, 2,
            "the entities on either side of it commit"
        );

        let message = tally
            .into_stats()
            .err()
            .ok_or("a dropped line invalidates the build")?
            .to_string();
        assert!(
            message.contains("unparseable dump lines: 1"),
            "the failure counts the malformed lines, got: {message}"
        );
        Ok(())
    }

    /// The build gate is about dump integrity, not data quality. An entity
    /// carrying a value the fact store can't hold loses that one reference,
    /// commits the rest, and leaves the run a valid build — otherwise a single
    /// bad title anywhere in a multi-million-item dump would block the artifact.
    #[tokio::test]
    async fn an_entity_with_an_unstorable_value_still_commits_and_keeps_the_build_valid()
    -> TestResult {
        let before = line(&item("Q1")?)?;
        let bad = line(&item_with_an_unstorable_sitelink("Q2")?)?;
        let after = line(&item("Q3")?)?;
        let (_dir, path) = dump(&[&before, &bad, &after])?;

        let store = MemoryFactStore::new();
        let tally = ingest_jsonl(&store, &path, &run_id()?, recorded_at()?, None).await?;

        assert_eq!(tally.malformed_lines, 0, "every line parsed");
        let stats = tally.into_stats()?;
        assert_eq!(stats.failed, 0, "a bad value is not a build failure");
        assert_eq!(stats.commits, 3, "every entity commits what it could");
        assert!(stats.issues > 0, "the dropped reference is tallied");
        Ok(())
    }

    #[tokio::test]
    async fn a_dump_with_nothing_wrong_ingests_every_line_and_yields_its_stats() -> TestResult {
        let lines = [
            line(&item("Q1")?)?,
            line(&item("Q2")?)?,
            line(&item("Q3")?)?,
        ];
        let (_dir, path) = dump(&lines.iter().map(String::as_str).collect::<Vec<_>>())?;

        let store = MemoryFactStore::new();
        let stats = ingest_jsonl(&store, &path, &run_id()?, recorded_at()?, None)
            .await?
            .into_stats()?;

        assert_eq!(stats.commits, 3, "every line committed");
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.skipped, 0);
        Ok(())
    }
}
