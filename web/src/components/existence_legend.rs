//! What a marker's look means, drawn by the routine that draws the markers.
//!
//! The swatches are canvases rather than styled divs for a reason that bites
//! silently: Tailwind scans source text for class names, so a computed
//! `format!("bg-[{fill}]")` compiles, ships, and paints nothing. Going through
//! the map's own drawing routine also means the legend cannot describe a look
//! the map has stopped drawing.

use leptos::prelude::*;

use crate::components::map::{MarkerFill, MarkerLook, SWATCH_SIZE, draw_swatch};

/// The FAQ heading the blurb links into.
///
/// A constant because the anchor is a promise between two files nothing else
/// compares: the heading is markdown and the link is a `view!`, so a test can
/// only hold them together through a value both sides use.
pub const EXISTENCE_STATES_ANCHOR: &str = "existence-states";

/// Every look the map draws, and what each one says.
///
/// The badge row names what a badge *is* rather than what it concludes. A
/// cluster stands for several entities at once and carries no verdict of its
/// own, so there is nothing here for it to promise.
///
/// Nothing for an absent entity: it is removed from the map rather than styled,
/// so a row for it would carry no swatch, and the blurb says it in words.
const ROWS: [(MarkerLook, &str); 4] = [
    (MarkerLook::Standing, "Presumed to exist"),
    (MarkerLook::Disputed, "Sources disagree"),
    (MarkerLook::Unevidenced, "Known where, not when"),
    (MarkerLook::Badge, "A cluster; zoom in to split it"),
];

/// Gap (CSS px) between a row's two renderings.
const SWATCH_GAP: f64 = 4.0;

/// Width (CSS px) of the swatch column: both renderings and the gap between
/// them. One number, so the badge's single swatch still leaves its words on the
/// margin every other row's sit on.
const SWATCH_COLUMN: f64 = SWATCH_SIZE * 2.0 + SWATCH_GAP;

/// One marker, drawn at the size and in the style the map draws it.
#[component]
fn Swatch(look: MarkerLook, fill: MarkerFill) -> impl IntoView {
    let canvas = NodeRef::<leptos::html::Canvas>::new();
    // The node arrives after the first run, so this re-runs when the ref
    // resolves, as the map's own mount effect does.
    Effect::new(move |_| {
        if let Some(canvas) = canvas.get()
            && let Err(e) = draw_swatch(&canvas, look, fill)
        {
            web_sys::console::warn_1(&format!("legend swatch draw failed: {e}").into());
        }
    });

    view! {
        // Sized inline off the same constant the drawing uses: a Tailwind class
        // built by `format!` never reaches the stylesheet.
        <canvas
            node_ref=canvas
            aria-hidden="true"
            class="shrink-0"
            style=format!("width: {SWATCH_SIZE}px; height: {SWATCH_SIZE}px")
        />
    }
}

/// What the year does to the map, and what each marker on it means.
#[component]
pub fn ExistenceLegend() -> impl IntoView {
    view! {
        <p class="text-xs text-sepia leading-relaxed">
            "The map shows what stood at the year on the slider. "
            "Anything unbuilt or destroyed is omitted. "
            // A new tab, so reading the long answer costs nobody the year they
            // scrubbed to and the place they panned to.
            <a
                href=format!("/faq#{EXISTENCE_STATES_ANCHOR}")
                target="_blank"
                rel="noopener noreferrer"
                class="text-copper hover:text-ink underline underline-offset-2"
            >
                "Read more"
            </a>
        </p>
        <ul class="mt-2 space-y-1" aria-label="What each marker on the map means">
            {ROWS.into_iter().map(|(look, meaning)| view! {
                <li class="flex items-center gap-3">
                    // Fixed width whatever the row draws, and the drawings sit at
                    // its right edge, so every row's words start on one margin
                    // and every row's markers end on another.
                    <span
                        class="shrink-0 flex justify-end"
                        style=format!("width: {SWATCH_COLUMN}px")
                    >
                        // A tinted tile behind the drawings, standing in for the
                        // map under them. The unevidenced pin is a parchment
                        // wash, which on a parchment card would show only its
                        // rim, so the one look that is *about* letting the
                        // ground through would be the one look that couldn't.
                        <span
                            class="flex w-fit items-center rounded bg-sepia/15"
                            style=format!("gap: {SWATCH_GAP}px")
                        >
                            {look.fills().iter().map(|fill| view! {
                                <Swatch look=look fill=*fill/>
                            }).collect::<Vec<_>>()}
                        </span>
                    </span>
                    <span class="text-xs text-sepia leading-snug">{meaning}</span>
                </li>
            }).collect::<Vec<_>>()}
        </ul>
    }
}
