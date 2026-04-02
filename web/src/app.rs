use leptos::prelude::*;
use leptos_router::components::*;
use leptos_router::path;

use crate::components::error_banner::ErrorBanner;
use crate::components::nav::Sidebar;
use crate::pages::about::About;
use crate::pages::faq::Faq;
use crate::pages::landing::Landing;

#[component]
pub fn App() -> impl IntoView {
    #[cfg(feature = "test-hooks")]
    crate::test_hooks::register_base();

    view! {
        <Router>
            <ErrorBanner/>
            // Skip link for keyboard/screen reader users to bypass sidebar nav
            <a href="#main-content" class="sr-only focus:not-sr-only focus:absolute focus:z-[100] focus:top-2 focus:left-2 focus:bg-parchment focus:px-4 focus:py-2 focus:rounded focus:shadow-md focus:text-ink font-sans text-sm">
                "Skip to content"
            </a>
            <div class="flex min-h-screen">
                <Sidebar/>
                <main id="main-content" class="flex-1 min-w-0">
                    <Routes fallback=|| "Not found.">
                        <Route path=path!("/") view=Landing/>
                        <Route path=path!("/about") view=About/>
                        <Route path=path!("/faq") view=Faq/>
                    </Routes>
                </main>
            </div>
        </Router>
    }
}
