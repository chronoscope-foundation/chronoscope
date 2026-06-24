//! [`ProjectedCitation`] — the read-side provenance sum, addressed by
//! [`JsonPath`] in a [`CitationMap`].

use std::collections::BTreeMap;

use serde::Serialize;

use crate::facts::citations::{FactualCitation, JudgmentSource};

use super::JsonPath;

/// Provenance of a projected value, one arm per stored-fact category that can
/// back a value. There is no unified citation type in the grammar — a stored
/// fact carries a [`FactualCitation`], a [`JudgmentSource`], or a
/// [`MetaSource`](crate::facts::citations::MetaSource) by its arm — so the
/// sidecar re-sums the two that warrant a value. `Meta` facts never back a
/// value, so the meta source has no arm here.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectedCitation<ImgId> {
    /// A factual claim's citation.
    Factual { citation: FactualCitation },
    /// A judgment's source (e.g. why two ids are one entity).
    Judgment { source: JudgmentSource<ImgId> },
}

/// The provenance attached to one [`JsonPath`] in a projection.
///
/// `supports` lists every fact backing the value at that path. A `conflicts`
/// field is reserved for later — tier-4 conflict detection extends this struct
/// rather than reshaping the sidecar, so the field set stays additive.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProvenanceAtPath<ImgId> {
    /// The facts backing the value at this path.
    pub supports: Vec<ProjectedCitation<ImgId>>,
}

/// Citations addressed by [`JsonPath`] into a [`ProjectedEntity`](super::ProjectedEntity).
/// Emit-only — serializes but does not deserialize.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct CitationMap<ImgId>(pub BTreeMap<JsonPath, ProvenanceAtPath<ImgId>>);

impl<ImgId> CitationMap<ImgId> {
    /// An empty map.
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    /// Look up the provenance at a path.
    pub fn get(&self, path: &JsonPath) -> Option<&ProvenanceAtPath<ImgId>> {
        self.0.get(path)
    }

    /// Record the supporting citations at a path. A path with no supports is
    /// not recorded — an unbacked slot leaves no sidecar entry.
    pub(super) fn insert_supports(
        &mut self,
        path: JsonPath,
        supports: Vec<ProjectedCitation<ImgId>>,
    ) {
        if supports.is_empty() {
            return;
        }
        self.0.insert(path, ProvenanceAtPath { supports });
    }
}
