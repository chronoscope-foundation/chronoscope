//! Tab containment for the modal overlays.
//!
//! Shared by the nav drawer and the image lightbox. Both cover the page with a
//! scrim, so both owe the same guarantee: Tab cycles inside the overlay rather
//! than walking onto the content behind it, which a sighted keyboard user
//! cannot see they have reached.

use wasm_bindgen::JsCast;

/// What a keyboard user can land on inside an overlay. Deliberately narrow —
/// these overlays hold links and buttons, and a broad selector would sweep in
/// elements the browser will not actually focus.
///
/// `disabled` and `inert` are part of that: the browser skips such a control,
/// so one at either end of the cycle makes Tab call `prevent_default` and then
/// focus nothing, swallowing the key. [`is_rendered`] covers the rest, which
/// CSS cannot ask about.
const FOCUSABLE: &str = ":is(button, a[href], input, [tabindex]:not([tabindex='-1']))\
                         :not([disabled]):not([inert])";

/// Whether an element is laid out, and so can take focus at all.
///
/// `offsetParent` alone is not enough: it is `null` for `position: fixed`
/// elements as well as for hidden ones, so a fixed overlay reads as invisible
/// while plainly on screen. The width fallback separates the two.
///
/// The browser tests' own visibility hooks answer this question with this
/// function, so what the trap will focus and what a test calls visible cannot
/// drift apart.
pub fn is_rendered(el: &web_sys::HtmlElement) -> bool {
    el.offset_parent().is_some() || el.offset_width() > 0
}

/// Wrap Tab across `roots`, in the order given.
///
/// Takes several roots because an overlay's controls are not always inside it:
/// the nav drawer's only close control is the trigger that opened it, which sits
/// over the drawer rather than within it. Listing both keeps the cycle honest —
/// a `role="dialog" aria-modal="true"` panel that lets Tab escape, or that never
/// offers its own close control, promises containment it does not deliver.
///
/// Call from the overlay's `keydown` handler when the key is Tab.
pub fn contain_tab(ev: &leptos::ev::KeyboardEvent, roots: &[&web_sys::HtmlElement]) {
    let mut focusables: Vec<web_sys::HtmlElement> = Vec::new();
    for root in roots {
        // A root can be focusable itself (the trigger is a button), and its own
        // descendants come after it, which is the order Tab would visit them in.
        if root.matches(FOCUSABLE).unwrap_or(false) && is_rendered(root) {
            focusables.push((*root).clone());
        }
        if let Ok(found) = root.query_selector_all(FOCUSABLE) {
            for i in 0..found.length() {
                if let Some(el) = found
                    .item(i)
                    .and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
                    .filter(is_rendered)
                {
                    focusables.push(el);
                }
            }
        }
    }
    let (Some(first), Some(last)) = (focusables.first(), focusables.last()) else {
        return;
    };
    let Some(active) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.active_element())
    else {
        return;
    };

    // Where the cycle currently is. `None` covers focus resting on the panel
    // itself, which is where it starts — the panel carries `tabindex="-1"` so it
    // is focusable but not tabbable, and is therefore not in this list. Treating
    // that as "outside the cycle" is what makes the very first Tab wrap instead
    // of letting the browser walk out onto the page behind the scrim.
    let position = focusables
        .iter()
        .position(|el| active == *AsRef::<web_sys::Element>::as_ref(el));

    let target = match (position, ev.shift_key()) {
        (None, true) => Some(last),
        (None, false) => Some(first),
        (Some(0), true) => Some(last),
        (Some(i), false) if i + 1 == focusables.len() => Some(first),
        // Between the ends: the browser's own order is already correct.
        _ => None,
    };
    if let Some(el) = target {
        ev.prevent_default();
        let _ = el.focus();
    }
}
