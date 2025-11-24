use crate::error::{MqttError, Result};
use crate::packet::{FixedHeader, MqttPacket, PacketType};
use crate::protocol::v5::properties::Properties;
use crate::protocol::v5::reason_codes::NORMAL_DISCONNECTION;
use crate::types::ReasonCode;
use bytes::{Buf, BufMut};

/// MQTT DISCONNECT packet
#[derive(Debug, Clone)]
pub struct DisconnectPacket {
    /// Disconnect reason code
    pub reason_code: ReasonCode,
    /// DISCONNECT properties (v5.0 only)
    pub properties: Properties,
}

impl DisconnectPacket {
    /// Creates a new DISCONNECT packet
    #[must_use]
    pub fn new(reason_code: ReasonCode) -> Self {
        Self {
            reason_code,
            properties: Properties::default(),
        }
    }

    /// Creates a normal disconnection packet
    #[must_use]
    pub fn normal() -> Self {
        Self::new(NORMAL_DISCONNECTION)
    }

    /// Sets the session expiry interval
    #[must_use]
    pub fn with_session_expiry_interval(mut self, seconds: u32) -> Self {
        self.properties.set_session_expiry_interval(seconds);
        self
    }

    /// Sets the reason string
    #[must_use]
    pub fn with_reason_string(mut self, reason: String) -> Self {
        self.properties.set_reason_string(reason);
        self
    }

    /// Sets the server reference for redirect
    #[must_use]
    pub fn with_server_reference(mut self, reference: String) -> Self {
        self.properties.set_server_reference(reference);
        self
    }

    /// Adds a user property
    #[must_use]
    pub fn with_user_property(mut self, key: String, value: String) -> Self {
        self.properties.add_user_property(key, value);
        self
    }

    /// Validates the reason code for DISCONNECT
    fn is_valid_disconnect_reason_code(code: ReasonCode) -> bool {
        matches!(
            code,
            NORMAL_DISCONNECTION
                | ReasonCode::DisconnectWithWillMessage
                | ReasonCode::UnspecifiedError
                | ReasonCode::MalformedPacket
                | ReasonCode::ProtocolError
                | ReasonCode::ImplementationSpecificError
                | ReasonCode::NotAuthorized
                | ReasonCode::ServerBusy
                | ReasonCode::ServerShuttingDown
                | ReasonCode::KeepAliveTimeout
                | ReasonCode::SessionTakenOver
                | ReasonCode::TopicFilterInvalid
                | ReasonCode::TopicNameInvalid
                | ReasonCode::ReceiveMaximumExceeded
                | ReasonCode::TopicAliasInvalid
                | ReasonCode::PacketTooLarge
                | ReasonCode::MessageRateTooHigh
                | ReasonCode::QuotaExceeded
                | ReasonCode::AdministrativeAction
                | ReasonCode::PayloadFormatInvalid
                | ReasonCode::RetainNotSupported
                | ReasonCode::QoSNotSupported
                | ReasonCode::UseAnotherServer
                | ReasonCode::ServerMoved
                | ReasonCode::SharedSubscriptionsNotSupported
                | ReasonCode::ConnectionRateExceeded
                | ReasonCode::MaximumConnectTime
                | ReasonCode::SubscriptionIdentifiersNotSupported
                | ReasonCode::WildcardSubscriptionsNotSupported
        )
    }
}

impl MqttPacket for DisconnectPacket {
    fn packet_type(&self) -> PacketType {
        PacketType::Disconnect
    }

    fn encode_body<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        // For v5.0, encode reason code and properties
        // For v3.1.1, DISCONNECT has no variable header or payload

        // Only encode if not normal disconnection or has properties
        if self.reason_code != NORMAL_DISCONNECTION || !self.properties.is_empty() {
            buf.put_u8(u8::from(self.reason_code));

            // Only encode properties if present
            if !self.properties.is_empty() {
                self.properties.encode(buf)?;
            }
        }

        Ok(())
    }

    fn decode_body<B: Buf>(buf: &mut B, _fixed_header: &FixedHeader) -> Result<Self> {
        // Check if we have any data
        if !buf.has_remaining() {
            // v3.1.1 style or v5.0 normal disconnection with no properties
            return Ok(Self::normal());
        }

        // Read reason code
        let reason_byte = buf.get_u8();
        let reason_code = ReasonCode::from_u8(reason_byte).ok_or_else(|| {
            MqttError::MalformedPacket(format!("Invalid DISCONNECT reason code: {reason_byte}"))
        })?;

        if !Self::is_valid_disconnect_reason_code(reason_code) {
            return Err(MqttError::MalformedPacket(format!(
                "Invalid DISCONNECT reason code: {reason_code:?}"
            )));
        }

        // Properties (if present)
        let properties = if buf.has_remaining() {
            Properties::decode(buf)?
        } else {
            Properties::default()
        };

        Ok(Self {
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
    fn test_disconnect_normal() {
        let packet = DisconnectPacket::normal();
        assert_eq!(packet.reason_code, NORMAL_DISCONNECTION);
        assert!(packet.properties.is_empty());
    }

    #[test]
    fn test_disconnect_with_reason() {
        let packet = DisconnectPacket::new(ReasonCode::ServerShuttingDown)
            .with_reason_string("Maintenance mode".to_string());

        assert_eq!(packet.reason_code, ReasonCode::ServerShuttingDown);
        assert!(packet.properties.contains(PropertyId::ReasonString));
    }

    #[test]
    fn test_disconnect_encode_decode_normal() {
        let packet = DisconnectPacket::normal();

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        assert_eq!(fixed_header.packet_type, PacketType::Disconnect);

        let decoded = DisconnectPacket::decode_body(&mut buf, &fixed_header).unwrap();
        assert_eq!(decoded.reason_code, NORMAL_DISCONNECTION);
    }

    #[test]
    fn test_disconnect_encode_decode_with_properties() {
        let packet = DisconnectPacket::new(ReasonCode::SessionTakenOver)
            .with_session_expiry_interval(0)
            .with_reason_string("Another client connected".to_string());

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        let decoded = DisconnectPacket::decode_body(&mut buf, &fixed_header).unwrap();

        assert_eq!(decoded.reason_code, ReasonCode::SessionTakenOver);
        assert!(decoded
            .properties
            .contains(PropertyId::SessionExpiryInterval));
        assert!(decoded.properties.contains(PropertyId::ReasonString));
    }

    #[test]
    fn test_disconnect_v311_style() {
        // Empty body should decode as normal disconnection
        let mut buf = BytesMut::new();
        let fixed_header = FixedHeader::new(PacketType::Disconnect, 0, 0);

        let decoded = DisconnectPacket::decode_body(&mut buf, &fixed_header).unwrap();
        assert_eq!(decoded.reason_code, NORMAL_DISCONNECTION);
        assert!(decoded.properties.is_empty());
    }
}
