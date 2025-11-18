use crate::error::{MqttError, Result};
use bytes::{Buf, BufMut, Bytes};

/// Encodes binary data with a 2-byte length prefix
///
/// # MQTT Binary Format:
/// - 2 bytes: data length (big-endian)
/// - N bytes: binary data
///
/// # Errors
///
/// Returns an error if the data length exceeds maximum string length
pub fn encode_binary<B: BufMut>(buf: &mut B, data: &[u8]) -> Result<()> {
    if data.len() > crate::constants::limits::MAX_STRING_LENGTH as usize {
        return Err(MqttError::MalformedPacket(format!(
            "Binary data length {} exceeds maximum {}",
            data.len(),
            crate::constants::limits::MAX_STRING_LENGTH
        )));
    }

    // Write length as 2-byte big-endian
    // Safe cast: length was already validated to be <= u16::MAX
    #[allow(clippy::cast_possible_truncation)]
    buf.put_u16(data.len() as u16);
    // Write binary data
    buf.put_slice(data);

    Ok(())
}

/// Decodes binary data with a 2-byte length prefix
///
/// # Errors
///
/// Returns an error if there are insufficient bytes in the buffer
pub fn decode_binary<B: Buf>(buf: &mut B) -> Result<Bytes> {
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

/// Encodes optional binary data
///
/// If data is None, nothing is written to the buffer
///
/// # Errors
///
/// Returns an error if the data length exceeds maximum string length
pub fn encode_optional_binary<B: BufMut>(buf: &mut B, data: Option<&[u8]>) -> Result<()> {
    if let Some(data) = data {
        encode_binary(buf, data)?;
    }
    Ok(())
}

/// Calculates the encoded length of binary data (2 bytes for length + data bytes)
#[must_use]
pub fn binary_len(data: &[u8]) -> usize {
    2 + data.len()
}

/// Calculates the encoded length of optional binary data
#[must_use]
pub fn optional_binary_len(data: Option<&[u8]>) -> usize {
    data.map_or(0, binary_len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[test]
    fn test_encode_decode_binary() {
        let mut buf = BytesMut::new();

        // Test various binary data
        let test_data = [
            vec![],
            vec![0x00],
            vec![0x01, 0x02, 0x03],
            vec![0xFF; 100],
            (0..=255).collect::<Vec<u8>>(),
        ];

        for data in &test_data {
            buf.clear();
            encode_binary(&mut buf, data).unwrap();

            let decoded = decode_binary(&mut buf).unwrap();
            assert_eq!(&decoded[..], data.as_slice());
        }
    }

    #[test]
    fn test_encode_binary_too_long() {
        let mut buf = BytesMut::new();
        let data = vec![0u8; crate::constants::limits::MAX_BINARY_LENGTH as usize];
        let result = encode_binary(&mut buf, &data);
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_insufficient_length_bytes() {
        let mut buf = BytesMut::new();
        buf.put_u8(0); // Only 1 byte instead of 2

        let result = decode_binary(&mut buf);
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_insufficient_data_bytes() {
        let mut buf = BytesMut::new();
        buf.put_u16(10); // Claims 10 bytes
        buf.put_slice(&[1, 2, 3, 4, 5]); // Only 5 bytes

        let result = decode_binary(&mut buf);
        assert!(result.is_err());
    }

    #[test]
    fn test_encode_optional_binary() {
        let mut buf = BytesMut::new();

        // Test None - nothing should be written
        encode_optional_binary(&mut buf, None).unwrap();
        assert_eq!(buf.len(), 0);

        // Test Some
        buf.clear();
        encode_optional_binary(&mut buf, Some(&[1, 2, 3])).unwrap();
        assert_eq!(buf.len(), 5); // 2 bytes length + 3 bytes data
    }

    #[test]
    fn test_binary_len() {
        assert_eq!(binary_len(&[]), 2);
        assert_eq!(binary_len(&[1, 2, 3]), 5);
        assert_eq!(binary_len(&[0u8; 100]), 102);
    }

    #[test]
    fn test_optional_binary_len() {
        assert_eq!(optional_binary_len(None), 0);
        assert_eq!(optional_binary_len(Some(&[])), 2);
        assert_eq!(optional_binary_len(Some(&[1, 2, 3])), 5);
    }

    #[test]
    fn test_empty_binary() {
        let mut buf = BytesMut::new();
        encode_binary(&mut buf, &[]).unwrap();

        assert_eq!(buf.len(), 2);
        assert_eq!(buf[0], 0);
        assert_eq!(buf[1], 0);

        let decoded = decode_binary(&mut buf).unwrap();
        assert_eq!(decoded.len(), 0);
    }
}
