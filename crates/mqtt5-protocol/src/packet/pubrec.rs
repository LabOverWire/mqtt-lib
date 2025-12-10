use super::ack_common::{define_ack_packet, is_valid_publish_ack_reason_code};
use crate::error::{MqttError, Result};
use crate::packet::{AckPacketHeader, PacketType};
use crate::protocol::v5::properties::Properties;

define_ack_packet! {
    /// MQTT PUBREC packet (`QoS` 2 publish received, part 1)
    pub struct PubRecPacket;
    packet_type = PacketType::PubRec;
    validator = is_valid_publish_ack_reason_code;
    error_prefix = "PUBREC";
}

impl PubRecPacket {
    #[must_use]
    pub fn create_header(&self) -> AckPacketHeader {
        AckPacketHeader::create(self.packet_id, self.reason_code)
    }

    /// # Errors
    /// Returns an error if the reason code in the header is invalid
    pub fn from_header(header: AckPacketHeader, properties: Properties) -> Result<Self> {
        let reason_code = header.get_reason_code().ok_or_else(|| {
            MqttError::MalformedPacket(format!(
                "Invalid PUBREC reason code: 0x{:02X}",
                header.reason_code
            ))
        })?;

        if !is_valid_publish_ack_reason_code(reason_code) {
            return Err(MqttError::MalformedPacket(format!(
                "Invalid PUBREC reason code: {reason_code:?}"
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

    #[test]
    fn test_pubrec_basic() {
        let packet = PubRecPacket::new(123);

        assert_eq!(packet.packet_id, 123);
        assert_eq!(packet.reason_code, ReasonCode::Success);
        assert!(packet.properties.is_empty());
    }

    #[test]
    fn test_pubrec_with_reason() {
        let packet = PubRecPacket::new_with_reason(456, ReasonCode::QuotaExceeded)
            .with_reason_string("Quota exceeded for client".to_string());

        assert_eq!(packet.packet_id, 456);
        assert_eq!(packet.reason_code, ReasonCode::QuotaExceeded);
        assert!(packet.properties.contains(PropertyId::ReasonString));
    }

    #[test]
    fn test_pubrec_encode_decode() {
        let packet = PubRecPacket::new(789);

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        assert_eq!(fixed_header.packet_type, PacketType::PubRec);

        let decoded = PubRecPacket::decode_body(&mut buf, &fixed_header).unwrap();
        assert_eq!(decoded.packet_id, 789);
        assert_eq!(decoded.reason_code, ReasonCode::Success);
    }

    #[test]
    fn test_pubrec_v311_style() {
        let mut buf = BytesMut::new();
        buf.put_u16(1234);

        let fixed_header = FixedHeader::new(PacketType::PubRec, 0, 2);
        let decoded = PubRecPacket::decode_body(&mut buf, &fixed_header).unwrap();

        assert_eq!(decoded.packet_id, 1234);
        assert_eq!(decoded.reason_code, ReasonCode::Success);
        assert!(decoded.properties.is_empty());
    }

    #[test]
    fn test_pubrec_invalid_reason_code() {
        let mut buf = BytesMut::new();
        buf.put_u16(123);
        buf.put_u8(0xFF);

        let fixed_header = FixedHeader::new(PacketType::PubRec, 0, 3);
        let result = PubRecPacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }

    #[test]
    fn test_pubrec_missing_packet_id() {
        let mut buf = BytesMut::new();
        buf.put_u8(0);

        let fixed_header = FixedHeader::new(PacketType::PubRec, 0, 1);
        let result = PubRecPacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }
}
