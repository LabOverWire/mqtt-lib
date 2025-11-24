//! MQTT string implementation using `BeBytes` 2.3.0 size expressions
//!
//! MQTT strings are prefixed with a 2-byte length field in big-endian format.

use crate::error::{MqttError, Result};
use bebytes::BeBytes;

/// MQTT string with automatic size handling via `BeBytes` size expressions
#[derive(Debug, Clone, PartialEq, Eq, BeBytes)]
pub struct MqttString {
    /// Length of the string in bytes (big-endian)
    #[bebytes(big_endian)]
    length: u16,

    /// UTF-8 string data with size determined by length field
    #[bebytes(size = "length")]
    data: String,
}

impl MqttString {
    /// Create a new MQTT string
    ///
    /// # Errors
    /// Returns an error if the string is longer than 65535 bytes
    pub fn create(s: &str) -> Result<Self> {
        let len = s.len();
        if len > u16::MAX as usize {
            return Err(MqttError::StringTooLong(len));
        }

        Ok(Self {
            #[allow(clippy::cast_possible_truncation)]
            length: len as u16, // Safe: we checked len <= u16::MAX above
            data: s.to_string(),
        })
    }

    /// Get the string value
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.data
    }

    /// Get the total encoded size (length field + data)
    #[must_use]
    pub fn encoded_size(&self) -> usize {
        2 + self.data.len()
    }
}

impl TryFrom<&str> for MqttString {
    type Error = MqttError;

    fn try_from(s: &str) -> Result<Self> {
        Self::create(s)
    }
}

impl TryFrom<String> for MqttString {
    type Error = MqttError;

    fn try_from(s: String) -> Result<Self> {
        Self::create(&s)
    }
}

/// Encodes a UTF-8 string with a 2-byte length prefix (compatibility function)
///
/// This function provides compatibility with the old string module API.
/// Prefer using `MqttString::create(string)?.to_be_bytes()` for new code.
///
/// # Errors
///
/// Returns an error if:
/// - The string contains null characters
/// - The string length exceeds maximum string length
pub fn encode_string<B: bytes::BufMut>(buf: &mut B, string: &str) -> Result<()> {
    // Check for null characters
    if string.contains('\0') {
        return Err(MqttError::MalformedPacket(
            "String contains null character".to_string(),
        ));
    }

    let mqtt_string = MqttString::create(string)?;
    let encoded = mqtt_string.to_be_bytes();
    buf.put_slice(&encoded);
    Ok(())
}

/// Decodes a UTF-8 string with a 2-byte length prefix (compatibility function)
///
/// This function provides compatibility with the old string module API.
/// Prefer using `MqttString::try_from_be_bytes()` for new code.
///
/// # Errors
///
/// Returns an error if:
/// - Insufficient bytes in buffer
/// - String is not valid UTF-8
/// - String contains null characters
pub fn decode_string<B: bytes::Buf>(buf: &mut B) -> Result<String> {
    if buf.remaining() < 2 {
        return Err(MqttError::MalformedPacket(
            "Insufficient bytes for string length".to_string(),
        ));
    }

    let len = buf.get_u16() as usize;

    if buf.remaining() < len {
        return Err(MqttError::MalformedPacket(format!(
            "Insufficient bytes for string data: expected {}, got {}",
            len,
            buf.remaining()
        )));
    }

    let mut bytes = vec![0u8; len];
    buf.copy_to_slice(&mut bytes);

    let string = String::from_utf8(bytes)
        .map_err(|e| MqttError::MalformedPacket(format!("Invalid UTF-8: {e}")))?;

    // Check for null characters
    if string.contains('\0') {
        return Err(MqttError::MalformedPacket(
            "String contains null character".to_string(),
        ));
    }

    Ok(string)
}

/// Calculates the encoded length of a string (compatibility function)
#[must_use]
pub fn string_len(string: &str) -> usize {
    2 + string.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mqtt_string_encoding() {
        let mqtt_str = MqttString::create("hello").unwrap();
        let bytes = mqtt_str.to_be_bytes();

        // Check encoding: 2-byte length (0x00, 0x05) + "hello"
        assert_eq!(bytes, vec![0x00, 0x05, b'h', b'e', b'l', b'l', b'o']);
    }

    #[test]
    fn test_mqtt_string_decoding() {
        let data = vec![0x00, 0x05, b'h', b'e', b'l', b'l', b'o'];
        let (mqtt_str, consumed) = MqttString::try_from_be_bytes(&data).unwrap();

        assert_eq!(mqtt_str.as_str(), "hello");
        assert_eq!(consumed, 7);
    }

    #[test]
    fn test_mqtt_string_round_trip() {
        let original = MqttString::create("test/topic").unwrap();
        let bytes = original.to_be_bytes();
        let (decoded, _) = MqttString::try_from_be_bytes(&bytes).unwrap();

        assert_eq!(original, decoded);
    }

    #[test]
    fn test_empty_string() {
        let mqtt_str = MqttString::create("").unwrap();
        let bytes = mqtt_str.to_be_bytes();

        assert_eq!(bytes, vec![0x00, 0x00]);
    }

    #[test]
    fn test_string_too_long() {
        let long_string = "x".repeat(65536);
        let result = MqttString::create(&long_string);

        assert!(result.is_err());
    }
}
