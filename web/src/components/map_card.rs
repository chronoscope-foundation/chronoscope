//! The collapsible card the map's corner overlays share.
//!
//! The About card and the time slider are the same shape: a panel pinned to a
//! map corner that collapses to a chip. Grown separately, they disagreed on
//! radius, shadow, border, blur, close glyph and hit target, and each let its
//! toggle land somewhere different once expanded — so the control moved out
//! from under the pointer every time you used it.
//!
//! Two properties keep the toggle still, and both are needed. It is one button
//! that never unmounts, so the states cannot be styled apart. And it is aligned
//! to a corner of the container rather than laid out by the body, so its box
//! cannot depend on whether the body is there or how big it is. A toggle
//! sitting in the body's flow satisfies the first and still moves.
//!
//! The two share one grid cell, which is what makes the card's own box honest:
//! it measures the larger of the chip and the body, so a collapsed card laid
//! into a column takes exactly the chip's room. Stacking them by absolute
//! positioning instead left the container measuring zero whenever the body was
//! away, and everything above it in that column dropped onto the chip.

use leptos::prelude::*;

use crate::components::controls::{FOCUS_RING, HIT_AREA, OVERLAY_INFO, SURFACE};
use crate::components::motion::REVEAL;

/// The map corner a card pins to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    TopRight,
    BottomLeft,
}

impl Corner {
    /// Places the container, and positions it either way.
    ///
    /// A component applying [`HIT_AREA`] cannot let the `::before` escape to
    /// whatever positioned ancestor it lands under (see `controls.rs`), and in
    /// flow that ancestor is the full-map wrapper, which turns the toggle into a
    /// map-sized click target that swallows every marker click.
    ///
    /// It takes its own pointer events back either way too. A card is the
    /// interactive thing in a corner otherwise full of read-only status, so it
    /// has to survive an ancestor that waves clicks through to the map.
    fn anchor(self) -> String {
        match self {
            // Only the two edges the corner names are set, so the card sizes to
            // its body and grows away from the corner while the corner stays
            // put. Clears the entity detail panel, which slides in below it.
            // Sits lower on narrow screens, where the card spans nearly the full
            // width and would otherwise run under the nav trigger — the trigger
            // is opaque and would cover the card's own heading.
            //
            // The card in this corner is the app's one purely explanatory
            // overlay, which is the rung it takes.
            Corner::TopRight => {
                format!("absolute top-14 right-3 md:top-3 {OVERLAY_INFO} pointer-events-auto")
            }
            // A flow child of the column that owns the map's bottom-left corner,
            // so the chips above it move for whatever height this card takes
            // rather than being told a number. The column carries the rung.
            Corner::BottomLeft => "relative pointer-events-auto".to_string(),
        }
    }

    /// Sits the toggle in that same corner of the shared grid cell.
    const fn toggle_anchor(self) -> &'static str {
        match self {
            Corner::TopRight => "self-start justify-self-end",
            Corner::BottomLeft => "self-end justify-self-start",
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
    /// The body's corner radius. The chip sits over one corner of it, so the
    /// two curves meet there and the caller is the one that can see whether
    /// they should match.
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
    //
    // `relative` for the same reason `DismissButton` carries it: [`HIT_AREA`]
    // emits an absolutely positioned pseudo-element, and a control that hands
    // that anchoring to whatever happens to be positioned above it gets a hit
    // area somewhere else entirely.
    let toggle_base = format!(
        "{} col-start-1 row-start-1 relative {FOCUS_RING} {HIT_AREA} {chip_class} \
         flex items-center justify-center gap-1 cursor-pointer \
         text-sepia hover:text-ink font-sans",
        corner.toggle_anchor()
    );
    let toggle_class = move || {
        let surface = if open.get() { "" } else { SURFACE };
        format!("{toggle_base} {surface}")
    };
    // Shares the toggle's cell, so the card measures the larger of the two and
    // the toggle rides the corner of whichever that is.
    let body_class = format!("col-start-1 row-start-1 {SURFACE} {body_radius}");
    // The control's own name, alongside the body's, so the toggle stays
    // addressable in both states: the browser tests follow one across a collapse
    // by it.
    let toggle_id = format!("{body_id}-toggle");

    view! {
        <div class=format!("grid {}", corner.anchor())>
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
