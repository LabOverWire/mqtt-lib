pub mod auth;
pub mod connack;
pub mod connect;
pub mod disconnect;
pub mod pingreq;
pub mod pingresp;
pub mod puback;
pub mod pubcomp;
pub mod publish;
pub mod pubrec;
pub mod pubrel;
pub mod suback;
pub mod subscribe;
pub mod unsuback;
pub mod unsubscribe;

#[cfg(test)]
mod property_tests;

#[cfg(test)]
mod bebytes_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn prop_mqtt_type_and_flags_round_trip(
            message_type in 1u8..=15,
            dup in 0u8..=1,
            qos in 0u8..=3,
            retain in 0u8..=1
        ) {
            let original = MqttTypeAndFlags {
                message_type,
                dup,
                qos,
                retain,
            };

            let bytes = original.to_be_bytes();
            let (decoded, _) = MqttTypeAndFlags::try_from_be_bytes(&bytes).unwrap();

            prop_assert_eq!(original, decoded);
        }

        #[test]
        fn prop_packet_type_round_trip(packet_type in 1u8..=15) {
            if let Some(pt) = PacketType::from_u8(packet_type) {
                let type_and_flags = MqttTypeAndFlags::for_packet_type(pt);
                let bytes = type_and_flags.to_be_bytes();
                let (decoded, _) = MqttTypeAndFlags::try_from_be_bytes(&bytes).unwrap();

                prop_assert_eq!(type_and_flags, decoded);
                prop_assert_eq!(decoded.packet_type(), Some(pt));
            }
        }

        #[test]
        fn prop_publish_flags_round_trip(
            qos in 0u8..=3,
            dup: bool,
            retain: bool
        ) {
            let type_and_flags = MqttTypeAndFlags::for_publish(qos, dup, retain);
            let bytes = type_and_flags.to_be_bytes();
            let (decoded, _) = MqttTypeAndFlags::try_from_be_bytes(&bytes).unwrap();

            prop_assert_eq!(type_and_flags, decoded);
            prop_assert_eq!(decoded.packet_type(), Some(PacketType::Publish));
            prop_assert_eq!(decoded.qos, qos);
            prop_assert_eq!(decoded.is_dup(), dup);
            prop_assert_eq!(decoded.is_retain(), retain);
        }
    }
}

use crate::encoding::{decode_variable_int, encode_variable_int};
use crate::error::{MqttError, Result};
use bebytes::BeBytes;
use bytes::{Buf, BufMut};

/// MQTT acknowledgment packet variable header using bebytes
/// Used by `PubAck`, `PubRec`, `PubRel`, and `PubComp` packets
#[derive(Debug, Clone, Copy, PartialEq, Eq, BeBytes)]
pub struct AckPacketHeader {
    /// Packet identifier (big-endian u16)
    #[bebytes(big_endian)]
    pub packet_id: u16,
    /// Reason code (single byte)
    pub reason_code: u8,
}

impl AckPacketHeader {
    /// Creates a new acknowledgment packet header
    #[must_use]
    pub fn create(packet_id: u16, reason_code: crate::types::ReasonCode) -> Self {
        Self {
            packet_id,
            reason_code: u8::from(reason_code),
        }
    }

    /// Gets the reason code as a `ReasonCode` enum
    #[must_use]
    pub fn get_reason_code(&self) -> Option<crate::types::ReasonCode> {
        crate::types::ReasonCode::from_u8(self.reason_code)
    }
}

/// MQTT Fixed Header Type and Flags byte using bebytes for bit field operations
#[derive(Debug, Clone, Copy, PartialEq, Eq, BeBytes)]
pub struct MqttTypeAndFlags {
    /// Message type (bits 7-4)
    #[bits(4)]
    pub message_type: u8,
    /// DUP flag (bit 3) - for PUBLISH packets
    #[bits(1)]
    pub dup: u8,
    /// `QoS` level (bits 2-1) - for PUBLISH packets
    #[bits(2)]
    pub qos: u8,
    /// RETAIN flag (bit 0) - for PUBLISH packets  
    #[bits(1)]
    pub retain: u8,
}

impl MqttTypeAndFlags {
    /// Creates a new `MqttTypeAndFlags` for a given packet type
    #[must_use]
    pub fn for_packet_type(packet_type: PacketType) -> Self {
        Self {
            message_type: packet_type as u8,
            dup: 0,
            qos: 0,
            retain: 0,
        }
    }

    /// Creates a new `MqttTypeAndFlags` for PUBLISH packets with `QoS` and flags
    #[must_use]
    pub fn for_publish(qos: u8, dup: bool, retain: bool) -> Self {
        Self {
            message_type: PacketType::Publish as u8,
            dup: u8::from(dup),
            qos,
            retain: u8::from(retain),
        }
    }

    /// Returns the packet type
    #[must_use]
    pub fn packet_type(&self) -> Option<PacketType> {
        PacketType::from_u8(self.message_type)
    }

    /// Returns true if the DUP flag is set
    #[must_use]
    pub fn is_dup(&self) -> bool {
        self.dup != 0
    }

    /// Returns true if the RETAIN flag is set
    #[must_use]
    pub fn is_retain(&self) -> bool {
        self.retain != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BeBytes)]
pub enum PacketType {
    Connect = 1,
    ConnAck = 2,
    Publish = 3,
    PubAck = 4,
    PubRec = 5,
    PubRel = 6,
    PubComp = 7,
    Subscribe = 8,
    SubAck = 9,
    Unsubscribe = 10,
    UnsubAck = 11,
    PingReq = 12,
    PingResp = 13,
    Disconnect = 14,
    Auth = 15,
}

impl PacketType {
    /// Converts a u8 to `PacketType`
    #[must_use]
    pub fn from_u8(value: u8) -> Option<Self> {
        // Use the TryFrom implementation generated by BeBytes
        Self::try_from(value).ok()
    }
}

impl From<PacketType> for u8 {
    fn from(packet_type: PacketType) -> Self {
        packet_type as u8
    }
}

/// MQTT packet fixed header
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedHeader {
    pub packet_type: PacketType,
    pub flags: u8,
    pub remaining_length: u32,
}

impl FixedHeader {
    /// Creates a new fixed header
    #[must_use]
    pub fn new(packet_type: PacketType, flags: u8, remaining_length: u32) -> Self {
        Self {
            packet_type,
            flags,
            remaining_length,
        }
    }

    /// Encodes the fixed header
    ///
    /// # Errors
    ///
    /// Returns an error if the remaining length is too large
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub fn encode<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        let byte1 =
            (u8::from(self.packet_type) << 4) | (self.flags & crate::constants::masks::FLAGS);
        buf.put_u8(byte1);
        encode_variable_int(buf, self.remaining_length)?;
        Ok(())
    }

    /// Decodes a fixed header from the buffer
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Insufficient bytes in buffer
    /// - Invalid packet type
    /// - Invalid remaining length
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub fn decode<B: Buf>(buf: &mut B) -> Result<Self> {
        if !buf.has_remaining() {
            return Err(MqttError::MalformedPacket(
                "No data for fixed header".to_string(),
            ));
        }

        let byte1 = buf.get_u8();
        let packet_type_val = (byte1 >> 4) & crate::constants::masks::FLAGS;
        let flags = byte1 & crate::constants::masks::FLAGS;

        let packet_type = PacketType::from_u8(packet_type_val)
            .ok_or(MqttError::InvalidPacketType(packet_type_val))?;

        let remaining_length = decode_variable_int(buf)?;

        Ok(Self {
            packet_type,
            flags,
            remaining_length,
        })
    }

    /// Validates the flags for the packet type
    #[must_use]
    pub fn validate_flags(&self) -> bool {
        match self.packet_type {
            PacketType::Publish => true, // Publish has variable flags
            PacketType::PubRel | PacketType::Subscribe | PacketType::Unsubscribe => {
                self.flags == 0x02 // Required flags for these packet types
            }
            _ => self.flags == 0,
        }
    }

    /// Returns the encoded length of the fixed header
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        // 1 byte for packet type + flags, plus variable length encoding of remaining length
        1 + crate::encoding::encoded_variable_int_len(self.remaining_length)
    }
}

/// Enum representing all MQTT packet types
#[derive(Debug, Clone)]
pub enum Packet {
    Connect(Box<connect::ConnectPacket>),
    ConnAck(connack::ConnAckPacket),
    Publish(publish::PublishPacket),
    PubAck(puback::PubAckPacket),
    PubRec(pubrec::PubRecPacket),
    PubRel(pubrel::PubRelPacket),
    PubComp(pubcomp::PubCompPacket),
    Subscribe(subscribe::SubscribePacket),
    SubAck(suback::SubAckPacket),
    Unsubscribe(unsubscribe::UnsubscribePacket),
    UnsubAck(unsuback::UnsubAckPacket),
    PingReq,
    PingResp,
    Disconnect(disconnect::DisconnectPacket),
    Auth(auth::AuthPacket),
}

impl Packet {
    /// Decode a packet body based on the packet type
    ///
    /// # Errors
    ///
    /// Returns an error if decoding fails
    pub fn decode_from_body<B: Buf>(
        packet_type: PacketType,
        fixed_header: &FixedHeader,
        buf: &mut B,
    ) -> Result<Self> {
        match packet_type {
            PacketType::Connect => {
                let packet = connect::ConnectPacket::decode_body(buf, fixed_header)?;
                Ok(Packet::Connect(Box::new(packet)))
            }
            PacketType::ConnAck => {
                let packet = connack::ConnAckPacket::decode_body(buf, fixed_header)?;
                Ok(Packet::ConnAck(packet))
            }
            PacketType::Publish => {
                let packet = publish::PublishPacket::decode_body(buf, fixed_header)?;
                Ok(Packet::Publish(packet))
            }
            PacketType::PubAck => {
                let packet = puback::PubAckPacket::decode_body(buf, fixed_header)?;
                Ok(Packet::PubAck(packet))
            }
            PacketType::PubRec => {
                let packet = pubrec::PubRecPacket::decode_body(buf, fixed_header)?;
                Ok(Packet::PubRec(packet))
            }
            PacketType::PubRel => {
                let packet = pubrel::PubRelPacket::decode_body(buf, fixed_header)?;
                Ok(Packet::PubRel(packet))
            }
            PacketType::PubComp => {
                let packet = pubcomp::PubCompPacket::decode_body(buf, fixed_header)?;
                Ok(Packet::PubComp(packet))
            }
            PacketType::Subscribe => {
                let packet = subscribe::SubscribePacket::decode_body(buf, fixed_header)?;
                Ok(Packet::Subscribe(packet))
            }
            PacketType::SubAck => {
                let packet = suback::SubAckPacket::decode_body(buf, fixed_header)?;
                Ok(Packet::SubAck(packet))
            }
            PacketType::Unsubscribe => {
                let packet = unsubscribe::UnsubscribePacket::decode_body(buf, fixed_header)?;
                Ok(Packet::Unsubscribe(packet))
            }
            PacketType::UnsubAck => {
                let packet = unsuback::UnsubAckPacket::decode_body(buf, fixed_header)?;
                Ok(Packet::UnsubAck(packet))
            }
            PacketType::PingReq => Ok(Packet::PingReq),
            PacketType::PingResp => Ok(Packet::PingResp),
            PacketType::Disconnect => {
                let packet = disconnect::DisconnectPacket::decode_body(buf, fixed_header)?;
                Ok(Packet::Disconnect(packet))
            }
            PacketType::Auth => {
                let packet = auth::AuthPacket::decode_body(buf, fixed_header)?;
                Ok(Packet::Auth(packet))
            }
        }
    }
}

/// Trait for MQTT packets
pub trait MqttPacket: Sized {
    /// Returns the packet type
    fn packet_type(&self) -> PacketType;

    /// Returns the fixed header flags
    fn flags(&self) -> u8 {
        0
    }

    /// Encodes the packet body (without fixed header)
    ///
    /// # Errors
    ///
    /// Returns an error if encoding fails
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    fn encode_body<B: BufMut>(&self, buf: &mut B) -> Result<()>;

    /// Decodes the packet body (without fixed header)
    ///
    /// # Errors
    ///
    /// Returns an error if decoding fails
    fn decode_body<B: Buf>(buf: &mut B, fixed_header: &FixedHeader) -> Result<Self>;

    /// Encodes the complete packet (with fixed header)
    ///
    /// # Errors
    ///
    /// Returns an error if encoding fails
    fn encode<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        // First encode to temporary buffer to get remaining length
        let mut body = Vec::new();
        self.encode_body(&mut body)?;

        let fixed_header = FixedHeader::new(
            self.packet_type(),
            self.flags(),
            body.len().try_into().unwrap_or(u32::MAX),
        );

        fixed_header.encode(buf)?;
        buf.put_slice(&body);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[test]
    fn test_packet_type_from_u8() {
        assert_eq!(PacketType::from_u8(1), Some(PacketType::Connect));
        assert_eq!(PacketType::from_u8(2), Some(PacketType::ConnAck));
        assert_eq!(PacketType::from_u8(15), Some(PacketType::Auth));
        assert_eq!(PacketType::from_u8(0), None);
        assert_eq!(PacketType::from_u8(16), None);
    }

    #[test]
    fn test_fixed_header_encode_decode() {
        let mut buf = BytesMut::new();

        let header = FixedHeader::new(PacketType::Connect, 0, 100);
        header.encode(&mut buf).unwrap();

        let decoded = FixedHeader::decode(&mut buf).unwrap();
        assert_eq!(decoded.packet_type, PacketType::Connect);
        assert_eq!(decoded.flags, 0);
        assert_eq!(decoded.remaining_length, 100);
    }

    #[test]
    fn test_fixed_header_with_flags() {
        let mut buf = BytesMut::new();

        let header = FixedHeader::new(PacketType::Publish, 0x0D, 50);
        header.encode(&mut buf).unwrap();

        let decoded = FixedHeader::decode(&mut buf).unwrap();
        assert_eq!(decoded.packet_type, PacketType::Publish);
        assert_eq!(decoded.flags, 0x0D);
        assert_eq!(decoded.remaining_length, 50);
    }

    #[test]
    fn test_validate_flags() {
        let header = FixedHeader::new(PacketType::Connect, 0, 0);
        assert!(header.validate_flags());

        let header = FixedHeader::new(PacketType::Connect, 1, 0);
        assert!(!header.validate_flags());

        let header = FixedHeader::new(PacketType::Subscribe, 0x02, 0);
        assert!(header.validate_flags());

        let header = FixedHeader::new(PacketType::Subscribe, 0x00, 0);
        assert!(!header.validate_flags());

        let header = FixedHeader::new(PacketType::Publish, 0x0F, 0);
        assert!(header.validate_flags());
    }

    #[test]
    fn test_decode_insufficient_data() {
        let mut buf = BytesMut::new();
        let result = FixedHeader::decode(&mut buf);
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_invalid_packet_type() {
        let mut buf = BytesMut::new();
        buf.put_u8(0x00); // Invalid packet type 0
        buf.put_u8(0x00); // Remaining length

        let result = FixedHeader::decode(&mut buf);
        assert!(result.is_err());
    }

    #[test]
    fn test_packet_type_bebytes_serialization() {
        // Test BeBytes to_be_bytes and try_from_be_bytes
        let packet_type = PacketType::Publish;
        let bytes = packet_type.to_be_bytes();
        assert_eq!(bytes, vec![3]);

        let (decoded, consumed) = PacketType::try_from_be_bytes(&bytes).unwrap();
        assert_eq!(decoded, PacketType::Publish);
        assert_eq!(consumed, 1);

        // Test other packet types
        let packet_type = PacketType::Connect;
        let bytes = packet_type.to_be_bytes();
        assert_eq!(bytes, vec![1]);

        let (decoded, consumed) = PacketType::try_from_be_bytes(&bytes).unwrap();
        assert_eq!(decoded, PacketType::Connect);
        assert_eq!(consumed, 1);
    }
}
