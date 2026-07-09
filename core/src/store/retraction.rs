//! Effective-retraction resolution, shared across backends.
//!
//! A fact is retracted by the lowest of its retractors that is visible at the
//! snapshot and not itself effectively retracted — a non-monotone fixpoint
//! (retracting a retraction restores its target). Backends differ only in how
//! they fetch retractor edges (in-memory index maps, a SQL recursive CTE), so
//! the fixpoint lives here once and both resolve identically.

use std::collections::{BTreeSet, HashMap};

use crate::grammar::ids::FactId;

/// Retractor edges as target → retractors, fully materialized before the
/// fixpoint runs — [`effective_retractor`] does no I/O, so a SQL backend
/// fetches once per batch of rows under consideration and resolves each in
/// memory.
pub struct RetractionEdges {
    retractors: HashMap<FactId, Vec<FactId>>,
}

impl RetractionEdges {
    /// Build from `(target, retractor)` edges, in any order, without
    /// snapshot filtering — the resolution filters, so every backend applies
    /// the same bound.
    pub fn from_edges(edges: impl IntoIterator<Item = (FactId, FactId)>) -> Self {
        let mut retractors: HashMap<FactId, Vec<FactId>> = HashMap::new();
        for (target, retractor) in edges {
            retractors.entry(target).or_default().push(retractor);
        }
        Self { retractors }
    }

    /// Every fact id retracting `id`.
    fn retractors_of(&self, id: FactId) -> impl Iterator<Item = FactId> + '_ {
        self.retractors.get(&id).into_iter().flatten().copied()
    }
}

/// The lowest [`FactId`] effectively retracting `fact_id` at `snapshot`, or
/// `None`.
///
/// Walks only the subgraph bearing on `fact_id` — the facts retracting it,
/// the ones retracting those, and so on. Submit validation forces every
/// retractor's id above its target's, so the walk climbs strictly (no cycle)
/// and resolves high-to-low, each retractor's status known before the fact it
/// retracts.
pub fn effective_retractor(
    fact_id: FactId,
    snapshot: FactId,
    edges: &RetractionEdges,
) -> Option<FactId> {
    // No retractor edge at all → active, before the walk below allocates its
    // frontier and subgraph.
    edges.retractors_of(fact_id).next()?;

    // Collect the facts reachable upward from `fact_id` through visible
    // retractor edges. Edges climb in id, so the frontier drains and a shared
    // retractor is collected once.
    let mut subgraph: BTreeSet<FactId> = BTreeSet::new();
    let mut frontier = vec![fact_id];
    while let Some(id) = frontier.pop() {
        if !subgraph.insert(id) {
            continue;
        }
        frontier.extend(
            edges
                .retractors_of(id)
                .filter(|candidate| candidate.get() < snapshot.get()),
        );
    }

    // Resolve the subgraph high-to-low: each retractor resolves before the
    // fact it retracts. A fact is retracted by the lowest visible retractor
    // that is itself unretracted.
    let mut retracted: HashMap<FactId, Option<FactId>> = HashMap::new();
    for &id in subgraph.iter().rev() {
        let by = edges
            .retractors_of(id)
            .filter(|candidate| candidate.get() < snapshot.get())
            .filter(|candidate| retracted.get(candidate).copied().flatten().is_none())
            .min();
        retracted.insert(id, by);
    }
    retracted.get(&fact_id).copied().flatten()
}
