use crate::error::{MqttError, Result};
use crate::packet::{FixedHeader, MqttPacket, PacketType};
use crate::protocol::v5::properties::Properties;
use bytes::{Buf, BufMut};

/// MQTT UNSUBACK packet
#[derive(Debug, Clone)]
pub struct UnsubAckPacket {
    /// Packet identifier
    pub packet_id: u16,
    /// Reason codes for each unsubscription
    pub reason_codes: Vec<UnsubAckReasonCode>,
    /// UNSUBACK properties (v5.0 only)
    pub properties: Properties,
}

/// UNSUBACK reason codes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UnsubAckReasonCode {
    /// Success
    Success = 0x00,
    /// No subscription existed
    NoSubscriptionExisted = 0x11,
    /// Unspecified error
    UnspecifiedError = 0x80,
    /// Implementation specific error
    ImplementationSpecificError = 0x83,
    /// Not authorized
    NotAuthorized = 0x87,
    /// Topic filter invalid
    TopicFilterInvalid = 0x8F,
    /// Packet identifier in use
    PacketIdentifierInUse = 0x91,
}

impl UnsubAckReasonCode {
    /// Converts a u8 to an `UnsubAckReasonCode`
    #[must_use]
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x00 => Some(Self::Success),
            0x11 => Some(Self::NoSubscriptionExisted),
            0x80 => Some(Self::UnspecifiedError),
            0x83 => Some(Self::ImplementationSpecificError),
            0x87 => Some(Self::NotAuthorized),
            0x8F => Some(Self::TopicFilterInvalid),
            0x91 => Some(Self::PacketIdentifierInUse),
            _ => None,
        }
    }

    /// Returns true if this is a success code
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success)
    }
}

impl UnsubAckPacket {
    /// Creates a new UNSUBACK packet
    #[must_use]
    pub fn new(packet_id: u16) -> Self {
        Self {
            packet_id,
            reason_codes: Vec::new(),
            properties: Properties::default(),
        }
    }

    /// Adds a reason code
    #[must_use]
    pub fn add_reason_code(mut self, code: UnsubAckReasonCode) -> Self {
        self.reason_codes.push(code);
        self
    }

    /// Adds a success reason code
    #[must_use]
    pub fn add_success(mut self) -> Self {
        self.reason_codes.push(UnsubAckReasonCode::Success);
        self
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
}

impl MqttPacket for UnsubAckPacket {
    fn packet_type(&self) -> PacketType {
        PacketType::UnsubAck
    }

    fn encode_body<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        // Variable header
        buf.put_u16(self.packet_id);

        // Properties (v5.0)
        self.properties.encode(buf)?;

        // Payload - reason codes
        if self.reason_codes.is_empty() {
            return Err(MqttError::MalformedPacket(
                "UNSUBACK packet must contain at least one reason code".to_string(),
            ));
        }

        for code in &self.reason_codes {
            buf.put_u8(*code as u8);
        }

        Ok(())
    }

    fn decode_body<B: Buf>(buf: &mut B, _fixed_header: &FixedHeader) -> Result<Self> {
        // Packet identifier
        if buf.remaining() < 2 {
            return Err(MqttError::MalformedPacket(
                "UNSUBACK missing packet identifier".to_string(),
            ));
        }
        let packet_id = buf.get_u16();

        // Properties (v5.0)
        let properties = Properties::decode(buf)?;

        // Payload - reason codes
        let mut reason_codes = Vec::new();

        if !buf.has_remaining() {
            return Err(MqttError::MalformedPacket(
                "UNSUBACK packet must contain at least one reason code".to_string(),
            ));
        }

        while buf.has_remaining() {
            let code_byte = buf.get_u8();
            let code = UnsubAckReasonCode::from_u8(code_byte).ok_or_else(|| {
                MqttError::MalformedPacket(format!(
                    "Invalid UNSUBACK reason code: 0x{code_byte:02X}"
                ))
            })?;
            reason_codes.push(code);
        }

        Ok(Self {
            packet_id,
            reason_codes,
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
    fn test_unsuback_reason_code_is_success() {
        assert!(UnsubAckReasonCode::Success.is_success());
        assert!(!UnsubAckReasonCode::NoSubscriptionExisted.is_success());
        assert!(!UnsubAckReasonCode::NotAuthorized.is_success());
    }

    #[test]
    fn test_unsuback_basic() {
        let packet = UnsubAckPacket::new(123)
            .add_success()
            .add_success()
            .add_reason_code(UnsubAckReasonCode::NoSubscriptionExisted);

        assert_eq!(packet.packet_id, 123);
        assert_eq!(packet.reason_codes.len(), 3);
        assert_eq!(packet.reason_codes[0], UnsubAckReasonCode::Success);
        assert_eq!(packet.reason_codes[1], UnsubAckReasonCode::Success);
        assert_eq!(
            packet.reason_codes[2],
            UnsubAckReasonCode::NoSubscriptionExisted
        );
    }

    #[test]
    fn test_unsuback_encode_decode() {
        let packet = UnsubAckPacket::new(789)
            .add_success()
            .add_reason_code(UnsubAckReasonCode::NotAuthorized)
            .add_reason_code(UnsubAckReasonCode::TopicFilterInvalid)
            .with_reason_string("Invalid topic filter".to_string());

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        assert_eq!(fixed_header.packet_type, PacketType::UnsubAck);

        let decoded = UnsubAckPacket::decode_body(&mut buf, &fixed_header).unwrap();
        assert_eq!(decoded.packet_id, 789);
        assert_eq!(decoded.reason_codes.len(), 3);
        assert_eq!(decoded.reason_codes[0], UnsubAckReasonCode::Success);
        assert_eq!(decoded.reason_codes[1], UnsubAckReasonCode::NotAuthorized);
        assert_eq!(
            decoded.reason_codes[2],
            UnsubAckReasonCode::TopicFilterInvalid
        );

        let reason_str = decoded.properties.get(PropertyId::ReasonString);
        assert!(reason_str.is_some());
    }

    #[test]
    fn test_unsuback_empty_reason_codes() {
        let packet = UnsubAckPacket::new(123);

        let mut buf = BytesMut::new();
        let result = packet.encode(&mut buf);
        assert!(result.is_err());
    }
}
