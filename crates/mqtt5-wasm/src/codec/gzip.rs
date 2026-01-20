use super::WasmPayloadCodec;
use miniz_oxide::deflate::compress_to_vec;
use miniz_oxide::inflate::decompress_to_vec;
use wasm_bindgen::prelude::*;

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn gzip_compress_native(input: &[u8], level: u8) -> Result<Vec<u8>, String> {
    let deflated = compress_to_vec(input, level);

    let mut output = Vec::with_capacity(10 + deflated.len() + 8);
    output.extend_from_slice(&[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff]);
    output.extend_from_slice(&deflated);

    let crc = crc32(input);
    output.extend_from_slice(&crc.to_le_bytes());
    output.extend_from_slice(&(input.len() as u32).to_le_bytes());

    Ok(output)
}

#[wasm_bindgen]
pub struct WasmGzipCodec {
    level: u8,
    min_size: usize,
}

#[wasm_bindgen]
impl WasmGzipCodec {
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

impl Default for WasmGzipCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl WasmPayloadCodec for WasmGzipCodec {
    fn name(&self) -> &'static str {
        "gzip"
    }

    fn content_type(&self) -> &'static str {
        "application/gzip"
    }

    fn min_size_threshold(&self) -> usize {
        self.min_size
    }

    fn encode(&self, payload: &[u8]) -> Result<Vec<u8>, String> {
        if payload.is_empty() {
            return Ok(payload.to_vec());
        }

        gzip_compress_native(payload, self.level)
    }

    fn decode(&self, payload: &[u8]) -> Result<Vec<u8>, String> {
        if payload.len() < 18 {
            return Err("Gzip data too short".to_string());
        }

        if payload[0] != 0x1f || payload[1] != 0x8b {
            return Err("Invalid gzip magic number".to_string());
        }

        if payload[2] != 8 {
            return Err("Unsupported gzip compression method".to_string());
        }

        let flags = payload[3];
        let mut offset = 10;

        if flags & 0x04 != 0 {
            if payload.len() < offset + 2 {
                return Err("Gzip FEXTRA truncated".to_string());
            }
            let xlen = u16::from_le_bytes([payload[offset], payload[offset + 1]]) as usize;
            offset += 2 + xlen;
        }

        if flags & 0x08 != 0 {
            while offset < payload.len() && payload[offset] != 0 {
                offset += 1;
            }
            offset += 1;
        }

        if flags & 0x10 != 0 {
            while offset < payload.len() && payload[offset] != 0 {
                offset += 1;
            }
            offset += 1;
        }

        if flags & 0x02 != 0 {
            offset += 2;
        }

        if offset >= payload.len() - 8 {
            return Err("Gzip header too long or data truncated".to_string());
        }

        let deflate_data = &payload[offset..payload.len() - 8];
        decompress_to_vec(deflate_data).map_err(|e| format!("Gzip decompression failed: {e}"))
    }
}
