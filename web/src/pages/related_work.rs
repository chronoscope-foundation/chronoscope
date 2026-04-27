use leptos::prelude::*;

use crate::markdown::md_to_html;

const RELATED_WORK_MARKDOWN: &str = include_str!("../../content/related-work.md");

#[component]
pub fn RelatedWork() -> impl IntoView {
    let html_content = md_to_html(RELATED_WORK_MARKDOWN);

    view! {
        <article class="px-6 py-12 max-w-3xl mx-auto prose-chronoscope">
            <h1 class="text-3xl font-bold mb-8">"Related work and where we fit"</h1>
            <div inner_html=html_content></div>
        </article>
    }
}
