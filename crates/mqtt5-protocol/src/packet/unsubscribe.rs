use crate::encoding::{decode_string, encode_string};
use crate::error::{MqttError, Result};
use crate::packet::{FixedHeader, MqttPacket, PacketType};
use crate::protocol::v5::properties::Properties;
use bytes::{Buf, BufMut};

/// MQTT UNSUBSCRIBE packet
#[derive(Debug, Clone)]
pub struct UnsubscribePacket {
    /// Packet identifier
    pub packet_id: u16,
    /// Topic filters to unsubscribe from
    pub filters: Vec<String>,
    /// UNSUBSCRIBE properties (v5.0 only)
    pub properties: Properties,
}

impl UnsubscribePacket {
    /// Creates a new UNSUBSCRIBE packet
    #[must_use]
    pub fn new(packet_id: u16) -> Self {
        Self {
            packet_id,
            filters: Vec::new(),
            properties: Properties::new(),
        }
    }

    /// Adds a topic filter to unsubscribe from
    #[must_use]
    pub fn add_filter(mut self, filter: impl Into<String>) -> Self {
        self.filters.push(filter.into());
        self
    }

    /// Adds a user property
    #[must_use]
    pub fn with_user_property(mut self, key: String, value: String) -> Self {
        self.properties.add_user_property(key, value);
        self
    }
}

impl MqttPacket for UnsubscribePacket {
    fn packet_type(&self) -> PacketType {
        PacketType::Unsubscribe
    }

    fn flags(&self) -> u8 {
        0x02 // UNSUBSCRIBE must have flags = 0x02
    }

    fn encode_body<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        // Variable header
        buf.put_u16(self.packet_id);

        // Properties (v5.0)
        self.properties.encode(buf)?;

        // Payload - topic filters
        if self.filters.is_empty() {
            return Err(MqttError::MalformedPacket(
                "UNSUBSCRIBE packet must contain at least one topic filter".to_string(),
            ));
        }

        for filter in &self.filters {
            encode_string(buf, filter)?;
        }

        Ok(())
    }

    fn decode_body<B: Buf>(buf: &mut B, fixed_header: &FixedHeader) -> Result<Self> {
        // Validate flags
        if fixed_header.flags != 0x02 {
            return Err(MqttError::MalformedPacket(format!(
                "Invalid UNSUBSCRIBE flags: expected 0x02, got 0x{:02X}",
                fixed_header.flags
            )));
        }

        // Packet identifier
        if buf.remaining() < 2 {
            return Err(MqttError::MalformedPacket(
                "UNSUBSCRIBE missing packet identifier".to_string(),
            ));
        }
        let packet_id = buf.get_u16();

        // Properties (v5.0)
        let properties = Properties::decode(buf)?;

        // Payload - topic filters
        let mut filters = Vec::new();

        if !buf.has_remaining() {
            return Err(MqttError::MalformedPacket(
                "UNSUBSCRIBE packet must contain at least one topic filter".to_string(),
            ));
        }

        while buf.has_remaining() {
            let filter = decode_string(buf)?;
            filters.push(filter);
        }

        Ok(Self {
            packet_id,
            filters,
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
    fn test_unsubscribe_basic() {
        let packet = UnsubscribePacket::new(123)
            .add_filter("temperature/+")
            .add_filter("humidity/#");

        assert_eq!(packet.packet_id, 123);
        assert_eq!(packet.filters.len(), 2);
        assert_eq!(packet.filters[0], "temperature/+");
        assert_eq!(packet.filters[1], "humidity/#");
    }

    #[test]
    fn test_unsubscribe_with_properties() {
        let packet = UnsubscribePacket::new(456)
            .add_filter("test/topic")
            .with_user_property("reason".to_string(), "cleanup".to_string());

        assert_eq!(packet.filters.len(), 1);
        assert!(packet.properties.contains(PropertyId::UserProperty));
    }

    #[test]
    fn test_unsubscribe_encode_decode() {
        let packet = UnsubscribePacket::new(789)
            .add_filter("sensor/temp")
            .add_filter("sensor/humidity")
            .add_filter("sensor/pressure");

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        assert_eq!(fixed_header.packet_type, PacketType::Unsubscribe);
        assert_eq!(fixed_header.flags, 0x02);

        let decoded = UnsubscribePacket::decode_body(&mut buf, &fixed_header).unwrap();
        assert_eq!(decoded.packet_id, 789);
        assert_eq!(decoded.filters.len(), 3);
        assert_eq!(decoded.filters[0], "sensor/temp");
        assert_eq!(decoded.filters[1], "sensor/humidity");
        assert_eq!(decoded.filters[2], "sensor/pressure");
    }

    #[test]
    fn test_unsubscribe_invalid_flags() {
        let mut buf = BytesMut::new();
        buf.put_u16(123);

        let fixed_header = FixedHeader::new(PacketType::Unsubscribe, 0x00, 2); // Wrong flags
        let result = UnsubscribePacket::decode_body(&mut buf, &fixed_header);
        assert!(result.is_err());
    }

    #[test]
    fn test_unsubscribe_empty_filters() {
        let packet = UnsubscribePacket::new(123);

        let mut buf = BytesMut::new();
        let result = packet.encode(&mut buf);
        assert!(result.is_err());
    }
}
