//! JSON manifest parsing for corpus test suites.

use std::collections::{BTreeMap, HashSet};
use std::num::NonZeroU32;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::CorpusError;

/// Deserialize a `BTreeMap<usize, V>` from JSON objects with string keys.
///
/// JSON object keys are always strings, so `{"0": ...}` needs string-to-usize
/// conversion.
fn deserialize_usize_key_map<'de, D, V>(deserializer: D) -> Result<BTreeMap<usize, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    V: Deserialize<'de>,
{
    let string_map = BTreeMap::<String, V>::deserialize(deserializer)?;
    string_map
        .into_iter()
        .map(|(k, v)| {
            k.parse::<usize>()
                .map(|idx| (idx, v))
                .map_err(|_| serde::de::Error::custom(format!("invalid usize key: {k}")))
        })
        .collect()
}

/// Serialize a `BTreeMap<usize, V>` to JSON objects with string keys.
fn serialize_usize_key_map<S, V>(map: &BTreeMap<usize, V>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
    V: Serialize,
{
    use serde::ser::SerializeMap;
    let mut m = serializer.serialize_map(Some(map.len()))?;
    for (k, v) in map {
        m.serialize_entry(&k.to_string(), v)?;
    }
    m.end()
}

/// Top-level corpus manifest parsed from `corpus.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct CorpusManifest {
    pub images: BTreeMap<String, ImageEntry>,
    #[serde(default)]
    pub clusters: BTreeMap<String, ClusterEntry>,
}

/// Layout of a composite image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositeLayout {
    SideBySide,
    Vertical,
    Grid,
}

impl std::fmt::Display for CompositeLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SideBySide => write!(f, "side_by_side"),
            Self::Vertical => write!(f, "vertical"),
            Self::Grid => write!(f, "grid"),
        }
    }
}

/// A top-level image entry in the corpus manifest.
///
/// Discriminated by `"type"`: `"single"` or `"composite"`.
///
/// Images from Reddit galleries use the same types but include a `reddit_index`
/// field indicating which media item to reference from the post. The entry ID
/// encodes the gallery position (e.g., `"schwerin-palace.0"`).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ImageEntry {
    /// One image, one subimage.
    #[serde(rename = "single")]
    Single {
        url: String,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reddit_index: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        regions: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        known_issue: Option<String>,
    },
    /// One image, multiple subimages.
    #[serde(rename = "composite")]
    Composite {
        url: String,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reddit_index: Option<usize>,
        layout: CompositeLayout,
        count: NonZeroU32,
        expect_similar: bool,
        #[serde(
            default,
            deserialize_with = "deserialize_usize_key_map",
            serialize_with = "serialize_usize_key_map"
        )]
        subimages: BTreeMap<usize, SubimageAssertions>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        known_issue: Option<String>,
    },
}

/// Per-subimage assertions within a composite image.
///
/// Currently only holds `regions`; kept as a named type for future assertion
/// expansion (e.g., expected entity types, similarity thresholds).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SubimageAssertions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regions: Option<u32>,
}

/// A similarity cluster: groups of images/subimages expected to have
/// similar embeddings (high intra-cluster, low inter-cluster similarity).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterEntry {
    /// Corpus addresses of cluster members (must have >= 2).
    pub members: Vec<String>,
    /// Names of other clusters whose members are visually similar.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub similar: Vec<String>,
    /// Known issue for intra-cluster similarity checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_issue_intra: Option<String>,
    /// Known issue for similar-cluster cross-checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_issue_similar: Option<String>,
}

impl CorpusManifest {
    /// Load a corpus manifest from a JSON file.
    pub fn load(path: &Path) -> Result<Self, CorpusError> {
        let content = std::fs::read_to_string(path).map_err(CorpusError::Io)?;
        let manifest: Self =
            serde_json::from_str(&content).map_err(|e| CorpusError::Manifest(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate manifest internal consistency.
    ///
    /// Collects all validation errors so the caller sees every problem at once.
    pub fn validate(&self) -> Result<(), CorpusError> {
        let entry_ids: HashSet<&str> = self.images.keys().map(|k| k.as_str()).collect();
        let cluster_names: HashSet<&str> = self.clusters.keys().map(|k| k.as_str()).collect();
        let mut errors: Vec<String> = Vec::new();

        for (name, cluster) in &self.clusters {
            if cluster.members.len() < 2 {
                errors.push(format!("cluster '{name}': must have at least 2 members"));
            }
            for member in &cluster.members {
                let base = extract_base_id(member);
                if !entry_ids.contains(base) {
                    errors.push(format!(
                        "cluster '{name}': member '{member}' does not match any entry ID"
                    ));
                }
            }
            for similar_name in &cluster.similar {
                if !cluster_names.contains(similar_name.as_str()) {
                    errors.push(format!(
                        "cluster '{name}': similar reference '{similar_name}' does not match any cluster name"
                    ));
                }
                if similar_name == name {
                    errors.push(format!(
                        "cluster '{name}': cannot reference itself as similar"
                    ));
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(CorpusError::Manifest(errors.join("\n")))
        }
    }
}

/// Extract the base entry ID from a corpus address.
///
/// `"foo.0/1[3]"` → `"foo.0"`, `"foo/1"` → `"foo"`, `"foo[0]"` → `"foo"`
///
/// Dots are part of the entry ID (e.g., gallery-derived IDs like `"palace.0"`).
/// Only `/` and `[` are address separators.
fn extract_base_id(addr: &str) -> &str {
    let s = addr.split_once('/').map_or(addr, |(base, _)| base);
    s.split_once('[').map_or(s, |(base, _)| base)
}
