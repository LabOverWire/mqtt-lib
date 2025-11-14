use crate::encoding::{decode_binary, decode_string, encode_binary, encode_string};
use crate::error::{MqttError, Result};
use crate::flags::ConnectFlags;
use crate::packet::{FixedHeader, MqttPacket, PacketType};
use crate::protocol::v5::properties::{Properties, PropertyId, PropertyValue};
use crate::types::{ConnectOptions, WillMessage, WillProperties};
use crate::QoS;
use bytes::{Buf, BufMut, Bytes};

const PROTOCOL_NAME: &str = "MQTT";
const PROTOCOL_VERSION_V5: u8 = 5;
const PROTOCOL_VERSION_V311: u8 = 4;

/// MQTT CONNECT packet
#[derive(Debug, Clone)]
pub struct ConnectPacket {
    /// Protocol version (4 for v3.1.1, 5 for v5.0)
    pub protocol_version: u8,
    /// Clean start flag (Clean Session in v3.1.1)
    pub clean_start: bool,
    /// Keep alive interval in seconds
    pub keep_alive: u16,
    /// Client identifier
    pub client_id: String,
    /// Username (optional)
    pub username: Option<String>,
    /// Password (optional)
    pub password: Option<Vec<u8>>,
    /// Will message (optional)
    pub will: Option<WillMessage>,
    /// CONNECT properties (v5.0 only)
    pub properties: Properties,
    /// Will properties (v5.0 only)
    pub will_properties: Properties,
}

impl ConnectPacket {
    /// Creates a new CONNECT packet from options
    #[must_use]
    pub fn new(options: ConnectOptions) -> Self {
        let properties = Self::build_connect_properties(&options.properties);
        let will_properties = options
            .will
            .as_ref()
            .map_or_else(Properties::default, |will| {
                Self::build_will_properties(&will.properties)
            });

        Self {
            protocol_version: PROTOCOL_VERSION_V5,
            clean_start: options.clean_start,
            keep_alive: Self::calculate_keep_alive(options.keep_alive),
            client_id: options.client_id,
            username: options.username,
            password: options.password,
            will: options.will,
            properties,
            will_properties,
        }
    }

    /// Builds CONNECT properties from options
    fn build_connect_properties(props: &crate::types::ConnectProperties) -> Properties {
        let mut properties = Properties::default();

        if let Some(val) = props.session_expiry_interval {
            let _ = properties.add(
                PropertyId::SessionExpiryInterval,
                PropertyValue::FourByteInteger(val),
            );
        }
        if let Some(val) = props.receive_maximum {
            let _ = properties.add(
                PropertyId::ReceiveMaximum,
                PropertyValue::TwoByteInteger(val),
            );
        }
        if let Some(val) = props.maximum_packet_size {
            let _ = properties.add(
                PropertyId::MaximumPacketSize,
                PropertyValue::FourByteInteger(val),
            );
        }
        if let Some(val) = props.topic_alias_maximum {
            let _ = properties.add(
                PropertyId::TopicAliasMaximum,
                PropertyValue::TwoByteInteger(val),
            );
        }
        if let Some(val) = props.request_response_information {
            let _ = properties.add(
                PropertyId::RequestResponseInformation,
                PropertyValue::Byte(u8::from(val)),
            );
        }
        if let Some(val) = props.request_problem_information {
            let _ = properties.add(
                PropertyId::RequestProblemInformation,
                PropertyValue::Byte(u8::from(val)),
            );
        }
        if let Some(val) = &props.authentication_method {
            let _ = properties.add(
                PropertyId::AuthenticationMethod,
                PropertyValue::Utf8String(val.clone()),
            );
        }
        if let Some(val) = &props.authentication_data {
            let _ = properties.add(
                PropertyId::AuthenticationData,
                PropertyValue::BinaryData(val.clone().into()),
            );
        }
        for (key, value) in &props.user_properties {
            let _ = properties.add(
                PropertyId::UserProperty,
                PropertyValue::Utf8StringPair(key.clone(), value.clone()),
            );
        }

        properties
    }

    /// Builds will properties from will options
    fn build_will_properties(will_props: &crate::types::WillProperties) -> Properties {
        let mut properties = Properties::default();

        if let Some(val) = will_props.will_delay_interval {
            let _ = properties.add(
                PropertyId::WillDelayInterval,
                PropertyValue::FourByteInteger(val),
            );
        }
        if let Some(val) = will_props.payload_format_indicator {
            let _ = properties.add(
                PropertyId::PayloadFormatIndicator,
                PropertyValue::Byte(u8::from(val)),
            );
        }
        if let Some(val) = will_props.message_expiry_interval {
            let _ = properties.add(
                PropertyId::MessageExpiryInterval,
                PropertyValue::FourByteInteger(val),
            );
        }
        if let Some(val) = &will_props.content_type {
            let _ = properties.add(
                PropertyId::ContentType,
                PropertyValue::Utf8String(val.clone()),
            );
        }
        if let Some(val) = &will_props.response_topic {
            let _ = properties.add(
                PropertyId::ResponseTopic,
                PropertyValue::Utf8String(val.clone()),
            );
        }
        if let Some(val) = &will_props.correlation_data {
            let _ = properties.add(
                PropertyId::CorrelationData,
                PropertyValue::BinaryData(val.clone().into()),
            );
        }
        for (key, value) in &will_props.user_properties {
            let _ = properties.add(
                PropertyId::UserProperty,
                PropertyValue::Utf8StringPair(key.clone(), value.clone()),
            );
        }

        properties
    }

    /// Calculates keep alive value, clamping to u16 range
    fn calculate_keep_alive(keep_alive: std::time::Duration) -> u16 {
        keep_alive
            .as_secs()
            .min(u64::from(u16::MAX))
            .try_into()
            .unwrap_or(u16::MAX)
    }

    /// Creates a v3.1.1 compatible CONNECT packet
    #[must_use]
    pub fn new_v311(options: ConnectOptions) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION_V311,
            clean_start: options.clean_start,
            keep_alive: Self::calculate_keep_alive(options.keep_alive),
            client_id: options.client_id,
            username: options.username,
            password: options.password,
            will: options.will,
            properties: Properties::default(),
            will_properties: Properties::default(),
        }
    }

    /// Creates connect flags byte
    fn connect_flags(&self) -> u8 {
        let mut flags = 0u8;

        if self.clean_start {
            flags |= ConnectFlags::CleanStart as u8;
        }

        if let Some(ref will) = self.will {
            flags |= ConnectFlags::WillFlag as u8;
            flags = ConnectFlags::with_will_qos(flags, will.qos as u8);
            if will.retain {
                flags |= ConnectFlags::WillRetain as u8;
            }
        }

        if self.username.is_some() {
            flags |= ConnectFlags::UsernameFlag as u8;
        }

        if self.password.is_some() {
            flags |= ConnectFlags::PasswordFlag as u8;
        }

        flags
    }
}

impl MqttPacket for ConnectPacket {
    fn packet_type(&self) -> PacketType {
        PacketType::Connect
    }

    fn encode_body<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        // Variable header
        encode_string(buf, PROTOCOL_NAME)?;
        buf.put_u8(self.protocol_version);
        buf.put_u8(self.connect_flags());
        buf.put_u16(self.keep_alive);

        // Properties (v5.0 only)
        if self.protocol_version == PROTOCOL_VERSION_V5 {
            self.properties.encode(buf)?;
        }

        // Payload
        encode_string(buf, &self.client_id)?;

        // Will
        if let Some(ref will) = self.will {
            if self.protocol_version == PROTOCOL_VERSION_V5 {
                self.will_properties.encode(buf)?;
            }
            encode_string(buf, &will.topic)?;
            encode_binary(buf, &will.payload)?;
        }

        // Username
        if let Some(ref username) = self.username {
            encode_string(buf, username)?;
        }

        // Password
        if let Some(ref password) = self.password {
            encode_binary(buf, password)?;
        }

        Ok(())
    }

    fn decode_body<B: Buf>(buf: &mut B, _fixed_header: &FixedHeader) -> Result<Self> {
        // Decode variable header
        let protocol_version = Self::decode_protocol_header(buf)?;
        let (flags, keep_alive) = Self::decode_connect_flags_and_keepalive(buf)?;

        // Properties (v5.0 only)
        let properties = if protocol_version == PROTOCOL_VERSION_V5 {
            Properties::decode(buf)?
        } else {
            Properties::default()
        };

        // Decode payload
        let client_id = decode_string(buf)?;
        let (will, will_properties) = Self::decode_will(buf, &flags, protocol_version)?;
        let (username, password) = Self::decode_credentials(buf, &flags)?;

        Ok(Self {
            protocol_version,
            clean_start: flags.clean_start,
            keep_alive,
            client_id,
            username,
            password: password.map(|p| p.to_vec()),
            will,
            properties,
            will_properties,
        })
    }
}

/// Helper struct to hold decoded connect flags
struct DecodedConnectFlags {
    clean_start: bool,
    will_flag: bool,
    will_qos: u8,
    will_retain: bool,
    credentials: CredentialFlags,
}

struct CredentialFlags {
    username_flag: bool,
    password_flag: bool,
}

impl ConnectPacket {
    /// Decode and validate protocol header
    fn decode_protocol_header<B: Buf>(buf: &mut B) -> Result<u8> {
        // Protocol name
        let protocol_name = decode_string(buf)?;
        if protocol_name != PROTOCOL_NAME {
            return Err(MqttError::ProtocolError(format!(
                "Invalid protocol name: {protocol_name}"
            )));
        }

        // Protocol version
        if !buf.has_remaining() {
            return Err(MqttError::MalformedPacket(
                "Missing protocol version".to_string(),
            ));
        }
        let protocol_version = buf.get_u8();

        Ok(protocol_version)
    }

    /// Decode connect flags and keep alive
    fn decode_connect_flags_and_keepalive<B: Buf>(
        buf: &mut B,
    ) -> Result<(DecodedConnectFlags, u16)> {
        // Connect flags
        if !buf.has_remaining() {
            return Err(MqttError::MalformedPacket(
                "Missing connect flags".to_string(),
            ));
        }
        let flags = buf.get_u8();

        // Parse flags using BeBytes decomposition
        let decomposed_flags = ConnectFlags::decompose(flags);

        // Validate reserved bit
        if decomposed_flags.contains(&ConnectFlags::Reserved) {
            return Err(MqttError::MalformedPacket(
                "Reserved flag bit must be 0".to_string(),
            ));
        }

        let credentials = CredentialFlags {
            username_flag: decomposed_flags.contains(&ConnectFlags::UsernameFlag),
            password_flag: decomposed_flags.contains(&ConnectFlags::PasswordFlag),
        };

        let decoded_flags = DecodedConnectFlags {
            clean_start: decomposed_flags.contains(&ConnectFlags::CleanStart),
            will_flag: decomposed_flags.contains(&ConnectFlags::WillFlag),
            will_qos: ConnectFlags::extract_will_qos(flags),
            will_retain: decomposed_flags.contains(&ConnectFlags::WillRetain),
            credentials,
        };

        // Keep alive
        if buf.remaining() < 2 {
            return Err(MqttError::MalformedPacket("Missing keep alive".to_string()));
        }
        let keep_alive = buf.get_u16();

        Ok((decoded_flags, keep_alive))
    }

    /// Decode will message if present
    fn decode_will<B: Buf>(
        buf: &mut B,
        flags: &DecodedConnectFlags,
        protocol_version: u8,
    ) -> Result<(Option<WillMessage>, Properties)> {
        if !flags.will_flag {
            return Ok((None, Properties::default()));
        }

        let will_properties = if protocol_version == PROTOCOL_VERSION_V5 {
            Properties::decode(buf)?
        } else {
            Properties::default()
        };

        let topic = decode_string(buf)?;
        let payload = decode_binary(buf)?;

        let qos = match flags.will_qos {
            0 => QoS::AtMostOnce,
            1 => QoS::AtLeastOnce,
            2 => QoS::ExactlyOnce,
            _ => return Err(MqttError::MalformedPacket("Invalid will QoS".to_string())),
        };

        // Convert Properties to WillProperties
        let will_props = Self::properties_to_will_properties(&will_properties);

        let will = WillMessage {
            topic,
            payload: payload.to_vec(),
            qos,
            retain: flags.will_retain,
            properties: will_props,
        };

        Ok((Some(will), will_properties))
    }

    /// Convert Properties to WillProperties
    fn properties_to_will_properties(props: &Properties) -> WillProperties {
        use crate::protocol::v5::properties::{PropertyId, PropertyValue};

        let mut will_props = WillProperties::default();

        // Extract will delay interval
        if let Some(PropertyValue::FourByteInteger(delay)) =
            props.get(PropertyId::WillDelayInterval)
        {
            will_props.will_delay_interval = Some(*delay);
        }

        // Extract payload format indicator
        if let Some(PropertyValue::Byte(indicator)) = props.get(PropertyId::PayloadFormatIndicator)
        {
            will_props.payload_format_indicator = Some(*indicator != 0);
        }

        // Extract message expiry interval
        if let Some(PropertyValue::FourByteInteger(expiry)) =
            props.get(PropertyId::MessageExpiryInterval)
        {
            will_props.message_expiry_interval = Some(*expiry);
        }

        // Extract content type
        if let Some(PropertyValue::Utf8String(content_type)) = props.get(PropertyId::ContentType) {
            will_props.content_type = Some(content_type.clone());
        }

        // Extract response topic
        if let Some(PropertyValue::Utf8String(topic)) = props.get(PropertyId::ResponseTopic) {
            will_props.response_topic = Some(topic.clone());
        }

        // Extract correlation data
        if let Some(PropertyValue::BinaryData(data)) = props.get(PropertyId::CorrelationData) {
            will_props.correlation_data = Some(data.to_vec());
        }

        // Extract user properties
        if let Some(values) = props.get_all(PropertyId::UserProperty) {
            for value in values {
                if let PropertyValue::Utf8StringPair(key, val) = value {
                    will_props.user_properties.push((key.clone(), val.clone()));
                }
            }
        }

        will_props
    }

    /// Decode username and password if present
    fn decode_credentials<B: Buf>(
        buf: &mut B,
        flags: &DecodedConnectFlags,
    ) -> Result<(Option<String>, Option<Bytes>)> {
        let username = if flags.credentials.username_flag {
            Some(decode_string(buf)?)
        } else {
            None
        };

        let password = if flags.credentials.password_flag {
            Some(decode_binary(buf)?)
        } else {
            None
        };

        // Validate password without username
        if password.is_some() && username.is_none() {
            return Err(MqttError::MalformedPacket(
                "Password without username is not allowed".to_string(),
            ));
        }

        Ok((username, password))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;
    use crate::time::Duration;

    #[test]
    fn test_connect_packet_basic() {
        let options = ConnectOptions::new("test-client");
        let packet = ConnectPacket::new(options);

        assert_eq!(packet.protocol_version, PROTOCOL_VERSION_V5);
        assert!(packet.clean_start);
        assert_eq!(packet.keep_alive, 60);
        assert_eq!(packet.client_id, "test-client");
        assert!(packet.username.is_none());
        assert!(packet.password.is_none());
        assert!(packet.will.is_none());
    }

    #[test]
    fn test_connect_packet_with_credentials() {
        let options = ConnectOptions::new("test-client").with_credentials("user", b"pass");
        let packet = ConnectPacket::new(options);

        assert_eq!(packet.username, Some("user".to_string()));
        assert_eq!(packet.password, Some(b"pass".to_vec()));
    }

    #[test]
    fn test_connect_packet_with_will() {
        let will = WillMessage::new("will/topic", b"will payload")
            .with_qos(QoS::AtLeastOnce)
            .with_retain(true);
        let options = ConnectOptions::new("test-client").with_will(will);
        let packet = ConnectPacket::new(options);

        assert!(packet.will.is_some());
        let will = packet.will.as_ref().unwrap();
        assert_eq!(will.topic, "will/topic");
        assert_eq!(will.payload, b"will payload");
        assert_eq!(will.qos, QoS::AtLeastOnce);
        assert!(will.retain);
    }

    #[test]
    fn test_connect_flags() {
        let packet = ConnectPacket::new(ConnectOptions::new("test"));
        assert_eq!(packet.connect_flags(), 0x02); // Clean start only

        let options = ConnectOptions::new("test")
            .with_clean_start(false)
            .with_credentials("user", b"pass");
        let packet = ConnectPacket::new(options);
        assert_eq!(packet.connect_flags(), 0xC0); // Username + Password

        let will = WillMessage::new("topic", b"payload")
            .with_qos(QoS::ExactlyOnce)
            .with_retain(true);
        let options = ConnectOptions::new("test").with_will(will);
        let packet = ConnectPacket::new(options);
        assert_eq!(packet.connect_flags(), 0x36); // Clean start + Will + QoS 2 + Retain
    }

    #[test]
    fn test_connect_encode_decode_v5() {
        let options = ConnectOptions::new("test-client-123")
            .with_keep_alive(Duration::from_secs(120))
            .with_credentials("testuser", b"testpass");
        let packet = ConnectPacket::new(options);

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        assert_eq!(fixed_header.packet_type, PacketType::Connect);

        let decoded = ConnectPacket::decode_body(&mut buf, &fixed_header).unwrap();
        assert_eq!(decoded.protocol_version, PROTOCOL_VERSION_V5);
        assert_eq!(decoded.client_id, "test-client-123");
        assert_eq!(decoded.keep_alive, 120);
        assert_eq!(decoded.username, Some("testuser".to_string()));
        assert_eq!(decoded.password, Some(b"testpass".to_vec()));
    }

    #[test]
    fn test_connect_encode_decode_v311() {
        let options = ConnectOptions::new("mqtt-311-client");
        let packet = ConnectPacket::new_v311(options);

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        let decoded = ConnectPacket::decode_body(&mut buf, &fixed_header).unwrap();

        assert_eq!(decoded.protocol_version, PROTOCOL_VERSION_V311);
        assert_eq!(decoded.client_id, "mqtt-311-client");
    }

    #[test]
    fn test_connect_invalid_protocol_name() {
        let mut buf = BytesMut::new();
        encode_string(&mut buf, "INVALID").unwrap();
        buf.put_u8(5);

        let fixed_header = FixedHeader::new(PacketType::Connect, 0, 0);
        let result = ConnectPacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }

    #[test]
    fn test_connect_invalid_protocol_version() {
        let mut buf = BytesMut::new();
        encode_string(&mut buf, "MQTT").unwrap();
        buf.put_u8(99); // Invalid version

        let fixed_header = FixedHeader::new(PacketType::Connect, 0, 0);
        let result = ConnectPacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }

    #[test]
    fn test_connect_password_without_username() {
        let mut buf = BytesMut::new();
        encode_string(&mut buf, "MQTT").unwrap();
        buf.put_u8(5); // v5.0
        buf.put_u8(0x40); // Password flag only
        buf.put_u16(60); // Keep alive
        buf.put_u8(0); // Empty properties
        encode_string(&mut buf, "client").unwrap();
        encode_binary(&mut buf, b"password").unwrap();

        let fixed_header = FixedHeader::new(PacketType::Connect, 0, 0);
        let result = ConnectPacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }
}
