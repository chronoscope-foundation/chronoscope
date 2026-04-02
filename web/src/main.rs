mod api;
mod app;
mod components;
mod maplibre;
mod markdown;
mod pages;
#[cfg(feature = "test-hooks")]
pub mod test_hooks;

use leptos::mount::mount_to_body;

fn main() {
    console_error_panic_hook::set_once();
    mount_to_body(app::App);
}
