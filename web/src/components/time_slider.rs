//! The map's time control — rewind the map to a historical moment.
//!
//! Drives the `as_of` instant every tile read reports existence at. The map owns
//! the signal (see [`TimeControl`]); this component is its UI, so the slider can
//! sit anywhere in the page layout while the map stays the single source of the
//! instant.
//!
//! A range input is linear and the axis is piecewise, so the track carries
//! [`TrackUnit`]s and this component converts at the edges (see
//! [`crate::time_scale`]).

use chrono::{Datelike, NaiveDate};
use leptos::prelude::*;

use crate::components::existence_legend::ExistenceLegend;
use crate::components::map::{SelectedEntity, TimeControl};
use crate::components::map_card::{Corner, MapCard};
use crate::time_scale::{LabelRoom, TickKind, TimeScale, TrackUnit, era_label};

/// The year chip's box. Fixed width, so the track row's left inset can clear
/// both it and its pointer overhang, and so the readout holds still while the
/// track appears beside it. Sized for the widest label the axis can show,
/// "2000 BCE".
const CHIP_BOX: &str = "rounded-full w-28 h-9 text-sm font-medium";

/// A uniform track and a plain round thumb, across the vendor pseudo-elements.
///
/// The browser default fills the track up to the thumb in the accent colour,
/// which reads as a meaningful quantity — it isn't one, and it collided with the
/// palette the markers use. One flat colour says "position on an axis" and
/// nothing more.
const TRACK_AND_THUMB: &str = "\
    appearance-none bg-transparent \
    [&::-webkit-slider-runnable-track]:h-1.5 \
    [&::-webkit-slider-runnable-track]:rounded-full \
    [&::-webkit-slider-runnable-track]:bg-sepia/25 \
    [&::-webkit-slider-thumb]:appearance-none \
    [&::-webkit-slider-thumb]:w-4 [&::-webkit-slider-thumb]:h-4 \
    [&::-webkit-slider-thumb]:-mt-[5px] \
    [&::-webkit-slider-thumb]:rounded-full \
    [&::-webkit-slider-thumb]:bg-sepia \
    [&::-moz-range-track]:h-1.5 \
    [&::-moz-range-track]:rounded-full \
    [&::-moz-range-track]:bg-sepia/25 \
    [&::-moz-range-thumb]:w-4 [&::-moz-range-thumb]:h-4 \
    [&::-moz-range-thumb]:border-0 \
    [&::-moz-range-thumb]:rounded-full \
    [&::-moz-range-thumb]:bg-sepia";

/// The year a date falls in, for the slider's handle position.
fn year_of(date: NaiveDate) -> i32 {
    date.year()
}

/// A year as the instant the map reads it at: mid-year, so a marker dated only
/// to a year reads as present across that whole year rather than hinging on
/// January 1st.
fn instant_for_year(year: i32) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(year, 7, 1)
}

/// Where a track unit sits along the track, as CSS.
///
/// The thumb is 16 px wide, so its centre travels `[8px, width - 8px]`, and a
/// mark on that same inset lands exactly where the thumb reads its year. The
/// naive fraction is off by up to 8 px, worst at the anchors it labels.
fn mark_offset(unit: TrackUnit, total: TrackUnit) -> String {
    format!(
        "left: calc(8px + (100% - 16px) * {} / {}); transform: translateX(-50%)",
        unit.0, total.0
    )
}

/// Whether a named year's label is drawn, given how much track it needs.
///
/// A 320 px viewport leaves the track 144 px, on which the floor's "2000 BCE"
/// reaches most of the way to the era seam's mark a fifth along. From 400 px
/// the seam's year clears both neighbours by half its own width. Its mark
/// stands either way, so a narrow axis keeps its shape and loses one word.
///
/// Both classes are literals because Tailwind scans source text: the call site
/// interpolates one into the label's class, and a name built by `format!` gets
/// no rule in the stylesheet.
const fn label_visibility(room: LabelRoom) -> &'static str {
    match room {
        LabelRoom::Any => "block",
        LabelRoom::WideTrack => "hidden min-[400px]:block",
    }
}

/// The box the card opens into, and the band inside it that scrolls.
///
/// The slider row is deliberately outside the scroller. It is the control the
/// card exists for, and its chip is pinned to the card's corner, so scrolling
/// the track away from its own year readout is the one arrangement to avoid.
const BOX_WIDTH: &str = "w-[min(90vw,24rem)]";
const SCROLLING_BAND: &str = "max-h-[min(55vh,22rem)] overflow-y-auto px-4 pt-4 pb-2";

/// Where this card stands while a detail panel is open, and where it steps out.
///
/// The two want the same pixels below a width their own boxes name: this card is
/// 24rem at the column's 12 px inset, the panel is `md:w-96`, also 24rem, so
/// they first clear each other at 12 + 384 + 384 = 780. Tailwind's `md` is 768,
/// twelve pixels short, and the card would sit on the panel's left edge over
/// that dozen pixels.
///
/// A literal because Tailwind scans source text: a breakpoint arrived at by
/// `format!` gets no rule in the stylesheet.
const CLEARS_THE_DETAIL_PANEL: &str = "hidden min-[780px]:block";

/// The time slider: a year scrubber over the map, and what its markers mean.
///
/// Grows upward from the year chip, so its rows read bottom-up: the track sits
/// on the bottom row with the chip over its left end, the legend above that,
/// and the blurb above that. The chip is the toggle, and [`MapCard`] pins the
/// toggle to the card's bottom-left corner in both states, so that order is the
/// one the corner allows.
///
/// The year labels ride *above* the track for the same reason. The track has to
/// share the chip's centre line to read as one row with the year, which leaves
/// nothing under it but the card's own edge.
///
/// Starts expanded: a control nobody notices is a feature nobody uses, and
/// rewinding is the whole point of the map.
///
/// There is no "now" button. Scrubbing to the right edge is the same gesture
/// and the track's far end is never more than one drag away.
#[component]
pub fn TimeSlider() -> impl IntoView {
    let Some(control) = use_context::<TimeControl>() else {
        // The map provides the control; without it there is no instant to drive.
        return ().into_any();
    };
    let SelectedEntity(selected, _) = expect_context::<SelectedEntity>();

    // The map's single sample, not a fresh one: a second `today()` could straddle
    // midnight and put the track's right edge on a different day than the map's.
    let now = control.now;
    let max_year = year_of(now);
    let scale = TimeScale::new(max_year);
    let total = scale.total_units();
    let expanded = RwSignal::new(true);
    let year = Signal::derive(move || year_of(control.as_of.get()));

    let set_unit = move |value: u32| {
        let unit = TrackUnit(value.min(total.0));
        // At the right edge, snap to today rather than mid-year, so scrubbing
        // there lands on exactly the default view.
        let instant = if unit == total {
            Some(now)
        } else {
            instant_for_year(scale.year(unit))
        };
        if let Some(instant) = instant {
            control.set_as_of.set(instant);
        }
    };

    view! {
        // Yields the corner to the detail sheet only where they collide. On a
        // phone the sheet spans the bottom two thirds and this card draws after
        // it, so a card left standing would take the sheet's taps; once the two
        // fit side by side the year stays readable beside the panel.
        <div class=move || {
            if selected.get().is_some() { CLEARS_THE_DETAIL_PANEL } else { "block" }
        }>
            // The status and fetch-error chips share this corner and stack above the
            // bar, so an opaque panel here can neither hide an error nor swallow the
            // clicks on its Retry button.
            <MapCard
                corner=Corner::BottomLeft
                open=expanded
                chip_class=CHIP_BOX
                expand_label="Show the time slider"
                collapse_label="Hide the time slider"
                body_id="time-slider-panel"
                body_radius="rounded-2xl"
                chip=move || view! {
                    <span id="time-slider-year" class="tabular-nums">
                        {move || era_label(year.get())}
                    </span>
                    // Points the way the bar moves: right to open, left to close.
                    // Drawn rather than set as a glyph, for the same reason as the
                    // nav's icon — a text chevron is tiny and faint at this size.
                    <svg
                        aria-hidden="true"
                        class=move || format!(
                            "w-3 h-3 shrink-0 text-sepia/70 transition-transform duration-150 {}",
                            if expanded.get() { "rotate-180" } else { "" }
                        )
                        viewBox="0 0 12 12"
                        fill="none"
                        stroke="currentColor"
                        stroke-width="1.75"
                        stroke-linecap="round"
                        stroke-linejoin="round"
                    >
                        <path d="M4.5 2.5L8 6l-3.5 3.5"/>
                    </svg>
                }
            >
                <div class=BOX_WIDTH>
                    <div class=SCROLLING_BAND>
                        <ExistenceLegend/>
                    </div>
                    // The left inset clears the chip, which sits over this row's
                    // left end rather than in this flow — and clears its widened
                    // pointer area too, which reaches 6 px past the chip's own
                    // width. An inset merely equal to that width leaves the
                    // overhang over the track's first pixels, where a click
                    // collapses the card instead of scrubbing to the earliest
                    // year.
                    //
                    // The rule above it sets the control apart from the legend
                    // that explains it, so the row you drag reads as its own
                    // thing.
                    <div class="pl-32 pr-4 pt-1 border-t border-sepia/15">
                        // Spans the track exactly, so the marks can be placed
                        // against the same box the thumb travels. The track row
                        // is the band's bottom 36 px, which is the chip's own
                        // height, so the year and the thumb share a centre line.
                        <div class="relative h-[42px] w-full">
                            // Before the input, so the thumb paints over the
                            // marks it passes.
                            <div aria-hidden="true" class="absolute inset-0 pointer-events-none">
                                {move || {
                                    scale.ticks().into_iter().map(|tick| {
                                        let offset = mark_offset(tick.unit, total);
                                        match tick.kind {
                                            TickKind::Named(room) => {
                                                // The years share one line, close
                                                // enough above the marks to read
                                                // as attached to them.
                                                let shown = label_visibility(room);
                                                view! {
                                                    <span
                                                        class=format!(
                                                            "absolute top-[2px] {shown} \
                                                             text-[9px] leading-none \
                                                             tabular-nums text-sepia/70"
                                                        )
                                                        style=offset.clone()
                                                    >
                                                        {era_label(tick.year)}
                                                    </span>
                                                    // Runs from the track up to
                                                    // the label's line, so the
                                                    // two read as one mark.
                                                    <span
                                                        class="absolute top-[13px] h-[17px] \
                                                               w-px bg-sepia/50"
                                                        style=offset
                                                    ></span>
                                                }.into_any()
                                            }
                                            // Concentric with the named marks and
                                            // lighter, so the subdivisions recede
                                            // behind the years that explain them.
                                            TickKind::Minor => view! {
                                                <span
                                                    class="absolute top-[20px] h-2 w-px \
                                                           bg-sepia/30"
                                                    style=offset
                                                ></span>
                                            }.into_any(),
                                        }
                                    }).collect::<Vec<_>>()
                                }}
                            </div>
                            <div class="absolute inset-x-0 bottom-0 h-9 flex items-center">
                                <input
                                    id="time-slider"
                                    type="range"
                                    class=format!("relative w-full cursor-pointer {TRACK_AND_THUMB}")
                                    min="0"
                                    max=total.0.to_string()
                                    step="1"
                                    prop:value=move || scale.position(year.get()).0.to_string()
                                    aria-label="Year to show the map as of"
                                    // A screen reader reads the value, which is a
                                    // track unit, so the year it announces has to
                                    // come from here.
                                    aria-valuetext=move || era_label(year.get())
                                    // The browser's own year, so a test can build
                                    // the same axis without a clock of its own.
                                    data-max-year=max_year.to_string()
                                    on:input=move |ev| {
                                        if let Ok(value) = event_target_value(&ev).parse::<u32>() {
                                            set_unit(value);
                                        }
                                    }
                                />
                            </div>
                        </div>
                    </div>
                </div>
            </MapCard>
        </div>
    }
    .into_any()
}
