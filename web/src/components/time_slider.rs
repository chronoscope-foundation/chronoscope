//! The map's time control — rewind the map to a historical moment.
//!
//! Drives the `as_of` instant every tile read reports existence at. The map owns
//! the signal (see [`TimeControl`]); this component is its UI, so the slider can
//! sit anywhere in the page layout while the map stays the single source of the
//! instant.

use chrono::{Datelike, NaiveDate};
use leptos::prelude::*;

use crate::components::map::TimeControl;
use crate::components::map_card::{Corner, MapCard};

/// The year chip's box. Fixed width, so the bar's left inset can clear both it
/// and its pointer overhang, and so the readout holds still while the track
/// appears beside it. Four tabular digits always, since `MIN_YEAR` is 1000.
const CHIP_BOX: &str = "rounded-full w-20 h-9 text-sm font-medium";

/// The earliest year the slider reaches. A fixed span keeps the axis stable as
/// you pan — the same handle position always means the same year. Scaling it
/// non-linearly toward recent centuries, and deriving the bounds from the data
/// in view, are both wanted later.
const MIN_YEAR: i32 = 1000;

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

/// The time slider: a year scrubber over the map.
///
/// One row — a year readout and a track — so it spends almost no map space on
/// its own background. Collapses to the readout alone, and starts expanded: a
/// control nobody notices is a feature nobody uses, and rewinding is the whole
/// point of the map.
///
/// There is no "now" button. Scrubbing to the right edge is the same gesture
/// and the track's far end is never more than one drag away.
#[component]
pub fn TimeSlider() -> impl IntoView {
    let Some(control) = use_context::<TimeControl>() else {
        // The map provides the control; without it there is no instant to drive.
        return ().into_any();
    };

    // The map's single sample, not a fresh one: a second `today()` could straddle
    // midnight and put the track's right edge on a different day than the map's.
    let now = control.now;
    let max_year = year_of(now);
    let expanded = RwSignal::new(true);
    let year = Signal::derive(move || year_of(control.as_of.get()));

    let set_year = move |value: i32| {
        let clamped = value.clamp(MIN_YEAR, max_year);
        // At the present year, snap to today rather than mid-year, so scrubbing
        // to the right edge lands on exactly the default view.
        let instant = if clamped == max_year {
            Some(now)
        } else {
            instant_for_year(clamped)
        };
        if let Some(instant) = instant {
            control.set_as_of.set(instant);
        }
    };

    view! {
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
            body_radius="rounded-full"
            chip=move || view! {
                <span id="time-slider-year" class="tabular-nums">
                    {move || year.get().to_string()}
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
            // One row, so the bar is mostly track. The left inset clears the
            // chip, which sits over this bar's left end rather than in this
            // flow — and clears its widened pointer area too, which reaches 6 px
            // past the chip's `w-20`. At exactly `pl-20` that overhang covered
            // the track's first pixels, so a click at the far left collapsed the
            // card instead of scrubbing to the earliest year.
            <div class="h-9 flex items-center pl-24 pr-4 w-[min(85vw,20rem)]">
                <input
                    id="time-slider"
                    type="range"
                    class=format!("w-full cursor-pointer {TRACK_AND_THUMB}")
                    min=MIN_YEAR.to_string()
                    max=max_year.to_string()
                    step="1"
                    prop:value=move || year.get().to_string()
                    aria-label="Year to show the map as of"
                    on:input=move |ev| {
                        if let Ok(value) = event_target_value(&ev).parse::<i32>() {
                            set_year(value);
                        }
                    }
                />
            </div>
        </MapCard>
    }
    .into_any()
}
