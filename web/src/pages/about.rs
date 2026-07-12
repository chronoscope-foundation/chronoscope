use leptos::prelude::*;

use crate::components::markdown_article::MarkdownArticle;

const ABOUT_MARKDOWN: &str = include_str!("../../content/about.md");

#[component]
pub fn About() -> impl IntoView {
    view! {
        <MarkdownArticle title="About Chronoscope" markdown=ABOUT_MARKDOWN/>
    }
}
