//! MQTT Protocol Constants
//!
//! This module defines constants for MQTT packet types and flags to avoid magic numbers
//! throughout the codebase.

use crate::PacketType;

/// Fixed header byte 1 values (packet type << 4 | flags)
pub mod fixed_header {
    /// CONNECT packet fixed header (0x10)
    pub const CONNECT: u8 = (super::PacketType::Connect as u8) << 4;

    /// CONNACK packet fixed header (0x20)
    pub const CONNACK: u8 = (super::PacketType::ConnAck as u8) << 4;

    /// PUBLISH packet fixed header base (0x30) - flags vary
    pub const PUBLISH_BASE: u8 = (super::PacketType::Publish as u8) << 4;

    /// PUBACK packet fixed header (0x40)
    pub const PUBACK: u8 = (super::PacketType::PubAck as u8) << 4;

    /// PUBREC packet fixed header (0x50)
    pub const PUBREC: u8 = (super::PacketType::PubRec as u8) << 4;

    /// PUBREL packet fixed header (0x62) - has required flags
    pub const PUBREL: u8 = (super::PacketType::PubRel as u8) << 4 | 0x02;

    /// PUBCOMP packet fixed header (0x70)
    pub const PUBCOMP: u8 = (super::PacketType::PubComp as u8) << 4;

    /// SUBSCRIBE packet fixed header (0x82) - has required flags
    pub const SUBSCRIBE: u8 = (super::PacketType::Subscribe as u8) << 4 | 0x02;

    /// SUBACK packet fixed header (0x90)
    pub const SUBACK: u8 = (super::PacketType::SubAck as u8) << 4;

    /// UNSUBSCRIBE packet fixed header (0xA2) - has required flags
    pub const UNSUBSCRIBE: u8 = (super::PacketType::Unsubscribe as u8) << 4 | 0x02;

    /// UNSUBACK packet fixed header (0xB0)
    pub const UNSUBACK: u8 = (super::PacketType::UnsubAck as u8) << 4;

    /// PINGREQ packet fixed header (0xC0)
    pub const PINGREQ: u8 = (super::PacketType::PingReq as u8) << 4;

    /// PINGRESP packet fixed header (0xD0)
    pub const PINGRESP: u8 = (super::PacketType::PingResp as u8) << 4;

    /// DISCONNECT packet fixed header (0xE0)
    pub const DISCONNECT: u8 = (super::PacketType::Disconnect as u8) << 4;

    /// AUTH packet fixed header (0xF0)
    pub const AUTH: u8 = (super::PacketType::Auth as u8) << 4;
}

/// Masks for extracting fields from fixed header
pub mod masks {
    /// Mask for extracting packet type from fixed header byte 1 (0xF0)
    pub const PACKET_TYPE: u8 = 0xF0;

    /// Mask for extracting flags from fixed header byte 1 (0x0F)
    pub const FLAGS: u8 = 0x0F;

    /// Mask for checking continuation bit in variable byte integer (0x80)
    pub const CONTINUATION_BIT: u8 = 0x80;

    /// Mask for extracting value from variable byte integer (0x7F)
    pub const VARIABLE_BYTE_VALUE: u8 = 0x7F;
}

/// Common packet payloads
pub mod packets {
    /// PINGREQ packet as bytes
    pub const PINGREQ_BYTES: [u8; 2] = [super::fixed_header::PINGREQ, 0x00];

    /// PINGRESP packet as bytes
    pub const PINGRESP_BYTES: [u8; 2] = [super::fixed_header::PINGRESP, 0x00];
}

/// Subscription option masks
pub mod subscription {
    /// Mask for `QoS` bits (bits 0-1)
    pub const QOS_MASK: u8 = 0x03;

    /// Mask for No Local flag (bit 2)
    pub const NO_LOCAL_MASK: u8 = 0x04;

    /// Mask for Retain As Published flag (bit 3)
    pub const RETAIN_AS_PUBLISHED_MASK: u8 = 0x08;

    /// Mask for Retain Handling (bits 4-5)
    pub const RETAIN_HANDLING_MASK: u8 = 0x30;

    /// Shift for Retain Handling
    pub const RETAIN_HANDLING_SHIFT: u8 = 4;

    /// Mask for reserved bits (bits 6-7)
    pub const RESERVED_BITS_MASK: u8 = 0xC0;
}

/// CONNECT flags masks
pub mod connect_flags {
    /// Mask for clearing Will `QoS` bits (bits 3-4)
    pub const WILL_QOS_CLEAR_MASK: u8 = !0x18;
    /// Mask for extracting Will `QoS` (bits 3-4 shifted)
    pub const WILL_QOS_MASK: u8 = 0x03;
    /// Shift for Will `QoS`
    pub const WILL_QOS_SHIFT: u8 = 3;
}

/// String and binary data limits
pub mod limits {
    /// Maximum string length in MQTT (65535)
    pub const MAX_STRING_LENGTH: u16 = u16::MAX;

    /// Maximum client ID length (128 characters)
    pub const MAX_CLIENT_ID_LENGTH: usize = 128;

    /// Maximum packet size (256 MB)
    pub const MAX_PACKET_SIZE: u32 = 268_435_456;

    /// Maximum binary data length (65536)
    pub const MAX_BINARY_LENGTH: u32 = 65_536;
}

/// Time-related constants
pub mod time {
    use crate::time::Duration;

    /// Default session expiry interval (1 hour)
    pub const DEFAULT_SESSION_EXPIRY: Duration = Duration::from_secs(3600);

    /// Default keep alive interval (60 seconds)
    pub const DEFAULT_KEEP_ALIVE: Duration = Duration::from_secs(60);
}

/// Buffer and capacity constants
pub mod buffer {
    /// Default buffer capacity (1024 bytes)
    pub const DEFAULT_CAPACITY: usize = 1024;

    /// Default buffer size for encoding/decoding (2048 bytes)
    pub const DEFAULT_BUFFER_SIZE: usize = 2048;

    /// Large buffer size for bulk operations (4096 bytes)
    pub const LARGE_BUFFER_SIZE: usize = 4096;

    /// Maximum buffer size (8192 bytes)
    pub const MAX_BUFFER_SIZE: usize = 8192;

    /// Very large buffer for high-throughput scenarios (16384 bytes)
    pub const VERY_LARGE_BUFFER_SIZE: usize = 16384;

    /// Huge buffer for maximum performance (32768 bytes)
    pub const HUGE_BUFFER_SIZE: usize = 32768;
}

/// Variable byte integer constants
pub mod variable_byte {
    /// Maximum value for single byte (127)
    pub const SINGLE_BYTE_MAX: u8 = 127;

    /// Maximum value for variable byte integer (268435455)
    pub const MAX_VALUE: u32 = 268_435_455;
}

/// Protocol version constants
pub mod version {
    /// MQTT protocol version 5.0
    pub const MQTT_V5: u8 = 5;
}

/// PUBLISH flags masks
pub mod publish_flags {
    /// Mask for clearing `QoS` bits (bits 1-2)
    pub const QOS_CLEAR_MASK: u8 = !0x06;
    /// Mask for extracting `QoS` (bits 1-2 shifted)
    pub const QOS_MASK: u8 = 0x03;
    /// Shift for `QoS`
    pub const QOS_SHIFT: u8 = 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fixed_header_values() {
        assert_eq!(fixed_header::CONNECT, 0x10);
        assert_eq!(fixed_header::CONNACK, 0x20);
        assert_eq!(fixed_header::PUBLISH_BASE, 0x30);
        assert_eq!(fixed_header::PINGREQ, 0xC0);
        assert_eq!(fixed_header::PINGRESP, 0xD0);
    }

    #[test]
    fn test_packets() {
        assert_eq!(packets::PINGREQ_BYTES, [0xC0, 0x00]);
        assert_eq!(packets::PINGRESP_BYTES, [0xD0, 0x00]);
    }
}
