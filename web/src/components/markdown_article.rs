//! Shared markdown article shell for the static content pages.

use leptos::prelude::*;

use crate::markdown::md_to_html;

/// The article shell the static content pages share: a centered prose column
/// with a title heading over a rendered-markdown body.
#[component]
pub fn MarkdownArticle(title: &'static str, markdown: &'static str) -> impl IntoView {
    let html_content = md_to_html(markdown);

    view! {
        <article class="px-6 py-12 max-w-3xl mx-auto prose-chronoscope">
            <h1 class="text-3xl font-bold mb-8">{title}</h1>
            <div inner_html=html_content></div>
        </article>
    }
}
