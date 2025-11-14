use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmMqttClient {
    client_id: String,
}

#[wasm_bindgen]
impl WasmMqttClient {
    #[wasm_bindgen(constructor)]
    pub fn new(client_id: String) -> Self {
        console_error_panic_hook::set_once();

        Self { client_id }
    }

    pub async fn connect(&self, url: &str) -> Result<(), JsValue> {
        web_sys::console::log_1(&format!("Connecting to: {}", url).into());
        Ok(())
    }

    pub async fn publish(&self, topic: &str, _payload: &[u8]) -> Result<(), JsValue> {
        web_sys::console::log_1(&format!("Publishing to topic: {}", topic).into());
        Ok(())
    }

    pub async fn subscribe(&self, topic: &str) -> Result<u16, JsValue> {
        web_sys::console::log_1(&format!("Subscribing to topic: {}", topic).into());
        Ok(1)
    }
}
