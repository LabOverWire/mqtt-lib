use crate::encoding::{decode_string, encode_string};
use crate::error::{MqttError, Result};
use crate::packet::{FixedHeader, MqttPacket, PacketType};
use crate::prelude::{format, String, ToString, Vec};
use crate::protocol::v5::properties::Properties;
use crate::types::ProtocolVersion;
use crate::QoS;
use bytes::{Buf, BufMut};

pub use super::subscribe_options::{RetainHandling, SubscriptionOptions, SubscriptionOptionsBits};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicFilter {
    pub filter: String,
    pub options: SubscriptionOptions,
}

impl TopicFilter {
    #[must_use]
    pub fn new(filter: impl Into<String>, qos: QoS) -> Self {
        Self {
            filter: filter.into(),
            options: SubscriptionOptions::new(qos),
        }
    }

    #[must_use]
    pub fn with_options(filter: impl Into<String>, options: SubscriptionOptions) -> Self {
        Self {
            filter: filter.into(),
            options,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SubscribePacket {
    pub packet_id: u16,
    pub filters: Vec<TopicFilter>,
    pub properties: Properties,
    pub protocol_version: u8,
}

impl SubscribePacket {
    #[must_use]
    pub fn new(packet_id: u16) -> Self {
        Self {
            packet_id,
            filters: Vec::new(),
            properties: Properties::default(),
            protocol_version: 5,
        }
    }

    #[must_use]
    pub fn new_v311(packet_id: u16) -> Self {
        Self {
            packet_id,
            filters: Vec::new(),
            properties: Properties::default(),
            protocol_version: 4,
        }
    }

    #[must_use]
    pub fn add_filter(mut self, filter: impl Into<String>, qos: QoS) -> Self {
        self.filters.push(TopicFilter::new(filter, qos));
        self
    }

    #[must_use]
    pub fn add_filter_with_options(mut self, filter: TopicFilter) -> Self {
        self.filters.push(filter);
        self
    }

    #[must_use]
    pub fn with_subscription_identifier(mut self, id: u32) -> Self {
        self.properties.set_subscription_identifier(id);
        self
    }

    #[must_use]
    pub fn with_user_property(mut self, key: String, value: String) -> Self {
        self.properties.add_user_property(key, value);
        self
    }
}

impl MqttPacket for SubscribePacket {
    fn packet_type(&self) -> PacketType {
        PacketType::Subscribe
    }

    fn flags(&self) -> u8 {
        0x02
    }

    fn encode_body<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        buf.put_u16(self.packet_id);

        if self.protocol_version == 5 {
            self.properties.encode(buf)?;
        }

        if self.filters.is_empty() {
            return Err(MqttError::MalformedPacket(
                "SUBSCRIBE packet must contain at least one topic filter".to_string(),
            ));
        }

        for filter in &self.filters {
            encode_string(buf, &filter.filter)?;
            if self.protocol_version == 5 {
                buf.put_u8(filter.options.encode());
            } else {
                buf.put_u8(filter.options.qos as u8);
            }
        }

        Ok(())
    }

    fn decode_body<B: Buf>(buf: &mut B, fixed_header: &FixedHeader) -> Result<Self> {
        Self::decode_body_with_version(buf, fixed_header, 5)
    }
}

impl SubscribePacket {
    /// # Errors
    /// Returns an error if decoding fails.
    pub fn decode_body_with_version<B: Buf>(
        buf: &mut B,
        fixed_header: &FixedHeader,
        protocol_version: u8,
    ) -> Result<Self> {
        ProtocolVersion::try_from(protocol_version)
            .map_err(|()| MqttError::UnsupportedProtocolVersion)?;

        if fixed_header.flags != 0x02 {
            return Err(MqttError::MalformedPacket(format!(
                "Invalid SUBSCRIBE flags: expected 0x02, got 0x{:02X}",
                fixed_header.flags
            )));
        }

        if buf.remaining() < 2 {
            return Err(MqttError::MalformedPacket(
                "SUBSCRIBE missing packet identifier".to_string(),
            ));
        }
        let packet_id = buf.get_u16();

        let properties = if protocol_version == 5 {
            Properties::decode(buf)?
        } else {
            Properties::default()
        };

        let mut filters = Vec::new();

        if !buf.has_remaining() {
            return Err(MqttError::MalformedPacket(
                "SUBSCRIBE packet must contain at least one topic filter".to_string(),
            ));
        }

        while buf.has_remaining() {
            let filter_str = decode_string(buf)?;

            if !buf.has_remaining() {
                return Err(MqttError::MalformedPacket(
                    "Missing subscription options for topic filter".to_string(),
                ));
            }

            let options_byte = buf.get_u8();
            let options = if protocol_version == 5 {
                SubscriptionOptions::decode(options_byte)?
            } else {
                SubscriptionOptions {
                    qos: QoS::from(options_byte & 0x03),
                    ..Default::default()
                }
            };

            filters.push(TopicFilter {
                filter: filter_str,
                options,
            });
        }

        Ok(Self {
            packet_id,
            filters,
            properties,
            protocol_version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::v5::properties::PropertyId;
    use bytes::BytesMut;

    #[test]
    fn test_subscribe_basic() {
        let packet = SubscribePacket::new(123)
            .add_filter("temperature/+", QoS::AtLeastOnce)
            .add_filter("humidity/#", QoS::ExactlyOnce);

        assert_eq!(packet.packet_id, 123);
        assert_eq!(packet.filters.len(), 2);
        assert_eq!(packet.filters[0].filter, "temperature/+");
        assert_eq!(packet.filters[0].options.qos, QoS::AtLeastOnce);
        assert_eq!(packet.filters[1].filter, "humidity/#");
        assert_eq!(packet.filters[1].options.qos, QoS::ExactlyOnce);
    }

    #[test]
    fn test_subscribe_with_options() {
        let options = SubscriptionOptions {
            qos: QoS::AtLeastOnce,
            no_local: true,
            retain_as_published: false,
            retain_handling: RetainHandling::DoNotSend,
        };

        let packet = SubscribePacket::new(456)
            .add_filter_with_options(TopicFilter::with_options("test/topic", options));

        assert!(packet.filters[0].options.no_local);
        assert_eq!(
            packet.filters[0].options.retain_handling,
            RetainHandling::DoNotSend
        );
    }

    #[test]
    fn test_subscribe_encode_decode() {
        let packet = SubscribePacket::new(789)
            .add_filter("sensor/temp", QoS::AtMostOnce)
            .add_filter("sensor/humidity", QoS::AtLeastOnce)
            .with_subscription_identifier(42);

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        assert_eq!(fixed_header.packet_type, PacketType::Subscribe);
        assert_eq!(fixed_header.flags, 0x02);

        let decoded = SubscribePacket::decode_body(&mut buf, &fixed_header).unwrap();
        assert_eq!(decoded.packet_id, 789);
        assert_eq!(decoded.filters.len(), 2);
        assert_eq!(decoded.filters[0].filter, "sensor/temp");
        assert_eq!(decoded.filters[0].options.qos, QoS::AtMostOnce);
        assert_eq!(decoded.filters[1].filter, "sensor/humidity");
        assert_eq!(decoded.filters[1].options.qos, QoS::AtLeastOnce);

        let sub_id = decoded.properties.get(PropertyId::SubscriptionIdentifier);
        assert!(sub_id.is_some());
    }

    #[test]
    fn test_subscribe_invalid_flags() {
        let mut buf = BytesMut::new();
        buf.put_u16(123);

        let fixed_header = FixedHeader::new(PacketType::Subscribe, 0x00, 2);
        let result = SubscribePacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }

    #[test]
    fn test_subscribe_empty_filters() {
        let packet = SubscribePacket::new(123);

        let mut buf = BytesMut::new();
        let result = packet.encode(&mut buf);
        assert!(result.is_err());
    }
}
