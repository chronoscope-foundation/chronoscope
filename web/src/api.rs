//! API client integration for the web frontend.
//!
//! Uses the shared [`Client`] from `chronoscope-api-client` for typed API
//! access. The client's base URL is the current page origin plus the `/api`
//! mount that the front door reverse-proxies to the backend.

pub use chronoscope_api_client::{
    ApiError, Client, Cursor, EntityId, EventId, ImageId, Snapshot, image_caption,
};

/// Client-facing instantiations of the generic read DTOs at the opaque wire ids.
/// The web frontend is a leaf consumer, so it pins the id params once here rather
/// than spelling them at every use site.
pub type ClickAction = chronoscope_api_client::ClickAction<EntityId>;
pub type EntityDetail = chronoscope_api_client::EntityDetail<EntityId, EventId, ImageId>;
pub type Marker = chronoscope_api_client::Marker<EntityId>;

// ==================== Client construction ====================

/// Build a [`Client`] pointed at the same-origin `/api` mount.
///
/// The front door — Trunk in dev, the harness proxy in tests, Cloudflare in
/// prod — serves this bundle and reverse-proxies `/api/*` to the backend, so
/// the base is `window.location.origin` + `/api`. Per-endpoint paths stay
/// root-relative, so `{origin}/api` joined with `/entities` yields
/// `{origin}/api/entities`.
///
/// Returns `None` when the browser context is unavailable; callers treat a
/// missing client as "API features disabled".
fn client_from_origin() -> Option<Client> {
    let window = web_sys::window()?;
    let origin = window
        .location()
        .origin()
        .map_err(|e| {
            web_sys::console::warn_1(
                &format!("failed to read window.location.origin: {e:?}").into(),
            );
        })
        .ok()?;
    Some(Client::new(format!("{origin}/api")))
}

// ==================== Lazy initialization ====================

/// Lazily initialize the API client on first call, memoizing the result.
///
/// Returns a clone of the initialized client, or `None` if the origin can't be
/// read. [`Client`] is cheap to clone (just a URL string + `reqwest::Client`).
pub async fn get_or_init_client(
    handle: &std::rc::Rc<std::cell::RefCell<Option<Client>>>,
) -> Option<Client> {
    let needs_init = handle.borrow().is_none();
    if needs_init {
        let client = client_from_origin()?;
        *handle.borrow_mut() = Some(client);
    }
    handle.borrow().clone()
}
