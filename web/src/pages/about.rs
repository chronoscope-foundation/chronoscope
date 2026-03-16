use leptos::prelude::*;

use crate::markdown::md_to_html;

const ABOUT_MARKDOWN: &str = include_str!("../../content/about.md");

#[component]
pub fn About() -> impl IntoView {
    let html_content = md_to_html(ABOUT_MARKDOWN);

    view! {
        <article class="px-6 py-12 max-w-3xl mx-auto prose-chronoscope">
            <h1 class="text-3xl font-bold mb-8">"About Chronoscope"</h1>
            <div inner_html=html_content></div>
        </article>
    }
}
