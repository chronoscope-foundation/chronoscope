// MapLibre GL JS initialization helpers.
// Called from Rust via wasm_bindgen extern bindings.

export function initMap(container, styleUrl, centerLng, centerLat, zoom) {
    if (typeof maplibregl === "undefined") {
        console.warn("maplibregl not loaded");
        return null;
    }
    return new maplibregl.Map({
        container,
        style: styleUrl,
        center: [centerLng, centerLat],
        zoom,
    });
}

export function removeMap(map) {
    if (map) {
        map.remove();
    }
}
