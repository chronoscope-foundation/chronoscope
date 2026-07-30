//! The collapsible card the map's corner overlays share.
//!
//! The About card and the time slider are the same shape: a panel pinned to a
//! map corner that collapses to a chip. Grown separately, they disagreed on
//! radius, shadow, border, blur, close glyph and hit target, and each let its
//! toggle land somewhere different once expanded — so the control moved out
//! from under the pointer every time you used it.
//!
//! Two properties keep the toggle still, and both are needed. It is one button
//! that never unmounts, so the states cannot be styled apart. And it is
//! positioned against the corner-anchored container rather than laid out by the
//! body, so its box cannot depend on whether the body is there or how big it
//! is. A toggle sitting in the body's flow satisfies the first and still moves.

use leptos::prelude::*;

use crate::components::controls::{FOCUS_RING, HIT_AREA, SURFACE};
use crate::components::motion::REVEAL;

/// The map corner a card pins to.
///
/// Only the two edges the corner names are set, so the card sizes to its body
/// and grows away from the corner while the corner itself stays put.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    TopRight,
    BottomLeft,
}

impl Corner {
    /// Pins the container to the map.
    const fn anchor(self) -> &'static str {
        match self {
            // Clears the entity detail panel, which slides in below it. Sits
            // lower on narrow screens, where the card spans nearly the full
            // width and would otherwise run under the nav trigger — the trigger
            // is opaque and would cover the card's own heading.
            Corner::TopRight => "absolute top-14 right-3 md:top-3 z-10",
            // Flush to the corner, on the same 12 px inset every other floating
            // gizmo uses. The map's status chips share this corner and now stack
            // above it, rather than the bar being pushed up to clear them.
            Corner::BottomLeft => "absolute bottom-3 left-3 z-10",
        }
    }

    /// Pins the toggle to that same corner inside the container.
    const fn toggle_anchor(self) -> &'static str {
        match self {
            Corner::TopRight => "absolute top-0 right-0",
            Corner::BottomLeft => "absolute bottom-0 left-0",
        }
    }
}

/// A map overlay that collapses to a chip, with a toggle that holds its place.
///
/// The caller owns `open`, and with it any persistence: the About card
/// remembers a dismissal across visits, the slider deliberately does not.
#[component]
pub fn MapCard(
    corner: Corner,
    open: RwSignal<bool>,
    /// Sizes and shapes the toggle. Applied identically in both states, so the
    /// box is the caller's choice but not the state's.
    chip_class: &'static str,
    /// Accessible name for the toggle while collapsed.
    expand_label: &'static str,
    /// Accessible name for the toggle while expanded.
    collapse_label: &'static str,
    /// Names the body the toggle discloses: `aria-controls` while that body is
    /// mounted, and the stem of the toggle's own id.
    body_id: &'static str,
    /// The body's corner radius. A tall panel and a single-row bar want
    /// different curves, and the bar's has to match its chip's to read as one
    /// shape rather than a chip sitting on a card.
    body_radius: &'static str,
    /// The toggle's content. Free to change with `open`; its box is not.
    #[prop(into)]
    chip: ViewFn,
    /// The body, rendered only while expanded. Owns its own padding — the two
    /// cards legitimately want different insets.
    children: ChildrenFn,
) -> impl IntoView {
    // The box is fixed across states; the surface is not, and shouldn't be.
    // Collapsed, the chip *is* the card and carries the surface. Expanded, it is
    // a control sitting inside one, so a second background and ring there would
    // draw a seam across the card's own corner.
    let toggle_base = format!(
        "{} {FOCUS_RING} {HIT_AREA} {chip_class} \
         flex items-center justify-center gap-1 cursor-pointer \
         text-sepia hover:text-ink font-sans",
        corner.toggle_anchor()
    );
    let toggle_class = move || {
        let surface = if open.get() { "" } else { SURFACE };
        format!("{toggle_base} {surface}")
    };
    let body_class = format!("{SURFACE} {body_radius}");
    // The control's own name, alongside the body's, so the toggle stays
    // addressable in both states: the browser tests follow one across a collapse
    // by it.
    let toggle_id = format!("{body_id}-toggle");

    view! {
        <div class=corner.anchor()>
            <Show when=move || open.get()>
                {
                    // `Show` takes a reactive `Fn` children, so the closure may
                    // not move the handle out. Clone it into this block local.
                    let children = children.clone();
                    // The surface and its contents animate separately: the
                    // surface has to be opaque from the first frame, or the
                    // chip's own area goes unpainted while it fades.
                    view! {
                        <div id=body_id class=body_class.clone()>
                            <div class=REVEAL>{children()}</div>
                        </div>
                    }
                }
            </Show>
            <button
                type="button"
                id=toggle_id
                class=toggle_class
                aria-expanded=move || if open.get() { "true" } else { "false" }
                // Only while the body is there to point at: a reader follows
                // this reference from the collapsed chip, which is exactly when
                // the body has left the DOM.
                aria-controls=move || open.get().then_some(body_id)
                aria-label=move || if open.get() { collapse_label } else { expand_label }
                on:click=move |_| open.update(|shown| *shown = !*shown)
            >
                {chip.run()}
            </button>
        </div>
    }
}
