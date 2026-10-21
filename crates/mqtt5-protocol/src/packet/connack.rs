use crate::error::{MqttError, Result};
use crate::flags::ConnAckFlags;
use crate::packet::{FixedHeader, MqttPacket, PacketType};
use crate::prelude::{format, String, ToString, Vec};
use crate::protocol::v5::properties::{Properties, PropertyId, PropertyValue};
use crate::types::ReasonCode;
use bytes::{Buf, BufMut};

/// MQTT CONNACK packet
#[derive(Debug, Clone)]
pub struct ConnAckPacket {
    /// Session present flag
    pub session_present: bool,
    /// Connect reason code
    pub reason_code: ReasonCode,
    /// CONNACK properties (v5.0 only)
    pub properties: Properties,
    /// Protocol version (for encoding/decoding)
    pub protocol_version: u8,
}

impl ConnAckPacket {
    /// Creates a new CONNACK packet
    #[must_use]
    pub fn new(session_present: bool, reason_code: ReasonCode) -> Self {
        Self {
            session_present,
            reason_code,
            properties: Properties::default(),
            protocol_version: 5,
        }
    }

    /// Creates a new v3.1.1 CONNACK packet
    #[must_use]
    pub fn new_v311(session_present: bool, reason_code: ReasonCode) -> Self {
        Self {
            session_present,
            reason_code,
            properties: Properties::default(),
            protocol_version: 4,
        }
    }

    /// Sets the session expiry interval
    #[must_use]
    pub fn with_session_expiry_interval(mut self, interval: u32) -> Self {
        self.properties.set_session_expiry_interval(interval);
        self
    }

    /// Sets the receive maximum
    #[must_use]
    pub fn with_receive_maximum(mut self, max: u16) -> Self {
        self.properties.set_receive_maximum(max);
        self
    }

    /// Sets the maximum `QoS`
    #[must_use]
    pub fn with_maximum_qos(mut self, qos: u8) -> Self {
        self.properties.set_maximum_qos(qos);
        self
    }

    /// Sets whether retain is available
    #[must_use]
    pub fn with_retain_available(mut self, available: bool) -> Self {
        self.properties.set_retain_available(available);
        self
    }

    /// Sets the maximum packet size
    #[must_use]
    pub fn with_maximum_packet_size(mut self, size: u32) -> Self {
        self.properties.set_maximum_packet_size(size);
        self
    }

    /// Sets the assigned client identifier
    #[must_use]
    pub fn with_assigned_client_id(mut self, id: String) -> Self {
        self.properties.set_assigned_client_identifier(id);
        self
    }

    /// Sets the topic alias maximum
    #[must_use]
    pub fn with_topic_alias_maximum(mut self, max: u16) -> Self {
        self.properties.set_topic_alias_maximum(max);
        self
    }

    /// Sets the reason string
    #[must_use]
    pub fn with_reason_string(mut self, reason: String) -> Self {
        self.properties.set_reason_string(reason);
        self
    }

    /// Sets whether wildcards are available
    #[must_use]
    pub fn with_wildcard_subscription_available(mut self, available: bool) -> Self {
        self.properties
            .set_wildcard_subscription_available(available);
        self
    }

    /// Sets whether subscription identifiers are available
    #[must_use]
    pub fn with_subscription_identifier_available(mut self, available: bool) -> Self {
        self.properties
            .set_subscription_identifier_available(available);
        self
    }

    /// Sets whether shared subscriptions are available
    #[must_use]
    pub fn with_shared_subscription_available(mut self, available: bool) -> Self {
        self.properties.set_shared_subscription_available(available);
        self
    }

    /// Sets the server keep alive
    #[must_use]
    pub fn with_server_keep_alive(mut self, keep_alive: u16) -> Self {
        self.properties.set_server_keep_alive(keep_alive);
        self
    }

    /// Sets the response information
    #[must_use]
    pub fn with_response_information(mut self, info: String) -> Self {
        self.properties.set_response_information(info);
        self
    }

    /// Sets the server reference
    #[must_use]
    pub fn with_server_reference(mut self, reference: String) -> Self {
        self.properties.set_server_reference(reference);
        self
    }

    /// Sets the authentication method
    #[must_use]
    pub fn with_authentication_method(mut self, method: String) -> Self {
        self.properties.set_authentication_method(method);
        self
    }

    /// Sets the authentication data
    #[must_use]
    pub fn with_authentication_data(mut self, data: Vec<u8>) -> Self {
        self.properties.set_authentication_data(data.into());
        self
    }

    /// Adds a user property
    #[must_use]
    pub fn with_user_property(mut self, key: String, value: String) -> Self {
        self.properties.add_user_property(key, value);
        self
    }

    #[must_use]
    /// Gets the topic alias maximum from properties
    pub fn topic_alias_maximum(&self) -> Option<u16> {
        self.properties
            .get(PropertyId::TopicAliasMaximum)
            .and_then(|prop| {
                if let PropertyValue::TwoByteInteger(max) = prop {
                    Some(*max)
                } else {
                    None
                }
            })
    }

    #[must_use]
    /// Gets the receive maximum from properties
    pub fn receive_maximum(&self) -> Option<u16> {
        self.properties
            .get(PropertyId::ReceiveMaximum)
            .and_then(|prop| {
                if let PropertyValue::TwoByteInteger(max) = prop {
                    Some(*max)
                } else {
                    None
                }
            })
    }

    #[must_use]
    /// Gets the maximum packet size from properties
    pub fn maximum_packet_size(&self) -> Option<u32> {
        self.properties
            .get(PropertyId::MaximumPacketSize)
            .and_then(|prop| {
                if let PropertyValue::FourByteInteger(max) = prop {
                    Some(*max)
                } else {
                    None
                }
            })
    }

    /// Validates the reason code for CONNACK
    fn is_valid_connack_reason_code(code: ReasonCode) -> bool {
        matches!(
            code,
            ReasonCode::Success
                | ReasonCode::UnspecifiedError
                | ReasonCode::MalformedPacket
                | ReasonCode::ProtocolError
                | ReasonCode::ImplementationSpecificError
                | ReasonCode::UnsupportedProtocolVersion
                | ReasonCode::ClientIdentifierNotValid
                | ReasonCode::BadUsernameOrPassword
                | ReasonCode::NotAuthorized
                | ReasonCode::ServerUnavailable
                | ReasonCode::ServerBusy
                | ReasonCode::Banned
                | ReasonCode::BadAuthenticationMethod
                | ReasonCode::TopicNameInvalid
                | ReasonCode::PacketTooLarge
                | ReasonCode::QuotaExceeded
                | ReasonCode::PayloadFormatInvalid
                | ReasonCode::RetainNotSupported
                | ReasonCode::QoSNotSupported
                | ReasonCode::UseAnotherServer
                | ReasonCode::ServerMoved
                | ReasonCode::ConnectionRateExceeded
        )
    }
}

fn v311_return_code(reason_code: ReasonCode) -> u8 {
    match reason_code {
        ReasonCode::Success => 0x00,
        ReasonCode::UnsupportedProtocolVersion => 0x01,
        ReasonCode::ClientIdentifierNotValid => 0x02,
        ReasonCode::UnspecifiedError
        | ReasonCode::ImplementationSpecificError
        | ReasonCode::ServerUnavailable
        | ReasonCode::ServerBusy
        | ReasonCode::ServerShuttingDown
        | ReasonCode::QuotaExceeded
        | ReasonCode::UseAnotherServer
        | ReasonCode::ServerMoved
        | ReasonCode::ConnectionRateExceeded
        | ReasonCode::GrantedQoS1
        | ReasonCode::GrantedQoS2
        | ReasonCode::DisconnectWithWillMessage
        | ReasonCode::NoMatchingSubscribers
        | ReasonCode::NoSubscriptionExisted
        | ReasonCode::ContinueAuthentication
        | ReasonCode::ReAuthenticate
        | ReasonCode::KeepAliveTimeout
        | ReasonCode::SessionTakenOver
        | ReasonCode::TopicFilterInvalid
        | ReasonCode::PacketIdentifierInUse
        | ReasonCode::PacketIdentifierNotFound
        | ReasonCode::ReceiveMaximumExceeded
        | ReasonCode::TopicAliasInvalid
        | ReasonCode::MessageRateTooHigh
        | ReasonCode::AdministrativeAction
        | ReasonCode::SharedSubscriptionsNotSupported
        | ReasonCode::MaximumConnectTime
        | ReasonCode::SubscriptionIdentifiersNotSupported
        | ReasonCode::WildcardSubscriptionsNotSupported
        | ReasonCode::MqoqNoFlowState
        | ReasonCode::MqoqNotFlowOwner
        | ReasonCode::MqoqStreamTypeError
        | ReasonCode::MqoqBadFlowId
        | ReasonCode::MqoqPersistentTopic
        | ReasonCode::MqoqPersistentSubError
        | ReasonCode::MqoqOptionalHeader
        | ReasonCode::MqoqIncompletePacket
        | ReasonCode::MqoqFlowOpenIdle
        | ReasonCode::MqoqFlowCancelled
        | ReasonCode::MqoqFlowPacketCancelled
        | ReasonCode::MqoqFlowRefused
        | ReasonCode::MqoqDiscardState
        | ReasonCode::MqoqServerPushNotWelcome
        | ReasonCode::MqoqRecoveryFailed => 0x03,
        ReasonCode::BadUsernameOrPassword => 0x04,
        ReasonCode::MalformedPacket
        | ReasonCode::ProtocolError
        | ReasonCode::NotAuthorized
        | ReasonCode::Banned
        | ReasonCode::BadAuthenticationMethod
        | ReasonCode::TopicNameInvalid
        | ReasonCode::PacketTooLarge
        | ReasonCode::PayloadFormatInvalid
        | ReasonCode::RetainNotSupported
        | ReasonCode::QoSNotSupported => 0x05,
    }
}

impl MqttPacket for ConnAckPacket {
    fn packet_type(&self) -> PacketType {
        PacketType::ConnAck
    }

    fn encode_body<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        let flags = if self.session_present {
            ConnAckFlags::SessionPresent as u8
        } else {
            0x00
        };
        buf.put_u8(flags);

        if self.protocol_version == 4 {
            buf.put_u8(v311_return_code(self.reason_code));
        } else {
            buf.put_u8(u8::from(self.reason_code));
            self.properties.encode(buf)?;
        }

        Ok(())
    }

    fn decode_body<B: Buf>(buf: &mut B, _fixed_header: &FixedHeader) -> Result<Self> {
        // Validate reserved bits - only bit 0 (session present) is valid
        const RESERVED_BITS_MASK: u8 = 0xFE; // All bits except bit 0

        // Acknowledge flags
        if !buf.has_remaining() {
            return Err(MqttError::MalformedPacket(
                "Missing acknowledge flags".to_string(),
            ));
        }
        let flags = buf.get_u8();

        // Use BeBytes decomposition to parse flags
        let decomposed_flags = ConnAckFlags::decompose(flags);
        let session_present = decomposed_flags.contains(&ConnAckFlags::SessionPresent);

        if (flags & RESERVED_BITS_MASK) != 0 {
            return Err(MqttError::MalformedPacket(
                "Invalid acknowledge flags - reserved bits must be 0".to_string(),
            ));
        }

        // Reason code
        if !buf.has_remaining() {
            return Err(MqttError::MalformedPacket(
                "Missing reason code".to_string(),
            ));
        }
        let reason_byte = buf.get_u8();

        // For v3.1.1, we need to determine protocol version from the reason code
        let (reason_code, protocol_version) = if reason_byte <= 5 && !buf.has_remaining() {
            // Likely v3.1.1 - map return codes to reason codes
            let code = match reason_byte {
                0x00 => ReasonCode::Success,
                0x01 => ReasonCode::UnsupportedProtocolVersion,
                0x02 => ReasonCode::ClientIdentifierNotValid,
                0x03 => ReasonCode::ServerUnavailable,
                0x04 => ReasonCode::BadUsernameOrPassword,
                0x05 => ReasonCode::NotAuthorized,
                _ => unreachable!(),
            };
            (code, 4)
        } else {
            // v5.0 - decode reason code
            let code = ReasonCode::from_u8(reason_byte).ok_or_else(|| {
                MqttError::MalformedPacket(format!("Invalid reason code: {reason_byte}"))
            })?;

            if !Self::is_valid_connack_reason_code(code) {
                return Err(MqttError::MalformedPacket(format!(
                    "Invalid CONNACK reason code: {code:?}"
                )));
            }

            (code, 5)
        };

        // Properties (v5.0 only)
        let properties = if protocol_version == 5 && buf.has_remaining() {
            Properties::decode(buf)?
        } else {
            Properties::default()
        };

        Ok(Self {
            session_present,
            reason_code,
            properties,
            protocol_version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[test]
    fn test_connack_basic() {
        let packet = ConnAckPacket::new(true, ReasonCode::Success);

        assert!(packet.session_present);
        assert_eq!(packet.reason_code, ReasonCode::Success);
    }

    #[test]
    fn test_connack_with_properties() {
        let packet = ConnAckPacket::new(false, ReasonCode::Success)
            .with_session_expiry_interval(3600)
            .with_receive_maximum(100)
            .with_maximum_qos(1)
            .with_retain_available(true)
            .with_assigned_client_id("auto-123".to_string())
            .with_user_property("foo".to_string(), "bar".to_string());

        assert!(!packet.session_present);
        assert!(packet
            .properties
            .get(PropertyId::SessionExpiryInterval)
            .is_some());
        assert!(packet.properties.get(PropertyId::ReceiveMaximum).is_some());
        assert!(packet.properties.get(PropertyId::MaximumQoS).is_some());
        assert!(packet.properties.get(PropertyId::RetainAvailable).is_some());
        assert!(packet
            .properties
            .get(PropertyId::AssignedClientIdentifier)
            .is_some());
        assert!(packet.properties.get(PropertyId::UserProperty).is_some());
    }

    #[test]
    fn test_connack_encode_decode_v5() {
        let packet = ConnAckPacket::new(true, ReasonCode::Success)
            .with_session_expiry_interval(7200)
            .with_receive_maximum(50);

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        assert_eq!(fixed_header.packet_type, PacketType::ConnAck);

        let decoded = ConnAckPacket::decode_body(&mut buf, &fixed_header).unwrap();
        assert!(decoded.session_present);
        assert_eq!(decoded.reason_code, ReasonCode::Success);
        assert_eq!(decoded.protocol_version, 5);

        let session_expiry = decoded.properties.get(PropertyId::SessionExpiryInterval);
        assert!(session_expiry.is_some());
        if let Some(PropertyValue::FourByteInteger(val)) = session_expiry {
            assert_eq!(*val, 7200);
        } else {
            panic!("Wrong property type");
        }
    }

    #[test]
    fn test_connack_encode_decode_v311() {
        let packet = ConnAckPacket::new_v311(false, ReasonCode::BadUsernameOrPassword);

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        let decoded = ConnAckPacket::decode_body(&mut buf, &fixed_header).unwrap();

        assert!(!decoded.session_present);
        assert_eq!(decoded.reason_code, ReasonCode::BadUsernameOrPassword);
        assert_eq!(decoded.protocol_version, 4);
    }

    #[test]
    fn test_connack_v311_return_codes() {
        let expected = [
            (ReasonCode::Success, 0x00),
            (ReasonCode::UnsupportedProtocolVersion, 0x01),
            (ReasonCode::ClientIdentifierNotValid, 0x02),
            (ReasonCode::ServerUnavailable, 0x03),
            (ReasonCode::ServerBusy, 0x03),
            (ReasonCode::UseAnotherServer, 0x03),
            (ReasonCode::ServerMoved, 0x03),
            (ReasonCode::ConnectionRateExceeded, 0x03),
            (ReasonCode::ServerShuttingDown, 0x03),
            (ReasonCode::QuotaExceeded, 0x03),
            (ReasonCode::UnspecifiedError, 0x03),
            (ReasonCode::ImplementationSpecificError, 0x03),
            (ReasonCode::BadUsernameOrPassword, 0x04),
            (ReasonCode::NotAuthorized, 0x05),
            (ReasonCode::Banned, 0x05),
            (ReasonCode::BadAuthenticationMethod, 0x05),
            (ReasonCode::MalformedPacket, 0x05),
            (ReasonCode::ProtocolError, 0x05),
            (ReasonCode::PacketTooLarge, 0x05),
            (ReasonCode::QoSNotSupported, 0x05),
            (ReasonCode::RetainNotSupported, 0x05),
        ];
        for (code, return_code) in expected {
            let mut buf = BytesMut::new();
            ConnAckPacket::new_v311(false, code)
                .encode(&mut buf)
                .unwrap();
            assert_eq!(&buf[..], &[0x20, 0x02, 0x00, return_code], "{code:?}");
        }
    }

    #[test]
    fn test_connack_error_codes() {
        let error_codes = vec![
            ReasonCode::UnspecifiedError,
            ReasonCode::MalformedPacket,
            ReasonCode::ProtocolError,
            ReasonCode::UnsupportedProtocolVersion,
            ReasonCode::ClientIdentifierNotValid,
            ReasonCode::BadUsernameOrPassword,
            ReasonCode::NotAuthorized,
            ReasonCode::ServerUnavailable,
            ReasonCode::ServerBusy,
            ReasonCode::Banned,
        ];

        for code in error_codes {
            let packet = ConnAckPacket::new(false, code);
            let mut buf = BytesMut::new();
            packet.encode(&mut buf).unwrap();

            let fixed_header = FixedHeader::decode(&mut buf).unwrap();
            let decoded = ConnAckPacket::decode_body(&mut buf, &fixed_header).unwrap();
            assert_eq!(decoded.reason_code, code);
        }
    }

    #[test]
    fn test_connack_invalid_flags() {
        let mut buf = BytesMut::new();
        buf.put_u8(0xFF); // Invalid flags - reserved bits set
        buf.put_u8(0x00); // Success code

        let fixed_header = FixedHeader::new(PacketType::ConnAck, 0, 0);
        let result = ConnAckPacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }

    #[test]
    fn test_connack_valid_reason_codes() {
        assert!(ConnAckPacket::is_valid_connack_reason_code(
            ReasonCode::Success
        ));
        assert!(ConnAckPacket::is_valid_connack_reason_code(
            ReasonCode::NotAuthorized
        ));
        assert!(ConnAckPacket::is_valid_connack_reason_code(
            ReasonCode::ServerBusy
        ));

        // Invalid CONNACK reason codes
        assert!(!ConnAckPacket::is_valid_connack_reason_code(
            ReasonCode::NoSubscriptionExisted
        ));
        assert!(!ConnAckPacket::is_valid_connack_reason_code(
            ReasonCode::SubscriptionIdentifiersNotSupported
        ));
    }
}
