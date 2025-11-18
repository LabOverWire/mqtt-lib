//! BeBytes-compatible variable length integer implementation for MQTT
//!
//! This module provides a variable length integer type that integrates with `BeBytes` 2.3.0
//! for automatic serialization/deserialization with size expressions support.

use crate::error::{MqttError, Result};
use bebytes::BeBytes;
use bytes::{Buf, BufMut};
use std::fmt;

/// Maximum value that can be encoded as a variable byte integer (268,435,455)
pub const VARIABLE_INT_MAX: u32 = 268_435_455;

/// Variable length integer as defined by MQTT specification
///
/// Encodes values using 1-4 bytes:
/// - 0-127: 1 byte
/// - 128-16,383: 2 bytes
/// - 16,384-2,097,151: 3 bytes
/// - 2,097,152-268,435,455: 4 bytes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VariableInt {
    value: u32,
}

impl VariableInt {
    /// Creates a new `VariableInt` from a u32 value
    ///
    /// # Errors
    ///
    /// Returns `MqttError::ProtocolError` if the value exceeds the maximum
    pub fn new(value: u32) -> Result<Self> {
        if value > VARIABLE_INT_MAX {
            return Err(MqttError::ProtocolError(format!(
                "Variable integer value {value} exceeds maximum {VARIABLE_INT_MAX}"
            )));
        }
        Ok(Self { value })
    }

    /// Creates a new `VariableInt` from a u32 value without validation
    ///
    /// # Safety
    ///
    /// The caller must ensure that value <= `VARIABLE_INT_MAX`
    #[must_use]
    pub fn new_unchecked(value: u32) -> Self {
        debug_assert!(value <= VARIABLE_INT_MAX);
        Self { value }
    }

    /// Returns the actual value
    #[must_use]
    pub fn value(&self) -> u32 {
        self.value
    }

    /// Returns the number of bytes needed to encode this value
    #[must_use]
    pub fn encoded_size(&self) -> u32 {
        match self.value {
            0..=127 => 1,
            128..=16_383 => 2,
            16_384..=2_097_151 => 3,
            2_097_152..=VARIABLE_INT_MAX => 4,
            _ => unreachable!("Invalid variable int value"),
        }
    }

    /// Encodes this variable integer into the provided buffer
    ///
    /// # Errors
    ///
    /// Always succeeds for valid `VariableInt` values
    pub fn encode<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        let mut val = self.value;
        loop {
            let mut byte = (val % 128) as u8;
            val /= 128;
            if val > 0 {
                byte |= 0x80; // Set continuation bit
            }
            buf.put_u8(byte);
            if val == 0 {
                break;
            }
        }
        Ok(())
    }

    /// Decodes a variable integer from the buffer
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The buffer doesn't contain enough bytes
    /// - The encoded value exceeds the maximum
    /// - More than 4 bytes are used (protocol violation)
    pub fn decode<B: Buf>(buf: &mut B) -> Result<Self> {
        let mut value = 0u32;
        let mut multiplier = 1u32;
        let mut byte_count = 0;

        loop {
            if !buf.has_remaining() {
                return Err(MqttError::MalformedPacket(
                    "Insufficient bytes for variable integer".to_string(),
                ));
            }

            byte_count += 1;
            if byte_count > 4 {
                return Err(MqttError::MalformedPacket(
                    "Variable integer exceeds 4 bytes".to_string(),
                ));
            }

            let byte = buf.get_u8();
            value += u32::from(byte & 0x7F) * multiplier;

            if (byte & 0x80) == 0 {
                break;
            }

            multiplier *= 128;
            if multiplier > 128 * 128 * 128 {
                return Err(MqttError::MalformedPacket(
                    "Variable integer overflow".to_string(),
                ));
            }
        }

        if value > VARIABLE_INT_MAX {
            return Err(MqttError::MalformedPacket(format!(
                "Variable integer value {value} exceeds maximum"
            )));
        }

        Ok(Self { value })
    }
}

impl fmt::Display for VariableInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.value)
    }
}

impl From<VariableInt> for u32 {
    fn from(v: VariableInt) -> Self {
        v.value
    }
}

impl TryFrom<u32> for VariableInt {
    type Error = MqttError;

    fn try_from(value: u32) -> Result<Self> {
        Self::new(value)
    }
}

impl TryFrom<usize> for VariableInt {
    type Error = MqttError;

    fn try_from(value: usize) -> Result<Self> {
        let value = u32::try_from(value).map_err(|_| {
            MqttError::ProtocolError("Value too large for variable integer".to_string())
        })?;
        Self::new(value)
    }
}

// BeBytes implementation
impl BeBytes for VariableInt {
    fn field_size() -> usize {
        // Variable length, so we return 0 as it's determined at runtime
        0
    }

    fn to_be_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        let _ = self.encode(&mut buf); // Encoding can't fail for valid VariableInt
        buf
    }

    fn try_from_be_bytes(
        bytes: &[u8],
    ) -> std::result::Result<(Self, usize), bebytes::BeBytesError> {
        use bytes::Bytes;
        let mut buf = Bytes::copy_from_slice(bytes);
        let start_len = buf.len();

        match Self::decode(&mut buf) {
            Ok(var_int) => {
                let consumed = start_len - buf.len();
                Ok((var_int, consumed))
            }
            Err(_) => Err(bebytes::BeBytesError::InsufficientData {
                expected: 1, // At least 1 byte expected for variable int
                actual: bytes.len(),
            }),
        }
    }

    fn to_le_bytes(&self) -> Vec<u8> {
        // MQTT uses big-endian encoding only
        self.to_be_bytes()
    }

    fn try_from_le_bytes(
        bytes: &[u8],
    ) -> std::result::Result<(Self, usize), bebytes::BeBytesError> {
        // MQTT uses big-endian encoding only
        Self::try_from_be_bytes(bytes)
    }
}

/// Encodes a u32 value as a variable byte integer (compatibility function)
///
/// This function provides compatibility with the old variable_byte module API.
/// Prefer using `VariableInt::new(value)?.encode(buf)` for new code.
///
/// # Errors
///
/// Returns `MqttError::ProtocolError` if the value exceeds the maximum
pub fn encode_variable_int<B: BufMut>(buf: &mut B, value: u32) -> Result<()> {
    VariableInt::new(value)?.encode(buf)
}

/// Decodes a variable byte integer from the buffer (compatibility function)
///
/// This function provides compatibility with the old variable_byte module API.
/// Prefer using `VariableInt::decode(buf)?.value()` for new code.
///
/// # Errors
///
/// Returns an error if decoding fails
pub fn decode_variable_int<B: Buf>(buf: &mut B) -> Result<u32> {
    Ok(VariableInt::decode(buf)?.value())
}

/// Calculates the number of bytes needed to encode a value (compatibility function)
///
/// This function provides compatibility with the old variable_byte module API.
/// Prefer using `VariableInt::new(value)?.encoded_size()` for new code.
#[must_use]
pub fn variable_int_len(value: u32) -> usize {
    VariableInt::new_unchecked(value.min(VARIABLE_INT_MAX)).encoded_size() as usize
}

/// Alias for `variable_int_len` for consistency (compatibility function)
#[must_use]
pub fn encoded_variable_int_len(value: u32) -> usize {
    variable_int_len(value)
}

/// Maximum value that can be encoded as a variable byte integer (compatibility constant)
pub const VARIABLE_BYTE_INT_MAX: u32 = VARIABLE_INT_MAX;

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[test]
    fn test_new_valid_values() {
        assert!(VariableInt::new(0).is_ok());
        assert!(VariableInt::new(127).is_ok());
        assert!(VariableInt::new(128).is_ok());
        assert!(VariableInt::new(16_383).is_ok());
        assert!(VariableInt::new(16_384).is_ok());
        assert!(VariableInt::new(2_097_151).is_ok());
        assert!(VariableInt::new(2_097_152).is_ok());
        assert!(VariableInt::new(VARIABLE_INT_MAX).is_ok());
    }

    #[test]
    fn test_new_invalid_values() {
        assert!(VariableInt::new(VARIABLE_INT_MAX + 1).is_err());
        assert!(VariableInt::new(u32::MAX).is_err());
    }

    #[test]
    fn test_encoded_size() {
        assert_eq!(VariableInt::new_unchecked(0).encoded_size(), 1);
        assert_eq!(VariableInt::new_unchecked(127).encoded_size(), 1);
        assert_eq!(VariableInt::new_unchecked(128).encoded_size(), 2);
        assert_eq!(VariableInt::new_unchecked(16_383).encoded_size(), 2);
        assert_eq!(VariableInt::new_unchecked(16_384).encoded_size(), 3);
        assert_eq!(VariableInt::new_unchecked(2_097_151).encoded_size(), 3);
        assert_eq!(VariableInt::new_unchecked(2_097_152).encoded_size(), 4);
        assert_eq!(
            VariableInt::new_unchecked(VARIABLE_INT_MAX).encoded_size(),
            4
        );
    }

    #[test]
    fn test_encode_decode_single_byte() {
        let mut buf = BytesMut::new();

        for value in [0, 1, 64, 127] {
            buf.clear();
            let var_int = VariableInt::new(value).unwrap();
            var_int.encode(&mut buf).unwrap();
            assert_eq!(buf.len(), 1);

            let decoded = VariableInt::decode(&mut buf).unwrap();
            assert_eq!(decoded.value(), value);
        }
    }

    #[test]
    fn test_encode_decode_two_bytes() {
        let mut buf = BytesMut::new();

        for value in [128, 129, 321, 16_383] {
            buf.clear();
            let var_int = VariableInt::new(value).unwrap();
            var_int.encode(&mut buf).unwrap();
            assert_eq!(buf.len(), 2);

            let decoded = VariableInt::decode(&mut buf).unwrap();
            assert_eq!(decoded.value(), value);
        }
    }

    #[test]
    fn test_encode_decode_three_bytes() {
        let mut buf = BytesMut::new();

        for value in [16_384, 65_535, 2_097_151] {
            buf.clear();
            let var_int = VariableInt::new(value).unwrap();
            var_int.encode(&mut buf).unwrap();
            assert_eq!(buf.len(), 3);

            let decoded = VariableInt::decode(&mut buf).unwrap();
            assert_eq!(decoded.value(), value);
        }
    }

    #[test]
    fn test_encode_decode_four_bytes() {
        let mut buf = BytesMut::new();

        for value in [2_097_152, 10_000_000, VARIABLE_INT_MAX] {
            buf.clear();
            let var_int = VariableInt::new(value).unwrap();
            var_int.encode(&mut buf).unwrap();
            assert_eq!(buf.len(), 4);

            let decoded = VariableInt::decode(&mut buf).unwrap();
            assert_eq!(decoded.value(), value);
        }
    }

    #[test]
    fn test_mqtt_spec_examples() {
        let mut buf = BytesMut::new();

        // Example from MQTT spec: 64 decimal = 0x40
        let var_int = VariableInt::new(64).unwrap();
        var_int.encode(&mut buf).unwrap();
        assert_eq!(buf[0], 0x40);

        // Example: 321 decimal = 0xC1 0x02
        buf.clear();
        let var_int = VariableInt::new(321).unwrap();
        var_int.encode(&mut buf).unwrap();
        assert_eq!(buf[0], 0xC1);
        assert_eq!(buf[1], 0x02);
    }

    #[test]
    fn test_bebytes_integration() {
        use bebytes::BeBytes;

        // Test BeBytes to_be_bytes
        let var_int = VariableInt::new(321).unwrap();
        let bytes = var_int.to_be_bytes();
        assert_eq!(bytes.len(), 2);
        assert_eq!(bytes[0], 0xC1);
        assert_eq!(bytes[1], 0x02);

        // Test BeBytes try_from_be_bytes
        let (decoded, consumed) = VariableInt::try_from_be_bytes(&bytes).unwrap();
        assert_eq!(decoded.value(), 321);
        assert_eq!(consumed, 2);

        // Test field_size (static method)
        assert_eq!(VariableInt::field_size(), 0); // Variable length
    }

    #[test]
    fn test_decode_insufficient_bytes() {
        let mut buf = BytesMut::new();
        buf.put_u8(0x80); // Continuation bit set but no following byte

        let result = VariableInt::decode(&mut buf);
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_too_many_bytes() {
        let mut buf = BytesMut::new();
        // 5 bytes with continuation bits
        buf.put_u8(0x80);
        buf.put_u8(0x80);
        buf.put_u8(0x80);
        buf.put_u8(0x80);
        buf.put_u8(0x01);

        let result = VariableInt::decode(&mut buf);
        assert!(result.is_err());
    }

    #[test]
    fn test_conversions() {
        let var_int = VariableInt::new(123).unwrap();

        // Test From<VariableInt> for u32
        let value: u32 = var_int.into();
        assert_eq!(value, 123);

        // Test TryFrom<u32> for VariableInt
        let var_int2 = VariableInt::try_from(456u32).unwrap();
        assert_eq!(var_int2.value(), 456);

        // Test TryFrom<usize> for VariableInt
        let var_int3 = VariableInt::try_from(789usize).unwrap();
        assert_eq!(var_int3.value(), 789);

        // Test invalid conversions
        assert!(VariableInt::try_from(VARIABLE_INT_MAX + 1).is_err());
    }

    #[test]
    fn test_display() {
        let var_int = VariableInt::new(12345).unwrap();
        assert_eq!(format!("{var_int}"), "12345");
    }

    #[cfg(test)]
    mod property_tests {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn prop_round_trip(value in 0u32..=VARIABLE_INT_MAX) {
                let mut buf = BytesMut::new();

                let var_int = VariableInt::new(value).unwrap();
                var_int.encode(&mut buf).unwrap();
                let decoded = VariableInt::decode(&mut buf).unwrap();

                prop_assert_eq!(decoded.value(), value);
            }

            #[test]
            fn prop_encoded_size_matches_actual(value in 0u32..=VARIABLE_INT_MAX) {
                let mut buf = BytesMut::new();

                let var_int = VariableInt::new(value).unwrap();
                let predicted_size = var_int.encoded_size();
                var_int.encode(&mut buf).unwrap();
                let actual_size = u32::try_from(buf.len()).expect("buffer size should fit in u32");

                prop_assert_eq!(predicted_size, actual_size);
            }

            #[test]
            fn prop_bebytes_round_trip(value in 0u32..=VARIABLE_INT_MAX) {
                use bebytes::BeBytes;

                let var_int = VariableInt::new(value).unwrap();
                let bytes = var_int.to_be_bytes();
                prop_assert_eq!(bytes.len(), var_int.encoded_size() as usize);

                let (decoded, consumed) = VariableInt::try_from_be_bytes(&bytes).unwrap();
                prop_assert_eq!(decoded.value(), value);
                prop_assert_eq!(consumed, bytes.len());
            }

            #[test]
            fn prop_invalid_values_rejected(value in (VARIABLE_INT_MAX + 1)..=u32::MAX) {
                let result = VariableInt::new(value);
                prop_assert!(result.is_err());
            }

            #[test]
            fn prop_size_boundaries(value in 0u32..=VARIABLE_INT_MAX) {
                let var_int = VariableInt::new(value).unwrap();
                let size = var_int.encoded_size();

                match value {
                    0..=127 => prop_assert_eq!(size, 1),
                    128..=16_383 => prop_assert_eq!(size, 2),
                    16_384..=2_097_151 => prop_assert_eq!(size, 3),
                    2_097_152..=VARIABLE_INT_MAX => prop_assert_eq!(size, 4),
                    _ => unreachable!(),
                }
            }
        }
    }
}
