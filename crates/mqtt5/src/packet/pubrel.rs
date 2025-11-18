use crate::error::{MqttError, Result};
use crate::packet::{FixedHeader, MqttPacket, PacketType};
use crate::protocol::v5::properties::Properties;
use crate::types::ReasonCode;
use bytes::{Buf, BufMut};

/// MQTT PUBREL packet (`QoS` 2 publish release, part 2)
#[derive(Debug, Clone)]
pub struct PubRelPacket {
    /// Packet identifier
    pub packet_id: u16,
    /// Reason code
    pub reason_code: ReasonCode,
    /// PUBREL properties (v5.0 only)
    pub properties: Properties,
}

impl PubRelPacket {
    /// Creates a new PUBREL packet
    #[must_use]
    pub fn new(packet_id: u16) -> Self {
        Self {
            packet_id,
            reason_code: ReasonCode::Success,
            properties: Properties::default(),
        }
    }

    /// Creates a new PUBREL packet with a reason code
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

    /// Validates the reason code for PUBREL
    fn is_valid_pubrel_reason_code(code: ReasonCode) -> bool {
        matches!(
            code,
            ReasonCode::Success | ReasonCode::PacketIdentifierNotFound
        )
    }
}

impl MqttPacket for PubRelPacket {
    fn packet_type(&self) -> PacketType {
        PacketType::PubRel
    }

    fn flags(&self) -> u8 {
        0x02 // PUBREL must have flags = 0x02
    }

    fn encode_body<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        // Variable header
        buf.put_u16(self.packet_id);

        // For v5.0, always encode reason code and properties
        // For v3.1.1, we would skip these if reason_code is Success and no properties
        if self.reason_code != ReasonCode::Success || !self.properties.is_empty() {
            buf.put_u8(u8::from(self.reason_code));
            self.properties.encode(buf)?;
        }

        Ok(())
    }

    fn decode_body<B: Buf>(buf: &mut B, fixed_header: &FixedHeader) -> Result<Self> {
        // Validate flags
        if fixed_header.flags != 0x02 {
            return Err(MqttError::MalformedPacket(format!(
                "Invalid PUBREL flags: expected 0x02, got 0x{:02X}",
                fixed_header.flags
            )));
        }

        // Packet identifier
        if buf.remaining() < 2 {
            return Err(MqttError::MalformedPacket(
                "PUBREL missing packet identifier".to_string(),
            ));
        }
        let packet_id = buf.get_u16();

        // Reason code and properties (optional in v3.1.1, always present in v5.0)
        let (reason_code, properties) = if buf.has_remaining() {
            // Read reason code
            let reason_byte = buf.get_u8();
            let code = ReasonCode::from_u8(reason_byte).ok_or_else(|| {
                MqttError::MalformedPacket(format!("Invalid PUBREL reason code: {reason_byte}"))
            })?;

            if !Self::is_valid_pubrel_reason_code(code) {
                return Err(MqttError::MalformedPacket(format!(
                    "Invalid PUBREL reason code: {code:?}"
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
            // v3.1.1 style - no reason code or properties means success
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

        let fixed_header = FixedHeader::new(PacketType::PubRel, 0x00, 2); // Wrong flags
        let result = PubRelPacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }
}
