use leptos::prelude::*;
use leptos_router::components::*;
use leptos_router::path;

use crate::components::error_banner::ErrorBanner;
use crate::components::markdown_article::ArticleRoute;
use crate::components::nav::{NAV_CLEARANCE, Nav};
use crate::content::articles::article_routes;
use crate::pages::faq::Faq;
use crate::pages::landing::Landing;

#[component]
pub fn App() -> impl IntoView {
    #[cfg(feature = "test-hooks")]
    crate::test_hooks::register_base();

    view! {
        <Router>
            <ErrorBanner/>
            // Skip link for keyboard/screen reader users to bypass the nav trigger
            <a href="#main-content" class="sr-only focus:not-sr-only focus:absolute focus:z-[100] focus:top-2 focus:left-2 focus:bg-parchment focus:px-4 focus:py-2 focus:rounded focus:shadow-md focus:text-ink font-sans text-sm">
                "Skip to content"
            </a>
            <Nav/>
            <main id="main-content" class="min-h-dvh">
                // The routes written here, plus one per article row spliced in
                // by the generated macro. An article page routes because its row
                // exists, so there is no line to forget: a page nothing routed
                // would still be listed in the drawer and still be linked from
                // the content, and every guard over the table would still pass.
                //
                // Invoked in a block: `view!` parses its own tokens before any
                // inner macro runs, so the expansion has to arrive as an
                // expression. In place rather than from a `let` above, because
                // `<Routes>` reads the router's context as it is built and
                // finds none outside `<Router>`.
                //
                // The fallback needs the same clearance the article pages get:
                // the nav trigger floats over the top-left corner, and a bare
                // text node here rendered underneath it, leaving a mistyped URL
                // looking like a blank page.
                {article_routes! {
                    fallback = || view! {
                        // Named, so the browser suite can ask whether a link
                        // landed here instead of matching on wording that is
                        // free to change.
                        <p id="not-found" class=format!("px-6 text-sepia font-sans {NAV_CLEARANCE}")>"Not found."</p>
                    };
                    <Route path=path!("/") view=Landing/>
                    <Route path=path!("/faq") view=Faq/>
                }}
            </main>
        </Router>
    }
}
