//! Python subprocess runner with content-addressed caching.
//!
//! Runs the SAM3 + DINOv3 pipeline via `run_local.py` and caches results
//! in content-addressed run directories keyed by pipeline code hash.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};
use tracing::info;

use crate::schema::AnalysisResult;

use super::CorpusError;

/// Cached results and per-image timings from a pipeline run.
struct RunCache {
    results: BTreeMap<String, AnalysisResult>,
    timings: BTreeMap<String, ImageTiming>,
}

/// Glob pattern for model files that determine the pipeline hash.
const MODEL_GLOB: &str = "triton/models/*/1/model.py";

/// Additional non-model files included in the pipeline hash.
const EXTRA_PIPELINE_FILES: &[&str] = &["triton/mock_triton.py", "triton/run_local.py"];

/// Truncated hash length for run directory names.
const HASH_PREFIX_LEN: usize = 12;

/// Directory for cached runs (relative to analysis dir).
const CORPUS_RUN_DIR: &str = "corpus/runs";

/// Metadata for a corpus run.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct RunMeta {
    pub pipeline_hash: String,
    pub created_at: String,
    pub timings: BTreeMap<String, ImageTiming>,
}

/// Per-image timing from the pipeline.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImageTiming {
    pub elapsed_ms: u64,
    pub image_size_bytes: u64,
}

/// Compute the pipeline hash from source files.
///
/// Discovers model files via glob and includes additional infrastructure files.
pub fn compute_pipeline_hash(analysis_dir: &Path) -> Result<String, CorpusError> {
    let mut hasher = Sha256::new();

    // Discover model files via glob.
    let pattern = analysis_dir.join(MODEL_GLOB).to_string_lossy().to_string();
    let mut model_paths: Vec<PathBuf> = glob::glob(&pattern)
        .map_err(|e| CorpusError::Pipeline(format!("invalid glob pattern: {e}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| CorpusError::Pipeline(format!("reading model directory: {e}")))?;
    model_paths.sort();

    if model_paths.is_empty() {
        return Err(CorpusError::Pipeline(format!(
            "no model files found matching {MODEL_GLOB}"
        )));
    }

    for path in &model_paths {
        let content = std::fs::read(path).map_err(CorpusError::Io)?;
        hasher.update(&content);
    }

    // Include extra files.
    for &file in EXTRA_PIPELINE_FILES {
        let path = analysis_dir.join(file);
        let content = std::fs::read(&path).map_err(CorpusError::Io)?;
        hasher.update(&content);
    }

    Ok(hex::encode(hasher.finalize()))
}

/// Get the run directory for a given pipeline hash.
fn run_dir(analysis_dir: &Path, hash: &str) -> Result<PathBuf, CorpusError> {
    let prefix = hash.get(..HASH_PREFIX_LEN).ok_or_else(|| {
        CorpusError::Pipeline(format!(
            "pipeline hash too short ({} chars, need {HASH_PREFIX_LEN})",
            hash.len()
        ))
    })?;
    Ok(analysis_dir.join(CORPUS_RUN_DIR).join(prefix))
}

/// Load individual result files from a run directory.
///
/// Reads `{composite_id}.json` files, skipping `_meta.json` and non-JSON files.
fn load_result_files(dir: &Path) -> Result<BTreeMap<String, AnalysisResult>, CorpusError> {
    let mut results = BTreeMap::new();
    if !dir.exists() {
        return Ok(results);
    }
    for entry in std::fs::read_dir(dir).map_err(CorpusError::Io)? {
        let entry = entry.map_err(CorpusError::Io)?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('_') || !name.ends_with(".json") {
            continue;
        }
        let composite_id = name.trim_end_matches(".json").to_string();
        let json = std::fs::read_to_string(entry.path()).map_err(CorpusError::Io)?;
        let result: AnalysisResult = serde_json::from_str(&json)
            .map_err(|e| CorpusError::Serialization(format!("{composite_id}: {e}")))?;
        results.insert(composite_id, result);
    }
    Ok(results)
}

/// Load cached results and timings from a run directory.
///
/// Returns empty maps if:
/// - The directory doesn't exist
/// - No `_meta.json` exists (no previous run — directory is cleared)
/// - The pipeline hash doesn't match (prefix collision — directory is cleared)
///
/// Partially-completed runs are handled gracefully: `_meta.json` is written
/// before processing starts, so a crash mid-run leaves a valid sentinel.
/// Individual result files are loaded and only missing images are re-run.
fn load_cache(dir: &Path, pipeline_hash: &str) -> Result<RunCache, CorpusError> {
    let meta_path = dir.join("_meta.json");

    if !meta_path.exists() {
        // No previous run or incomplete — start fresh.
        if dir.exists() {
            std::fs::remove_dir_all(dir).map_err(CorpusError::Io)?;
        }
        return Ok(RunCache {
            results: BTreeMap::new(),
            timings: BTreeMap::new(),
        });
    }

    let meta_json = std::fs::read_to_string(&meta_path).map_err(CorpusError::Io)?;
    let meta: RunMeta =
        serde_json::from_str(&meta_json).map_err(|e| CorpusError::Serialization(e.to_string()))?;

    // Verify the full hash matches (not just the prefix).
    if meta.pipeline_hash != pipeline_hash {
        return Err(CorpusError::Pipeline(format!(
            "pipeline hash prefix collision in {}: expected {pipeline_hash}, found {}",
            dir.display(),
            meta.pipeline_hash
        )));
    }

    let results = load_result_files(dir)?;
    info!(
        dir = %dir.display(),
        count = results.len(),
        "loaded cached corpus results"
    );
    Ok(RunCache {
        results,
        timings: meta.timings,
    })
}

/// Line from the Python runner's JSON output.
///
/// Must match the JSON structure in `triton/run_local.py`.
#[derive(Debug, serde::Deserialize)]
struct RunnerOutputLine {
    path: String,
    elapsed_ms: u64,
    result: AnalysisResult,
}

/// Build the path-to-id reverse index and stdin payload from image paths.
///
/// Validates UTF-8 once and builds both data structures in a single pass.
fn build_runner_input(
    image_paths: &BTreeMap<String, PathBuf>,
) -> Result<(BTreeMap<String, String>, String), CorpusError> {
    let mut path_to_id = BTreeMap::new();
    let mut stdin_data = String::new();

    for (i, (id, path)) in image_paths.iter().enumerate() {
        let path_str = path
            .to_str()
            .ok_or_else(|| CorpusError::Pipeline(format!("non-UTF-8 path: {}", path.display())))?;
        path_to_id.insert(path_str.to_string(), id.clone());
        if i > 0 {
            stdin_data.push('\n');
        }
        stdin_data.push_str(path_str);
    }

    Ok((path_to_id, stdin_data))
}

/// Spawn the Python runner subprocess and write image paths to its stdin.
fn spawn_runner(analysis_dir: &Path, stdin_data: &str) -> Result<std::process::Child, CorpusError> {
    let triton_dir = analysis_dir.join("triton");
    let venv_python = triton_dir.join(".venv/bin/python");

    let mut child = Command::new(&venv_python)
        .arg("run_local.py")
        .current_dir(&triton_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| {
            CorpusError::Pipeline(format!(
                "failed to spawn Python runner ({}): {e}",
                venv_python.display()
            ))
        })?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| CorpusError::Pipeline("failed to open stdin pipe".into()))?;
    stdin
        .write_all(stdin_data.as_bytes())
        .map_err(|e| CorpusError::Pipeline(format!("writing to stdin: {e}")))?;
    drop(stdin);

    Ok(child)
}

/// Read results from the runner's stdout and write each to the output directory.
fn collect_results(
    reader: impl std::io::BufRead,
    path_to_id: &BTreeMap<String, String>,
    out_dir: &Path,
) -> Result<RunCache, CorpusError> {
    let mut results = BTreeMap::new();
    let mut timings = BTreeMap::new();

    for line_result in reader.lines() {
        let line = line_result
            .map_err(|e| CorpusError::Pipeline(format!("reading runner stdout: {e}")))?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parsed: RunnerOutputLine = serde_json::from_str(line).map_err(|e| {
            CorpusError::Serialization(format!("parsing runner output: {e}\n  line: {line:.200}"))
        })?;

        let composite_id = path_to_id
            .get(&parsed.path)
            .ok_or_else(|| {
                CorpusError::Pipeline(format!(
                    "unknown path in runner output: {} (expected one of {} known paths)",
                    parsed.path,
                    path_to_id.len()
                ))
            })?
            .clone();

        // Get file size for timing metadata.
        let file_size = std::fs::metadata(&parsed.path)
            .map(|m| m.len())
            .unwrap_or(0);

        timings.insert(
            composite_id.clone(),
            ImageTiming {
                elapsed_ms: parsed.elapsed_ms,
                image_size_bytes: file_size,
            },
        );

        // Write individual result to temp dir.
        let result_json = serde_json::to_string_pretty(&parsed.result)
            .map_err(|e| CorpusError::Serialization(e.to_string()))?;
        std::fs::write(
            out_dir.join(format!("{composite_id}.json")),
            result_json.as_bytes(),
        )
        .map_err(CorpusError::Io)?;

        results.insert(composite_id, parsed.result);
    }

    Ok(RunCache { results, timings })
}

/// Write (or update) run metadata in the run directory.
fn write_meta(
    dir: &Path,
    pipeline_hash: &str,
    timings: &BTreeMap<String, ImageTiming>,
) -> Result<(), CorpusError> {
    let meta = RunMeta {
        pipeline_hash: pipeline_hash.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        timings: timings.clone(),
    };
    let meta_json = serde_json::to_string_pretty(&meta)
        .map_err(|e| CorpusError::Serialization(e.to_string()))?;
    std::fs::write(dir.join("_meta.json"), meta_json.as_bytes()).map_err(CorpusError::Io)?;
    Ok(())
}

/// Run the Python pipeline on images not yet cached, returning all results.
///
/// Loads existing cached results from the run directory, determines which
/// requested images are missing, and runs only those through the Python
/// pipeline. New results are written directly to the run directory.
///
/// `image_paths` maps composite IDs to file paths.
pub fn run_pipeline(
    analysis_dir: &Path,
    pipeline_hash: &str,
    image_paths: &BTreeMap<String, PathBuf>,
) -> Result<BTreeMap<String, AnalysisResult>, CorpusError> {
    let dir = run_dir(analysis_dir, pipeline_hash)?;

    // Load existing cache (clears dir on hash mismatch or incomplete state).
    let RunCache {
        mut results,
        mut timings,
    } = load_cache(&dir, pipeline_hash)?;

    // Find images that aren't cached yet.
    let missing: BTreeMap<String, PathBuf> = image_paths
        .iter()
        .filter(|(id, _)| !results.contains_key(*id))
        .map(|(id, path)| (id.clone(), path.clone()))
        .collect();

    if missing.is_empty() {
        info!(count = results.len(), "all corpus images cached");
        return Ok(results);
    }

    info!(
        cached = results.len(),
        missing = missing.len(),
        total = image_paths.len(),
        "running Python pipeline on missing corpus images"
    );

    std::fs::create_dir_all(&dir).map_err(CorpusError::Io)?;

    // Write sentinel _meta.json before processing so that a crash mid-run
    // still leaves a valid cache directory. On next load, load_cache finds
    // the sentinel, verifies the hash, loads whatever result files exist,
    // and run_pipeline only re-runs the missing images.
    write_meta(&dir, pipeline_hash, &timings)?;

    let (path_to_id, stdin_data) = build_runner_input(&missing)?;
    let mut child = spawn_runner(analysis_dir, &stdin_data)?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| CorpusError::Pipeline("failed to open stdout pipe".into()))?;
    let new = collect_results(std::io::BufReader::new(child_stdout), &path_to_id, &dir)?;

    // Wait for the process to finish.
    let status = child
        .wait()
        .map_err(|e| CorpusError::Pipeline(format!("waiting for Python runner: {e}")))?;
    if !status.success() {
        return Err(CorpusError::Pipeline(format!(
            "Python runner exited with status {status}"
        )));
    }

    // Merge new results into cache.
    let new_count = new.results.len();
    results.extend(new.results);
    timings.extend(new.timings);

    // Update _meta.json with final timings.
    write_meta(&dir, pipeline_hash, &timings)?;

    info!(
        dir = %dir.display(),
        new = new_count,
        total = results.len(),
        "cached corpus results"
    );

    Ok(results)
}
