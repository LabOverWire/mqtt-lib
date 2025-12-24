use super::ack_common::{define_ack_packet, is_valid_pubrel_reason_code};
use crate::packet::PacketType;
use crate::prelude::{format, String, ToString};

define_ack_packet! {
    /// MQTT PUBCOMP packet (`QoS` 2 publish complete, part 3)
    pub struct PubCompPacket;
    packet_type = PacketType::PubComp;
    validator = is_valid_pubrel_reason_code;
    error_prefix = "PUBCOMP";
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::{FixedHeader, MqttPacket};
    use crate::protocol::v5::properties::PropertyId;
    use crate::types::ReasonCode;
    use bytes::{BufMut, BytesMut};

    #[test]
    fn test_pubcomp_basic() {
        let packet = PubCompPacket::new(123);

        assert_eq!(packet.packet_id, 123);
        assert_eq!(packet.reason_code, ReasonCode::Success);
        assert!(packet.properties.is_empty());
    }

    #[test]
    fn test_pubcomp_with_reason() {
        let packet = PubCompPacket::new_with_reason(456, ReasonCode::PacketIdentifierNotFound)
            .with_reason_string("Packet ID not found".to_string());

        assert_eq!(packet.packet_id, 456);
        assert_eq!(packet.reason_code, ReasonCode::PacketIdentifierNotFound);
        assert!(packet.properties.contains(PropertyId::ReasonString));
    }

    #[test]
    fn test_pubcomp_encode_decode() {
        let packet = PubCompPacket::new(789);

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        assert_eq!(fixed_header.packet_type, PacketType::PubComp);

        let decoded = PubCompPacket::decode_body(&mut buf, &fixed_header).unwrap();
        assert_eq!(decoded.packet_id, 789);
        assert_eq!(decoded.reason_code, ReasonCode::Success);
    }

    #[test]
    fn test_pubcomp_encode_decode_with_properties() {
        let packet = PubCompPacket::new(999)
            .with_user_property("status".to_string(), "completed".to_string());

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        let decoded = PubCompPacket::decode_body(&mut buf, &fixed_header).unwrap();

        assert_eq!(decoded.packet_id, 999);
        assert!(decoded.properties.contains(PropertyId::UserProperty));
    }

    #[test]
    fn test_pubcomp_v311_style() {
        let mut buf = BytesMut::new();
        buf.put_u16(1234);

        let fixed_header = FixedHeader::new(PacketType::PubComp, 0, 2);
        let decoded = PubCompPacket::decode_body(&mut buf, &fixed_header).unwrap();

        assert_eq!(decoded.packet_id, 1234);
        assert_eq!(decoded.reason_code, ReasonCode::Success);
    }

    #[test]
    fn test_pubcomp_invalid_reason_code() {
        let mut buf = BytesMut::new();
        buf.put_u16(123);
        buf.put_u8(0xFF);

        let fixed_header = FixedHeader::new(PacketType::PubComp, 0, 3);
        let result = PubCompPacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }

    #[test]
    fn test_pubcomp_missing_packet_id() {
        let mut buf = BytesMut::new();
        buf.put_u8(0);

        let fixed_header = FixedHeader::new(PacketType::PubComp, 0, 1);
        let result = PubCompPacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }
}
