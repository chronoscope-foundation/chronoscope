//! Pre-computed analysis result loading.
//!
//! Reads JSONL output produced by the Nix `analysis-results` derivation
//! (Python SAM3 + DINOv3 pipeline). The Nix store path provides content-
//! addressing — no separate pipeline hash needed.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::Path;

use tracing::info;

use crate::schema::AnalysisResult;

use super::CorpusError;

/// Line from the Python runner's JSON output.
///
/// Must match the JSON structure in `triton/run_local.py`.
#[derive(Debug, serde::Deserialize)]
struct RunnerOutputLine {
    path: String,
    result: AnalysisResult,
}

/// Load pre-computed analysis results from a JSONL file.
///
/// Each line is a `RunnerOutputLine`. The entry ID is extracted from the
/// path basename (e.g., `images/schwerin-palace.0` → `schwerin-palace.0`).
pub fn load_results_jsonl(path: &Path) -> Result<BTreeMap<String, AnalysisResult>, CorpusError> {
    let file = std::fs::File::open(path).map_err(CorpusError::Io)?;
    let reader = std::io::BufReader::new(file);
    let mut results = BTreeMap::new();

    for line_result in reader.lines() {
        let line = line_result.map_err(CorpusError::Io)?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parsed: RunnerOutputLine = serde_json::from_str(line).map_err(|e| {
            CorpusError::Serialization(format!("parsing results JSONL: {e}\n  line: {line:.200}"))
        })?;
        let id = Path::new(&parsed.path)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                CorpusError::Pipeline(format!("no filename in result path: {}", parsed.path))
            })?
            .to_string();
        results.insert(id, parsed.result);
    }

    info!(count = results.len(), path = %path.display(), "loaded pre-computed results");
    Ok(results)
}
