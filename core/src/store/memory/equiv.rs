//! Equivalence classes over the active identity edges of a [`ReadCore`]
//! snapshot: the fact-bag scan feeding the backend-shared
//! [`EquivAdjacency`].

use super::{MemStoredFact, ReadCore};
use crate::store::equiv::EquivAdjacency;
use crate::store::schema::EquivClass;

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
        S: Copy + Ord,
    {
        EquivAdjacency::from_edges(self.visible_facts().filter_map(|(fid, fact)| {
            let edge = edge_of(fact)?;
            // A retracted equivalence is no edge — retraction dissolves the
            // link at later snapshots.
            if self.retracted_by(fid).is_some() {
                return None;
            }
            Some(edge)
        }))
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
        S: Copy + Ord,
    {
        self.equiv_adjacency(edge_of).class_of(member)
    }
}
