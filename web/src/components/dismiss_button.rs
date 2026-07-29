use leptos::prelude::*;

use crate::components::controls::{FOCUS_RING, HIT_AREA};

/// A dismiss/close button with a ✕ icon.
///
/// Drawn at 32 px, which clears WCAG 2.5.8 (AA) on its own; [`HIT_AREA`]
/// carries the pointer target out past that so the small glyph stays easy to
/// hit. Accepts a color class for text styling (e.g.,
/// `"text-sepia/50 hover:text-ink"` for light backgrounds,
/// `"text-white/70 hover:text-white"` for dark backgrounds).
#[component]
pub fn DismissButton(
    on_click: impl Fn(leptos::ev::MouseEvent) + 'static,
    #[prop(default = "Close")] label: &'static str,
    #[prop(default = "text-sepia/50 hover:text-ink")] color: &'static str,
    #[prop(default = "")] extra_class: &'static str,
) -> impl IntoView {
    // `relative` is not decoration: [`HIT_AREA`] emits an absolutely positioned
    // pseudo-element, which anchors to the nearest positioned ancestor. Without
    // its own containing block this button's hit area escapes to whatever
    // happens to be positioned above it — inside the error banner's `fixed`
    // strip that made it a viewport-wide target wired to one row's dismiss.
    // A component cannot know what its host positions, so it owns this itself;
    // callers needing to place it wrap it rather than passing `absolute` here,
    // which would fight `relative` for the same property.
    let class = format!(
        "relative w-8 h-8 flex items-center justify-center rounded-full \
         text-lg leading-none cursor-pointer {HIT_AREA} {FOCUS_RING} \
         {color} {extra_class}"
    );
    view! {
        <button class=class on:click=on_click aria-label=label>
            "\u{2715}"
        </button>
    }
}
