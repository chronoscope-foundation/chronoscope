//! The entity timeline as one canonical store with two views.
//!
//! A [`Timeline`] holds the data-carrying [`TimelineEvent`]s once and the
//! chronologically-ordered, (A)-collapsed moment sequence over them once. The
//! order is derivable from the events, but deriving it once here (at
//! projection→typed time, via [`moment::decompose`](crate::moment::decompose) +
//! [`topological_order`](crate::moment::topological_order)) keeps a subtle
//! topsort in one place and every client in agreement on the order.
//!
//! - [`Timeline::events`] reads the bundled event data.
//! - [`Timeline::moments`] reads the ordered display view, each moment's date
//!   resolved from its event's `Period` by role.
//!
//! On the wire a timeline is `{ events, moments }`, each moment carrying its
//! resolved display date denormalized; the heavy event data stays referenced by
//! `event_index`.
//!
//! The opaque type keeps that choice reversible: the store and wire form could
//! become events-only, derived per client, behind the stable `events`/`moments`
//! methods.

use std::borrow::Cow;

use schemars::JsonSchema;
use serde::de::{self, DeserializeOwned};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::date::UncertainDate;
use crate::moment::{TransitionRole, decompose, topological_order};

use super::{
    Bounded, EventDetail, InteriorEvent, Period, TimelineEvent, dated_bound, has_date,
    interior_event_bounds,
};

/// One position in a timeline's ordered moment sequence: which event it projects
/// (`event_index` into [`Timeline::events`]), the transition role, whether a
/// both-undated durational collapsed to one bare token, and whether this token
/// renders the event's secondary text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MomentToken {
    pub event_index: usize,
    pub role: TransitionRole,
    pub collapsed: bool,
    pub carries_description: bool,
}

/// One moment resolved for display: its role, the collapsed/description flags,
/// the endpoint date looked up from the event's `Period` by role, and the
/// back-reference to the data-carrying event.
#[derive(Debug)]
pub struct MomentView<'a, EvtId, ImgId> {
    pub role: TransitionRole,
    pub collapsed: bool,
    pub carries_description: bool,
    pub date: Option<&'a Bounded<UncertainDate, ImgId>>,
    pub event: &'a TimelineEvent<EvtId, ImgId>,
    pub event_index: usize,
}

/// An entity's lifecycle: the data-carrying events stored once, and the ordered
/// moment sequence over them stored once. Opaque — consumers read [`events`] and
/// [`moments`], not the internal representation, so the storage can change
/// without breaking them.
///
/// [`events`]: Timeline::events
/// [`moments`]: Timeline::moments
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timeline<EvtId, ImgId> {
    events: Vec<TimelineEvent<EvtId, ImgId>>,
    order: Vec<MomentToken>,
}

impl<EvtId, ImgId> Timeline<EvtId, ImgId> {
    /// Build the canonical store from the assembled events: the ordering is the
    /// (A)-collapsed moment sequence over them, stored as owned tokens.
    pub(crate) fn build(events: Vec<TimelineEvent<EvtId, ImgId>>) -> Self {
        let order = topological_order(decompose(&events))
            .into_iter()
            .map(|moment| MomentToken {
                event_index: moment.event_index,
                role: moment.role,
                collapsed: moment.collapsed,
                carries_description: moment.carries_description,
            })
            .collect();
        Self { events, order }
    }

    /// The data-carrying events, in their deterministic structural order
    /// (construction first, interiors by id, demolition last).
    pub fn events(&self) -> &[TimelineEvent<EvtId, ImgId>] {
        &self.events
    }

    /// The ordered display moments, each resolved to its role, flags, date, and
    /// the event it projects.
    pub fn moments(&self) -> impl Iterator<Item = MomentView<'_, EvtId, ImgId>> {
        self.order.iter().filter_map(move |token| {
            let event = self.events.get(token.event_index)?;
            Some(MomentView {
                role: token.role,
                collapsed: token.collapsed,
                carries_description: token.carries_description,
                date: resolve_moment_date(token.role, &event.detail),
                event,
                event_index: token.event_index,
            })
        })
    }
}

/// The date a moment displays, looked up from its event's `Period` by role: a
/// durational start role reads `started`, an end role reads `completed`, a point
/// event reads its instant, and an ambiguous event takes the first dated of its
/// [`interior_event_bounds`]. Mirrors how [`decompose`] dated each endpoint.
fn resolve_moment_date<'a, EvtId, ImgId>(
    role: TransitionRole,
    detail: &'a EventDetail<EvtId, ImgId>,
) -> Option<&'a Bounded<UncertainDate, ImgId>> {
    let endpoint = |period: &'a Period<ImgId>| {
        if role.durational_end().is_some() {
            dated_bound(&period.started)
        } else {
            dated_bound(&period.completed)
        }
    };
    match detail {
        EventDetail::Constructed { period, .. } | EventDetail::Demolished { period } => {
            endpoint(period)
        }
        EventDetail::Existed { at } => dated_bound(at),
        EventDetail::Interior { kind, .. } => match kind {
            InteriorEvent::Modified { period }
            | InteriorEvent::Repaired { period }
            | InteriorEvent::Damaged { period, .. }
            | InteriorEvent::Moved { period, .. } => endpoint(period),
            InteriorEvent::UsageChanged { at, .. } | InteriorEvent::Designated { at, .. } => {
                dated_bound(at)
            }
            InteriorEvent::Ambiguous { .. } => interior_event_bounds(kind)
                .into_iter()
                .find(|b| has_date(b)),
        },
    }
}

// ----------------------------------------------------------------------------
// Wire form: `{ events, moments }` with each moment's date denormalized.
// ----------------------------------------------------------------------------

/// The wire form of one moment: its role, the display date denormalized off the
/// event's `Period`, the collapsed/description flags, and the `event_index`
/// back-reference into `events`.
#[derive(Serialize, Deserialize, JsonSchema)]
struct TimelineMoment {
    role: TransitionRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    date: Option<UncertainDate>,
    collapsed: bool,
    carries_description: bool,
    event_index: usize,
}

/// The wire form of a [`Timeline`]. Drives [`Timeline`]'s [`Deserialize`] and
/// [`JsonSchema`]; the [`Serialize`] impl emits the same `{ events, moments }`
/// shape, resolving each moment's date on the way out.
#[derive(Deserialize, JsonSchema)]
#[serde(bound(deserialize = "EvtId: DeserializeOwned, ImgId: DeserializeOwned"))]
struct TimelineWire<EvtId, ImgId> {
    events: Vec<TimelineEvent<EvtId, ImgId>>,
    moments: Vec<TimelineMoment>,
}

impl<EvtId, ImgId> Serialize for Timeline<EvtId, ImgId>
where
    EvtId: Serialize,
    ImgId: Serialize,
{
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let moments: Vec<TimelineMoment> = self
            .moments()
            .map(|moment| TimelineMoment {
                role: moment.role,
                date: moment.date.map(|b| b.possible.clone()),
                collapsed: moment.collapsed,
                carries_description: moment.carries_description,
                event_index: moment.event_index,
            })
            .collect();
        let mut state = serializer.serialize_struct("Timeline", 2)?;
        state.serialize_field("events", &self.events)?;
        state.serialize_field("moments", &moments)?;
        state.end()
    }
}

impl<'de, EvtId, ImgId> Deserialize<'de> for Timeline<EvtId, ImgId>
where
    EvtId: DeserializeOwned,
    ImgId: DeserializeOwned,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let TimelineWire { events, moments } =
            TimelineWire::<EvtId, ImgId>::deserialize(deserializer)?;
        let order = moments
            .into_iter()
            .map(|moment| {
                if moment.event_index >= events.len() {
                    return Err(<D::Error as de::Error>::custom(format!(
                        "timeline moment references event_index {} of {} events",
                        moment.event_index,
                        events.len()
                    )));
                }
                Ok(MomentToken {
                    event_index: moment.event_index,
                    role: moment.role,
                    collapsed: moment.collapsed,
                    carries_description: moment.carries_description,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Timeline { events, order })
    }
}

impl<EvtId, ImgId> JsonSchema for Timeline<EvtId, ImgId>
where
    EvtId: JsonSchema,
    ImgId: JsonSchema,
{
    fn schema_name() -> String {
        format!(
            "Timeline_for_{}_and_{}",
            EvtId::schema_name(),
            ImgId::schema_name()
        )
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Owned(format!(
            "chronoscope_core::typed::Timeline<{}, {}>",
            EvtId::schema_id(),
            ImgId::schema_id()
        ))
    }

    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        <TimelineWire<EvtId, ImgId>>::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::lattice::JoinSemilattice;
    use crate::date::DatePrecision;
    use crate::typed::Consensus;
    use chrono::{Datelike, NaiveDate};

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type Event = TimelineEvent<(), ()>;

    fn dated(year: i32) -> Result<Bounded<UncertainDate, ()>, Box<dyn std::error::Error>> {
        let dt = NaiveDate::from_ymd_opt(year, 1, 1).ok_or("invalid date")?;
        let value = UncertainDate::with_precision(dt, DatePrecision::Year)?;
        Ok(Bounded {
            possible: value.clone(),
            sources: Vec::new(),
            facts: Vec::new(),
            consensus: Consensus::Reached { value },
        })
    }

    /// An untouched slot: the honest ⊥ with an `Absent` consensus, for any field
    /// lattice.
    fn absent_bounded<V: JoinSemilattice>() -> Bounded<V, ()> {
        Bounded {
            possible: V::bottom(),
            sources: Vec::new(),
            facts: Vec::new(),
            consensus: Consensus::Absent,
        }
    }

    fn absent() -> Bounded<UncertainDate, ()> {
        absent_bounded()
    }

    fn constructed(
        started: Bounded<UncertainDate, ()>,
        completed: Bounded<UncertainDate, ()>,
    ) -> Event {
        TimelineEvent {
            detail: EventDetail::Constructed {
                period: Period { started, completed },
                location: absent_bounded(),
            },
            sources: Vec::new(),
        }
    }

    fn usage_changed(at: Bounded<UncertainDate, ()>) -> Event {
        TimelineEvent {
            detail: EventDetail::Interior {
                id: (),
                descriptions: Vec::new(),
                kind: InteriorEvent::UsageChanged {
                    at,
                    usages: absent_bounded(),
                },
            },
            sources: Vec::new(),
        }
    }

    /// The moment view as `(role, collapsed, carries_description, date-year)`.
    fn views(timeline: &Timeline<(), ()>) -> Vec<(TransitionRole, bool, bool, Option<i32>)> {
        timeline
            .moments()
            .map(|m| {
                (
                    m.role,
                    m.collapsed,
                    m.carries_description,
                    m.date.and_then(|b| b.possible.earliest()).map(|d| d.year()),
                )
            })
            .collect()
    }

    /// Notre-Dame: both construction dates known plus a mid-life usage change —
    /// the usage change interleaves between the endpoints and each moment
    /// resolves its own date. The completion carries the description.
    #[test]
    fn both_dated_construction_interleaves_and_resolves_dates() -> TestResult {
        let timeline = Timeline::build(vec![
            constructed(dated(1163)?, dated(1345)?),
            usage_changed(dated(1200)?),
        ]);
        assert_eq!(
            views(&timeline),
            vec![
                (TransitionRole::ConstructionStart, false, false, Some(1163)),
                (TransitionRole::UsageModified, false, true, Some(1200)),
                (TransitionRole::ConstructionEnd, false, true, Some(1345)),
            ],
            "both-dated construction emits two interleaved endpoints; completion carries the description"
        );
        Ok(())
    }

    /// Mole Antonelliana: construction dated only at completion. The (A) collapse
    /// drops the undated start — no phantom row — and the usage change sorts
    /// before the completion by date.
    #[test]
    fn completed_only_construction_drops_the_undated_start() -> TestResult {
        let timeline = Timeline::build(vec![
            constructed(absent(), dated(1889)?),
            usage_changed(dated(1888)?),
        ]);
        assert_eq!(
            views(&timeline),
            vec![
                (TransitionRole::UsageModified, false, true, Some(1888)),
                (TransitionRole::ConstructionEnd, false, true, Some(1889)),
            ],
            "a completion-only construction emits only the dated end, before the earlier usage change"
        );
        Ok(())
    }

    /// A start-only durational emits its dated start alone, carrying the
    /// description — no dateless completion row.
    #[test]
    fn started_only_durational_emits_single_dated_start() -> TestResult {
        let timeline = Timeline::build(vec![constructed(dated(1850)?, absent())]);
        assert_eq!(
            views(&timeline),
            vec![(TransitionRole::ConstructionStart, false, true, Some(1850))],
            "a start-only construction emits the dated start alone, carrying the description"
        );
        Ok(())
    }

    /// A both-undated durational collapses to one bare token with no date.
    #[test]
    fn both_undated_durational_collapses_to_one_bare_token() -> TestResult {
        let timeline = Timeline::build(vec![constructed(absent(), absent())]);
        assert_eq!(
            views(&timeline),
            vec![(TransitionRole::ConstructionStart, true, true, None)],
            "a both-undated construction collapses to one bare, dateless token"
        );
        Ok(())
    }

    /// The wire form is `{ events, moments }`, each moment carrying its resolved
    /// date denormalized; it survives a JSON round-trip.
    #[test]
    fn wire_shape_denormalizes_dates_and_round_trips() -> TestResult {
        let timeline = Timeline::build(vec![
            constructed(absent(), dated(1889)?),
            usage_changed(dated(1888)?),
        ]);
        let json = serde_json::to_value(&timeline)?;

        let events = json
            .get("events")
            .and_then(|v| v.as_array())
            .ok_or("wire carries an events array")?;
        assert_eq!(events.len(), 2, "both events serialize once");

        let moments = json
            .get("moments")
            .and_then(|v| v.as_array())
            .ok_or("wire carries a moments array")?;
        let first = moments.first().ok_or("at least one moment")?;
        assert_eq!(
            first.get("role").and_then(|r| r.as_str()),
            Some("usage_modified"),
            "the moment's role serializes snake_case"
        );
        assert!(
            first.get("date").is_some(),
            "the moment denormalizes its resolved display date"
        );
        assert_eq!(
            first.get("event_index").and_then(|i| i.as_u64()),
            Some(1),
            "the moment back-references its event by index"
        );

        let back: Timeline<(), ()> = serde_json::from_value(json)?;
        assert_eq!(back, timeline, "the timeline survives a JSON round-trip");
        Ok(())
    }

    /// A wire moment whose `event_index` is out of range is rejected loudly at
    /// the deserialize boundary rather than surfacing a dangling moment.
    #[test]
    fn deserialize_rejects_out_of_range_event_index() -> TestResult {
        let json = serde_json::json!({
            "events": [],
            "moments": [
                { "role": "construction_start", "collapsed": true, "carries_description": true, "event_index": 0 }
            ]
        });
        let result: Result<Timeline<(), ()>, _> = serde_json::from_value(json);
        assert!(
            result.is_err(),
            "a moment referencing a nonexistent event must be rejected"
        );
        Ok(())
    }
}
