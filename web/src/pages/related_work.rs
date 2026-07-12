use leptos::prelude::*;

use crate::components::markdown_article::MarkdownArticle;

const RELATED_WORK_MARKDOWN: &str = include_str!("../../content/related-work.md");

#[component]
pub fn RelatedWork() -> impl IntoView {
    view! {
        <MarkdownArticle title="Related work and where we fit" markdown=RELATED_WORK_MARKDOWN/>
    }
}
