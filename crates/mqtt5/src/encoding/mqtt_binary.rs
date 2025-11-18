//! MQTT binary data implementation using `BeBytes` 2.10.0 size expressions
//!
//! MQTT binary data is prefixed with a 2-byte length field in big-endian format.

use crate::error::{MqttError, Result};
use bebytes::BeBytes;
use bytes::Bytes;

/// MQTT binary data with automatic size handling via `BeBytes` size expressions
#[derive(Debug, Clone, PartialEq, Eq, BeBytes)]
pub struct MqttBinary {
    /// Length of the binary data in bytes (big-endian)
    #[bebytes(big_endian)]
    length: u16,

    /// Binary data with size determined by length field
    #[bebytes(size = "length")]
    data: Vec<u8>,
}

impl MqttBinary {
    /// Create new MQTT binary data
    ///
    /// # Errors
    /// Returns an error if the data is longer than 65535 bytes
    pub fn create(data: &[u8]) -> Result<Self> {
        let len = data.len();
        if len > u16::MAX as usize {
            return Err(MqttError::MalformedPacket(format!(
                "Binary data length {} exceeds maximum {}",
                len,
                u16::MAX
            )));
        }

        Ok(Self {
            #[allow(clippy::cast_possible_truncation)]
            length: len as u16, // Safe: we checked len <= u16::MAX above
            data: data.to_vec(),
        })
    }

    /// Create from Bytes
    ///
    /// # Errors
    /// Returns an error if the data is longer than 65535 bytes
    pub fn from_bytes(bytes: &Bytes) -> Result<Self> {
        Self::create(bytes)
    }

    /// Get the binary data as a slice
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    /// Get the binary data as a Vec
    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.data
    }

    /// Convert to Bytes
    #[must_use]
    pub fn into_bytes(self) -> Bytes {
        Bytes::from(self.data)
    }

    /// Get the total encoded size (length field + data)
    #[must_use]
    pub fn encoded_size(&self) -> usize {
        2 + self.data.len()
    }
}

impl TryFrom<&[u8]> for MqttBinary {
    type Error = MqttError;

    fn try_from(data: &[u8]) -> Result<Self> {
        Self::create(data)
    }
}

impl TryFrom<Vec<u8>> for MqttBinary {
    type Error = MqttError;

    fn try_from(data: Vec<u8>) -> Result<Self> {
        Self::create(&data)
    }
}

impl TryFrom<Bytes> for MqttBinary {
    type Error = MqttError;

    fn try_from(bytes: Bytes) -> Result<Self> {
        Self::from_bytes(&bytes)
    }
}

/// Encodes binary data with a 2-byte length prefix (compatibility function)
///
/// This function provides compatibility with the old binary module API.
/// Prefer using `MqttBinary::create(data)?.to_be_bytes()` for new code.
///
/// # Errors
///
/// Returns an error if the data length exceeds maximum
pub fn encode_binary<B: bytes::BufMut>(buf: &mut B, data: &[u8]) -> Result<()> {
    let mqtt_binary = MqttBinary::create(data)?;
    let encoded = mqtt_binary.to_be_bytes();
    buf.put_slice(&encoded);
    Ok(())
}

/// Decodes binary data with a 2-byte length prefix (compatibility function)
///
/// This function provides compatibility with the old binary module API.
/// Prefer using `MqttBinary::try_from_be_bytes()` for new code.
///
/// # Errors
///
/// Returns an error if there are insufficient bytes in the buffer
pub fn decode_binary<B: bytes::Buf>(buf: &mut B) -> Result<Bytes> {
    if buf.remaining() < 2 {
        return Err(MqttError::MalformedPacket(
            "Insufficient bytes for binary data length".to_string(),
        ));
    }

    let len = buf.get_u16() as usize;

    if buf.remaining() < len {
        return Err(MqttError::MalformedPacket(format!(
            "Insufficient bytes for binary data: expected {}, got {}",
            len,
            buf.remaining()
        )));
    }

    Ok(buf.copy_to_bytes(len))
}

/// Encodes optional binary data (compatibility function)
///
/// If data is None, nothing is written to the buffer
///
/// # Errors
///
/// Returns an error if the data length exceeds maximum
pub fn encode_optional_binary<B: bytes::BufMut>(buf: &mut B, data: Option<&[u8]>) -> Result<()> {
    if let Some(data) = data {
        encode_binary(buf, data)?;
    }
    Ok(())
}

/// Calculates the encoded length of binary data (compatibility function)
#[must_use]
pub fn binary_len(data: &[u8]) -> usize {
    2 + data.len()
}

/// Calculates the encoded length of optional binary data (compatibility function)
#[must_use]
pub fn optional_binary_len(data: Option<&[u8]>) -> usize {
    data.map_or(0, binary_len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[test]
    fn test_mqtt_binary_encoding() {
        let mqtt_bin = MqttBinary::create(&[0x01, 0x02, 0x03]).unwrap();
        let bytes = mqtt_bin.to_be_bytes();

        // Check encoding: 2-byte length (0x00, 0x03) + data
        assert_eq!(bytes, vec![0x00, 0x03, 0x01, 0x02, 0x03]);
    }

    #[test]
    fn test_mqtt_binary_decoding() {
        let data = vec![0x00, 0x03, 0x01, 0x02, 0x03];
        let (mqtt_bin, consumed) = MqttBinary::try_from_be_bytes(&data).unwrap();

        assert_eq!(mqtt_bin.as_slice(), &[0x01, 0x02, 0x03]);
        assert_eq!(consumed, 5);
    }

    #[test]
    fn test_mqtt_binary_round_trip() {
        let original = MqttBinary::create(&[0xFF, 0x00, 0xAB]).unwrap();
        let bytes = original.to_be_bytes();
        let (decoded, _) = MqttBinary::try_from_be_bytes(&bytes).unwrap();

        assert_eq!(original, decoded);
    }

    #[test]
    fn test_empty_binary() {
        let mqtt_bin = MqttBinary::create(&[]).unwrap();
        let bytes = mqtt_bin.to_be_bytes();

        assert_eq!(bytes, vec![0x00, 0x00]);
    }

    #[test]
    fn test_binary_too_long() {
        let long_data = vec![0u8; 65536];
        let result = MqttBinary::create(&long_data);

        assert!(result.is_err());
    }

    #[test]
    fn test_compatibility_functions() {
        let mut buf = BytesMut::new();
        let test_data = vec![0x01, 0x02, 0x03];

        // Test encode/decode compatibility
        encode_binary(&mut buf, &test_data).unwrap();
        let decoded = decode_binary(&mut buf).unwrap();

        assert_eq!(&decoded[..], &test_data[..]);
    }
}
