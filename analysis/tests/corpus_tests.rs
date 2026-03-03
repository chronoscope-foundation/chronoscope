//! Parametrized corpus test harness using libtest-mimic.
//!
//! Generates one test per image, plus one or two per cluster (intra, and
//! similar when declared). Each trial calls the per-entity check function
//! directly — no global check + partition step.
//!
//! Tests with `known_issue` annotations always execute but handle results
//! differently: expected failures pass silently, while resolved issues
//! fail the test to prompt annotation removal.

#![deny(clippy::unwrap_used)]
#![deny(unsafe_code)]

use std::fmt::Write as _;
use std::process::ExitCode;

use libtest_mimic::{Arguments, Trial};

use chronoscope_analysis::corpus::CorpusFixture;
use chronoscope_analysis::corpus::assertions::{
    self, CheckedImageReport, ClusterReport, Thresholds,
};
use chronoscope_analysis::corpus::manifest::ImageEntry;

fn main() -> ExitCode {
    let args = Arguments::from_args();

    // Load fixture once, leak to &'static for Send + 'static closures.
    // The process exits after tests run, so this is not a real leak.
    let fixture: &'static CorpusFixture = match CorpusFixture::load() {
        Ok(f) => Box::leak(Box::new(f)),
        Err(e) => {
            eprintln!("Failed to load corpus: {e}");
            return ExitCode::FAILURE;
        }
    };

    let manifest = fixture.manifest();

    // Pre-compute shared data for cluster checks.
    let all_embeddings: &'static std::collections::HashMap<
        String,
        chronoscope_analysis::Embedding,
    > = Box::leak(Box::new(fixture.all_addressable_embeddings()));
    let thresholds: &'static Thresholds = Box::leak(Box::new(Thresholds::default()));

    let mut trials = Vec::new();

    // Image trials: one per image.
    for (id, entry) in &manifest.images {
        let id: &'static str = Box::leak(id.clone().into_boxed_str());
        let known_issue = match entry {
            ImageEntry::Single { known_issue, .. } | ImageEntry::Composite { known_issue, .. } => {
                known_issue.clone()
            }
        };

        trials.push(make_trial(format!("image::{id}"), known_issue, move || {
            let entry = &fixture.manifest().images[id];
            match assertions::check_image(id, entry, fixture, thresholds) {
                Err(reason) => Some(format!("  ImageRejected: {reason}\n")),
                Ok(report) if !report.is_ok() => Some(format_checked_report(&report)),
                Ok(_) => None,
            }
        }));
    }

    // Cluster trials: intra (always) + similar (when declared).
    // Each cluster's report is computed once and shared between intra/similar trials.
    for (name, cluster) in &manifest.clusters {
        let related: Vec<(String, chronoscope_analysis::corpus::manifest::ClusterEntry)> = cluster
            .similar
            .iter()
            .filter_map(|similar_name| {
                manifest
                    .clusters
                    .get(similar_name)
                    .map(|c| (similar_name.clone(), c.clone()))
            })
            .collect();

        let report: &'static ClusterReport = Box::leak(Box::new(assertions::check_cluster(
            cluster,
            all_embeddings,
            &related,
            thresholds,
        )));

        // Intra trial.
        trials.push(make_trial(
            format!("cluster::{name}::intra"),
            cluster.known_issue_intra.clone(),
            move || {
                if report.is_intra_ok() {
                    None
                } else {
                    Some(format_cluster_intra(report))
                }
            },
        ));

        // Similar trial (only when declared).
        if !cluster.similar.is_empty() {
            trials.push(make_trial(
                format!("cluster::{name}::similar"),
                cluster.known_issue_similar.clone(),
                move || {
                    if report.is_similar_ok() {
                        None
                    } else {
                        Some(format_cluster_similar(report))
                    }
                },
            ));
        }
    }

    let conclusion = libtest_mimic::run(&args, trials);

    print_known_issues(manifest);

    conclusion.exit_code()
}

// ==================== Trial Construction ====================

/// Build a trial that handles `known_issue` annotations.
///
/// `check` returns `None` if the check passed, or `Some(message)` if it failed.
///
/// Tests with `known_issue` annotations always execute but handle results
/// differently: expected failures pass silently, while resolved issues
/// fail the test to prompt annotation removal.
///
/// - No known issue + passes -> Ok
/// - No known issue + fails -> Err (real failure)
/// - Known issue + still fails -> Ok (expected, passes silently)
/// - Known issue + now passes -> Err (stale annotation, forces removal)
fn make_trial(
    name: String,
    known_issue: Option<String>,
    check: impl Fn() -> Option<String> + Send + 'static,
) -> Trial {
    Trial::test(name, move || {
        let failure = check();
        match (known_issue.as_ref(), failure) {
            (None, None) => Ok(()),
            (None, Some(msg)) => Err(msg.into()),
            (Some(_), Some(_)) => Ok(()),
            (Some(reason), None) => Err(format!(
                "known issue appears resolved — remove known_issue from the corpus manifest\n  \
                 was: {reason}"
            )
            .into()),
        }
    })
}

// ==================== Formatting ====================

fn format_checked_report(report: &CheckedImageReport) -> String {
    let mut out = String::new();
    for idx in &report.segmentation {
        let _ = writeln!(out, "  subimage [{idx}]: not segmented");
    }
    if let Some(sc) = &report.subimage_count {
        let _ = writeln!(
            out,
            "  subimage count: expected {}, got {} (layout: {})",
            sc.expected, sc.actual, sc.layout
        );
    }
    for (idx, rc) in &report.region_counts {
        let _ = writeln!(
            out,
            "  subimage [{idx}]: expected {} regions, got {}",
            rc.expected, rc.actual
        );
    }
    for sv in &report.composite_similarity {
        let _ = writeln!(out, "  {}", sv.message);
    }
    out
}

fn format_cluster_intra(report: &ClusterReport) -> String {
    let mut out = String::new();
    for m in &report.member_missing {
        let _ = writeln!(out, "  member not found: {}", m.addr);
    }
    for f in &report.intra {
        let _ = writeln!(
            out,
            "  {} vs {}: similarity {:.4} < {:.4}",
            f.addr_a, f.addr_b, f.similarity, f.threshold
        );
    }
    out
}

fn format_cluster_similar(report: &ClusterReport) -> String {
    let mut out = String::new();
    for f in &report.similar {
        let _ = writeln!(
            out,
            "  {} vs {} (cluster {}): similarity {:.4} < {:.4}",
            f.addr_a, f.addr_b, f.other_cluster, f.similarity, f.threshold
        );
    }
    out
}

// ==================== Post-run Summary ====================

/// Print a manifest-only summary of known issues (no re-computation).
///
/// The `make_trial` ratchet handles pass/fail semantics; this just provides
/// a consolidated reference at the end of the test run.
fn print_known_issues(manifest: &chronoscope_analysis::corpus::manifest::CorpusManifest) {
    let mut known: Vec<(String, String)> = Vec::new();

    for (id, entry) in &manifest.images {
        let issue = match entry {
            ImageEntry::Single { known_issue, .. } | ImageEntry::Composite { known_issue, .. } => {
                known_issue.as_deref()
            }
        };
        if let Some(reason) = issue {
            known.push((format!("image::{id}"), reason.to_string()));
        }
    }
    for (name, cluster) in &manifest.clusters {
        if let Some(reason) = &cluster.known_issue_intra {
            known.push((format!("cluster::{name}::intra"), reason.clone()));
        }
        if let Some(reason) = &cluster.known_issue_similar {
            known.push((format!("cluster::{name}::similar"), reason.clone()));
        }
    }

    if !known.is_empty() {
        eprintln!("\nKnown issues ({}):", known.len());
        for (test_name, reason) in &known {
            eprintln!("  {test_name}: {reason}");
        }
    }
}
