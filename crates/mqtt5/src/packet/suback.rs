use crate::error::{MqttError, Result};
use crate::packet::{FixedHeader, MqttPacket, PacketType};
use crate::protocol::v5::properties::Properties;
use crate::QoS;
use bytes::{Buf, BufMut};

/// MQTT SUBACK packet
#[derive(Debug, Clone)]
pub struct SubAckPacket {
    /// Packet identifier
    pub packet_id: u16,
    /// Reason codes for each subscription
    pub reason_codes: Vec<SubAckReasonCode>,
    /// SUBACK properties (v5.0 only)
    pub properties: Properties,
}

/// SUBACK reason codes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SubAckReasonCode {
    /// Maximum `QoS` 0
    GrantedQoS0 = 0x00,
    /// Maximum `QoS` 1
    GrantedQoS1 = 0x01,
    /// Maximum `QoS` 2
    GrantedQoS2 = 0x02,
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
    /// Quota exceeded
    QuotaExceeded = 0x97,
    /// Shared subscriptions not supported
    SharedSubscriptionsNotSupported = 0x9E,
    /// Subscription identifiers not supported
    SubscriptionIdentifiersNotSupported = 0xA1,
    /// Wildcard subscriptions not supported
    WildcardSubscriptionsNotSupported = 0xA2,
}

impl SubAckReasonCode {
    /// Creates a reason code from a granted `QoS` level
    #[must_use]
    pub fn from_qos(qos: QoS) -> Self {
        match qos {
            QoS::AtMostOnce => Self::GrantedQoS0,
            QoS::AtLeastOnce => Self::GrantedQoS1,
            QoS::ExactlyOnce => Self::GrantedQoS2,
        }
    }

    /// Converts a u8 to a `SubAckReasonCode`
    #[must_use]
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x00 => Some(Self::GrantedQoS0),
            0x01 => Some(Self::GrantedQoS1),
            0x02 => Some(Self::GrantedQoS2),
            0x80 => Some(Self::UnspecifiedError),
            0x83 => Some(Self::ImplementationSpecificError),
            0x87 => Some(Self::NotAuthorized),
            0x8F => Some(Self::TopicFilterInvalid),
            0x91 => Some(Self::PacketIdentifierInUse),
            0x97 => Some(Self::QuotaExceeded),
            0x9E => Some(Self::SharedSubscriptionsNotSupported),
            0xA1 => Some(Self::SubscriptionIdentifiersNotSupported),
            0xA2 => Some(Self::WildcardSubscriptionsNotSupported),
            _ => None,
        }
    }

    /// Returns true if this is a success code
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(
            self,
            Self::GrantedQoS0 | Self::GrantedQoS1 | Self::GrantedQoS2
        )
    }

    /// Returns the granted `QoS` level if this is a success code
    #[must_use]
    pub fn granted_qos(&self) -> Option<QoS> {
        match self {
            Self::GrantedQoS0 => Some(QoS::AtMostOnce),
            Self::GrantedQoS1 => Some(QoS::AtLeastOnce),
            Self::GrantedQoS2 => Some(QoS::ExactlyOnce),
            _ => None,
        }
    }
}

impl SubAckPacket {
    /// Creates a new SUBACK packet
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
    pub fn add_reason_code(mut self, code: SubAckReasonCode) -> Self {
        self.reason_codes.push(code);
        self
    }

    /// Adds a reason code for a granted `QoS`
    #[must_use]
    pub fn add_granted_qos(mut self, qos: QoS) -> Self {
        self.reason_codes.push(SubAckReasonCode::from_qos(qos));
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

impl MqttPacket for SubAckPacket {
    fn packet_type(&self) -> PacketType {
        PacketType::SubAck
    }

    fn encode_body<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        // Variable header
        buf.put_u16(self.packet_id);

        // Properties (v5.0)
        self.properties.encode(buf)?;

        // Payload - reason codes
        if self.reason_codes.is_empty() {
            return Err(MqttError::MalformedPacket(
                "SUBACK packet must contain at least one reason code".to_string(),
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
                "SUBACK missing packet identifier".to_string(),
            ));
        }
        let packet_id = buf.get_u16();

        // Properties (v5.0)
        let properties = Properties::decode(buf)?;

        // Payload - reason codes
        let mut reason_codes = Vec::new();

        if !buf.has_remaining() {
            return Err(MqttError::MalformedPacket(
                "SUBACK packet must contain at least one reason code".to_string(),
            ));
        }

        while buf.has_remaining() {
            let code_byte = buf.get_u8();
            let code = SubAckReasonCode::from_u8(code_byte).ok_or_else(|| {
                MqttError::MalformedPacket(format!("Invalid SUBACK reason code: 0x{code_byte:02X}"))
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
    fn test_suback_reason_code_from_qos() {
        assert_eq!(
            SubAckReasonCode::from_qos(QoS::AtMostOnce),
            SubAckReasonCode::GrantedQoS0
        );
        assert_eq!(
            SubAckReasonCode::from_qos(QoS::AtLeastOnce),
            SubAckReasonCode::GrantedQoS1
        );
        assert_eq!(
            SubAckReasonCode::from_qos(QoS::ExactlyOnce),
            SubAckReasonCode::GrantedQoS2
        );
    }

    #[test]
    fn test_suback_reason_code_is_success() {
        assert!(SubAckReasonCode::GrantedQoS0.is_success());
        assert!(SubAckReasonCode::GrantedQoS1.is_success());
        assert!(SubAckReasonCode::GrantedQoS2.is_success());
        assert!(!SubAckReasonCode::NotAuthorized.is_success());
        assert!(!SubAckReasonCode::TopicFilterInvalid.is_success());
    }

    #[test]
    fn test_suback_basic() {
        let packet = SubAckPacket::new(123)
            .add_granted_qos(QoS::AtLeastOnce)
            .add_granted_qos(QoS::ExactlyOnce)
            .add_reason_code(SubAckReasonCode::NotAuthorized);

        assert_eq!(packet.packet_id, 123);
        assert_eq!(packet.reason_codes.len(), 3);
        assert_eq!(packet.reason_codes[0], SubAckReasonCode::GrantedQoS1);
        assert_eq!(packet.reason_codes[1], SubAckReasonCode::GrantedQoS2);
        assert_eq!(packet.reason_codes[2], SubAckReasonCode::NotAuthorized);
    }

    #[test]
    fn test_suback_encode_decode() {
        let packet = SubAckPacket::new(789)
            .add_granted_qos(QoS::AtMostOnce)
            .add_granted_qos(QoS::AtLeastOnce)
            .add_reason_code(SubAckReasonCode::TopicFilterInvalid)
            .with_reason_string("Invalid wildcard usage".to_string());

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        assert_eq!(fixed_header.packet_type, PacketType::SubAck);

        let decoded = SubAckPacket::decode_body(&mut buf, &fixed_header).unwrap();
        assert_eq!(decoded.packet_id, 789);
        assert_eq!(decoded.reason_codes.len(), 3);
        assert_eq!(decoded.reason_codes[0], SubAckReasonCode::GrantedQoS0);
        assert_eq!(decoded.reason_codes[1], SubAckReasonCode::GrantedQoS1);
        assert_eq!(
            decoded.reason_codes[2],
            SubAckReasonCode::TopicFilterInvalid
        );

        let reason_str = decoded.properties.get(PropertyId::ReasonString);
        assert!(reason_str.is_some());
    }

    #[test]
    fn test_suback_empty_reason_codes() {
        let packet = SubAckPacket::new(123);

        let mut buf = BytesMut::new();
        let result = packet.encode(&mut buf);
        assert!(result.is_err());
    }
}
