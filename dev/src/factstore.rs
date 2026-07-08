//! Curated-JSONL fact store loader.
//!
//! Dev/test fixture concern: reads the curated Wikidata `entities.jsonl`
//! snapshot (one [`WikidataEntity`] per line) and ingests it into a fresh
//! [`MemoryFactStore`]. The production API server has no such data source —
//! it starts with an empty store (see `chronoscope-api`'s `main.rs`) — so
//! this loader lives here rather than in `chronoscope-core` or
//! `chronoscope-api`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};
use chronoscope_core::grammar::ids::IngesterRunId;
use chronoscope_core::store::memory::MemoryFactStore;
use chronoscope_ingestion::wikidata::commits::{IngestError, IngestStats, ingest_entities};
use chronoscope_integrations::wikidata::WikidataEntity;

/// Errors loading the curated fact store.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// The JSONL file couldn't be read.
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A line didn't parse as a [`WikidataEntity`].
    #[error("parsing {path} line {line}: {source}")]
    Parse {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    /// The fixed recorded-at timestamp failed to construct. Practically
    /// unreachable — the literal date/time below is valid — but `Utc`'s
    /// `with_ymd_and_hms` returns a fallible `LocalResult`, so the failure
    /// path is threaded through rather than assumed away.
    #[error("curated snapshot recorded-at timestamp is invalid")]
    RecordedAt,
    /// The store rejected a curated entity. A bad curated entity is a
    /// fixture bug — this is fatal, not skipped.
    #[error("ingesting curated entities: {0}")]
    Ingest(#[from] IngestError),
}

/// Read `jsonl_path`, parse each non-empty line as a [`WikidataEntity`], and
/// ingest all of them into a fresh [`MemoryFactStore`] under one ingester
/// run. Returns the populated store alongside the ingest tally so the caller
/// can log it.
///
/// Every entity is recorded under the same fixed timestamp — the curated
/// snapshot's own date, not the load time — so re-loading the same snapshot
/// content-addresses to the same `CommitId`s.
pub async fn load_curated_fact_store(
    jsonl_path: &Path,
) -> Result<(MemoryFactStore, IngestStats), LoadError> {
    let content = tokio::fs::read_to_string(jsonl_path)
        .await
        .map_err(|source| LoadError::Read {
            path: jsonl_path.to_path_buf(),
            source,
        })?;

    let mut entities = Vec::new();
    for (i, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entity: WikidataEntity =
            serde_json::from_str(line).map_err(|source| LoadError::Parse {
                path: jsonl_path.to_path_buf(),
                line: i + 1,
                source,
            })?;
        entities.push(entity);
    }

    let recorded_at = curated_snapshot_recorded_at()?;
    let run = IngesterRunId::new("dev-startup");
    let store = MemoryFactStore::new();
    let stats = ingest_entities(&store, entities, &run, recorded_at).await?;
    Ok((store, stats))
}

/// The curated snapshot's pinned date: 2022-01-03T00:00:00Z. Matches the
/// timestamp `ingestion/tests/real_entity_ingest.rs` uses for the same
/// `entities.jsonl` snapshot.
fn curated_snapshot_recorded_at() -> Result<DateTime<Utc>, LoadError> {
    Utc.with_ymd_and_hms(2022, 1, 3, 0, 0, 0)
        .single()
        .ok_or(LoadError::RecordedAt)
}
