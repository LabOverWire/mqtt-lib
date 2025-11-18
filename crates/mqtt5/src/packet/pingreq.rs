use crate::error::Result;
use crate::packet::{FixedHeader, MqttPacket, PacketType};
use bebytes::BeBytes;
use bytes::{Buf, BufMut};

/// MQTT PINGREQ packet - complete bebytes implementation
#[derive(Debug, Clone, Copy, PartialEq, Eq, BeBytes)]
pub struct PingReqPacket {
    /// Fixed header type and flags (PINGREQ = 0xC0, remaining length = 0x00)
    #[bebytes(big_endian)]
    fixed_header: u16, // 0xC000
}

impl PingReqPacket {
    /// The fixed header value for PINGREQ packets
    pub const FIXED_HEADER: u16 = 0xC000; // PINGREQ (0xC0) + remaining length 0 (0x00)
}

impl Default for PingReqPacket {
    fn default() -> Self {
        Self {
            fixed_header: Self::FIXED_HEADER,
        }
    }
}

impl MqttPacket for PingReqPacket {
    fn packet_type(&self) -> PacketType {
        PacketType::PingReq
    }

    fn encode_body<B: BufMut>(&self, _buf: &mut B) -> Result<()> {
        // PINGREQ has no variable header or payload - everything is in fixed header
        Ok(())
    }

    fn decode_body<B: Buf>(_buf: &mut B, _fixed_header: &FixedHeader) -> Result<Self> {
        // PINGREQ has no variable header or payload - everything is in fixed header
        Ok(Self::default())
    }
}

impl PingReqPacket {
    /// Encode directly to bytes using bebytes
    #[must_use]
    pub fn encode_complete(&self) -> Vec<u8> {
        self.to_be_bytes()
    }

    /// Decode directly from bytes using bebytes
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Insufficient bytes in data
    /// - Invalid PINGREQ packet structure
    /// - Fixed header doesn't match expected PINGREQ values
    pub fn decode_complete(data: &[u8]) -> Result<Self> {
        let (packet, _consumed) = Self::try_from_be_bytes(data).map_err(|e| {
            crate::error::MqttError::MalformedPacket(format!("Invalid PINGREQ packet: {e}"))
        })?;

        // Validate the packet has the correct fixed header
        if packet.fixed_header != Self::FIXED_HEADER {
            return Err(crate::error::MqttError::MalformedPacket(format!(
                "Invalid PINGREQ packet: expected 0x{:04X}, got 0x{:04X}",
                Self::FIXED_HEADER,
                packet.fixed_header
            )));
        }

        Ok(packet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[cfg(test)]
    mod property_tests {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn prop_pingreq_encode_decode_round_trip(_data in any::<u32>()) {
                let packet = PingReqPacket::default();
                let bytes = packet.encode_complete();
                let decoded = PingReqPacket::decode_complete(&bytes).unwrap();

                // Both should represent the same logical packet
                prop_assert_eq!(bytes, vec![0xC0, 0x00]);
                prop_assert_eq!(decoded.packet_type(), PacketType::PingReq);
                prop_assert_eq!(packet, decoded);
            }

            #[test]
            fn prop_pingreq_consistent_encoding(_data in any::<u16>()) {
                let packet1 = PingReqPacket::default();
                let packet2 = PingReqPacket::default();

                prop_assert_eq!(packet1.encode_complete(), packet2.encode_complete());
                prop_assert_eq!(packet1, packet2);
            }
        }
    }

    #[test]
    fn test_pingreq_bebytes_encode_decode() {
        let packet = PingReqPacket::default();

        // Test bebytes direct encoding
        let bytes = packet.encode_complete();
        assert_eq!(bytes.len(), 2);
        assert_eq!(bytes[0], 0xC0); // PINGREQ packet type
        assert_eq!(bytes[1], 0x00); // Remaining length = 0

        // Test bebytes direct decoding
        let decoded = PingReqPacket::decode_complete(&bytes).unwrap();
        assert_eq!(decoded, packet);
    }

    #[test]
    fn test_pingreq_mqtt_packet_interface() {
        let packet = PingReqPacket::default();

        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();

        assert_eq!(buf.len(), 2); // Fixed header only
        assert_eq!(buf[0], 0xC0); // PINGREQ packet type
        assert_eq!(buf[1], 0x00); // Remaining length = 0

        let fixed_header = FixedHeader::decode(&mut buf).unwrap();
        assert_eq!(fixed_header.packet_type, PacketType::PingReq);
        assert_eq!(fixed_header.remaining_length, 0);

        let decoded = PingReqPacket::decode_body(&mut buf, &fixed_header).unwrap();
        assert_eq!(decoded, packet);
    }

    #[test]
    fn test_pingreq_malformed_data() {
        // Test with insufficient data
        let result = PingReqPacket::decode_complete(&[0xC0]);
        assert!(result.is_err());

        // Test with wrong packet type
        let result = PingReqPacket::decode_complete(&[0xD0, 0x00]);
        assert!(result.is_err());
    }
}
