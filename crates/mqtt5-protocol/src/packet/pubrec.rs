use crate::error::{MqttError, Result};
use crate::packet::{AckPacketHeader, FixedHeader, MqttPacket, PacketType};
use crate::protocol::v5::properties::Properties;
use crate::types::ReasonCode;
use bytes::{Buf, BufMut};

/// MQTT PUBREC packet (`QoS` 2 publish received, part 1)
#[derive(Debug, Clone)]
pub struct PubRecPacket {
    /// Packet identifier
    pub packet_id: u16,
    /// Reason code
    pub reason_code: ReasonCode,
    /// PUBREC properties (v5.0 only)
    pub properties: Properties,
}

impl PubRecPacket {
    /// Creates a new PUBREC packet
    #[must_use]
    pub fn new(packet_id: u16) -> Self {
        Self {
            packet_id,
            reason_code: ReasonCode::Success,
            properties: Properties::default(),
        }
    }

    /// Creates a new PUBREC packet with a reason code
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

    /// Validates the reason code for PUBREC
    fn is_valid_pubrec_reason_code(code: ReasonCode) -> bool {
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
                "Invalid PUBREC reason code: 0x{:02X}",
                header.reason_code
            ))
        })?;

        if !Self::is_valid_pubrec_reason_code(reason_code) {
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

impl MqttPacket for PubRecPacket {
    fn packet_type(&self) -> PacketType {
        PacketType::PubRec
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

    fn decode_body<B: Buf>(buf: &mut B, _fixed_header: &FixedHeader) -> Result<Self> {
        // Packet identifier
        if buf.remaining() < 2 {
            return Err(MqttError::MalformedPacket(
                "PUBREC missing packet identifier".to_string(),
            ));
        }
        let packet_id = buf.get_u16();

        // Reason code and properties (optional in v3.1.1, always present in v5.0)
        let (reason_code, properties) = if buf.has_remaining() {
            // Read reason code
            let reason_byte = buf.get_u8();
            let code = ReasonCode::from_u8(reason_byte).ok_or_else(|| {
                MqttError::MalformedPacket(format!("Invalid PUBREC reason code: {reason_byte}"))
            })?;

            if !Self::is_valid_pubrec_reason_code(code) {
                return Err(MqttError::MalformedPacket(format!(
                    "Invalid PUBREC reason code: {code:?}"
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
}
