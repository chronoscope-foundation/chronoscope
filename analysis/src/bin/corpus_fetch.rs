//! Corpus image fetcher with rate-limited downloads.
//!
//! Two modes:
//!
//! 1. **Single URL** (for Nix FODs):
//!    `corpus-fetch <url> <output-dir>`
//!    Downloads content to numbered files (0, 1, 2, ...).
//!
//! 2. **Hash generation** (for corpus-hashes.json):
//!    `corpus-fetch hash`
//!    Iterates over unique URLs in the corpus manifest, downloads each to a
//!    temp directory, computes the recursive NAR hash via `nix hash path`,
//!    and writes/updates corpus-hashes.json. Skips already-hashed URLs.
//!
//! Requires the `corpus-test` feature.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Arc;

use url::Url;

use chronoscope_integrations::{HttpClient, ReqwestClient};

use chronoscope_analysis::corpus::{CorpusError, CorpusManifest, ImageDownloader, manifest_path};

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::level_filters::LevelFilter::INFO.into()),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().collect();

    if args.len() == 2 && args[1] == "hash" {
        return run_hash();
    }

    if args.len() == 3 && args[1] != "hash" {
        return run_fetch(&args[1], &args[2]);
    }

    eprintln!("Usage:");
    eprintln!("  corpus-fetch <url> <output-dir>   Download a single URL (for Nix FODs)");
    eprintln!("  corpus-fetch hash                  Generate/update corpus-hashes.json");
    ExitCode::FAILURE
}

/// Single-URL download mode (used inside Nix FODs).
fn run_fetch(url_str: &str, output_dir: &str) -> ExitCode {
    let url = match Url::parse(url_str) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("Invalid URL '{url_str}': {e}");
            return ExitCode::FAILURE;
        }
    };

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("Failed to create tokio runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    let http: Arc<dyn HttpClient> = match ReqwestClient::new() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("Failed to create HTTP client: {e}");
            return ExitCode::FAILURE;
        }
    };

    let downloader = ImageDownloader::new(http);

    match rt.block_on(downloader.download_fod(&url, Path::new(output_dir))) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Hash generation mode: download all unique URLs and compute NAR hashes.
fn run_hash() -> ExitCode {
    let manifest_path = match manifest_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let manifest = match CorpusManifest::load(&manifest_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!(
                "Failed to load manifest from {}: {e}",
                manifest_path.display()
            );
            return ExitCode::FAILURE;
        }
    };

    // Output path is always in the analysis crate directory (not next to CORPUS_MANIFEST,
    // which may point into the read-only Nix store).
    let hashes_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("corpus-hashes.json");

    // Load existing hashes.
    let mut hashes: BTreeMap<String, String> = if hashes_path.exists() {
        match std::fs::read_to_string(&hashes_path) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(
                        "Failed to parse {}: {e}, starting fresh",
                        hashes_path.display()
                    );
                    BTreeMap::new()
                }
            },
            Err(e) => {
                tracing::warn!(
                    "Failed to read {}: {e}, starting fresh",
                    hashes_path.display()
                );
                BTreeMap::new()
            }
        }
    } else {
        BTreeMap::new()
    };

    let unique_urls = manifest.unique_urls();
    let missing: Vec<&str> = unique_urls
        .iter()
        .filter(|url| !hashes.contains_key(url.as_str()))
        .map(|s| s.as_str())
        .collect();

    eprintln!(
        "{} unique URLs: {} already hashed, {} to fetch",
        unique_urls.len(),
        unique_urls.len() - missing.len(),
        missing.len()
    );

    if missing.is_empty() {
        eprintln!("Nothing to do.");
        return ExitCode::SUCCESS;
    }

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("Failed to create tokio runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    let http: Arc<dyn HttpClient> = match ReqwestClient::new() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("Failed to create HTTP client: {e}");
            return ExitCode::FAILURE;
        }
    };

    let downloader = ImageDownloader::new(http);
    let mut count = 0;
    let mut failed = 0;

    for url_str in &missing {
        let url = match Url::parse(url_str) {
            Ok(u) => u,
            Err(e) => {
                eprintln!("  SKIP (invalid URL): {url_str}: {e}");
                failed += 1;
                continue;
            }
        };

        let tmpdir = match tempfile::tempdir() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("  SKIP (tmpdir): {e}");
                failed += 1;
                continue;
            }
        };

        match rt.block_on(downloader.download_fod(&url, tmpdir.path())) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("  FAILED: {url_str}: {e}");
                failed += 1;
                continue;
            }
        }

        // Compute recursive NAR hash matching Nix FOD outputHashMode = "recursive".
        match nix_hash_path(tmpdir.path()) {
            Ok(sri_hash) => {
                eprintln!("  OK: {url_str} -> {sri_hash}");
                hashes.insert(url_str.to_string(), sri_hash);
                count += 1;
            }
            Err(e) => {
                eprintln!("  FAILED (hash): {url_str}: {e}");
                failed += 1;
            }
        }
    }

    // Write sorted output.
    match write_hashes(&hashes_path, &hashes) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("Failed to write {}: {e}", hashes_path.display());
            return ExitCode::FAILURE;
        }
    }

    eprintln!(
        "Done: {count} new, {} existing, {failed} failed. Wrote {}",
        unique_urls.len() - missing.len(),
        hashes_path.display()
    );

    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Compute SRI hash via `nix hash path`, matching FOD `outputHashMode = "recursive"`.
fn nix_hash_path(path: &Path) -> Result<String, CorpusError> {
    let output = Command::new("nix")
        .args(["hash", "path", "--sri", "--type", "sha256"])
        .arg(path)
        .output()
        .map_err(|e| CorpusError::Pipeline(format!("failed to run `nix hash path`: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CorpusError::Pipeline(format!(
            "`nix hash path` failed: {stderr}"
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Write hashes as sorted JSON.
fn write_hashes(path: &Path, hashes: &BTreeMap<String, String>) -> Result<(), CorpusError> {
    let json = serde_json::to_string_pretty(hashes)
        .map_err(|e| CorpusError::Serialization(e.to_string()))?;
    std::fs::write(path, json + "\n").map_err(CorpusError::Io)
}
