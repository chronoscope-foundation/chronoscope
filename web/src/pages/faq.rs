use leptos::prelude::*;
use leptos_router::hooks::use_location;

use crate::components::nav::NAV_CLEARANCE;
use crate::content::{FAQ, FaqEntry};

/// One accordion item: the question, the anchor it is deep-linked by, and the
/// answer under it.
///
/// Takes the whole [`FaqEntry`] rather than its three parts, so an item cannot
/// be built that opens on one question's anchor and answers with another's.
#[component]
fn FaqItem(entry: &'static FaqEntry) -> impl IntoView {
    let slug = entry.slug();
    // The router sets its URL signal as the incoming view is built and only
    // moves `window.location` once that build finishes, so the signal is the
    // hash source that is already correct here.
    let hash = use_location().hash;
    let targets_this_item = move |hash: &str| hash.strip_prefix('#') == Some(slug);

    // Seeded untracked so a deep-linked item is open on its first paint.
    let (open, set_open) = signal(targets_this_item(&hash.get_untracked()));
    let item_ref = NodeRef::<leptos::html::Div>::new();

    // A hash-only navigation rebuilds nothing, so a second anchor followed from
    // this same page has to expand reactively. The hash is the effect's only
    // trigger, so a manual collapse of the named item sticks.
    Effect::new(move || {
        if targets_this_item(&hash.get())
            && let Some(el) = item_ref.get()
        {
            set_open.set(true);
            el.scroll_into_view();
        }
    });

    view! {
        <div node_ref=item_ref id=slug class="border-b border-sepia/20">
            <button
                class="w-full py-5 flex justify-between items-center text-left cursor-pointer"
                on:click=move |_| set_open.update(|v| *v = !*v)
                aria-expanded=move || open.get().to_string()
            >
                <span class="text-lg font-semibold pr-4">{entry.question()}</span>
                <span class="text-sepia text-xl shrink-0 transition-transform duration-200 font-sans"
                    style=move || if open.get() { "transform: rotate(45deg)" } else { "" }
                >
                    "+"
                </span>
            </button>
            <div
                class="grid transition-[grid-template-rows] duration-200"
                style=move || if open.get() { "grid-template-rows: 1fr" } else { "grid-template-rows: 0fr" }
            >
                <div class="overflow-hidden">
                    <div class="pb-5 text-sepia leading-relaxed prose-chronoscope" inner_html=entry.answer_html().as_str()></div>
                </div>
            </div>
        </div>
    }
}

#[component]
pub fn Faq() -> impl IntoView {
    view! {
        <section class=format!("px-6 pb-12 max-w-3xl mx-auto {NAV_CLEARANCE}")>
            <h1 class="text-3xl font-bold mb-10">"Frequently Asked Questions"</h1>

            {FAQ.iter().map(|cat| view! {
                <div class="mb-10">
                    <h2 class="text-lg font-semibold text-copper mb-2 font-sans">{cat.title()}</h2>
                    <div class="divide-y divide-sepia/20">
                        {cat.entries().iter().map(|entry| view! {
                            <FaqItem entry=entry/>
                        }).collect::<Vec<_>>()}
                    </div>
                </div>
            }).collect::<Vec<_>>()}
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::existence_legend::EXISTENCE_STATES_ANCHOR;

    /// The map legend's deep link into the FAQ, checked against the anchors the
    /// FAQ assigns.
    ///
    /// Its own test rather than an entry in the build-time link guard: that one
    /// reads the markdown sources, and the legend's link is a Rust `view!` that
    /// no markdown parse can see. Both sides go through
    /// `EXISTENCE_STATES_ANCHOR`, so a renamed heading fails here rather than
    /// shipping a link to nowhere.
    ///
    /// Reads the compiled table rather than re-parsing, so what it checks is
    /// the anchor the page actually serves.
    #[test]
    fn the_map_legends_faq_link_names_a_real_anchor() {
        let slugs: Vec<&str> = FAQ
            .iter()
            .flat_map(|cat| cat.entries().iter().map(FaqEntry::slug))
            .collect();
        assert!(
            slugs.contains(&EXISTENCE_STATES_ANCHOR),
            "the map legend links to /faq#{EXISTENCE_STATES_ANCHOR}, which no FAQ heading anchors; anchors: {slugs:?}"
        );
    }
}
