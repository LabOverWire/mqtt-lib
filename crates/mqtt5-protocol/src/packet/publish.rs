use crate::encoding::{decode_string, encode_string};
use crate::error::{MqttError, Result};
use crate::flags::PublishFlags;
use crate::packet::{FixedHeader, MqttPacket, PacketType};
use crate::protocol::v5::properties::{Properties, PropertyId, PropertyValue};
use crate::QoS;
use bytes::{Buf, BufMut};

/// MQTT PUBLISH packet
#[derive(Debug, Clone)]
pub struct PublishPacket {
    /// Topic name
    pub topic_name: String,
    /// Packet identifier (required for `QoS` > 0)
    pub packet_id: Option<u16>,
    /// Message payload
    pub payload: Vec<u8>,
    /// Quality of Service level
    pub qos: QoS,
    /// Retain flag
    pub retain: bool,
    /// Duplicate delivery flag
    pub dup: bool,
    /// PUBLISH properties (v5.0 only)
    pub properties: Properties,
}

impl PublishPacket {
    /// Creates a new PUBLISH packet
    #[must_use]
    pub fn new(topic_name: impl Into<String>, payload: impl Into<Vec<u8>>, qos: QoS) -> Self {
        let packet_id = if qos == QoS::AtMostOnce {
            None
        } else {
            Some(0) // Will be assigned by the client
        };

        Self {
            topic_name: topic_name.into(),
            packet_id,
            payload: payload.into(),
            qos,
            retain: false,
            dup: false,
            properties: Properties::default(),
        }
    }

    /// Sets the packet identifier
    #[must_use]
    pub fn with_packet_id(mut self, id: u16) -> Self {
        if self.qos != QoS::AtMostOnce {
            self.packet_id = Some(id);
        }
        self
    }

    /// Sets the retain flag
    #[must_use]
    pub fn with_retain(mut self, retain: bool) -> Self {
        self.retain = retain;
        self
    }

    /// Sets the duplicate flag
    #[must_use]
    pub fn with_dup(mut self, dup: bool) -> Self {
        self.dup = dup;
        self
    }

    /// Sets the payload format indicator
    #[must_use]
    pub fn with_payload_format_indicator(mut self, is_utf8: bool) -> Self {
        self.properties.set_payload_format_indicator(is_utf8);
        self
    }

    /// Sets the message expiry interval
    #[must_use]
    pub fn with_message_expiry_interval(mut self, seconds: u32) -> Self {
        self.properties.set_message_expiry_interval(seconds);
        self
    }

    /// Sets the topic alias
    #[must_use]
    pub fn with_topic_alias(mut self, alias: u16) -> Self {
        self.properties.set_topic_alias(alias);
        self
    }

    /// Sets the response topic
    #[must_use]
    pub fn with_response_topic(mut self, topic: String) -> Self {
        self.properties.set_response_topic(topic);
        self
    }

    /// Sets the correlation data
    #[must_use]
    pub fn with_correlation_data(mut self, data: Vec<u8>) -> Self {
        self.properties.set_correlation_data(data.into());
        self
    }

    /// Adds a user property
    #[must_use]
    pub fn with_user_property(mut self, key: String, value: String) -> Self {
        self.properties.add_user_property(key, value);
        self
    }

    /// Adds a subscription identifier
    #[must_use]
    pub fn with_subscription_identifier(mut self, id: u32) -> Self {
        self.properties.set_subscription_identifier(id);
        self
    }

    /// Sets the content type
    #[must_use]
    pub fn with_content_type(mut self, content_type: String) -> Self {
        self.properties.set_content_type(content_type);
        self
    }

    #[must_use]
    /// Gets the topic alias from properties
    pub fn topic_alias(&self) -> Option<u16> {
        self.properties
            .get(PropertyId::TopicAlias)
            .and_then(|prop| {
                if let PropertyValue::TwoByteInteger(alias) = prop {
                    Some(*alias)
                } else {
                    None
                }
            })
    }

    #[must_use]
    /// Gets the message expiry interval from properties
    pub fn message_expiry_interval(&self) -> Option<u32> {
        self.properties
            .get(PropertyId::MessageExpiryInterval)
            .and_then(|prop| {
                if let PropertyValue::FourByteInteger(interval) = prop {
                    Some(*interval)
                } else {
                    None
                }
            })
    }
}

impl MqttPacket for PublishPacket {
    fn packet_type(&self) -> PacketType {
        PacketType::Publish
    }

    fn flags(&self) -> u8 {
        let mut flags = 0u8;

        if self.dup {
            flags |= PublishFlags::Dup as u8;
        }

        flags = PublishFlags::with_qos(flags, self.qos as u8);

        if self.retain {
            flags |= PublishFlags::Retain as u8;
        }

        flags
    }

    fn encode_body<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        // Variable header
        encode_string(buf, &self.topic_name)?;

        // Packet identifier (only for QoS > 0)
        if self.qos != QoS::AtMostOnce {
            let packet_id = self.packet_id.ok_or_else(|| {
                MqttError::MalformedPacket("Packet ID required for QoS > 0".to_string())
            })?;
            buf.put_u16(packet_id);
        }

        // Properties (v5.0 - always encode for v5.0)
        // For v3.1.1 compatibility, we'd skip this
        self.properties.encode(buf)?;

        // Payload
        buf.put_slice(&self.payload);

        Ok(())
    }

    fn decode_body<B: Buf>(buf: &mut B, fixed_header: &FixedHeader) -> Result<Self> {
        // Parse flags using BeBytes decomposition
        let flags = PublishFlags::decompose(fixed_header.flags);
        let dup = flags.contains(&PublishFlags::Dup);
        let qos_val = PublishFlags::extract_qos(fixed_header.flags);
        let retain = flags.contains(&PublishFlags::Retain);

        let qos = match qos_val {
            0 => QoS::AtMostOnce,
            1 => QoS::AtLeastOnce,
            2 => QoS::ExactlyOnce,
            _ => {
                return Err(MqttError::InvalidQoS(qos_val));
            }
        };

        // Variable header
        let topic_name = decode_string(buf)?;

        // Packet identifier (only for QoS > 0)
        let packet_id = if qos == QoS::AtMostOnce {
            None
        } else {
            if buf.remaining() < 2 {
                return Err(MqttError::MalformedPacket(
                    "Missing packet identifier".to_string(),
                ));
            }
            Some(buf.get_u16())
        };

        // Properties (v5.0)
        // Always try to decode properties first
        let properties = if buf.has_remaining() {
            match Properties::decode(buf) {
                Ok(props) => props,
                Err(_) => {
                    // If properties decode fails, it might be v3.1.1 format
                    // In v3.1.1, there are no properties
                    return Err(MqttError::MalformedPacket(
                        "Failed to decode PUBLISH properties".to_string(),
                    ));
                }
            }
        } else {
            // No properties means empty properties in v5.0
            Properties::default()
        };

        // Payload - all remaining bytes
        let payload = buf.copy_to_bytes(buf.remaining()).to_vec();

        Ok(Self {
            topic_name,
            packet_id,
            payload,
            qos,
            retain,
            dup,
            properties,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[test]
    fn test_publish_packet_qos0() {
        let packet = PublishPacket::new("test/topic", b"Hello, MQTT!", QoS::AtMostOnce);

        assert_eq!(packet.topic_name, "test/topic");
        assert_eq!(packet.payload, b"Hello, MQTT!");
        assert_eq!(packet.qos, QoS::AtMostOnce);
        assert!(packet.packet_id.is_none());
        assert!(!packet.retain);
        assert!(!packet.dup);
    }

    #[test]
    fn test_publish_packet_qos1() {
        let packet =
            PublishPacket::new("test/topic", b"Hello", QoS::AtLeastOnce).with_packet_id(123);

        assert_eq!(packet.qos, QoS::AtLeastOnce);
        assert_eq!(packet.packet_id, Some(123));
    }

    #[test]
    fn test_publish_packet_with_properties() {
        let packet = PublishPacket::new("test/topic", b"data", QoS::AtMostOnce)
            .with_retain(true)
            .with_payload_format_indicator(true)
            .with_message_expiry_interval(3600)
            .with_response_topic("response/topic".to_string())
            .with_user_property("key".to_string(), "value".to_string());

        assert!(packet.retain);
        assert!(packet
            .properties
            .contains(PropertyId::PayloadFormatIndicator));
        assert!(packet
            .properties
            .contains(PropertyId::MessageExpiryInterval));
        assert!(packet.properties.contains(PropertyId::ResponseTopic));
        assert!(packet.properties.contains(PropertyId::UserProperty));
    }

    #[test]
    fn test_publish_flags() {
        let packet = PublishPacket::new("topic", b"data", QoS::AtMostOnce);
        assert_eq!(packet.flags(), 0x00);

        let packet = PublishPacket::new("topic", b"data", QoS::AtLeastOnce).with_retain(true);
        assert_eq!(packet.flags(), 0x03); // QoS 1 + Retain

        let packet = PublishPacket::new("topic", b"data", QoS::ExactlyOnce).with_dup(true);
        assert_eq!(packet.flags(), 0x0C); // DUP + QoS 2

        let packet = PublishPacket::new("topic", b"data", QoS::ExactlyOnce)
            .with_dup(true)
            .with_retain(true);
        assert_eq!(packet.flags(), 0x0D); // DUP + QoS 2 + Retain
    }

    #[test]
    fn test_publish_encode_decode_qos0() {
        let packet =
            PublishPacket::new("sensor/temperature", b"23.5", QoS::AtMostOnce).with_retain(true);

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        assert_eq!(fixed_header.packet_type, PacketType::Publish);
        assert_eq!(
            fixed_header.flags & crate::flags::PublishFlags::Retain as u8,
            crate::flags::PublishFlags::Retain as u8
        ); // Retain flag

        let decoded = PublishPacket::decode_body(&mut buf, &fixed_header).unwrap();
        assert_eq!(decoded.topic_name, "sensor/temperature");
        assert_eq!(decoded.payload, b"23.5");
        assert_eq!(decoded.qos, QoS::AtMostOnce);
        assert!(decoded.retain);
        assert!(decoded.packet_id.is_none());
    }

    #[test]
    fn test_publish_encode_decode_qos1() {
        let packet =
            PublishPacket::new("test/qos1", b"QoS 1 message", QoS::AtLeastOnce).with_packet_id(456);

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        let decoded = PublishPacket::decode_body(&mut buf, &fixed_header).unwrap();

        assert_eq!(decoded.topic_name, "test/qos1");
        assert_eq!(decoded.payload, b"QoS 1 message");
        assert_eq!(decoded.qos, QoS::AtLeastOnce);
        assert_eq!(decoded.packet_id, Some(456));
    }

    #[test]
    fn test_publish_encode_decode_with_properties() {
        let packet = PublishPacket::new("test/props", b"data", QoS::ExactlyOnce)
            .with_packet_id(789)
            .with_message_expiry_interval(7200)
            .with_content_type("application/json".to_string());

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        let decoded = PublishPacket::decode_body(&mut buf, &fixed_header).unwrap();

        assert_eq!(decoded.qos, QoS::ExactlyOnce);
        assert_eq!(decoded.packet_id, Some(789));

        let expiry = decoded.properties.get(PropertyId::MessageExpiryInterval);
        assert!(expiry.is_some());
        if let Some(PropertyValue::FourByteInteger(val)) = expiry {
            assert_eq!(*val, 7200);
        }

        let content_type = decoded.properties.get(PropertyId::ContentType);
        assert!(content_type.is_some());
        if let Some(PropertyValue::Utf8String(val)) = content_type {
            assert_eq!(val, "application/json");
        }
    }

    #[test]
    fn test_publish_missing_packet_id() {
        let mut buf = BytesMut::new();
        encode_string(&mut buf, "topic").unwrap();
        // No packet ID for QoS > 0 - buffer ends here

        let fixed_header =
            FixedHeader::new(PacketType::Publish, 0x02, u32::try_from(buf.len()).unwrap()); // QoS 1
        let result = PublishPacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }

    #[test]
    fn test_publish_invalid_qos() {
        let mut buf = BytesMut::new();
        encode_string(&mut buf, "topic").unwrap();

        let fixed_header = FixedHeader::new(PacketType::Publish, 0x06, 0); // Invalid QoS 3
        let result = PublishPacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }
}
