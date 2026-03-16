use leptos::prelude::*;
use leptos_router::components::A;

const NAV_LINK_CLASS: &str = "block px-3 py-2 rounded-md text-sm font-sans font-medium text-sepia hover:text-ink hover:bg-ink/5 transition-colors";

#[component]
pub fn Sidebar() -> impl IntoView {
    let (mobile_open, set_mobile_open) = signal(false);

    let close_mobile = move |_| set_mobile_open.set(false);

    let nav_content = move || {
        view! {
            <div class="flex flex-col h-full">
                // Wordmark
                <div class="px-5 pt-6 pb-8">
                    <A href="/" attr:class="text-xl font-bold tracking-tight" on:click=close_mobile>
                        "Chronoscope"
                    </A>
                    <p class="text-sepia text-xs mt-1 font-sans">"Connecting places through time"</p>
                </div>

                // Navigation links
                <nav class="flex-1 px-3">
                    <A href="/" attr:class=NAV_LINK_CLASS on:click=close_mobile>"Explore"</A>
                    <A href="/about" attr:class=NAV_LINK_CLASS on:click=close_mobile>"About"</A>
                    <A href="/faq" attr:class=NAV_LINK_CLASS on:click=close_mobile>"FAQ"</A>
                </nav>

                // Footer area
                <div class="px-5 py-4 border-t border-sepia/15">
                    <a
                        href="https://github.com/copumpkin/chronoscope"
                        class="text-sepia/60 hover:text-ink text-xs font-sans transition-colors"
                        target="_blank"
                        rel="noopener noreferrer"
                    >
                        "GitHub"
                    </a>
                    <span class="text-sepia/30 text-xs font-sans">" \u{00b7} "</span>
                    <span class="text-sepia/40 text-xs font-sans">"MIT License"</span>
                </div>
            </div>
        }
    };

    view! {
        // Desktop sidebar
        <aside class="hidden md:block w-60 bg-parchment border-r border-sepia/15 shrink-0">
            {nav_content()}
        </aside>

        // Mobile: top bar + slide-out drawer.
        // Both desktop sidebar and mobile top bar exist in the DOM; Tailwind's
        // responsive prefixes (md:hidden / hidden md:block) use CSS media queries
        // at 768px to show one and hide the other. No JS breakpoint detection.
        <div class="md:hidden fixed top-0 left-0 right-0 z-50 bg-parchment/95 backdrop-blur-sm border-b border-sepia/20 h-14 flex items-center px-4">
            <button
                class="p-2 cursor-pointer"
                on:click=move |_| set_mobile_open.update(|v| *v = !*v)
                aria-label="Toggle menu"
                aria-expanded=move || mobile_open.get().to_string()
            >
                <span class="text-xl">
                    {move || if mobile_open.get() { "\u{2715}" } else { "\u{2630}" }}
                </span>
            </button>
            <A href="/" attr:class="ml-2 text-lg font-bold tracking-tight">
                "Chronoscope"
            </A>
        </div>

        // Mobile drawer overlay
        <div
            class="md:hidden fixed inset-0 z-40 transition-opacity duration-200"
            class:pointer-events-none=move || !mobile_open.get()
            style=move || if mobile_open.get() { "opacity: 1" } else { "opacity: 0" }
        >
            // Backdrop
            <div
                class="absolute inset-0 bg-ink/20"
                on:click=close_mobile
            ></div>
            // Drawer
            <aside
                class="absolute top-0 left-0 bottom-0 w-60 bg-parchment shadow-lg transition-transform duration-200"
                style=move || if mobile_open.get() { "transform: translateX(0)" } else { "transform: translateX(-100%)" }
            >
                {nav_content()}
            </aside>
        </div>

        // Mobile spacer for fixed top bar
        <div class="md:hidden h-14 shrink-0"></div>
    }
}
