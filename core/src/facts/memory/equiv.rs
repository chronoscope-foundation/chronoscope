//! Equivalence classes over the active identity edges of a [`ReadCore`]
//! snapshot.

use std::collections::{BTreeMap, BTreeSet};

use super::{MemStoredFact, ReadCore};
use crate::facts::schema::EquivClass;

impl ReadCore<'_> {
    /// One-pass adjacency over the active identity edges `edge_of` extracts
    /// at this snapshot. Built once per read; every class lookup on it
    /// answers from the map ([`EquivAdjacency::class_of`]) without
    /// rescanning the fact bag.
    pub(super) fn equiv_adjacency<S>(
        &self,
        edge_of: impl Fn(&MemStoredFact) -> Option<(S, S)>,
    ) -> EquivAdjacency<S>
    where
        S: Copy + Ord + std::hash::Hash,
    {
        let mut adjacency: BTreeMap<S, Vec<S>> = BTreeMap::new();
        for (fid, fact) in self.visible_facts() {
            let Some((a, b)) = edge_of(fact) else {
                continue;
            };
            // A retracted equivalence is no edge — retraction dissolves the
            // link at later snapshots.
            if self.retracted_by(fid).is_some() {
                continue;
            }
            adjacency.entry(a).or_default().push(b);
            adjacency.entry(b).or_default().push(a);
        }
        EquivAdjacency { adjacency }
    }

    /// The equivalence class of `member` at this snapshot: the connected
    /// component over the active identity edges `edge_of` extracts. The
    /// standalone class reads come through here; each call builds the
    /// adjacency once via [`Self::equiv_adjacency`].
    pub(super) fn equiv_class<S>(
        &self,
        member: S,
        edge_of: impl Fn(&MemStoredFact) -> Option<(S, S)>,
    ) -> EquivClass<S>
    where
        S: Copy + Ord + std::hash::Hash,
    {
        self.equiv_adjacency(edge_of).class_of(member)
    }
}

/// Adjacency over the active identity edges at one snapshot — the reusable
/// product of a single [`ReadCore::equiv_adjacency`] pass over the fact bag.
pub(super) struct EquivAdjacency<S> {
    adjacency: BTreeMap<S, Vec<S>>,
}

impl<S: Copy + Ord + std::hash::Hash> EquivAdjacency<S> {
    /// The equivalence class of `member`: the connected component containing
    /// it, with the minimum id as the canonical representative (deterministic
    /// across re-queries of one snapshot). A member with no incident edge —
    /// or one unknown to the store — is its own singleton class.
    pub(super) fn class_of(&self, member: S) -> EquivClass<S> {
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
