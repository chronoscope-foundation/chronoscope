use leptos::prelude::*;
use leptos_router::components::*;
use leptos_router::path;

use crate::components::error_banner::ErrorBanner;
use crate::components::nav::Nav;
use crate::pages::about::About;
use crate::pages::faq::Faq;
use crate::pages::landing::Landing;
use crate::pages::related_work::RelatedWork;

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
                // The fallback needs the same clearance the article pages get:
                // the nav trigger floats over the top-left corner, and a bare
                // text node here rendered underneath it, leaving a mistyped URL
                // looking like a blank page.
                <Routes fallback=|| view! {
                    <p class="px-6 pt-20 text-sepia font-sans">"Not found."</p>
                }>
                    <Route path=path!("/") view=Landing/>
                    <Route path=path!("/about") view=About/>
                    <Route path=path!("/faq") view=Faq/>
                    <Route path=path!("/related-work") view=RelatedWork/>
                </Routes>
            </main>
        </Router>
    }
}
