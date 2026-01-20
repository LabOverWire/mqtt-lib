use std::io::{Read, Write};

use bytes::Bytes;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;

use super::PayloadCodec;
use crate::error::{MqttError, Result};

pub struct DeflateCodec {
    level: Compression,
    min_size: usize,
}

impl Default for DeflateCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl DeflateCodec {
    #[must_use]
    pub fn new() -> Self {
        Self {
            level: Compression::default(),
            min_size: 128,
        }
    }

    #[must_use]
    pub fn with_level(mut self, level: u32) -> Self {
        self.level = Compression::new(level);
        self
    }

    #[must_use]
    pub fn with_min_size(mut self, size: usize) -> Self {
        self.min_size = size;
        self
    }
}

impl PayloadCodec for DeflateCodec {
    fn name(&self) -> &'static str {
        "deflate"
    }

    fn content_type(&self) -> &'static str {
        "application/x-deflate"
    }

    fn encode(&self, payload: &[u8]) -> Result<Bytes> {
        let mut encoder = DeflateEncoder::new(Vec::new(), self.level);
        encoder
            .write_all(payload)
            .map_err(|e| MqttError::ProtocolError(format!("deflate encode failed: {e}")))?;
        let compressed = encoder
            .finish()
            .map_err(|e| MqttError::ProtocolError(format!("deflate finish failed: {e}")))?;
        Ok(Bytes::from(compressed))
    }

    fn decode(&self, payload: &[u8]) -> Result<Bytes> {
        let mut decoder = DeflateDecoder::new(payload);
        let mut decompressed = Vec::new();
        decoder
            .read_to_end(&mut decompressed)
            .map_err(|e| MqttError::ProtocolError(format!("deflate decode failed: {e}")))?;
        Ok(Bytes::from(decompressed))
    }

    fn min_size_threshold(&self) -> usize {
        self.min_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deflate_roundtrip() {
        let codec = DeflateCodec::new();
        let original = b"Hello, World! This is a test payload for compression.";

        let encoded = codec.encode(original).unwrap();
        let decoded = codec.decode(&encoded).unwrap();

        assert_eq!(&decoded[..], original);
    }

    #[test]
    fn test_deflate_compression_ratio() {
        let codec = DeflateCodec::new();
        let original: Vec<u8> = std::iter::repeat_n(b'A', 1000).collect();

        let encoded = codec.encode(&original).unwrap();

        assert!(encoded.len() < original.len());
    }

    #[test]
    fn test_deflate_with_level() {
        let codec = DeflateCodec::new().with_level(9);
        let original = b"Hello, World! This is a test payload for compression.";

        let encoded = codec.encode(original).unwrap();
        let decoded = codec.decode(&encoded).unwrap();

        assert_eq!(&decoded[..], original);
    }

    #[test]
    fn test_deflate_content_type() {
        let codec = DeflateCodec::new();
        assert_eq!(codec.content_type(), "application/x-deflate");
    }

    #[test]
    fn test_deflate_min_size_threshold() {
        let codec = DeflateCodec::new().with_min_size(256);
        assert_eq!(codec.min_size_threshold(), 256);
    }

    #[test]
    fn test_deflate_should_encode() {
        let codec = DeflateCodec::new().with_min_size(100);

        let small_payload = vec![0u8; 50];
        let large_payload = vec![0u8; 150];

        assert!(!codec.should_encode(&small_payload));
        assert!(codec.should_encode(&large_payload));
    }

    #[test]
    fn test_deflate_invalid_data() {
        let codec = DeflateCodec::new();
        let invalid = b"\xff\xfe\xfd\xfc";

        let result = codec.decode(invalid);
        assert!(result.is_err());
    }
}
