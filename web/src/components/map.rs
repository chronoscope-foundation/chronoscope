use std::cell::RefCell;
use std::rc::Rc;

use leptos::prelude::*;
use send_wrapper::SendWrapper;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/static/map-init.js")]
extern "C" {
    #[wasm_bindgen(js_name = initMap)]
    fn init_map(
        container: &web_sys::HtmlDivElement,
        style_url: &str,
        center_lng: f64,
        center_lat: f64,
        zoom: f64,
    ) -> JsValue;

    #[wasm_bindgen(js_name = removeMap)]
    fn remove_map(map: &JsValue);
}

#[component]
pub fn MapView() -> impl IntoView {
    let container = NodeRef::<leptos::html::Div>::new();
    // JsValue is !Send, so use Rc<RefCell> (fine in single-threaded WASM).
    let map_handle: Rc<RefCell<Option<JsValue>>> = Rc::new(RefCell::new(None));
    let effect_handle = Rc::clone(&map_handle);

    Effect::new(move || {
        if let Some(el) = container.get() {
            // Destroy previous instance if re-mounting
            if let Some(old_map) = effect_handle.borrow().as_ref() {
                remove_map(old_map);
            }
            let map = init_map(
                &el,
                "https://tiles.openfreemap.org/styles/liberty",
                12.4964,
                41.9028,
                3.0,
            );
            *effect_handle.borrow_mut() = if map.is_null() { None } else { Some(map) };
        }
    });

    // Clean up WebGL context and event listeners when the component unmounts.
    // SendWrapper is safe here because WASM is single-threaded; on_cleanup
    // requires Send+Sync but the closure only runs on the main thread.
    let cleanup_handle = SendWrapper::new(Rc::clone(&map_handle));
    on_cleanup(move || {
        if let Some(map) = cleanup_handle.borrow().as_ref() {
            remove_map(map);
        }
    });

    view! {
        <div node_ref=container class="w-full h-full"/>
    }
}
