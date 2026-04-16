//! Per-endpoint projection over [`EntityTransition`].
//!
//! # Why this exists
//!
//! [`EntityTransition`] bundles durational events together: a `Constructed`
//! variant carries both `started_at` and `completed_at` in one record. Three
//! consumers need to reason about *individual endpoints* rather than whole
//! bundles:
//!
//! - **Display**: a building whose construction started in 1880 and finished
//!   in 1889 wants to render as two timeline rows ("Construction started 1880"
//!   and "Construction completed 1889"), each sortable independently against
//!   other events. The Mole Antonelliana case is the canonical example —
//!   `Constructed { completed_at: 1889 }` plus `UsageModified: 1888-03` should
//!   render the usage change *between* the (unknown) construction start and
//!   the construction completion, not lumped together by phase.
//! - **Consistency checking**: the temporal-violation rules
//!   (`CompletionBeforeStart`, `EventsOutOfOrder`, `EventAfterDemolished`)
//!   are all expressible as one mechanism: structural edges between roles
//!   plus date edges between dated moments, with violations as edges that
//!   contradict.
//! - **Eventual spatiotemporal solver**: needs the same partial-order view to
//!   reason about an entity's lifecycle as dated and undated constraints.
//!
//! All three want the same primitive: a flat sequence of dated moments with
//! known structural roles. This module provides it as a borrow over the
//! underlying transitions, plus a [`topological_order`] sort.
//!
//! # Status: interim shape
//!
//! The underlying `EntityTransition` data model is still being worked out —
//! we're choosing between keeping bundled durationals (current shape) and
//! splitting them into flat `Start`/`End` records linked by an identifier.
//! The design discussion is tracked outside the repo. Whichever way that
//! decision lands, the projection consumers (display, consistency, solver)
//! should be mostly stable — only [`decompose`] needs to change.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use chrono::NaiveDateTime;

use crate::date::UncertainDate;
use crate::entity::EntityTransition;
use crate::evidence::Cited;

/// The role of a [`Moment`] within an entity's lifecycle.
///
/// Variants are declared in canonical lifecycle order: construction
/// endpoints, then mid-life events, then demolition endpoints. The
/// `derive(Ord)` impl provides a deterministic tiebreaker for
/// [`topological_order`] when two moments have no edge between them.
/// Reordering variants changes tie-breaking behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TransitionRole {
    ConstructionStart,
    ConstructionEnd,
    ModificationStart,
    ModificationEnd,
    RepairStart,
    RepairEnd,
    Damaged,
    Moved,
    UsageModified,
    Designated,
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

    /// The role's matching end for a durational pair, or `None` for
    /// point-event roles.
    #[must_use]
    pub fn durational_end(self) -> Option<Self> {
        match self {
            Self::ConstructionStart => Some(Self::ConstructionEnd),
            Self::ModificationStart => Some(Self::ModificationEnd),
            Self::RepairStart => Some(Self::RepairEnd),
            Self::DemolitionStart => Some(Self::DemolitionEnd),
            _ => None,
        }
    }
}

/// One endpoint of a transition projected as an independently-sortable item.
///
/// A durational transition (`Constructed`, `Modified`, `Repaired`,
/// `Demolished`) decomposes into two `Moment`s — one for each endpoint —
/// when at least one date is known. When both dates are unknown, only a
/// single Start moment is emitted (two "date unknown" rows for one
/// transition is noise). A point-event transition decomposes into a single
/// `Moment`.
///
/// Borrows from the source transition list, so consumers that need access to
/// shared metadata (description, location, cause) reach through `transition`.
#[derive(Debug)]
pub struct Moment<'a, E, S> {
    /// Index of the source transition in the entity's transition list.
    pub transition_index: usize,
    /// The source transition this moment was projected from.
    pub transition: &'a EntityTransition<E, S>,
    pub role: TransitionRole,
    /// The date for this specific endpoint. `None` when unknown.
    pub date: Option<&'a Cited<UncertainDate, S>>,
    /// `true` when this is a durational Start whose End was suppressed
    /// because both dates were unknown.
    pub collapsed: bool,
}

impl<E, S> Moment<'_, E, S> {
    /// Earliest possible date for this moment.
    #[must_use]
    pub fn earliest(&self) -> Option<NaiveDateTime> {
        self.date.map(|c| c.value.earliest())
    }

    /// Latest possible date for this moment.
    #[must_use]
    pub fn latest(&self) -> Option<NaiveDateTime> {
        self.date.map(|c| c.value.latest())
    }
}

fn push_point<'a, E, S>(
    out: &mut Vec<Moment<'a, E, S>>,
    transition_index: usize,
    transition: &'a EntityTransition<E, S>,
    role: TransitionRole,
    date: Option<&'a Cited<UncertainDate, S>>,
) {
    out.push(Moment {
        transition_index,
        transition,
        role,
        date,
        collapsed: false,
    });
}

/// Decompose a list of transitions into a flat sequence of [`Moment`]s.
///
/// Each durational variant produces two moments (Start, then End) when at
/// least one date is known, or a single collapsed Start when both dates are
/// unknown. Each point variant produces one moment. The output preserves
/// the input order — sorting is the caller's responsibility via
/// [`topological_order`].
#[must_use]
pub fn decompose<E, S>(transitions: &[EntityTransition<E, S>]) -> Vec<Moment<'_, E, S>> {
    let mut out: Vec<Moment<'_, E, S>> = Vec::with_capacity(transitions.len() * 2);
    for (i, t) in transitions.iter().enumerate() {
        let (start_role, end_role, started, completed) = match t {
            EntityTransition::Constructed {
                started_at,
                completed_at,
                ..
            } => (
                TransitionRole::ConstructionStart,
                TransitionRole::ConstructionEnd,
                started_at.as_ref(),
                completed_at.as_ref(),
            ),
            EntityTransition::Modified {
                started_at,
                completed_at,
                ..
            } => (
                TransitionRole::ModificationStart,
                TransitionRole::ModificationEnd,
                started_at.as_ref(),
                completed_at.as_ref(),
            ),
            EntityTransition::Repaired {
                started_at,
                completed_at,
                ..
            } => (
                TransitionRole::RepairStart,
                TransitionRole::RepairEnd,
                started_at.as_ref(),
                completed_at.as_ref(),
            ),
            EntityTransition::Demolished {
                started_at,
                completed_at,
                ..
            } => (
                TransitionRole::DemolitionStart,
                TransitionRole::DemolitionEnd,
                started_at.as_ref(),
                completed_at.as_ref(),
            ),
            EntityTransition::Damaged { occurred_at, .. } => {
                push_point(
                    &mut out,
                    i,
                    t,
                    TransitionRole::Damaged,
                    occurred_at.as_ref(),
                );
                continue;
            }
            EntityTransition::Moved { occurred_at, .. } => {
                push_point(&mut out, i, t, TransitionRole::Moved, occurred_at.as_ref());
                continue;
            }
            EntityTransition::UsageModified { occurred_at, .. } => {
                push_point(
                    &mut out,
                    i,
                    t,
                    TransitionRole::UsageModified,
                    occurred_at.as_ref(),
                );
                continue;
            }
            EntityTransition::Designated { occurred_at, .. } => {
                push_point(
                    &mut out,
                    i,
                    t,
                    TransitionRole::Designated,
                    occurred_at.as_ref(),
                );
                continue;
            }
        };

        let collapsed = started.is_none() && completed.is_none();
        out.push(Moment {
            transition_index: i,
            transition: t,
            role: start_role,
            date: started,
            collapsed,
        });
        if !collapsed {
            out.push(Moment {
                transition_index: i,
                transition: t,
                role: end_role,
                date: completed,
                collapsed: false,
            });
        }
    }
    out
}

/// Returns `(from, to)` pairs representing structural precedence constraints
/// between moments. These encode lifecycle rules:
///
/// - Within a durational transition: start before end
/// - Construction endpoints before mid-life events
/// - Construction endpoints before demolition endpoints
/// - Mid-life events before demolition endpoints
///
/// Mid-life events have no structural ordering relative to each other.
#[must_use]
pub fn structural_edges<E, S>(moments: &[Moment<'_, E, S>]) -> Vec<(usize, usize)> {
    let mut edges = Vec::new();
    for (i, a) in moments.iter().enumerate() {
        for (j, b) in moments.iter().enumerate() {
            if i == j {
                continue;
            }
            let precedes =
                // Same-transition durational: start before end
                (a.transition_index == b.transition_index
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
///   "construction before mid-life." A structural edge is dropped when
///   date evidence contradicts it — dates win for display, and the
///   contradiction is surfaced separately by consistency checking.
/// - **Date edges**: if `A.latest < B.earliest`, A must precede B.
///
/// When multiple moments have no edges between them (e.g. two undated
/// mid-life events), ties are broken by `(role, transition_index)` for
/// determinism.
#[must_use]
pub fn topological_order<'a, E, S>(mut moments: Vec<Moment<'a, E, S>>) -> Vec<Moment<'a, E, S>> {
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

    // Kahn's algorithm. Tie-break by (role, transition_index) so the output
    // is deterministic when two moments have no ordering constraint.
    let mut queue: BinaryHeap<Reverse<(TransitionRole, usize, usize)>> = BinaryHeap::new();
    for i in 0..n {
        if in_deg[i] == 0 {
            queue.push(Reverse((moments[i].role, moments[i].transition_index, i)));
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
                    moments[neighbor].transition_index,
                    neighbor,
                )));
            }
        }
    }

    // Permute moments into the computed order.
    let mut slots: Vec<Option<Moment<'a, E, S>>> = moments.drain(..).map(Some).collect();
    order.into_iter().filter_map(|i| slots[i].take()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::date::UncertainDate;
    use crate::evidence::Cited;
    use chrono::NaiveDate;

    type T = EntityTransition<(), ()>;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn d(year: i32) -> Result<Cited<UncertainDate, ()>, Box<dyn std::error::Error>> {
        let dt = NaiveDate::from_ymd_opt(year, 1, 1)
            .ok_or("invalid date")?
            .and_hms_opt(0, 0, 0)
            .ok_or("invalid time")?;
        Ok(Cited::uncited(UncertainDate::exact(dt)?))
    }

    fn d_ym(year: i32, month: u32) -> Result<Cited<UncertainDate, ()>, Box<dyn std::error::Error>> {
        let dt = NaiveDate::from_ymd_opt(year, month, 1)
            .ok_or("invalid date")?
            .and_hms_opt(0, 0, 0)
            .ok_or("invalid time")?;
        Ok(Cited::uncited(UncertainDate::exact(dt)?))
    }

    /// Mole Antonelliana: a `Constructed` with only `completed_at = 1889`
    /// and a `UsageModified` at 1888-03 should render as
    /// `[ConstructionStart(?), UsageModified(1888), ConstructionEnd(1889)]`
    /// — the structural edge ConstructionEnd→UsageModified is dropped
    /// because the dates contradict it, and the date edge
    /// UsageModified→ConstructionEnd takes over.
    #[test]
    fn topological_order_mole_antonelliana() -> TestResult {
        let transitions: Vec<T> = vec![
            T::Constructed {
                started_at: None,
                completed_at: Some(d(1889)?),
                location: None,
                trigger_event: None,
            },
            T::UsageModified {
                occurred_at: Some(d_ym(1888, 3)?),
                new_usages: Default::default(),
                description: None,
                trigger_event: None,
            },
        ];
        let ordered = topological_order(decompose(&transitions));
        let roles: Vec<TransitionRole> = ordered.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
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
        let transitions: Vec<T> = vec![
            T::Demolished {
                started_at: None,
                completed_at: None,
                cause: None,
                trigger_event: None,
            },
            T::Constructed {
                started_at: None,
                completed_at: None,
                location: None,
                trigger_event: None,
            },
        ];
        let ordered = topological_order(decompose(&transitions));
        let roles: Vec<TransitionRole> = ordered.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            vec![
                TransitionRole::ConstructionStart,
                TransitionRole::DemolitionStart,
            ]
        );
        Ok(())
    }
}
