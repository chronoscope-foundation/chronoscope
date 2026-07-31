//! The article page shell, and the route that serves one.

use leptos::prelude::*;
use leptos_router::components::{Route, RouteProps};
use leptos_router::{MatchNestedRoutes, StaticSegment};

use crate::components::nav::NAV_CLEARANCE;
use crate::content::Article;

/// The route serving one article: the URL its own row named, over the prose
/// that row rendered.
///
/// A route names its article once, here, and both halves come from that one
/// value, so a route copied for a new page cannot serve the old page's prose at
/// the new page's URL. [`MarkdownArticle`] is private to this module, so the
/// shell pairing an article's heading with its body has one definition. That is
/// all the privacy buys: `Article::html()` is public, since the component
/// rendering a body has to read it, so another module can put an article's
/// prose on a route of its own.
///
/// The path comes from the article rather than from `path!("/about")`, which
/// would say the same thing in a literal nothing ties back to the page. The
/// one-segment tuple is what `path!` expands to, and `StaticSegment` takes the
/// leading slash in its stride.
///
/// Built from `RouteProps` rather than `view! { <Route/> }`, since that macro
/// wraps its result in a `View` and a `View` is not a route definition.
#[component(transparent)]
pub fn ArticleRoute(article: Article) -> impl MatchNestedRoutes + Clone + Send + 'static {
    Route(
        RouteProps::builder()
            .path((StaticSegment(article.path()),))
            .view(move || view! { <MarkdownArticle article=article/> })
            .build(),
    )
}

/// The whole of an article page: a centered prose column with the article's
/// heading over the body `build.rs` rendered from its markdown.
///
/// Takes the [`Article`] rather than its parts, so the page's heading travels
/// with the prose it heads. The body goes into `inner_html`, and the
/// `RenderedHtml` inside an `Article` is the only thing that reaches that sink.
#[component]
fn MarkdownArticle(article: Article) -> impl IntoView {
    view! {
        <article class=format!("px-6 pb-12 max-w-3xl mx-auto prose-chronoscope {NAV_CLEARANCE}")>
            <h1 class="text-3xl font-bold mb-8">{article.title()}</h1>
            <div inner_html=article.html().as_str()></div>
        </article>
    }
}
