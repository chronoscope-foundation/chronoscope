//! Per-endpoint projection over the typed lifecycle timeline for display
//! ordering.
//!
//! A [`TimelineEvent`] bundles a durational event's two endpoints together: a
//! construction carries both `started` and `completed` in one
//! [`Period`](crate::typed::Period). To render — and sort — endpoints
//! independently, `decompose` splits each
//! durational event into a start and an end `Moment` and leaves each point
//! event as one. `topological_order` then sorts the moments over structural
//! lifecycle edges and date edges, so an interior event interleaves between
//! construction's endpoints when its date falls there.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::date::UncertainDate;
use crate::typed::{Bounded, EventDetail, InteriorEvent, TimelineEvent, dated_bound, has_date};

/// The role of a `Moment` within an entity's lifecycle.
///
/// Variants are declared in canonical lifecycle order: construction endpoints,
/// then mid-life events (durational pairs, then point events), then demolition
/// endpoints. The `derive(Ord)` impl provides a deterministic tiebreaker for
/// `topological_order` when two moments have no edge between them. Reordering
/// variants changes tie-breaking behavior.
///
/// Serializes `snake_case` on the wire — it drives a client's per-moment label
/// choice, so presentation stays client-side.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TransitionRole {
    ConstructionStart,
    ConstructionEnd,
    KnownToExist,
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

/// How one lifecycle event anchors in time, over its raw (possibly undated)
/// bounds. The single mapping from event variant to temporal anchor: every
/// consumer — [`decompose`], `resolve_moment_date`, `entry_date_bounds` — reads
/// it, so a new event variant is classified in exactly one place.
///
/// Bounds are exposed raw, not pre-filtered to dated ones: the span folds need
/// every bound including undated ones, while the single-date consumers apply
/// their own first-dated pick.
pub(crate) enum EventTemporalShape<'a, ImgId> {
    /// A span with two independently-bounded endpoints and their transition
    /// roles.
    Durational {
        start_role: TransitionRole,
        end_role: TransitionRole,
        started: &'a Bounded<UncertainDate, ImgId>,
        completed: &'a Bounded<UncertainDate, ImgId>,
    },
    /// A single instant under one role.
    Point {
        role: TransitionRole,
        at: &'a Bounded<UncertainDate, ImgId>,
    },
    /// An unsettled interior event, whose date is the first dated of
    /// `[started, completed, occurred]` in that order.
    Ambiguous {
        bounds: [&'a Bounded<UncertainDate, ImgId>; 3],
    },
}

/// The temporal shape of one lifecycle event. The `Interior` arm delegates to
/// [`interior_event_temporal_shape`].
pub(crate) fn event_temporal_shape<EvtId, ImgId>(
    detail: &EventDetail<EvtId, ImgId>,
) -> EventTemporalShape<'_, ImgId> {
    match detail {
        EventDetail::Constructed { period, .. } => EventTemporalShape::Durational {
            start_role: TransitionRole::ConstructionStart,
            end_role: TransitionRole::ConstructionEnd,
            started: &period.started,
            completed: &period.completed,
        },
        EventDetail::Demolished { period } => EventTemporalShape::Durational {
            start_role: TransitionRole::DemolitionStart,
            end_role: TransitionRole::DemolitionEnd,
            started: &period.started,
            completed: &period.completed,
        },
        EventDetail::Existed { at } => EventTemporalShape::Point {
            role: TransitionRole::KnownToExist,
            at,
        },
        EventDetail::Interior { kind, .. } => interior_event_temporal_shape(kind),
    }
}

/// The temporal shape of one interior event — the `InteriorEvent`-level
/// classification that [`event_temporal_shape`] delegates its `Interior` arm
/// to.
pub(crate) fn interior_event_temporal_shape<ImgId>(
    kind: &InteriorEvent<ImgId>,
) -> EventTemporalShape<'_, ImgId> {
    match kind {
        InteriorEvent::Modified { period } => EventTemporalShape::Durational {
            start_role: TransitionRole::ModificationStart,
            end_role: TransitionRole::ModificationEnd,
            started: &period.started,
            completed: &period.completed,
        },
        InteriorEvent::Repaired { period } => EventTemporalShape::Durational {
            start_role: TransitionRole::RepairStart,
            end_role: TransitionRole::RepairEnd,
            started: &period.started,
            completed: &period.completed,
        },
        InteriorEvent::Damaged { period, .. } => EventTemporalShape::Durational {
            start_role: TransitionRole::DamagedStart,
            end_role: TransitionRole::DamagedEnd,
            started: &period.started,
            completed: &period.completed,
        },
        InteriorEvent::Moved { period, .. } => EventTemporalShape::Durational {
            start_role: TransitionRole::MovedStart,
            end_role: TransitionRole::MovedEnd,
            started: &period.started,
            completed: &period.completed,
        },
        InteriorEvent::UsageChanged { at, .. } => EventTemporalShape::Point {
            role: TransitionRole::UsageModified,
            at,
        },
        InteriorEvent::Designated { at, .. } => EventTemporalShape::Point {
            role: TransitionRole::Designated,
            at,
        },
        InteriorEvent::Ambiguous { facts, .. } => EventTemporalShape::Ambiguous {
            bounds: [&facts.started, &facts.completed, &facts.occurred],
        },
    }
}

/// One endpoint of a timeline event projected as an independently-sortable item.
///
/// A durational event (`Constructed`, `Demolished`, and the `Modified`,
/// `Repaired`, `Damaged`, `Moved` interior spans) decomposes into a `Moment` per
/// *dated* endpoint: both dates known → two moments, exactly one known → one
/// (the undated partner is dropped, not rendered as a phantom "date unknown"
/// row), neither known → one collapsed bare token. A point event decomposes into
/// a single `Moment`.
///
/// Carries `event_index`, a back-reference into the source timeline.
#[derive(Debug)]
pub(crate) struct Moment<'a, ImgId> {
    /// Index of the source event in the entity's timeline.
    pub event_index: usize,
    pub role: TransitionRole,
    /// The date for this specific endpoint. `None` only for a collapsed bare
    /// token.
    pub date: Option<&'a Bounded<UncertainDate, ImgId>>,
    /// `true` when this is a durational Start standing in for a both-undated
    /// event (its End suppressed) — the label reads as the bare verb.
    pub collapsed: bool,
    /// `true` when this moment renders the event's secondary text: the dated
    /// completion of a both-dated pair, the lone dated endpoint of a one-dated
    /// event, the bare token of an undated one, or any point event.
    pub carries_description: bool,
}

impl<ImgId> Moment<'_, ImgId> {
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

/// A point event's single moment. A point always carries the event's secondary
/// text — it has no partner endpoint to defer to.
fn push_point<'a, ImgId>(
    out: &mut Vec<Moment<'a, ImgId>>,
    event_index: usize,
    role: TransitionRole,
    date: Option<&'a Bounded<UncertainDate, ImgId>>,
) {
    out.push(Moment {
        event_index,
        role,
        date,
        collapsed: false,
        carries_description: true,
    });
}

/// Decompose a durational event under the (A) collapse: emit a moment only for a
/// *dated* endpoint. Both dated → two moments, the completion carrying the
/// description; exactly one dated → that endpoint alone, carrying the
/// description; neither dated → one collapsed bare Start.
fn push_durational<'a, ImgId>(
    out: &mut Vec<Moment<'a, ImgId>>,
    event_index: usize,
    start_role: TransitionRole,
    end_role: TransitionRole,
    started: &'a Bounded<UncertainDate, ImgId>,
    completed: &'a Bounded<UncertainDate, ImgId>,
) {
    let push = |out: &mut Vec<Moment<'a, ImgId>>, role, date, collapsed, carries_description| {
        out.push(Moment {
            event_index,
            role,
            date,
            collapsed,
            carries_description,
        });
    };
    match (dated_bound(started), dated_bound(completed)) {
        (Some(started), Some(completed)) => {
            push(out, start_role, Some(started), false, false);
            push(out, end_role, Some(completed), false, true);
        }
        (Some(started), None) => push(out, start_role, Some(started), false, true),
        (None, Some(completed)) => push(out, end_role, Some(completed), false, true),
        (None, None) => push(out, start_role, None, true, true),
    }
}

/// Decompose a typed timeline into a flat sequence of [`Moment`]s.
///
/// Each durational event produces a moment per dated endpoint (two when both are
/// dated, one when exactly one is, one collapsed bare Start when neither is).
/// Each point event produces one moment. The output preserves the input order —
/// sorting is the caller's job via [`topological_order`].
#[must_use]
pub(crate) fn decompose<EvtId, ImgId>(
    timeline: &[TimelineEvent<EvtId, ImgId>],
) -> Vec<Moment<'_, ImgId>> {
    let mut out: Vec<Moment<'_, ImgId>> = Vec::with_capacity(timeline.len() * 2);
    for (i, event) in timeline.iter().enumerate() {
        match event_temporal_shape(&event.detail) {
            EventTemporalShape::Durational {
                start_role,
                end_role,
                started,
                completed,
            } => push_durational(&mut out, i, start_role, end_role, started, completed),
            EventTemporalShape::Point { role, at } => {
                push_point(&mut out, i, role, dated_bound(at));
            }
            EventTemporalShape::Ambiguous { bounds } => {
                let date = bounds.into_iter().find(|b| has_date(b));
                push_point(&mut out, i, TransitionRole::Ambiguous, date);
            }
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
pub(crate) fn structural_edges<ImgId>(moments: &[Moment<'_, ImgId>]) -> Vec<(usize, usize)> {
    let mut edges = Vec::new();
    for (i, a) in moments.iter().enumerate() {
        for (j, b) in moments.iter().enumerate() {
            if i == j {
                continue;
            }
            let precedes =
                // Same-event durational: start before end
                (a.event_index == b.event_index
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
/// events), ties are broken by `(role, event_index)` for determinism.
#[must_use]
pub(crate) fn topological_order<'a, ImgId>(
    mut moments: Vec<Moment<'a, ImgId>>,
) -> Vec<Moment<'a, ImgId>> {
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
            queue.push(Reverse((moments[i].role, moments[i].event_index, i)));
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
                    moments[neighbor].event_index,
                    neighbor,
                )));
            }
        }
    }

    // Permute moments into the computed order.
    let mut slots: Vec<Option<Moment<'a, ImgId>>> = moments.drain(..).map(Some).collect();
    order.into_iter().filter_map(|i| slots[i].take()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::lattice::JoinSemilattice;
    use crate::date::{DatePrecision, UncertainDate};
    use crate::typed::{Consensus, Period};
    use chrono::NaiveDate;

    type Entry = TimelineEvent<(), ()>;
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
            facts: Vec::new(),
            consensus: Consensus::Reached { value },
            derivation: None,
        })
    }

    /// An untouched slot: the honest ⊥ with an `Absent` consensus.
    fn absent<V: JoinSemilattice>() -> Bounded<V, ()> {
        Bounded {
            possible: V::bottom(),
            sources: Vec::new(),
            facts: Vec::new(),
            consensus: Consensus::Absent,
            derivation: None,
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
    /// `[UsageModified(1888), ConstructionEnd(1889)]` — under the (A) collapse
    /// the undated construction start is dropped (no phantom row), and the usage
    /// change sorts before the completion by date.
    #[test]
    fn topological_order_mole_antonelliana() -> TestResult {
        let timeline = vec![
            constructed(absent(), dated(1889, 1)?),
            usage_changed(dated(1888, 3)?),
        ];
        assert_eq!(
            roles(&timeline),
            vec![
                TransitionRole::UsageModified,
                TransitionRole::ConstructionEnd,
            ]
        );
        Ok(())
    }

    /// The (A) collapse decides how many moments a durational emits and which
    /// carries the event's secondary text: both dated → two (completion
    /// carries), one dated → that endpoint alone (carries), neither → one bare
    /// collapsed Start (carries).
    #[test]
    fn durational_collapse_emits_one_moment_per_dated_endpoint() -> TestResult {
        // `(role, collapsed, carries_description, dated)` for one construction.
        fn shape(entry: Entry) -> Vec<(TransitionRole, bool, bool, bool)> {
            decompose(std::slice::from_ref(&entry))
                .iter()
                .map(|m| (m.role, m.collapsed, m.carries_description, m.date.is_some()))
                .collect()
        }

        assert_eq!(
            shape(constructed(dated(1800, 1)?, dated(1850, 1)?)),
            vec![
                (TransitionRole::ConstructionStart, false, false, true),
                (TransitionRole::ConstructionEnd, false, true, true),
            ],
            "both dated: two moments, the completion carrying the description"
        );
        assert_eq!(
            shape(constructed(dated(1800, 1)?, absent())),
            vec![(TransitionRole::ConstructionStart, false, true, true)],
            "start-only: the dated start alone, carrying the description"
        );
        assert_eq!(
            shape(constructed(absent(), dated(1850, 1)?)),
            vec![(TransitionRole::ConstructionEnd, false, true, true)],
            "completion-only: the dated end alone, carrying the description"
        );
        assert_eq!(
            shape(constructed(absent(), absent())),
            vec![(TransitionRole::ConstructionStart, true, true, false)],
            "neither dated: one bare collapsed Start"
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
