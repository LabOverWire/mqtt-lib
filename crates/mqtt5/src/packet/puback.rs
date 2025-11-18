use crate::error::{MqttError, Result};
use crate::packet::{AckPacketHeader, FixedHeader, MqttPacket, PacketType};
use crate::protocol::v5::properties::Properties;
use crate::types::ReasonCode;
use bytes::{Buf, BufMut};

/// MQTT PUBACK packet (`QoS` 1 publish acknowledgment)
#[derive(Debug, Clone)]
pub struct PubAckPacket {
    /// Packet identifier
    pub packet_id: u16,
    /// Reason code
    pub reason_code: ReasonCode,
    /// PUBACK properties (v5.0 only)
    pub properties: Properties,
}

impl PubAckPacket {
    /// Creates a new PUBACK packet
    #[must_use]
    pub fn new(packet_id: u16) -> Self {
        Self {
            packet_id,
            reason_code: ReasonCode::Success,
            properties: Properties::default(),
        }
    }

    /// Creates a new PUBACK packet with a reason code
    #[must_use]
    pub fn new_with_reason(packet_id: u16, reason_code: ReasonCode) -> Self {
        Self {
            packet_id,
            reason_code,
            properties: Properties::default(),
        }
    }

    /// Sets the reason string
    #[must_use]
    pub fn with_reason_string(mut self, reason: String) -> Self {
        self.properties.set_reason_string(reason);
        self
    }

    /// Adds a user property
    #[must_use]
    pub fn with_user_property(mut self, key: String, value: String) -> Self {
        self.properties.add_user_property(key, value);
        self
    }

    /// Validates the reason code for PUBACK
    fn is_valid_puback_reason_code(code: ReasonCode) -> bool {
        matches!(
            code,
            ReasonCode::Success
                | ReasonCode::NoMatchingSubscribers
                | ReasonCode::UnspecifiedError
                | ReasonCode::ImplementationSpecificError
                | ReasonCode::NotAuthorized
                | ReasonCode::TopicNameInvalid
                | ReasonCode::PacketIdentifierInUse
                | ReasonCode::QuotaExceeded
                | ReasonCode::PayloadFormatInvalid
        )
    }

    /// Creates a bebytes header for this packet
    #[must_use]
    pub fn create_header(&self) -> AckPacketHeader {
        AckPacketHeader::create(self.packet_id, self.reason_code)
    }

    /// Creates a packet from a bebytes header and properties
    ///
    /// # Errors
    ///
    /// Returns an error if the reason code in the header is invalid
    pub fn from_header(header: AckPacketHeader, properties: Properties) -> Result<Self> {
        let reason_code = header.get_reason_code().ok_or_else(|| {
            MqttError::MalformedPacket(format!(
                "Invalid PUBACK reason code: 0x{:02X}",
                header.reason_code
            ))
        })?;

        if !Self::is_valid_puback_reason_code(reason_code) {
            return Err(MqttError::MalformedPacket(format!(
                "Invalid PUBACK reason code: {reason_code:?}"
            )));
        }

        Ok(Self {
            packet_id: header.packet_id,
            reason_code,
            properties,
        })
    }
}

impl MqttPacket for PubAckPacket {
    fn packet_type(&self) -> PacketType {
        PacketType::PubAck
    }

    fn encode_body<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        // Always encode packet_id (required)
        buf.put_u16(self.packet_id);

        // For v5.0, encode reason code and properties if not default
        // For v3.1.1, we would skip these if reason_code is Success and no properties
        if self.reason_code != ReasonCode::Success || !self.properties.is_empty() {
            buf.put_u8(u8::from(self.reason_code));
            self.properties.encode(buf)?;
        }

        Ok(())
    }

    fn decode_body<B: Buf>(buf: &mut B, fixed_header: &FixedHeader) -> Result<Self> {
        tracing::trace!(
            fixed_header_remaining = fixed_header.remaining_length,
            buf_remaining = buf.remaining(),
            "PUBACK decode started"
        );

        // Packet identifier is always required (2 bytes minimum)
        if buf.remaining() < 2 {
            return Err(MqttError::MalformedPacket(
                "PUBACK missing packet identifier".to_string(),
            ));
        }
        let packet_id = buf.get_u16();

        // MQTT v5.0: Reason code and properties are optional
        // If remaining length is 2 (just packet ID), reason code defaults to Success (0x00)
        let (reason_code, properties) = if buf.has_remaining() {
            // Read reason code
            let reason_byte = buf.get_u8();
            let code = ReasonCode::from_u8(reason_byte).ok_or_else(|| {
                MqttError::MalformedPacket(format!(
                    "Invalid PUBACK reason code: {reason_byte} (0x{reason_byte:02X})"
                ))
            })?;

            if !Self::is_valid_puback_reason_code(code) {
                return Err(MqttError::MalformedPacket(format!(
                    "Invalid PUBACK reason code: {code:?}"
                )));
            }

            // Properties (if present)
            let props = if buf.has_remaining() {
                Properties::decode(buf)?
            } else {
                Properties::default()
            };

            (code, props)
        } else {
            // No reason code or properties - Success with empty properties
            (ReasonCode::Success, Properties::default())
        };

        Ok(Self {
            packet_id,
            reason_code,
            properties,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::v5::properties::PropertyId;
    use bytes::BytesMut;

    #[cfg(test)]
    mod bebytes_tests {
        use super::*;
        use bebytes::BeBytes;
        use proptest::prelude::*;

        #[test]
        fn test_ack_header_creation() {
            let header = AckPacketHeader::create(123, ReasonCode::Success);
            assert_eq!(header.packet_id, 123);
            assert_eq!(header.reason_code, 0x00);
            assert_eq!(header.get_reason_code(), Some(ReasonCode::Success));
        }

        #[test]
        fn test_ack_header_round_trip() {
            let header = AckPacketHeader::create(456, ReasonCode::QuotaExceeded);
            let bytes = header.to_be_bytes();
            assert_eq!(bytes.len(), 3); // 2 bytes packet_id + 1 byte reason_code

            let (decoded, consumed) = AckPacketHeader::try_from_be_bytes(&bytes).unwrap();
            assert_eq!(consumed, 3);
            assert_eq!(decoded, header);
            assert_eq!(decoded.packet_id, 456);
            assert_eq!(decoded.get_reason_code(), Some(ReasonCode::QuotaExceeded));
        }

        #[test]
        fn test_puback_from_header() {
            let header = AckPacketHeader::create(789, ReasonCode::NoMatchingSubscribers);
            let properties = Properties::default();

            let packet = PubAckPacket::from_header(header, properties).unwrap();
            assert_eq!(packet.packet_id, 789);
            assert_eq!(packet.reason_code, ReasonCode::NoMatchingSubscribers);
        }

        proptest! {
            #[test]
            fn prop_ack_header_round_trip(
                packet_id in any::<u16>(),
                reason_code in 0u8..=255u8
            ) {
                let header = AckPacketHeader {
                    packet_id,
                    reason_code,
                };

                let bytes = header.to_be_bytes();
                let (decoded, consumed) = AckPacketHeader::try_from_be_bytes(&bytes).unwrap();

                prop_assert_eq!(consumed, 3);
                prop_assert_eq!(decoded, header);
                prop_assert_eq!(decoded.packet_id, packet_id);
                prop_assert_eq!(decoded.reason_code, reason_code);
            }
        }
    }

    #[test]
    fn test_puback_basic() {
        let packet = PubAckPacket::new(123);

        assert_eq!(packet.packet_id, 123);
        assert_eq!(packet.reason_code, ReasonCode::Success);
        assert!(packet.properties.is_empty());
    }

    #[test]
    fn test_puback_with_reason() {
        let packet = PubAckPacket::new_with_reason(456, ReasonCode::NoMatchingSubscribers)
            .with_reason_string("No subscribers for topic".to_string());

        assert_eq!(packet.packet_id, 456);
        assert_eq!(packet.reason_code, ReasonCode::NoMatchingSubscribers);
        assert!(packet.properties.contains(PropertyId::ReasonString));
    }

    #[test]
    fn test_puback_encode_decode_minimal() {
        let packet = PubAckPacket::new(789);

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        assert_eq!(fixed_header.packet_type, PacketType::PubAck);

        let decoded = PubAckPacket::decode_body(&mut buf, &fixed_header).unwrap();
        assert_eq!(decoded.packet_id, 789);
        assert_eq!(decoded.reason_code, ReasonCode::Success);
    }

    #[test]
    fn test_puback_encode_decode_with_reason() {
        let packet = PubAckPacket::new_with_reason(999, ReasonCode::QuotaExceeded)
            .with_user_property("quota".to_string(), "exceeded".to_string());

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        let decoded = PubAckPacket::decode_body(&mut buf, &fixed_header).unwrap();

        assert_eq!(decoded.packet_id, 999);
        assert_eq!(decoded.reason_code, ReasonCode::QuotaExceeded);
        assert!(decoded.properties.contains(PropertyId::UserProperty));
    }

    #[test]
    fn test_puback_v311_style() {
        // v3.1.1 style - only packet ID
        let mut buf = BytesMut::new();
        buf.put_u16(1234);

        let fixed_header = FixedHeader::new(PacketType::PubAck, 0, 2);
        let decoded = PubAckPacket::decode_body(&mut buf, &fixed_header).unwrap();

        assert_eq!(decoded.packet_id, 1234);
        assert_eq!(decoded.reason_code, ReasonCode::Success);
        assert!(decoded.properties.is_empty());
    }

    #[test]
    fn test_puback_invalid_reason_code() {
        let mut buf = BytesMut::new();
        buf.put_u16(123);
        buf.put_u8(0xFF); // Invalid reason code

        let fixed_header = FixedHeader::new(PacketType::PubAck, 0, 3);
        let result = PubAckPacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }

    #[test]
    fn test_puback_missing_packet_id() {
        let mut buf = BytesMut::new();
        buf.put_u8(0); // Only one byte, not enough for packet ID

        let fixed_header = FixedHeader::new(PacketType::PubAck, 0, 1);
        let result = PubAckPacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }
}
