mod deflate;
mod gzip;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::prelude::*;

pub use deflate::WasmDeflateCodec;
pub use gzip::WasmGzipCodec;

#[allow(clippy::missing_errors_doc)]
pub trait WasmPayloadCodec {
    fn name(&self) -> &'static str;
    fn content_type(&self) -> &'static str;
    fn encode(&self, payload: &[u8]) -> Result<Vec<u8>, String>;
    fn decode(&self, payload: &[u8]) -> Result<Vec<u8>, String>;

    fn min_size_threshold(&self) -> usize {
        128
    }

    fn should_encode(&self, payload: &[u8]) -> bool {
        payload.len() >= self.min_size_threshold()
    }
}

#[wasm_bindgen]
pub struct WasmCodecRegistry {
    codecs: RefCell<HashMap<String, Rc<dyn WasmPayloadCodec>>>,
    default_codec: RefCell<Option<String>>,
}

#[wasm_bindgen]
impl WasmCodecRegistry {
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self {
            codecs: RefCell::new(HashMap::new()),
            default_codec: RefCell::new(None),
        }
    }

    /// # Errors
    /// Returns an error if no codec is registered for the specified content type.
    #[wasm_bindgen(js_name = "setDefault")]
    pub fn set_default(&self, content_type: &str) -> Result<(), JsValue> {
        let codecs = self.codecs.borrow();
        if codecs.contains_key(content_type) {
            *self.default_codec.borrow_mut() = Some(content_type.to_string());
            Ok(())
        } else {
            Err(JsValue::from_str(&format!(
                "No codec registered for content type: {content_type}"
            )))
        }
    }

    #[wasm_bindgen(js_name = "getDefault")]
    #[must_use]
    pub fn get_default(&self) -> Option<String> {
        self.default_codec.borrow().clone()
    }

    #[wasm_bindgen(js_name = "hasCodec")]
    #[must_use]
    pub fn has_codec(&self, content_type: &str) -> bool {
        self.codecs.borrow().contains_key(content_type)
    }

    #[wasm_bindgen(js_name = "registerGzip")]
    pub fn register_gzip(&self, codec: WasmGzipCodec) {
        let content_type = codec.content_type().to_string();
        self.codecs
            .borrow_mut()
            .insert(content_type, Rc::new(codec));
    }

    #[wasm_bindgen(js_name = "registerDeflate")]
    pub fn register_deflate(&self, codec: WasmDeflateCodec) {
        let content_type = codec.content_type().to_string();
        self.codecs
            .borrow_mut()
            .insert(content_type, Rc::new(codec));
    }
}

impl Default for WasmCodecRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl WasmCodecRegistry {
    pub fn register(&self, codec: impl WasmPayloadCodec + 'static) {
        let content_type = codec.content_type().to_string();
        self.codecs
            .borrow_mut()
            .insert(content_type, Rc::new(codec));
    }

    /// # Errors
    /// Returns an error if encoding fails.
    pub fn encode_with_default(&self, payload: &[u8]) -> Result<(Vec<u8>, Option<String>), String> {
        let default_ct = self.default_codec.borrow().clone();
        if let Some(content_type) = default_ct {
            let codecs = self.codecs.borrow();
            if let Some(codec) = codecs.get(&content_type) {
                if codec.should_encode(payload) {
                    let encoded = codec.encode(payload)?;
                    return Ok((encoded, Some(content_type)));
                }
            }
        }
        Ok((payload.to_vec(), None))
    }

    /// # Errors
    /// Returns an error if decoding fails.
    pub fn decode_if_needed(
        &self,
        payload: &[u8],
        content_type: Option<&str>,
    ) -> Result<Vec<u8>, String> {
        if let Some(ct) = content_type {
            let codecs = self.codecs.borrow();
            if let Some(codec) = codecs.get(ct) {
                return codec.decode(payload);
            }
        }
        Ok(payload.to_vec())
    }
}

impl Clone for WasmCodecRegistry {
    fn clone(&self) -> Self {
        Self {
            codecs: RefCell::new(self.codecs.borrow().clone()),
            default_codec: RefCell::new(self.default_codec.borrow().clone()),
        }
    }
}

#[wasm_bindgen(js_name = "createCodecRegistry")]
#[must_use]
pub fn create_codec_registry() -> WasmCodecRegistry {
    WasmCodecRegistry::new()
}

#[wasm_bindgen(js_name = "createGzipCodec")]
#[must_use]
pub fn create_gzip_codec(level: Option<u8>, min_size: Option<usize>) -> WasmGzipCodec {
    let mut codec = WasmGzipCodec::new();
    if let Some(l) = level {
        codec = codec.with_level(l);
    }
    if let Some(s) = min_size {
        codec = codec.with_min_size(s);
    }
    codec
}

#[wasm_bindgen(js_name = "createDeflateCodec")]
#[must_use]
pub fn create_deflate_codec(level: Option<u8>, min_size: Option<usize>) -> WasmDeflateCodec {
    let mut codec = WasmDeflateCodec::new();
    if let Some(l) = level {
        codec = codec.with_level(l);
    }
    if let Some(s) = min_size {
        codec = codec.with_min_size(s);
    }
    codec
}
