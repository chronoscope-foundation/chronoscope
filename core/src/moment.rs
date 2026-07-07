//! Per-endpoint projection over the typed lifecycle timeline for display
//! ordering.
//!
//! A [`TimelineEntry`] bundles a durational event's two endpoints together: a
//! construction carries both `started` and `completed` in one
//! [`Period`](crate::facts::typed::Period). To render — and sort — endpoints
//! independently, [`decompose`] splits each
//! durational entry into a start and an end [`Moment`] and leaves each point
//! entry as one. [`topological_order`] then sorts the moments over structural
//! lifecycle edges and date edges, so an interior event interleaves between
//! construction's endpoints when its date falls there.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use chrono::NaiveDate;

use crate::date::UncertainDate;
use crate::facts::typed::{Bounded, Consensus, EventDetail, InteriorEvent, TimelineEntry};

/// The role of a [`Moment`] within an entity's lifecycle.
///
/// Variants are declared in canonical lifecycle order: construction endpoints,
/// then mid-life events (durational pairs, then point events), then demolition
/// endpoints. The `derive(Ord)` impl provides a deterministic tiebreaker for
/// [`topological_order`] when two moments have no edge between them. Reordering
/// variants changes tie-breaking behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TransitionRole {
    ConstructionStart,
    ConstructionEnd,
    ModificationStart,
    ModificationEnd,
    RepairStart,
    RepairEnd,
    DamagedStart,
    DamagedEnd,
    MovedStart,
    MovedEnd,
    UsageModified,
    Designated,
    Ambiguous,
    DemolitionStart,
    DemolitionEnd,
}

impl TransitionRole {
    /// True for roles in the construction phase.
    #[must_use]
    pub fn is_construction(self) -> bool {
        matches!(self, Self::ConstructionStart | Self::ConstructionEnd)
    }

    /// True for roles in the demolition phase.
    #[must_use]
    pub fn is_demolition(self) -> bool {
        matches!(self, Self::DemolitionStart | Self::DemolitionEnd)
    }

    /// True for mid-life roles (everything that isn't construction or
    /// demolition).
    #[must_use]
    pub fn is_midlife(self) -> bool {
        !self.is_construction() && !self.is_demolition()
    }

    /// The role's matching end for a durational pair, or `None` for point-event
    /// roles.
    #[must_use]
    pub fn durational_end(self) -> Option<Self> {
        match self {
            Self::ConstructionStart => Some(Self::ConstructionEnd),
            Self::ModificationStart => Some(Self::ModificationEnd),
            Self::RepairStart => Some(Self::RepairEnd),
            Self::DamagedStart => Some(Self::DamagedEnd),
            Self::MovedStart => Some(Self::MovedEnd),
            Self::DemolitionStart => Some(Self::DemolitionEnd),
            _ => None,
        }
    }
}

/// One endpoint of a timeline entry projected as an independently-sortable item.
///
/// A durational entry (`Constructed`, `Demolished`, and the `Modified`,
/// `Repaired`, `Damaged`, `Moved` interior spans) decomposes into two `Moment`s
/// — one per endpoint — when at least one date is known. When both dates are
/// unknown, only a single collapsed Start is emitted, since two "date unknown"
/// rows for one event is noise. A point entry decomposes into a single `Moment`.
///
/// Borrows from the source timeline, so a consumer that needs the entry's
/// descriptions, cause, or destination reaches through `entry`.
#[derive(Debug)]
pub struct Moment<'a, EvtId, ImgId> {
    /// Index of the source entry in the entity's timeline.
    pub entry_index: usize,
    /// The source timeline entry this moment was projected from.
    pub entry: &'a TimelineEntry<EvtId, ImgId>,
    pub role: TransitionRole,
    /// The date for this specific endpoint. `None` when unknown.
    pub date: Option<&'a Bounded<UncertainDate, ImgId>>,
    /// `true` when this is a durational Start whose End was suppressed because
    /// both dates were unknown.
    pub collapsed: bool,
}

impl<EvtId, ImgId> Moment<'_, EvtId, ImgId> {
    /// Earliest possible date for this moment.
    #[must_use]
    pub fn earliest(&self) -> Option<NaiveDate> {
        self.date.and_then(|b| b.possible.earliest())
    }

    /// Latest possible date for this moment.
    #[must_use]
    pub fn latest(&self) -> Option<NaiveDate> {
        self.date.and_then(|b| b.possible.latest())
    }
}

/// Whether a date slot carries a claim. An `Absent` consensus never touched the
/// slot, so the moment is undated.
fn has_date<ImgId>(bounded: &Bounded<UncertainDate, ImgId>) -> bool {
    !matches!(bounded.consensus, Consensus::Absent)
}

/// The slot as a dated bound, or `None` when it is absent.
fn dated_bound<ImgId>(
    bounded: &Bounded<UncertainDate, ImgId>,
) -> Option<&Bounded<UncertainDate, ImgId>> {
    has_date(bounded).then_some(bounded)
}

fn push_point<'a, EvtId, ImgId>(
    out: &mut Vec<Moment<'a, EvtId, ImgId>>,
    entry_index: usize,
    entry: &'a TimelineEntry<EvtId, ImgId>,
    role: TransitionRole,
    date: Option<&'a Bounded<UncertainDate, ImgId>>,
) {
    out.push(Moment {
        entry_index,
        entry,
        role,
        date,
        collapsed: false,
    });
}

fn push_durational<'a, EvtId, ImgId>(
    out: &mut Vec<Moment<'a, EvtId, ImgId>>,
    entry_index: usize,
    entry: &'a TimelineEntry<EvtId, ImgId>,
    start_role: TransitionRole,
    end_role: TransitionRole,
    started: &'a Bounded<UncertainDate, ImgId>,
    completed: &'a Bounded<UncertainDate, ImgId>,
) {
    let started = dated_bound(started);
    let completed = dated_bound(completed);
    let collapsed = started.is_none() && completed.is_none();
    out.push(Moment {
        entry_index,
        entry,
        role: start_role,
        date: started,
        collapsed,
    });
    if !collapsed {
        out.push(Moment {
            entry_index,
            entry,
            role: end_role,
            date: completed,
            collapsed: false,
        });
    }
}

/// Decompose a typed timeline into a flat sequence of [`Moment`]s.
///
/// Each durational entry produces two moments (Start, then End) when at least
/// one date is known, or a single collapsed Start when both dates are unknown.
/// Each point entry produces one moment. The output preserves the input order —
/// sorting is the caller's job via [`topological_order`].
#[must_use]
pub fn decompose<EvtId, ImgId>(
    timeline: &[TimelineEntry<EvtId, ImgId>],
) -> Vec<Moment<'_, EvtId, ImgId>> {
    let mut out: Vec<Moment<'_, EvtId, ImgId>> = Vec::with_capacity(timeline.len() * 2);
    for (i, entry) in timeline.iter().enumerate() {
        match &entry.detail {
            EventDetail::Constructed { period, .. } => push_durational(
                &mut out,
                i,
                entry,
                TransitionRole::ConstructionStart,
                TransitionRole::ConstructionEnd,
                &period.started,
                &period.completed,
            ),
            EventDetail::Demolished { period } => push_durational(
                &mut out,
                i,
                entry,
                TransitionRole::DemolitionStart,
                TransitionRole::DemolitionEnd,
                &period.started,
                &period.completed,
            ),
            EventDetail::Interior { kind, .. } => match kind {
                InteriorEvent::Modified { period } => push_durational(
                    &mut out,
                    i,
                    entry,
                    TransitionRole::ModificationStart,
                    TransitionRole::ModificationEnd,
                    &period.started,
                    &period.completed,
                ),
                InteriorEvent::Repaired { period } => push_durational(
                    &mut out,
                    i,
                    entry,
                    TransitionRole::RepairStart,
                    TransitionRole::RepairEnd,
                    &period.started,
                    &period.completed,
                ),
                InteriorEvent::Damaged { period, .. } => push_durational(
                    &mut out,
                    i,
                    entry,
                    TransitionRole::DamagedStart,
                    TransitionRole::DamagedEnd,
                    &period.started,
                    &period.completed,
                ),
                InteriorEvent::Moved { period, .. } => push_durational(
                    &mut out,
                    i,
                    entry,
                    TransitionRole::MovedStart,
                    TransitionRole::MovedEnd,
                    &period.started,
                    &period.completed,
                ),
                InteriorEvent::UsageChanged { at, .. } => {
                    push_point(
                        &mut out,
                        i,
                        entry,
                        TransitionRole::UsageModified,
                        dated_bound(at),
                    );
                }
                InteriorEvent::Designated { at, .. } => {
                    push_point(
                        &mut out,
                        i,
                        entry,
                        TransitionRole::Designated,
                        dated_bound(at),
                    );
                }
                InteriorEvent::Ambiguous { facts, .. } => {
                    let date = [&facts.started, &facts.completed, &facts.occurred]
                        .into_iter()
                        .find(|&b| has_date(b));
                    push_point(&mut out, i, entry, TransitionRole::Ambiguous, date);
                }
            },
        }
    }
    out
}

/// Returns `(from, to)` pairs representing structural precedence constraints
/// between moments. These encode lifecycle rules:
///
/// - Within a durational entry: start before end
/// - Construction endpoints before mid-life events
/// - Construction endpoints before demolition endpoints
/// - Mid-life events before demolition endpoints
///
/// Mid-life events have no structural ordering relative to each other.
#[must_use]
pub fn structural_edges<EvtId, ImgId>(moments: &[Moment<'_, EvtId, ImgId>]) -> Vec<(usize, usize)> {
    let mut edges = Vec::new();
    for (i, a) in moments.iter().enumerate() {
        for (j, b) in moments.iter().enumerate() {
            if i == j {
                continue;
            }
            let precedes =
                // Same-entry durational: start before end
                (a.entry_index == b.entry_index
                    && a.role.durational_end() == Some(b.role))
                // Construction before mid-life
                || (a.role.is_construction() && b.role.is_midlife())
                // Construction before demolition
                || (a.role.is_construction() && b.role.is_demolition())
                // Mid-life before demolition
                || (a.role.is_midlife() && b.role.is_demolition());
            if precedes {
                edges.push((i, j));
            }
        }
    }
    edges
}

/// Sort moments using a topological sort over structural and date edges.
///
/// The constraint graph has two kinds of edges:
///
/// - **Structural edges** ([`structural_edges`]): lifecycle rules like
///   "construction before mid-life." A structural edge is dropped when date
///   evidence contradicts it — dates win for display.
/// - **Date edges**: if `A.latest < B.earliest`, A must precede B.
///
/// When multiple moments have no edges between them (e.g. two undated mid-life
/// events), ties are broken by `(role, entry_index)` for determinism.
#[must_use]
pub fn topological_order<'a, EvtId, ImgId>(
    mut moments: Vec<Moment<'a, EvtId, ImgId>>,
) -> Vec<Moment<'a, EvtId, ImgId>> {
    let n = moments.len();
    if n == 0 {
        return moments;
    }

    let struct_edges = structural_edges(&moments);

    let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
    let mut in_deg = vec![0u32; n];

    // Structural edges, dropping any contradicted by date evidence.
    for &(from, to) in &struct_edges {
        let contradicted = matches!(
            (moments[from].earliest(), moments[to].latest()),
            (Some(from_early), Some(to_late)) if to_late < from_early
        );
        if !contradicted {
            adj[from].push(to);
            in_deg[to] += 1;
        }
    }

    // Date edges: if A.latest < B.earliest, A precedes B.
    for i in 0..n {
        for j in (i + 1)..n {
            if let (Some(i_late), Some(j_early)) = (moments[i].latest(), moments[j].earliest())
                && i_late < j_early
            {
                adj[i].push(j);
                in_deg[j] += 1;
            }
            if let (Some(j_late), Some(i_early)) = (moments[j].latest(), moments[i].earliest())
                && j_late < i_early
            {
                adj[j].push(i);
                in_deg[i] += 1;
            }
        }
    }

    // Kahn's algorithm. Tie-break by (role, entry_index) so the output is
    // deterministic when two moments have no ordering constraint.
    let mut queue: BinaryHeap<Reverse<(TransitionRole, usize, usize)>> = BinaryHeap::new();
    for i in 0..n {
        if in_deg[i] == 0 {
            queue.push(Reverse((moments[i].role, moments[i].entry_index, i)));
        }
    }

    let mut order = Vec::with_capacity(n);
    while let Some(Reverse((_, _, idx))) = queue.pop() {
        order.push(idx);
        for &neighbor in &adj[idx] {
            in_deg[neighbor] -= 1;
            if in_deg[neighbor] == 0 {
                queue.push(Reverse((
                    moments[neighbor].role,
                    moments[neighbor].entry_index,
                    neighbor,
                )));
            }
        }
    }

    // Permute moments into the computed order.
    let mut slots: Vec<Option<Moment<'a, EvtId, ImgId>>> = moments.drain(..).map(Some).collect();
    order.into_iter().filter_map(|i| slots[i].take()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::lattice::JoinSemilattice;
    use crate::date::{DatePrecision, UncertainDate};
    use crate::facts::typed::{Consensus, Period};
    use chrono::NaiveDate;

    type Entry = TimelineEntry<(), ()>;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn dated(
        year: i32,
        month: u32,
    ) -> Result<Bounded<UncertainDate, ()>, Box<dyn std::error::Error>> {
        let dt = NaiveDate::from_ymd_opt(year, month, 1).ok_or("invalid date")?;
        let value = UncertainDate::with_precision(dt, DatePrecision::Day)?;
        Ok(Bounded {
            possible: value.clone(),
            sources: Vec::new(),
            consensus: Consensus::Reached { value },
        })
    }

    /// An untouched slot: the honest ⊥ with an `Absent` consensus.
    fn absent<V: JoinSemilattice>() -> Bounded<V, ()> {
        Bounded {
            possible: V::bottom(),
            sources: Vec::new(),
            consensus: Consensus::Absent,
        }
    }

    fn constructed(
        started: Bounded<UncertainDate, ()>,
        completed: Bounded<UncertainDate, ()>,
    ) -> Entry {
        Entry {
            detail: EventDetail::Constructed {
                period: Period { started, completed },
                location: absent(),
            },
            sources: Vec::new(),
        }
    }

    fn demolished(
        started: Bounded<UncertainDate, ()>,
        completed: Bounded<UncertainDate, ()>,
    ) -> Entry {
        Entry {
            detail: EventDetail::Demolished {
                period: Period { started, completed },
            },
            sources: Vec::new(),
        }
    }

    fn usage_changed(at: Bounded<UncertainDate, ()>) -> Entry {
        Entry {
            detail: EventDetail::Interior {
                id: (),
                descriptions: Vec::new(),
                kind: InteriorEvent::UsageChanged {
                    at,
                    usages: absent(),
                },
            },
            sources: Vec::new(),
        }
    }

    fn roles(timeline: &[Entry]) -> Vec<TransitionRole> {
        topological_order(decompose(timeline))
            .iter()
            .map(|m| m.role)
            .collect()
    }

    /// Notre-Dame: constructed 1163→1345 with a usage change in 1200. The usage
    /// change interleaves *between* the construction endpoints, not after the
    /// completion — the ConstructionEnd→UsageModified structural edge is dropped
    /// (dates contradict) and the date edge UsageModified→ConstructionEnd wins.
    #[test]
    fn interior_event_interleaves_between_construction_endpoints() -> TestResult {
        let timeline = vec![
            constructed(dated(1163, 1)?, dated(1345, 1)?),
            usage_changed(dated(1200, 1)?),
        ];
        assert_eq!(
            roles(&timeline),
            vec![
                TransitionRole::ConstructionStart,
                TransitionRole::UsageModified,
                TransitionRole::ConstructionEnd,
            ]
        );
        Ok(())
    }

    /// Mole Antonelliana: a `Constructed` with only `completed = 1889` and a
    /// usage change at 1888-03 renders as
    /// `[ConstructionStart(?), UsageModified(1888), ConstructionEnd(1889)]` —
    /// the undated start anchors first structurally, the usage change sorts
    /// before the completion by date.
    #[test]
    fn topological_order_mole_antonelliana() -> TestResult {
        let timeline = vec![
            constructed(absent(), dated(1889, 1)?),
            usage_changed(dated(1888, 3)?),
        ];
        assert_eq!(
            roles(&timeline),
            vec![
                TransitionRole::ConstructionStart,
                TransitionRole::UsageModified,
                TransitionRole::ConstructionEnd,
            ]
        );
        Ok(())
    }

    /// All-undated: structural edges are the only signal. Both-undated
    /// durationals collapse to a single Start moment.
    #[test]
    fn topological_order_all_undated_falls_back_to_structural() -> TestResult {
        let timeline = vec![
            demolished(absent(), absent()),
            constructed(absent(), absent()),
        ];
        assert_eq!(
            roles(&timeline),
            vec![
                TransitionRole::ConstructionStart,
                TransitionRole::DemolitionStart,
            ]
        );
        Ok(())
    }
}
