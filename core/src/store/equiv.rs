//! Equivalence classes over identity edges, shared across backends.
//!
//! The canonical subject equivalences (`SameEntity`, `SameArtifact`) are
//! connected components over active identity edges at a snapshot. Backends
//! differ only in how they gather the edges (an in-memory fact-bag scan, a
//! SQL component query); the adjacency and component walk live here once so
//! representative selection cannot drift between them.

use std::collections::{BTreeMap, BTreeSet};

use crate::store::schema::EquivClass;

/// Adjacency over the active identity edges at one snapshot. Built once from
/// an edge list; every class lookup on it answers from the map without
/// re-fetching.
pub struct EquivAdjacency<S> {
    adjacency: BTreeMap<S, Vec<S>>,
}

impl<S: Copy + Ord> EquivAdjacency<S> {
    /// Build the adjacency from undirected edges. Callers gate the edges on
    /// snapshot and retraction before feeding them in — a retracted
    /// equivalence is no edge.
    pub fn from_edges(edges: impl IntoIterator<Item = (S, S)>) -> Self {
        let mut adjacency: BTreeMap<S, Vec<S>> = BTreeMap::new();
        for (a, b) in edges {
            adjacency.entry(a).or_default().push(b);
            adjacency.entry(b).or_default().push(a);
        }
        Self { adjacency }
    }

    /// The equivalence class of `member`: the connected component containing
    /// it, with the minimum id as the canonical representative (deterministic
    /// across re-queries of one snapshot). A member with no incident edge —
    /// or one unknown to the store — is its own singleton class.
    pub fn class_of(&self, member: S) -> EquivClass<S> {
        let mut members: BTreeSet<S> = BTreeSet::new();
        let mut frontier = vec![member];
        while let Some(m) = frontier.pop() {
            if !members.insert(m) {
                continue;
            }
            frontier.extend(self.adjacency.get(&m).into_iter().flatten().copied());
        }
        // `members` holds at least `member`, so the minimum always exists.
        let representative = members.first().copied().unwrap_or(member);
        EquivClass {
            representative,
            members,
        }
    }
}
