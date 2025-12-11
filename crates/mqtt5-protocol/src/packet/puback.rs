use super::ack_common::{define_ack_packet, is_valid_publish_ack_reason_code};
use crate::error::{MqttError, Result};
use crate::packet::{AckPacketHeader, PacketType};
use crate::protocol::v5::properties::Properties;

define_ack_packet! {
    /// MQTT PUBACK packet (`QoS` 1 publish acknowledgment)
    pub struct PubAckPacket;
    packet_type = PacketType::PubAck;
    validator = is_valid_publish_ack_reason_code;
    error_prefix = "PUBACK";
}

impl PubAckPacket {
    #[must_use]
    pub fn create_header(&self) -> AckPacketHeader {
        AckPacketHeader::create(self.packet_id, self.reason_code)
    }

    /// # Errors
    /// Returns an error if the reason code in the header is invalid
    pub fn from_header(header: AckPacketHeader, properties: Properties) -> Result<Self> {
        let reason_code = header.get_reason_code().ok_or_else(|| {
            MqttError::MalformedPacket(format!(
                "Invalid PUBACK reason code: 0x{:02X}",
                header.reason_code
            ))
        })?;

        if !is_valid_publish_ack_reason_code(reason_code) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::{FixedHeader, MqttPacket};
    use crate::protocol::v5::properties::PropertyId;
    use crate::types::ReasonCode;
    use bytes::{BufMut, BytesMut};

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
            assert_eq!(bytes.len(), 3);

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
        buf.put_u8(0xFF);

        let fixed_header = FixedHeader::new(PacketType::PubAck, 0, 3);
        let result = PubAckPacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }

    #[test]
    fn test_puback_missing_packet_id() {
        let mut buf = BytesMut::new();
        buf.put_u8(0);

        let fixed_header = FixedHeader::new(PacketType::PubAck, 0, 1);
        let result = PubAckPacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }
}
