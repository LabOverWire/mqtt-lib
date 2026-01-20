use super::WasmPayloadCodec;
use miniz_oxide::deflate::compress_to_vec;
use miniz_oxide::inflate::decompress_to_vec;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmDeflateCodec {
    level: u8,
    min_size: usize,
}

#[wasm_bindgen]
impl WasmDeflateCodec {
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self {
            level: 6,
            min_size: 128,
        }
    }

    #[wasm_bindgen(js_name = "withLevel")]
    #[must_use]
    pub fn with_level(mut self, level: u8) -> Self {
        self.level = level.clamp(1, 9);
        self
    }

    #[wasm_bindgen(js_name = "withMinSize")]
    #[must_use]
    pub fn with_min_size(mut self, size: usize) -> Self {
        self.min_size = size;
        self
    }
}

impl Default for WasmDeflateCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl WasmPayloadCodec for WasmDeflateCodec {
    fn name(&self) -> &'static str {
        "deflate"
    }

    fn content_type(&self) -> &'static str {
        "application/x-deflate"
    }

    fn min_size_threshold(&self) -> usize {
        self.min_size
    }

    fn encode(&self, payload: &[u8]) -> Result<Vec<u8>, String> {
        if payload.is_empty() {
            return Ok(payload.to_vec());
        }

        Ok(compress_to_vec(payload, self.level))
    }

    fn decode(&self, payload: &[u8]) -> Result<Vec<u8>, String> {
        decompress_to_vec(payload).map_err(|e| format!("Deflate decompression failed: {e}"))
    }
}
