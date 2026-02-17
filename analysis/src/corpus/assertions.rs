//! Structured assertion checks for corpus manifest vs pipeline results.
//!
//! Checks are organized per-entity: `check_image` for a single image,
//! `check_cluster` for a single cluster. Each returns a typed report
//! with per-check-category failure structs.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;

use crate::schema::{AnalysisResult, Embedding, SubimageAnalysis};

use super::CorpusFixture;
use super::manifest::{ClusterEntry, CompositeLayout, ImageEntry};

// ==================== Thresholds ====================

/// Configurable thresholds for similarity checks.
#[derive(Debug)]
pub struct Thresholds {
    pub min_composite_similar: f32,
    pub max_composite_dissimilar: f32,
    pub min_intra_cluster: f32,
    pub min_similar_cluster: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            min_composite_similar: 0.7,
            max_composite_dissimilar: 0.5,
            min_intra_cluster: 0.5,
            min_similar_cluster: 0.3,
        }
    }
}

// ==================== Image failure types ====================

/// Not enough subimages detected in a composite.
#[derive(Debug, Clone, Serialize)]
pub struct SubimageCountMismatch {
    pub expected: usize,
    pub actual: usize,
    pub layout: CompositeLayout,
}

/// Region count doesn't match manifest expectation.
#[derive(Debug, Clone, Serialize)]
pub struct RegionCountMismatch {
    pub expected: u32,
    pub actual: usize,
}

/// Composite subimage pair similarity violated an assertion.
#[derive(Debug, Clone, Serialize)]
pub struct SimilarityViolation {
    pub subimage_a: usize,
    pub subimage_b: usize,
    pub similarity: f32,
    pub expected_similar: bool,
    pub message: String,
}

// ==================== Image report ====================

/// Per-subimage check results for a successful pipeline run.
#[derive(Debug, Clone, Serialize)]
pub struct CheckedImageReport {
    /// Subimage indices that were not segmented (e.g. `Rejected`, `Error`).
    pub segmentation: BTreeSet<usize>,
    /// Subimage count mismatch (composites only).
    pub subimage_count: Option<SubimageCountMismatch>,
    /// Region count mismatches, keyed by subimage index.
    pub region_counts: BTreeMap<usize, RegionCountMismatch>,
    /// Composite similarity violations.
    pub composite_similarity: Vec<SimilarityViolation>,
}

impl CheckedImageReport {
    /// True if all checks passed.
    pub fn is_ok(&self) -> bool {
        self.segmentation.is_empty()
            && self.subimage_count.is_none()
            && self.region_counts.is_empty()
            && self.composite_similarity.is_empty()
    }
}

// ==================== Cluster failure types ====================

/// An intra-cluster pair has similarity below the threshold.
#[derive(Debug, Clone, Serialize)]
pub struct IntraClusterFailure {
    pub addr_a: String,
    pub addr_b: String,
    pub similarity: f32,
    pub threshold: f32,
}

/// A cluster member address could not be resolved to an embedding.
#[derive(Debug, Clone, Serialize)]
pub struct MemberMissing {
    pub addr: String,
}

/// A cross-cluster pair (declared similar) has similarity below the threshold.
#[derive(Debug, Clone, Serialize)]
pub struct SimilarClusterFailure {
    pub other_cluster: String,
    pub addr_a: String,
    pub addr_b: String,
    pub similarity: f32,
    pub threshold: f32,
}

// ==================== Cluster report ====================

/// All check results for a single cluster.
#[derive(Debug, Clone, Serialize)]
pub struct ClusterReport {
    pub member_missing: Vec<MemberMissing>,
    pub intra: Vec<IntraClusterFailure>,
    pub similar: Vec<SimilarClusterFailure>,
}

impl ClusterReport {
    pub fn is_intra_ok(&self) -> bool {
        self.member_missing.is_empty() && self.intra.is_empty()
    }

    pub fn is_similar_ok(&self) -> bool {
        self.similar.is_empty()
    }
}

// ==================== Utilities ====================

/// Cosine similarity between two embeddings.
///
/// Since `Embedding` is L2-normalized on construction, this is just the dot product.
pub fn cosine_similarity(a: &Embedding, b: &Embedding) -> f32 {
    a.as_slice()
        .iter()
        .zip(b.as_slice())
        .map(|(x, y)| x * y)
        .sum()
}

/// Build expected region counts for an image entry.
///
/// Returns only subimage indices with explicit assertions in the manifest.
fn region_expectations(entry: &ImageEntry) -> BTreeMap<usize, u32> {
    match entry {
        ImageEntry::Composite {
            count, subimages, ..
        } => (0..count.get() as usize)
            .filter_map(|idx| {
                subimages
                    .get(&idx)
                    .and_then(|a| a.regions)
                    .map(|r| (idx, r))
            })
            .collect(),
        ImageEntry::Single { regions, .. } => regions
            .map(|r| BTreeMap::from([(0, r)]))
            .unwrap_or_default(),
    }
}

// ==================== Per-image checks ====================

/// Run all checks for a single image.
///
/// Returns `Err` if the pipeline rejected the image (with the rejection reason).
/// Returns `Ok(report)` for successful results with per-subimage check findings.
///
/// # Panics
///
/// Panics if the fixture has no result for this image (this indicates a bug
/// in the test infrastructure, not a per-image failure).
pub fn check_image(
    id: &str,
    entry: &ImageEntry,
    fixture: &CorpusFixture,
    t: &Thresholds,
) -> Result<CheckedImageReport, String> {
    let result = fixture
        .result(id)
        .ok_or_else(|| format!("no result for image '{id}' in corpus fixture"))?;

    let subimages = match result {
        AnalysisResult::Success { subimages, .. } => subimages,
        AnalysisResult::ImageRejected { reason } => return Err(reason.clone()),
    };

    let mut report = CheckedImageReport {
        segmentation: BTreeSet::new(),
        subimage_count: None,
        region_counts: BTreeMap::new(),
        composite_similarity: Vec::new(),
    };

    // Per-subimage segmentation check.
    for (i, sub) in subimages.iter().enumerate() {
        if !matches!(
            sub.analysis,
            SubimageAnalysis::Segmented { .. } | SubimageAnalysis::Analyzed { .. }
        ) {
            report.segmentation.insert(i);
        }
    }

    // Subimage count (composites only).
    if let ImageEntry::Composite {
        layout,
        count,
        expect_similar,
        ..
    } = entry
    {
        let expected = count.get() as usize;
        if subimages.len() < expected {
            report.subimage_count = Some(SubimageCountMismatch {
                expected,
                actual: subimages.len(),
                layout: *layout,
            });
        }

        // Composite similarity (2+ subimages with embeddings).
        if subimages.len() >= 2 {
            let embedded: Vec<(usize, &Embedding)> = subimages
                .iter()
                .enumerate()
                .filter_map(|(i, sub)| sub.analysis.embedding().map(|e| (i, e)))
                .collect();

            check_composite_similarity(&mut report, &embedded, *expect_similar, t);
        }
    }

    // Region counts (only for subimages with manifest assertions).
    let expectations = region_expectations(entry);
    for (sub_idx, expected_count) in expectations {
        let Some(sub) = subimages.get(sub_idx) else {
            continue;
        };
        let Some(regions) = sub.analysis.regions() else {
            continue;
        };
        if regions.len() != expected_count as usize {
            report.region_counts.insert(
                sub_idx,
                RegionCountMismatch {
                    expected: expected_count,
                    actual: regions.len(),
                },
            );
        }
    }

    Ok(report)
}

/// Check pairwise embedding similarity for composite subimages.
fn check_composite_similarity(
    report: &mut CheckedImageReport,
    embedded: &[(usize, &Embedding)],
    expect_similar: bool,
    t: &Thresholds,
) {
    #[allow(clippy::type_complexity)]
    let (threshold, is_violation, relation, label): (f32, fn(f32, f32) -> bool, &str, &str) =
        if expect_similar {
            (
                t.min_composite_similar,
                |s, t| s < t,
                "<",
                "expected similar",
            )
        } else {
            (
                t.max_composite_dissimilar,
                |s, t| s >= t,
                ">=",
                "expected dissimilar",
            )
        };

    for a in 0..embedded.len() {
        for b in (a + 1)..embedded.len() {
            let (i, emb_a) = embedded[a];
            let (j, emb_b) = embedded[b];
            let sim = cosine_similarity(emb_a, emb_b);

            if is_violation(sim, threshold) {
                report.composite_similarity.push(SimilarityViolation {
                    subimage_a: i,
                    subimage_b: j,
                    similarity: sim,
                    expected_similar: expect_similar,
                    message: format!(
                        "subimages [{i}] vs [{j}]: similarity {sim:.4} {relation} {threshold} ({label})"
                    ),
                });
            }
        }
    }
}

// ==================== Per-cluster checks ====================

/// Run all checks for a single cluster.
///
/// `all_embeddings` should come from `fixture.all_addressable_embeddings()` —
/// passed in so it can be computed once and shared across clusters.
pub fn check_cluster(
    cluster: &ClusterEntry,
    all_embeddings: &HashMap<String, Embedding>,
    related_clusters: &[(String, ClusterEntry)],
    t: &Thresholds,
) -> ClusterReport {
    let mut report = ClusterReport {
        member_missing: Vec::new(),
        intra: Vec::new(),
        similar: Vec::new(),
    };

    // Resolve members.
    let mut members: Vec<(&str, &Embedding)> = Vec::new();
    for addr in &cluster.members {
        match all_embeddings.get(addr.as_str()) {
            Some(e) => members.push((addr, e)),
            None => {
                report
                    .member_missing
                    .push(MemberMissing { addr: addr.clone() });
            }
        }
    }

    // Intra-cluster: all pairs >= threshold.
    for i in 0..members.len() {
        for j in (i + 1)..members.len() {
            let sim = cosine_similarity(members[i].1, members[j].1);
            if sim < t.min_intra_cluster {
                report.intra.push(IntraClusterFailure {
                    addr_a: members[i].0.to_string(),
                    addr_b: members[j].0.to_string(),
                    similarity: sim,
                    threshold: t.min_intra_cluster,
                });
            }
        }
    }

    // Similar-cluster cross-checks.
    for (similar_name, similar_cluster) in related_clusters {
        for m1 in &cluster.members {
            let Some(emb1) = all_embeddings.get(m1.as_str()) else {
                continue;
            };
            for m2 in &similar_cluster.members {
                let Some(emb2) = all_embeddings.get(m2.as_str()) else {
                    continue;
                };
                let sim = cosine_similarity(emb1, emb2);
                if sim < t.min_similar_cluster {
                    report.similar.push(SimilarClusterFailure {
                        other_cluster: similar_name.clone(),
                        addr_a: m1.clone(),
                        addr_b: m2.clone(),
                        similarity: sim,
                        threshold: t.min_similar_cluster,
                    });
                }
            }
        }
    }

    report
}
