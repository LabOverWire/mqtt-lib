use std::io::Read;
use std::io::Write;

use bytes::Bytes;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

use super::PayloadCodec;
use crate::error::{MqttError, Result};

const DEFAULT_MAX_DECOMPRESSED_SIZE: usize = 10 * 1024 * 1024;

pub struct GzipCodec {
    level: Compression,
    min_size: usize,
    max_decompressed_size: usize,
}

impl Default for GzipCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl GzipCodec {
    #[must_use]
    pub fn new() -> Self {
        Self {
            level: Compression::default(),
            min_size: 128,
            max_decompressed_size: DEFAULT_MAX_DECOMPRESSED_SIZE,
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

    #[must_use]
    pub fn with_max_decompressed_size(mut self, size: usize) -> Self {
        self.max_decompressed_size = size;
        self
    }
}

impl PayloadCodec for GzipCodec {
    fn name(&self) -> &'static str {
        "gzip"
    }

    fn content_type(&self) -> &'static str {
        "application/gzip"
    }

    fn encode(&self, payload: &[u8]) -> Result<Bytes> {
        let mut encoder = GzEncoder::new(Vec::new(), self.level);
        encoder
            .write_all(payload)
            .map_err(|e| MqttError::ProtocolError(format!("gzip encode failed: {e}")))?;
        let compressed = encoder
            .finish()
            .map_err(|e| MqttError::ProtocolError(format!("gzip finish failed: {e}")))?;
        Ok(Bytes::from(compressed))
    }

    fn decode(&self, payload: &[u8]) -> Result<Bytes> {
        let limit = self.max_decompressed_size;
        let mut decoder =
            GzDecoder::new(payload).take(u64::try_from(limit + 1).unwrap_or(u64::MAX));
        let mut decompressed = Vec::new();
        decoder
            .read_to_end(&mut decompressed)
            .map_err(|e| MqttError::ProtocolError(format!("gzip decode failed: {e}")))?;
        if decompressed.len() > limit {
            return Err(MqttError::ProtocolError(format!(
                "gzip decompressed size {} exceeds limit {limit}",
                decompressed.len()
            )));
        }
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
    fn test_gzip_roundtrip() {
        let codec = GzipCodec::new();
        let original = b"Hello, World! This is a test payload for compression.";

        let encoded = codec.encode(original).unwrap();
        let decoded = codec.decode(&encoded).unwrap();

        assert_eq!(&decoded[..], original);
    }

    #[test]
    fn test_gzip_compression_ratio() {
        let codec = GzipCodec::new();
        let original: Vec<u8> = std::iter::repeat_n(b'A', 1000).collect();

        let encoded = codec.encode(&original).unwrap();

        assert!(encoded.len() < original.len());
    }

    #[test]
    fn test_gzip_with_level() {
        let codec = GzipCodec::new().with_level(9);
        let original = b"Hello, World! This is a test payload for compression.";

        let encoded = codec.encode(original).unwrap();
        let decoded = codec.decode(&encoded).unwrap();

        assert_eq!(&decoded[..], original);
    }

    #[test]
    fn test_gzip_content_type() {
        let codec = GzipCodec::new();
        assert_eq!(codec.content_type(), "application/gzip");
    }

    #[test]
    fn test_gzip_min_size_threshold() {
        let codec = GzipCodec::new().with_min_size(256);
        assert_eq!(codec.min_size_threshold(), 256);
    }

    #[test]
    fn test_gzip_should_encode() {
        let codec = GzipCodec::new().with_min_size(100);

        let small_payload = vec![0u8; 50];
        let large_payload = vec![0u8; 150];

        assert!(!codec.should_encode(&small_payload));
        assert!(codec.should_encode(&large_payload));
    }

    #[test]
    fn test_gzip_invalid_data() {
        let codec = GzipCodec::new();
        let invalid = b"not gzip data";

        let result = codec.decode(invalid);
        assert!(result.is_err());
    }
}
