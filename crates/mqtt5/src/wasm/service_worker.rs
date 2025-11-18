use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn start_service_worker() {
    console_error_panic_hook::set_once();

    web_sys::console::log_1(&"MQTT Service Worker starting...".into());
}
