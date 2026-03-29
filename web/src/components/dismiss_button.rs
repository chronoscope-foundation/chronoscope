use leptos::prelude::*;

/// A dismiss/close button with a ✕ icon and WCAG-compliant 44px touch target.
///
/// Used for closing panels, dismissing banners, etc. Accepts a color class
/// for text styling (e.g., `"text-sepia/50 hover:text-ink"` for light
/// backgrounds, `"text-white/70 hover:text-white"` for dark backgrounds).
#[component]
pub fn DismissButton(
    on_click: impl Fn(leptos::ev::MouseEvent) + 'static,
    #[prop(default = "Close")] label: &'static str,
    #[prop(default = "text-sepia/50 hover:text-ink")] color: &'static str,
    #[prop(default = "")] extra_class: &'static str,
) -> impl IntoView {
    let class = format!(
        "min-w-[44px] min-h-[44px] flex items-center justify-center \
         text-lg leading-none cursor-pointer {color} {extra_class}"
    );
    view! {
        <button class=class on:click=on_click aria-label=label>
            "\u{2715}"
        </button>
    }
}
