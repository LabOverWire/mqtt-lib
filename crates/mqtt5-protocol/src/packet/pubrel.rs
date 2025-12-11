use super::ack_common::{define_ack_packet, is_valid_pubrel_reason_code};
use crate::packet::PacketType;

define_ack_packet! {
    /// MQTT PUBREL packet (`QoS` 2 publish release, part 2)
    pub struct PubRelPacket;
    packet_type = PacketType::PubRel;
    validator = is_valid_pubrel_reason_code;
    error_prefix = "PUBREL";
    flags = 0x02;
    validate_flags = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::{FixedHeader, MqttPacket};
    use crate::protocol::v5::properties::PropertyId;
    use crate::types::ReasonCode;
    use bytes::{BufMut, BytesMut};

    #[test]
    fn test_pubrel_basic() {
        let packet = PubRelPacket::new(123);

        assert_eq!(packet.packet_id, 123);
        assert_eq!(packet.reason_code, ReasonCode::Success);
        assert!(packet.properties.is_empty());
        assert_eq!(packet.flags(), 0x02);
    }

    #[test]
    fn test_pubrel_with_reason() {
        let packet = PubRelPacket::new_with_reason(456, ReasonCode::PacketIdentifierNotFound)
            .with_reason_string("Packet ID not found".to_string());

        assert_eq!(packet.packet_id, 456);
        assert_eq!(packet.reason_code, ReasonCode::PacketIdentifierNotFound);
        assert!(packet.properties.contains(PropertyId::ReasonString));
    }

    #[test]
    fn test_pubrel_encode_decode() {
        let packet = PubRelPacket::new(789);

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        assert_eq!(fixed_header.packet_type, PacketType::PubRel);
        assert_eq!(fixed_header.flags, 0x02);

        let decoded = PubRelPacket::decode_body(&mut buf, &fixed_header).unwrap();
        assert_eq!(decoded.packet_id, 789);
        assert_eq!(decoded.reason_code, ReasonCode::Success);
    }

    #[test]
    fn test_pubrel_invalid_flags() {
        let mut buf = BytesMut::new();
        buf.put_u16(123);

        let fixed_header = FixedHeader::new(PacketType::PubRel, 0x00, 2);
        let result = PubRelPacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }

    #[test]
    fn test_pubrel_v311_style() {
        let mut buf = BytesMut::new();
        buf.put_u16(1234);

        let fixed_header = FixedHeader::new(PacketType::PubRel, 0x02, 2);
        let decoded = PubRelPacket::decode_body(&mut buf, &fixed_header).unwrap();

        assert_eq!(decoded.packet_id, 1234);
        assert_eq!(decoded.reason_code, ReasonCode::Success);
    }

    #[test]
    fn test_pubrel_invalid_reason_code() {
        let mut buf = BytesMut::new();
        buf.put_u16(123);
        buf.put_u8(0xFF);

        let fixed_header = FixedHeader::new(PacketType::PubRel, 0x02, 3);
        let result = PubRelPacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }

    #[test]
    fn test_pubrel_missing_packet_id() {
        let mut buf = BytesMut::new();
        buf.put_u8(0);

        let fixed_header = FixedHeader::new(PacketType::PubRel, 0x02, 1);
        let result = PubRelPacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }
}
