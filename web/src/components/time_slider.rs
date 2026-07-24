//! The map's time control — rewind the map to a historical moment.
//!
//! Drives the `as_of` instant every tile read reports existence at. The map owns
//! the signal (see [`TimeControl`]); this component is its UI, so the slider can
//! sit anywhere in the page layout while the map stays the single source of the
//! instant.

use chrono::{Datelike, NaiveDate};
use leptos::prelude::*;

use crate::components::map::TimeControl;

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

/// The time slider: a year scrubber over the map, with a reset to the present.
///
/// Collapses to a single pill so it costs no map space when unused. It starts
/// expanded — a control nobody notices is a feature nobody uses, and rewinding
/// is the whole point of the map.
#[component]
pub fn TimeSlider() -> impl IntoView {
    let Some(control) = use_context::<TimeControl>() else {
        // The map provides the control; without it there is no instant to drive.
        return ().into_any();
    };

    // The map's single sample, not a fresh one: a second `today()` could straddle
    // midnight and leave the Now button enabled on a freshly loaded map.
    let now = control.now;
    let max_year = year_of(now);
    let (expanded, set_expanded) = signal(true);
    let year = Signal::derive(move || year_of(control.as_of.get()));
    let is_now = Signal::derive(move || control.as_of.get() == now);

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
        // Clears the map status and fetch-error chips, which sit at bottom-3 in
        // this same corner — an opaque panel over them would hide the error and
        // swallow clicks on its Retry button.
        <div class="absolute bottom-14 left-4 z-10">
            <Show
                when=move || expanded.get()
                fallback=move || {
                    view! {
                        <button
                            type="button"
                            class="rounded-full bg-parchment/95 shadow-lg border border-sepia/20
                                   px-4 py-2 text-sm font-medium text-sepia tabular-nums
                                   hover:bg-parchment"
                            aria-label="Show the time slider"
                            on:click=move |_| set_expanded.set(true)
                        >
                            {move || year.get().to_string()}
                        </button>
                    }
                }
            >
                <div class="rounded-xl bg-parchment/95 shadow-lg border border-sepia/20
                            px-4 py-3 w-[min(80vw,22rem)]">
                    <div class="flex items-center justify-between gap-3 mb-2">
                        <span
                            id="time-slider-year"
                            class="text-lg font-semibold text-sepia tabular-nums"
                        >
                            {move || year.get().to_string()}
                        </span>
                        <div class="flex items-center gap-1">
                            <button
                                type="button"
                                class="text-xs px-2 py-1 rounded border border-sepia/30
                                       text-sepia hover:bg-sepia/10 disabled:opacity-40"
                                disabled=move || is_now.get()
                                aria-label="Reset the map to the present day"
                                on:click=move |_| control.set_as_of.set(now)
                            >
                                "Now"
                            </button>
                            <button
                                type="button"
                                class="text-sepia/60 hover:text-sepia px-1.5 py-1 rounded
                                       hover:bg-sepia/10 leading-none"
                                aria-label="Hide the time slider"
                                on:click=move |_| set_expanded.set(false)
                            >
                                "\u{00d7}"
                            </button>
                        </div>
                    </div>

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
            </Show>
        </div>
    }
    .into_any()
}
