use bytes::Bytes;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

use super::PayloadCodec;
use crate::error::{MqttError, Result};

pub struct CodecRegistry {
    codecs: RwLock<HashMap<String, Arc<dyn PayloadCodec>>>,
    default_codec: RwLock<Option<String>>,
}

impl Default for CodecRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CodecRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            codecs: RwLock::new(HashMap::new()),
            default_codec: RwLock::new(None),
        }
    }

    pub fn register(&self, codec: impl PayloadCodec + 'static) {
        let content_type = codec.content_type().to_string();
        self.codecs.write().insert(content_type, Arc::new(codec));
    }

    /// # Errors
    /// Returns an error if the codec is not registered
    pub fn set_default(&self, content_type: &str) -> Result<()> {
        if self.codecs.read().contains_key(content_type) {
            *self.default_codec.write() = Some(content_type.to_string());
            Ok(())
        } else {
            Err(MqttError::ProtocolError(format!(
                "codec not registered: {content_type}"
            )))
        }
    }

    #[must_use]
    pub fn get(&self, content_type: &str) -> Option<Arc<dyn PayloadCodec>> {
        self.codecs.read().get(content_type).cloned()
    }

    #[must_use]
    pub fn default_codec(&self) -> Option<Arc<dyn PayloadCodec>> {
        let default = self.default_codec.read();
        default.as_ref().and_then(|ct| self.get(ct))
    }

    /// # Errors
    /// Returns an error if encoding fails
    pub fn encode_with_default(&self, payload: &[u8]) -> Result<(Bytes, Option<String>)> {
        let Some(codec) = self.default_codec() else {
            return Ok((Bytes::copy_from_slice(payload), None));
        };

        if !codec.should_encode(payload) {
            return Ok((Bytes::copy_from_slice(payload), None));
        }

        let encoded = codec.encode(payload)?;
        Ok((encoded, Some(codec.content_type().to_string())))
    }

    /// # Errors
    /// Returns an error if decoding fails
    pub fn decode_if_needed(&self, payload: &[u8], content_type: Option<&str>) -> Result<Bytes> {
        let Some(ct) = content_type else {
            return Ok(Bytes::copy_from_slice(payload));
        };

        let Some(codec) = self.get(ct) else {
            return Ok(Bytes::copy_from_slice(payload));
        };

        codec.decode(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockCodec {
        min_size: usize,
    }

    impl MockCodec {
        fn new() -> Self {
            Self { min_size: 10 }
        }
    }

    impl PayloadCodec for MockCodec {
        fn name(&self) -> &'static str {
            "mock"
        }

        fn content_type(&self) -> &'static str {
            "application/mock"
        }

        fn encode(&self, payload: &[u8]) -> Result<Bytes> {
            let mut encoded = b"MOCK:".to_vec();
            encoded.extend_from_slice(payload);
            Ok(Bytes::from(encoded))
        }

        fn decode(&self, payload: &[u8]) -> Result<Bytes> {
            if payload.starts_with(b"MOCK:") {
                Ok(Bytes::copy_from_slice(&payload[5..]))
            } else {
                Err(MqttError::ProtocolError("invalid mock payload".into()))
            }
        }

        fn min_size_threshold(&self) -> usize {
            self.min_size
        }
    }

    #[test]
    fn test_register_and_get() {
        let registry = CodecRegistry::new();
        registry.register(MockCodec::new());

        assert!(registry.get("application/mock").is_some());
        assert!(registry.get("application/other").is_none());
    }

    #[test]
    fn test_set_default() {
        let registry = CodecRegistry::new();
        registry.register(MockCodec::new());

        assert!(registry.default_codec().is_none());

        registry.set_default("application/mock").unwrap();
        assert!(registry.default_codec().is_some());
    }

    #[test]
    fn test_set_default_unregistered() {
        let registry = CodecRegistry::new();
        let result = registry.set_default("application/unknown");
        assert!(result.is_err());
    }

    #[test]
    fn test_encode_with_default() {
        let registry = CodecRegistry::new();
        registry.register(MockCodec::new());
        registry.set_default("application/mock").unwrap();

        let payload = b"hello world test data";
        let (encoded, content_type) = registry.encode_with_default(payload).unwrap();

        assert!(content_type.is_some());
        assert_eq!(content_type.unwrap(), "application/mock");
        assert!(encoded.starts_with(b"MOCK:"));
    }

    #[test]
    fn test_encode_skip_small_payload() {
        let registry = CodecRegistry::new();
        registry.register(MockCodec::new());
        registry.set_default("application/mock").unwrap();

        let payload = b"small";
        let (encoded, content_type) = registry.encode_with_default(payload).unwrap();

        assert!(content_type.is_none());
        assert_eq!(&encoded[..], payload);
    }

    #[test]
    fn test_decode_if_needed() {
        let registry = CodecRegistry::new();
        registry.register(MockCodec::new());

        let payload = b"MOCK:hello world";
        let decoded = registry
            .decode_if_needed(payload, Some("application/mock"))
            .unwrap();

        assert_eq!(&decoded[..], b"hello world");
    }

    #[test]
    fn test_decode_no_content_type() {
        let registry = CodecRegistry::new();
        registry.register(MockCodec::new());

        let payload = b"raw payload";
        let decoded = registry.decode_if_needed(payload, None).unwrap();

        assert_eq!(&decoded[..], payload);
    }

    #[test]
    fn test_decode_unknown_content_type() {
        let registry = CodecRegistry::new();

        let payload = b"raw payload";
        let decoded = registry
            .decode_if_needed(payload, Some("application/unknown"))
            .unwrap();

        assert_eq!(&decoded[..], payload);
    }
}
