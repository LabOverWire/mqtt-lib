use crate::error::{MqttError, Result};
use crate::packet::{FixedHeader, MqttPacket, PacketType};
use crate::protocol::v5::properties::Properties;
use crate::protocol::v5::reason_codes::ReasonCode;
use bytes::{Buf, BufMut};

/// AUTH packet for MQTT v5.0 enhanced authentication
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthPacket {
    /// Reason code indicating the result of the authentication
    pub reason_code: ReasonCode,
    /// Properties associated with the authentication
    pub properties: Properties,
}

impl AuthPacket {
    /// Creates a new AUTH packet
    #[must_use]
    pub fn new(reason_code: ReasonCode) -> Self {
        Self {
            reason_code,
            properties: Properties::default(),
        }
    }

    #[must_use]
    /// Creates a new AUTH packet with properties
    pub fn with_properties(reason_code: ReasonCode, properties: Properties) -> Self {
        Self {
            reason_code,
            properties,
        }
    }

    /// Creates an AUTH packet for continuing authentication
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub fn continue_authentication(
        auth_method: String,
        auth_data: Option<Vec<u8>>,
    ) -> Result<Self> {
        let mut properties = Properties::default();

        // Authentication method is required
        properties.add(
            crate::protocol::v5::properties::PropertyId::AuthenticationMethod,
            crate::protocol::v5::properties::PropertyValue::Utf8String(auth_method),
        )?;

        // Authentication data is optional
        if let Some(data) = auth_data {
            properties.add(
                crate::protocol::v5::properties::PropertyId::AuthenticationData,
                crate::protocol::v5::properties::PropertyValue::BinaryData(data.into()),
            )?;
        }

        Ok(Self::with_properties(
            ReasonCode::ContinueAuthentication,
            properties,
        ))
    }

    /// Creates an AUTH packet for re-authentication
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub fn re_authenticate(auth_method: String, auth_data: Option<Vec<u8>>) -> Result<Self> {
        let mut properties = Properties::default();

        // Authentication method is required
        properties.add(
            crate::protocol::v5::properties::PropertyId::AuthenticationMethod,
            crate::protocol::v5::properties::PropertyValue::Utf8String(auth_method),
        )?;

        // Authentication data is optional
        if let Some(data) = auth_data {
            properties.add(
                crate::protocol::v5::properties::PropertyId::AuthenticationData,
                crate::protocol::v5::properties::PropertyValue::BinaryData(data.into()),
            )?;
        }

        Ok(Self::with_properties(
            ReasonCode::ReAuthenticate,
            properties,
        ))
    }

    /// Creates a successful authentication response
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub fn success(auth_method: String) -> Result<Self> {
        let mut properties = Properties::default();

        properties.add(
            crate::protocol::v5::properties::PropertyId::AuthenticationMethod,
            crate::protocol::v5::properties::PropertyValue::Utf8String(auth_method),
        )?;

        Ok(Self::with_properties(ReasonCode::Success, properties))
    }

    /// Creates an authentication failure response
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub fn failure(reason_code: ReasonCode, reason_string: Option<String>) -> Result<Self> {
        if reason_code.is_success() {
            return Err(MqttError::ProtocolError(
                "Cannot create failure AUTH packet with success reason code".to_string(),
            ));
        }

        let mut properties = Properties::default();

        if let Some(reason) = reason_string {
            properties.add(
                crate::protocol::v5::properties::PropertyId::ReasonString,
                crate::protocol::v5::properties::PropertyValue::Utf8String(reason),
            )?;
        }

        Ok(Self::with_properties(reason_code, properties))
    }

    #[must_use]
    /// Gets the authentication method from properties
    pub fn authentication_method(&self) -> Option<&str> {
        if let Some(crate::protocol::v5::properties::PropertyValue::Utf8String(method)) = self
            .properties
            .get(crate::protocol::v5::properties::PropertyId::AuthenticationMethod)
        {
            Some(method)
        } else {
            None
        }
    }

    #[must_use]
    /// Gets the authentication data from properties
    pub fn authentication_data(&self) -> Option<&[u8]> {
        if let Some(crate::protocol::v5::properties::PropertyValue::BinaryData(data)) = self
            .properties
            .get(crate::protocol::v5::properties::PropertyId::AuthenticationData)
        {
            Some(data)
        } else {
            None
        }
    }

    #[must_use]
    /// Gets the reason string from properties
    pub fn reason_string(&self) -> Option<&str> {
        if let Some(crate::protocol::v5::properties::PropertyValue::Utf8String(reason)) = self
            .properties
            .get(crate::protocol::v5::properties::PropertyId::ReasonString)
        {
            Some(reason)
        } else {
            None
        }
    }

    /// Validates the AUTH packet
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub fn validate(&self) -> Result<()> {
        // For AUTH packets, authentication method is required for Continue and ReAuth
        if self.authentication_method().is_none()
            && (self.reason_code == ReasonCode::ContinueAuthentication
                || self.reason_code == ReasonCode::ReAuthenticate)
        {
            return Err(MqttError::ProtocolError(
                "Authentication method is required for AUTH packets with ContinueAuthentication or ReAuthenticate reason codes".to_string()
            ));
        }

        Ok(())
    }
}

impl MqttPacket for AuthPacket {
    fn packet_type(&self) -> PacketType {
        PacketType::Auth
    }

    fn encode_body<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        self.validate()?;

        // For AUTH packets, if reason code is Success and properties are empty,
        // we can encode as an empty packet (remaining length = 0)
        if self.reason_code == ReasonCode::Success && self.properties.is_empty() {
            // Empty packet, nothing to encode
            return Ok(());
        }

        // Encode reason code
        buf.put_u8(u8::from(self.reason_code));

        // Encode properties
        self.properties.encode(buf)?;

        Ok(())
    }

    fn decode_body<B: Buf>(buf: &mut B, fixed_header: &FixedHeader) -> Result<Self> {
        if fixed_header.remaining_length == 0 {
            // Empty AUTH packet defaults to Success with no properties
            return Ok(Self::new(ReasonCode::Success));
        }

        // Decode reason code
        if !buf.has_remaining() {
            return Err(MqttError::MalformedPacket(
                "Missing reason code in AUTH packet".to_string(),
            ));
        }

        let reason_code_val = buf.get_u8();
        let reason_code = ReasonCode::from_u8(reason_code_val)
            .ok_or(MqttError::InvalidReasonCode(reason_code_val))?;

        // Decode properties (if remaining length > 1)
        let properties = if buf.has_remaining() {
            Properties::decode(buf)?
        } else {
            Properties::default()
        };

        let packet = Self {
            reason_code,
            properties,
        };

        packet.validate()?;
        Ok(packet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::v5::properties::{PropertyId, PropertyValue};
    use bytes::BytesMut;

    #[test]
    fn test_auth_packet_new() {
        let packet = AuthPacket::new(ReasonCode::Success);
        assert_eq!(packet.reason_code, ReasonCode::Success);
        assert!(packet.properties.is_empty());
    }

    #[test]
    fn test_auth_packet_continue_authentication() {
        let packet = AuthPacket::continue_authentication(
            "SCRAM-SHA-256".to_string(),
            Some(b"challenge_data".to_vec()),
        )
        .unwrap();

        assert_eq!(packet.reason_code, ReasonCode::ContinueAuthentication);
        assert_eq!(packet.authentication_method(), Some("SCRAM-SHA-256"));
        assert_eq!(
            packet.authentication_data(),
            Some(b"challenge_data".as_ref())
        );
    }

    #[test]
    fn test_auth_packet_re_authenticate() {
        let packet = AuthPacket::re_authenticate("OAUTH2".to_string(), None).unwrap();

        assert_eq!(packet.reason_code, ReasonCode::ReAuthenticate);
        assert_eq!(packet.authentication_method(), Some("OAUTH2"));
        assert!(packet.authentication_data().is_none());
    }

    #[test]
    fn test_auth_packet_success() {
        let packet = AuthPacket::success("PLAIN".to_string()).unwrap();

        assert_eq!(packet.reason_code, ReasonCode::Success);
        assert_eq!(packet.authentication_method(), Some("PLAIN"));
    }

    #[test]
    fn test_auth_packet_failure() {
        let packet = AuthPacket::failure(
            ReasonCode::BadAuthenticationMethod,
            Some("Unsupported method".to_string()),
        )
        .unwrap();

        assert_eq!(packet.reason_code, ReasonCode::BadAuthenticationMethod);
        assert_eq!(packet.reason_string(), Some("Unsupported method"));
    }

    #[test]
    fn test_auth_packet_failure_with_success_code_fails() {
        let result = AuthPacket::failure(ReasonCode::Success, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_auth_packet_encode_decode_empty() {
        let packet = AuthPacket::new(ReasonCode::Success);
        let mut buf = BytesMut::new();

        packet.encode(&mut buf).unwrap();

        // Decode the complete packet (including fixed header)
        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        let decoded = AuthPacket::decode_body(&mut buf, &fixed_header).unwrap();

        assert_eq!(decoded.reason_code, ReasonCode::Success);
        assert!(decoded.properties.is_empty());
    }

    #[test]
    fn test_auth_packet_encode_decode_with_properties() {
        let packet = AuthPacket::continue_authentication(
            "SCRAM-SHA-1".to_string(),
            Some(b"server_nonce".to_vec()),
        )
        .unwrap();

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        let decoded = AuthPacket::decode_body(&mut buf, &fixed_header).unwrap();

        assert_eq!(decoded.reason_code, ReasonCode::ContinueAuthentication);
        assert_eq!(decoded.authentication_method(), Some("SCRAM-SHA-1"));
        assert_eq!(
            decoded.authentication_data(),
            Some(b"server_nonce".as_ref())
        );
    }

    #[test]
    fn test_auth_packet_encode_decode_failure() {
        let packet = AuthPacket::failure(
            ReasonCode::NotAuthorized,
            Some("Invalid credentials".to_string()),
        )
        .unwrap();

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        let decoded = AuthPacket::decode_body(&mut buf, &fixed_header).unwrap();

        assert_eq!(decoded.reason_code, ReasonCode::NotAuthorized);
        assert_eq!(decoded.reason_string(), Some("Invalid credentials"));
    }

    #[test]
    fn test_auth_packet_validation_missing_auth_method() {
        let packet = AuthPacket::new(ReasonCode::ContinueAuthentication);
        // Packet without authentication method should be invalid
        let result = packet.validate();
        assert!(result.is_err());
    }

    #[test]
    fn test_auth_packet_decode_malformed() {
        let mut buf = BytesMut::new();
        // Empty buffer should fail for packets with remaining length > 0

        let result = AuthPacket::decode_body(&mut buf, &FixedHeader::new(PacketType::Auth, 0, 1));
        assert!(result.is_err());
    }

    #[test]
    fn test_auth_packet_decode_invalid_reason_code() {
        let mut buf = BytesMut::new();
        buf.put_u8(0xFF); // Invalid reason code
        buf.put_u8(0x00); // Empty properties

        let result = AuthPacket::decode_body(&mut buf, &FixedHeader::new(PacketType::Auth, 0, 2));
        assert!(result.is_err());
    }

    #[test]
    fn test_auth_packet_property_getters() {
        let mut properties = Properties::default();
        properties
            .add(
                PropertyId::AuthenticationMethod,
                PropertyValue::Utf8String("TEST".to_string()),
            )
            .unwrap();
        properties
            .add(
                PropertyId::AuthenticationData,
                PropertyValue::BinaryData(b"data".to_vec().into()),
            )
            .unwrap();
        properties
            .add(
                PropertyId::ReasonString,
                PropertyValue::Utf8String("reason".to_string()),
            )
            .unwrap();

        let packet = AuthPacket::with_properties(ReasonCode::Success, properties);

        assert_eq!(packet.authentication_method(), Some("TEST"));
        assert_eq!(packet.authentication_data(), Some(b"data".as_ref()));
        assert_eq!(packet.reason_string(), Some("reason"));
    }
}
