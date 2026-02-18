//! Raw TCP client for sending hand-crafted MQTT packets.
//!
//! The normal [`MqttClient`](mqtt5::MqttClient) API enforces well-formed packets,
//! making it impossible to test how the broker handles malformed input.
//! [`RawMqttClient`] operates at the TCP byte level, and [`RawPacketBuilder`]
//! constructs both valid and deliberately invalid MQTT v5.0 packets for
//! conformance edge-case testing.

#![allow(clippy::cast_possible_truncation, clippy::missing_errors_doc)]

use bytes::{BufMut, BytesMut};
use mqtt5_protocol::packet::connack::ConnAckPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::FixedHeader;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A raw TCP client for sending arbitrary bytes to an MQTT broker.
///
/// Unlike [`MqttClient`](mqtt5::MqttClient), this client performs no packet
/// validation or encoding — it writes raw bytes directly to the TCP stream.
/// Use this for testing broker behavior with malformed, truncated, or
/// protocol-violating packets.
pub struct RawMqttClient {
    stream: TcpStream,
}

impl RawMqttClient {
    /// Opens a raw TCP connection to the broker.
    pub async fn connect_tcp(addr: SocketAddr) -> std::io::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        Ok(Self { stream })
    }

    /// Sends raw bytes over the TCP connection.
    pub async fn send_raw(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.stream.write_all(data).await
    }

    /// Reads raw response bytes from the broker within the given timeout.
    ///
    /// Returns `None` if the timeout elapses, the connection closes, or a
    /// read error occurs.
    pub async fn read_packet_bytes(&mut self, timeout_dur: Duration) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; 8192];
        match tokio::time::timeout(timeout_dur, self.stream.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => {
                buf.truncate(n);
                Some(buf)
            }
            _ => None,
        }
    }

    /// Reads and parses a CONNACK packet, returning `(flags, reason_code)`.
    ///
    /// Returns `None` if the response is not a valid CONNACK or the timeout
    /// elapses.
    pub async fn expect_connack(&mut self, timeout_dur: Duration) -> Option<(u8, u8)> {
        let data = self.read_packet_bytes(timeout_dur).await?;
        if data.len() < 4 || data[0] != 0x20 {
            return None;
        }
        let remaining_start = 1;
        let mut idx = remaining_start;
        let mut remaining_len: u32 = 0;
        let mut shift = 0;
        loop {
            if idx >= data.len() {
                return None;
            }
            let byte = data[idx];
            idx += 1;
            remaining_len |= u32::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 21 {
                return None;
            }
        }
        if data.len() < idx + remaining_len as usize || remaining_len < 2 {
            return None;
        }
        let flags = data[idx];
        let reason_code = data[idx + 1];
        Some((flags, reason_code))
    }

    /// Reads and parses a fully decoded [`ConnAckPacket`] from the broker.
    ///
    /// Returns `None` if the response is not a valid CONNACK or the timeout
    /// elapses. All properties are accessible on the returned packet.
    pub async fn expect_connack_packet(&mut self, timeout_dur: Duration) -> Option<ConnAckPacket> {
        let data = self.read_packet_bytes(timeout_dur).await?;
        if data.len() < 4 || data[0] != 0x20 {
            return None;
        }
        let mut buf = &data[..];
        let fixed_header = FixedHeader::decode(&mut buf).ok()?;
        ConnAckPacket::decode_body(&mut buf, &fixed_header).ok()
    }

    /// Sends a CONNECT, reads CONNACK, and returns `(flags, reason_code)`.
    ///
    /// Convenience wrapper combining [`send_raw`] with [`expect_connack`].
    /// Returns `None` if CONNACK is not received within the timeout.
    pub async fn connect_and_establish(
        &mut self,
        client_id: &str,
        timeout_dur: Duration,
    ) -> Option<(u8, u8)> {
        self.send_raw(&RawPacketBuilder::valid_connect(client_id))
            .await
            .ok()?;
        self.expect_connack(timeout_dur).await
    }

    /// Reads and parses a PUBACK packet, returning `(packet_id, reason_code)`.
    ///
    /// Returns `None` if the response is not a valid PUBACK or the timeout
    /// elapses. PUBACK fixed header byte is `0x40`.
    pub async fn expect_puback(&mut self, timeout_dur: Duration) -> Option<(u16, u8)> {
        let data = self.read_packet_bytes(timeout_dur).await?;
        if data.len() < 4 || data[0] != 0x40 {
            return None;
        }
        parse_ack_packet(&data)
    }

    /// Reads and parses a PUBREC packet, returning `(packet_id, reason_code)`.
    ///
    /// Returns `None` if the response is not a valid PUBREC or the timeout
    /// elapses. PUBREC fixed header byte is `0x50`.
    pub async fn expect_pubrec(&mut self, timeout_dur: Duration) -> Option<(u16, u8)> {
        let data = self.read_packet_bytes(timeout_dur).await?;
        if data.len() < 4 || data[0] != 0x50 {
            return None;
        }
        parse_ack_packet(&data)
    }

    /// Reads and parses a PUBREL packet, returning `(first_byte, packet_id, reason_code)`.
    ///
    /// Returns the raw first byte so callers can verify flags (valid PUBREL
    /// has first byte `0x62`, i.e. flags = `0x02`). Returns `None` if the
    /// timeout elapses or the packet is too short.
    pub async fn expect_pubrel_raw(&mut self, timeout_dur: Duration) -> Option<(u8, u16, u8)> {
        let data = self.read_packet_bytes(timeout_dur).await?;
        if data.len() < 4 || (data[0] & 0xF0) != 0x60 {
            return None;
        }
        let (packet_id, reason_code) = parse_ack_packet(&data)?;
        Some((data[0], packet_id, reason_code))
    }

    /// Reads and parses a PUBCOMP packet, returning `(packet_id, reason_code)`.
    ///
    /// Returns `None` if the response is not a valid PUBCOMP or the timeout
    /// elapses. PUBCOMP fixed header byte is `0x70`.
    pub async fn expect_pubcomp(&mut self, timeout_dur: Duration) -> Option<(u16, u8)> {
        let data = self.read_packet_bytes(timeout_dur).await?;
        if data.len() < 4 || data[0] != 0x70 {
            return None;
        }
        parse_ack_packet(&data)
    }

    /// Reads a `QoS` 2 PUBLISH packet from the server and returns its packet ID.
    ///
    /// Verifies the packet is a PUBLISH with `QoS`=2 (bits 2-1 of first byte == `0b10`).
    /// Skips past the topic string to extract the packet identifier.
    /// Returns `None` if the packet is not a `QoS` 2 PUBLISH or the timeout elapses.
    pub async fn expect_publish_qos2(&mut self, timeout_dur: Duration) -> Option<u16> {
        let data = self.read_packet_bytes(timeout_dur).await?;
        if data.is_empty() || (data[0] & 0xF0) != 0x30 {
            return None;
        }
        let qos = (data[0] >> 1) & 0x03;
        if qos != 2 {
            return None;
        }
        let mut idx = 1;
        let mut remaining_len: u32 = 0;
        let mut shift = 0;
        loop {
            if idx >= data.len() {
                return None;
            }
            let byte = data[idx];
            idx += 1;
            remaining_len |= u32::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 21 {
                return None;
            }
        }
        if data.len() < idx + remaining_len as usize {
            return None;
        }
        if idx + 2 > data.len() {
            return None;
        }
        let topic_len = u16::from_be_bytes([data[idx], data[idx + 1]]) as usize;
        idx += 2 + topic_len;
        if idx + 2 > data.len() {
            return None;
        }
        Some(u16::from_be_bytes([data[idx], data[idx + 1]]))
    }

    /// Reads and parses a SUBACK packet, returning `(packet_id, reason_codes)`.
    ///
    /// SUBACK fixed header byte is `0x90`. Parses: remaining length, packet ID
    /// (2 bytes), properties length (variable int, skipped), then collects all
    /// remaining bytes as reason codes.
    /// Returns `None` if the response is not a valid SUBACK or the timeout elapses.
    pub async fn expect_suback(&mut self, timeout_dur: Duration) -> Option<(u16, Vec<u8>)> {
        let data = self.read_packet_bytes(timeout_dur).await?;
        if data.len() < 4 || data[0] != 0x90 {
            return None;
        }
        let mut idx = 1;
        let mut remaining_len: u32 = 0;
        let mut shift = 0;
        loop {
            if idx >= data.len() {
                return None;
            }
            let byte = data[idx];
            idx += 1;
            remaining_len |= u32::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 21 {
                return None;
            }
        }
        let payload_start = idx;
        if data.len() < payload_start + remaining_len as usize || remaining_len < 3 {
            return None;
        }
        let packet_id = u16::from_be_bytes([data[idx], data[idx + 1]]);
        idx += 2;
        let mut props_len: u32 = 0;
        let mut props_shift = 0;
        loop {
            if idx >= data.len() {
                return None;
            }
            let byte = data[idx];
            idx += 1;
            props_len |= u32::from(byte & 0x7F) << props_shift;
            if byte & 0x80 == 0 {
                break;
            }
            props_shift += 7;
            if props_shift > 21 {
                return None;
            }
        }
        idx += props_len as usize;
        let end = payload_start + remaining_len as usize;
        if idx > end {
            return None;
        }
        let reason_codes = data[idx..end].to_vec();
        Some((packet_id, reason_codes))
    }

    /// Reads and parses an UNSUBACK packet, returning `(packet_id, reason_codes)`.
    ///
    /// UNSUBACK fixed header byte is `0xB0`. Same structure as SUBACK:
    /// remaining length, packet ID (2 bytes), properties length (variable int,
    /// skipped), then all remaining bytes are reason codes.
    /// Returns `None` if the response is not a valid UNSUBACK or the timeout elapses.
    pub async fn expect_unsuback(&mut self, timeout_dur: Duration) -> Option<(u16, Vec<u8>)> {
        let data = self.read_packet_bytes(timeout_dur).await?;
        if data.len() < 4 || data[0] != 0xB0 {
            return None;
        }
        let mut idx = 1;
        let mut remaining_len: u32 = 0;
        let mut shift = 0;
        loop {
            if idx >= data.len() {
                return None;
            }
            let byte = data[idx];
            idx += 1;
            remaining_len |= u32::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 21 {
                return None;
            }
        }
        let payload_start = idx;
        if data.len() < payload_start + remaining_len as usize || remaining_len < 3 {
            return None;
        }
        let packet_id = u16::from_be_bytes([data[idx], data[idx + 1]]);
        idx += 2;
        let mut props_len: u32 = 0;
        let mut props_shift = 0;
        loop {
            if idx >= data.len() {
                return None;
            }
            let byte = data[idx];
            idx += 1;
            props_len |= u32::from(byte & 0x7F) << props_shift;
            if byte & 0x80 == 0 {
                break;
            }
            props_shift += 7;
            if props_shift > 21 {
                return None;
            }
        }
        idx += props_len as usize;
        let end = payload_start + remaining_len as usize;
        if idx > end {
            return None;
        }
        let reason_codes = data[idx..end].to_vec();
        Some((packet_id, reason_codes))
    }

    /// Reads and parses a PUBLISH packet from the server, returning
    /// `(qos, topic, payload)`.
    ///
    /// Returns `None` if the packet is not a PUBLISH or the timeout elapses.
    pub async fn expect_publish(&mut self, timeout_dur: Duration) -> Option<(u8, String, Vec<u8>)> {
        let data = self.read_packet_bytes(timeout_dur).await?;
        if data.is_empty() || (data[0] & 0xF0) != 0x30 {
            return None;
        }
        let qos = (data[0] >> 1) & 0x03;
        let mut idx = 1;
        let mut remaining_len: u32 = 0;
        let mut shift = 0;
        loop {
            if idx >= data.len() {
                return None;
            }
            let byte = data[idx];
            idx += 1;
            remaining_len |= u32::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 21 {
                return None;
            }
        }
        let payload_start = idx;
        if data.len() < payload_start + remaining_len as usize {
            return None;
        }
        if idx + 2 > data.len() {
            return None;
        }
        let topic_len = u16::from_be_bytes([data[idx], data[idx + 1]]) as usize;
        idx += 2;
        if idx + topic_len > data.len() {
            return None;
        }
        let topic = String::from_utf8_lossy(&data[idx..idx + topic_len]).to_string();
        idx += topic_len;
        if qos > 0 {
            idx += 2;
        }
        let mut props_len: u32 = 0;
        let mut props_shift = 0;
        loop {
            if idx >= data.len() {
                return None;
            }
            let byte = data[idx];
            idx += 1;
            props_len |= u32::from(byte & 0x7F) << props_shift;
            if byte & 0x80 == 0 {
                break;
            }
            props_shift += 7;
            if props_shift > 21 {
                return None;
            }
        }
        idx += props_len as usize;
        let end = payload_start + remaining_len as usize;
        let payload = data[idx..end].to_vec();
        Some((qos, topic, payload))
    }

    /// Reads and parses a PUBLISH packet, returning `(first_byte, qos, topic, payload)`.
    ///
    /// Same as [`expect_publish`] but also returns the raw first byte so callers
    /// can inspect DUP (bit 3) and RETAIN (bit 0) flags.
    /// Returns `None` if the packet is not a PUBLISH or the timeout elapses.
    pub async fn expect_publish_raw_header(
        &mut self,
        timeout_dur: Duration,
    ) -> Option<(u8, u8, String, Vec<u8>)> {
        let data = self.read_packet_bytes(timeout_dur).await?;
        if data.is_empty() || (data[0] & 0xF0) != 0x30 {
            return None;
        }
        let first_byte = data[0];
        let qos = (first_byte >> 1) & 0x03;
        let mut idx = 1;
        let mut remaining_len: u32 = 0;
        let mut shift = 0;
        loop {
            if idx >= data.len() {
                return None;
            }
            let byte = data[idx];
            idx += 1;
            remaining_len |= u32::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 21 {
                return None;
            }
        }
        let payload_start = idx;
        if data.len() < payload_start + remaining_len as usize {
            return None;
        }
        if idx + 2 > data.len() {
            return None;
        }
        let topic_len = u16::from_be_bytes([data[idx], data[idx + 1]]) as usize;
        idx += 2;
        if idx + topic_len > data.len() {
            return None;
        }
        let topic = String::from_utf8_lossy(&data[idx..idx + topic_len]).to_string();
        idx += topic_len;
        if qos > 0 {
            idx += 2;
        }
        let mut props_len: u32 = 0;
        let mut props_shift = 0;
        loop {
            if idx >= data.len() {
                return None;
            }
            let byte = data[idx];
            idx += 1;
            props_len |= u32::from(byte & 0x7F) << props_shift;
            if byte & 0x80 == 0 {
                break;
            }
            props_shift += 7;
            if props_shift > 21 {
                return None;
            }
        }
        idx += props_len as usize;
        let end = payload_start + remaining_len as usize;
        let payload = data[idx..end].to_vec();
        Some((first_byte, qos, topic, payload))
    }

    /// Reads and checks whether the next packet is a PINGRESP (`0xD0 0x00`).
    ///
    /// Returns `true` if PINGRESP is received within the timeout, `false`
    /// otherwise (timeout, connection closed, or different packet type).
    pub async fn expect_pingresp(&mut self, timeout_dur: Duration) -> bool {
        matches!(
            self.read_packet_bytes(timeout_dur).await,
            Some(data) if data.len() >= 2 && data[0] == 0xD0 && data[1] == 0x00
        )
    }

    /// Reads and parses a DISCONNECT packet from the server, returning the
    /// reason code byte.
    ///
    /// Returns `None` if the response is not a DISCONNECT, the timeout elapses,
    /// or the connection is closed without sending DISCONNECT.
    pub async fn expect_disconnect_packet(&mut self, timeout_dur: Duration) -> Option<u8> {
        let data = self.read_packet_bytes(timeout_dur).await?;
        if data.is_empty() || data[0] != 0xE0 {
            return None;
        }
        let mut idx = 1;
        let mut remaining_len: u32 = 0;
        let mut shift = 0;
        loop {
            if idx >= data.len() {
                return None;
            }
            let byte = data[idx];
            idx += 1;
            remaining_len |= u32::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 21 {
                return None;
            }
        }
        if remaining_len == 0 {
            return Some(0x00);
        }
        if data.len() < idx + remaining_len as usize {
            return None;
        }
        Some(data[idx])
    }

    /// Waits for the broker to close the connection or send a DISCONNECT packet.
    ///
    /// Returns `true` if the broker either closed the TCP connection (read
    /// returns 0 bytes), sent a DISCONNECT packet (first byte `0xE0`), or
    /// the connection errored. Returns `false` if the timeout elapses with
    /// the connection still open.
    pub async fn expect_disconnect(&mut self, timeout_dur: Duration) -> bool {
        let mut buf = [0u8; 4096];
        match tokio::time::timeout(timeout_dur, self.stream.read(&mut buf)).await {
            Ok(Ok(0) | Err(_)) => true,
            Ok(Ok(n)) => {
                if buf[..n].first() == Some(&0xE0) {
                    return true;
                }
                let mut second_buf = [0u8; 1];
                match tokio::time::timeout(timeout_dur, self.stream.read(&mut second_buf)).await {
                    Ok(Ok(0) | Err(_)) => true,
                    Ok(Ok(_)) | Err(_) => false,
                }
            }
            Err(_) => false,
        }
    }
}

/// Builds hand-crafted MQTT v5.0 packet byte sequences.
///
/// Each method returns a `Vec<u8>` containing a complete MQTT packet
/// (fixed header + body). Methods that produce invalid packets document
/// which spec rule they are designed to violate.
pub struct RawPacketBuilder;

impl RawPacketBuilder {
    /// Builds a well-formed MQTT v5.0 CONNECT packet with `clean_start=true`,
    /// keepalive=60s, and no properties.
    #[must_use]
    pub fn valid_connect(client_id: &str) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x02);
        body.put_u16(60);
        body.put_u8(0);
        put_mqtt_string(&mut body, client_id);

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a CONNECT packet with an arbitrary protocol version byte.
    ///
    /// Used to test `[MQTT-3.1.2-2]`: server MUST respond with CONNACK 0x84
    /// for unsupported protocol versions.
    #[must_use]
    pub fn connect_with_protocol_version(version: u8) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(version);
        body.put_u8(0x02);
        body.put_u16(60);
        body.put_u8(0);
        put_mqtt_string(&mut body, "test-version-client");

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a CONNECT packet with protocol name "XXXX" instead of "MQTT".
    ///
    /// Used to test `[MQTT-3.1.2-1]`: server MUST close the connection if
    /// the protocol name is not "MQTT".
    #[must_use]
    pub fn connect_with_invalid_protocol_name() -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"XXXX");
        body.put_u8(5);
        body.put_u8(0x02);
        body.put_u16(60);
        body.put_u8(0);
        put_mqtt_string(&mut body, "test-proto-client");

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a CONNECT packet with the reserved flag (bit 0) set to 1.
    ///
    /// Connect flags byte `0x03` = `clean_start` (bit 1) + reserved (bit 0).
    /// Used to test `[MQTT-3.1.2-3]`: reserved flag MUST be 0.
    #[must_use]
    pub fn connect_with_reserved_flag_set() -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x03);
        body.put_u16(60);
        body.put_u8(0);
        put_mqtt_string(&mut body, "test-reserved-client");

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a CONNECT packet with Will Flag set but no Will Topic or
    /// Will Payload in the payload.
    ///
    /// Connect flags `0x06` = Will Flag (bit 2) + `clean_start` (bit 1).
    /// Used to test `[MQTT-3.1.2-6]`: Will Topic and Will Payload MUST be
    /// present when Will Flag is 1.
    #[must_use]
    pub fn connect_with_will_flag_no_payload() -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x06);
        body.put_u16(60);
        body.put_u8(0);
        put_mqtt_string(&mut body, "test-will-client");

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a `QoS` 0 PUBLISH packet (no prior CONNECT).
    ///
    /// Used to test `[MQTT-3.1.0-1]`: the first packet from the client
    /// MUST be a CONNECT.
    #[must_use]
    pub fn publish_without_connect() -> Vec<u8> {
        let mut body = BytesMut::new();
        put_mqtt_string(&mut body, "test/topic");
        body.put_u8(0);
        body.put_slice(b"hello");

        wrap_fixed_header(0x30, &body)
    }

    /// Builds a truncated CONNECT packet: fixed header claims 50 bytes
    /// remaining but only 3 bytes of body follow.
    ///
    /// Used to test `[MQTT-3.1.4-1]`: server MUST close the connection
    /// on malformed packets.
    #[must_use]
    pub fn connect_malformed_truncated() -> Vec<u8> {
        vec![0x10, 50, 0x00, 0x04, 0x4D]
    }

    /// Builds a CONNECT packet with a duplicated Session Expiry Interval
    /// property (property ID `0x11` appears twice).
    ///
    /// Used to test `[MQTT-3.1.4-2]`: duplicate non-repeatable properties
    /// are a protocol error.
    #[must_use]
    pub fn connect_with_duplicate_property() -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x02);
        body.put_u16(60);

        let mut props = BytesMut::new();
        props.put_u8(0x11);
        props.put_u32(300);
        props.put_u8(0x11);
        props.put_u32(600);

        encode_variable_int(&mut body, props.len() as u32);
        body.put(props);
        put_mqtt_string(&mut body, "test-dup-prop-client");

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a CONNECT packet with fixed header flags byte `0x11` instead
    /// of the required `0x10`.
    ///
    /// The lower 4 bits of a CONNECT fixed header MUST be `0x00`.
    /// Used to test `[MQTT-3.1.2-12]`.
    #[must_use]
    pub fn connect_with_invalid_fixed_header_flags() -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x02);
        body.put_u16(60);
        body.put_u8(0);
        put_mqtt_string(&mut body, "test-flags-client");

        wrap_fixed_header(0x11, &body)
    }

    /// Builds a `QoS` 0 PUBLISH with the given topic and payload.
    ///
    /// Fixed header byte `0x30` (PUBLISH, DUP=0, QoS=0, RETAIN=0).
    #[must_use]
    pub fn publish_qos0(topic: &str, payload: &[u8]) -> Vec<u8> {
        let mut body = BytesMut::new();
        put_mqtt_string(&mut body, topic);
        body.put_u8(0);
        body.put_slice(payload);
        wrap_fixed_header(0x30, &body)
    }

    /// Builds a `QoS` 1 PUBLISH with the given topic, payload, and packet ID.
    ///
    /// Fixed header byte `0x32` (PUBLISH, DUP=0, QoS=1, RETAIN=0).
    #[must_use]
    pub fn publish_qos1(topic: &str, payload: &[u8], packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        put_mqtt_string(&mut body, topic);
        body.put_u16(packet_id);
        body.put_u8(0);
        body.put_slice(payload);
        wrap_fixed_header(0x32, &body)
    }

    /// Builds a malformed PUBLISH with both `QoS` bits set (QoS=3).
    ///
    /// Fixed header byte `0x36` (PUBLISH, DUP=0, QoS=3, RETAIN=0).
    /// Violates `[MQTT-3.3.1-4]`: `QoS` MUST NOT be 3.
    #[must_use]
    pub fn publish_qos3_malformed(topic: &str, payload: &[u8]) -> Vec<u8> {
        let mut body = BytesMut::new();
        put_mqtt_string(&mut body, topic);
        body.put_u8(0);
        body.put_slice(payload);
        wrap_fixed_header(0x36, &body)
    }

    /// Builds a PUBLISH with DUP=1 and `QoS`=0.
    ///
    /// Fixed header byte `0x38` (PUBLISH, DUP=1, QoS=0, RETAIN=0).
    /// Violates `[MQTT-3.3.1-2]`: DUP MUST be 0 when `QoS` is 0.
    #[must_use]
    pub fn publish_dup_qos0(topic: &str, payload: &[u8]) -> Vec<u8> {
        let mut body = BytesMut::new();
        put_mqtt_string(&mut body, topic);
        body.put_u8(0);
        body.put_slice(payload);
        wrap_fixed_header(0x38, &body)
    }

    /// Builds a PUBLISH with a wildcard character (`#`) in the topic name.
    ///
    /// Violates `[MQTT-3.3.2-2]`: topic name MUST NOT contain wildcards.
    #[must_use]
    pub fn publish_with_wildcard_topic() -> Vec<u8> {
        let mut body = BytesMut::new();
        put_mqtt_string(&mut body, "test/#");
        body.put_u8(0);
        body.put_slice(b"payload");
        wrap_fixed_header(0x30, &body)
    }

    /// Builds a PUBLISH with an empty (zero-length) topic and no Topic Alias.
    ///
    /// Violates `[MQTT-3.3.2-1]`: topic name MUST be present if no Topic Alias.
    #[must_use]
    pub fn publish_with_empty_topic() -> Vec<u8> {
        let mut body = BytesMut::new();
        put_mqtt_string(&mut body, "");
        body.put_u8(0);
        body.put_slice(b"payload");
        wrap_fixed_header(0x30, &body)
    }

    /// Builds a PUBLISH with Topic Alias property set to 0.
    ///
    /// Violates `[MQTT-3.3.2-8]`: Topic Alias MUST NOT be 0.
    #[must_use]
    pub fn publish_with_topic_alias_zero(topic: &str, payload: &[u8]) -> Vec<u8> {
        let mut body = BytesMut::new();
        put_mqtt_string(&mut body, topic);
        let mut props = BytesMut::new();
        props.put_u8(0x23);
        props.put_u16(0);
        encode_variable_int(&mut body, props.len() as u32);
        body.put(props);
        body.put_slice(payload);
        wrap_fixed_header(0x30, &body)
    }

    /// Builds a PUBLISH with a Subscription Identifier property.
    ///
    /// Violates `[MQTT-3.3.4-6]`: Subscription Identifier MUST NOT be
    /// included in a PUBLISH from client to server.
    #[must_use]
    pub fn publish_with_subscription_id(topic: &str, payload: &[u8], sub_id: u32) -> Vec<u8> {
        let mut body = BytesMut::new();
        put_mqtt_string(&mut body, topic);
        let mut props = BytesMut::new();
        props.put_u8(0x0B);
        encode_variable_int(&mut props, sub_id);
        encode_variable_int(&mut body, props.len() as u32);
        body.put(props);
        body.put_slice(payload);
        wrap_fixed_header(0x30, &body)
    }

    /// Builds a CONNECT packet with Will Flag=1 and the specified Will `QoS`.
    ///
    /// Connect flags: Will Flag (bit 2) + `clean_start` (bit 1) + Will `QoS`
    /// shifted into bits 4-3. Includes Will Properties (empty), Will Topic,
    /// and Will Payload in the payload.
    #[must_use]
    pub fn connect_with_will_qos(client_id: &str, will_qos: u8) -> Vec<u8> {
        let flags = 0x06 | ((will_qos & 0x03) << 3);
        build_connect_with_will(client_id, flags)
    }

    /// Builds a CONNECT packet with Will Flag=1 and Will Retain=1.
    ///
    /// Connect flags: Will Retain (bit 5) + Will Flag (bit 2) +
    /// `clean_start` (bit 1). Includes Will Properties (empty), Will Topic,
    /// and Will Payload in the payload.
    #[must_use]
    pub fn connect_with_will_retain(client_id: &str) -> Vec<u8> {
        let flags = 0x26;
        build_connect_with_will(client_id, flags)
    }

    /// Builds a SUBSCRIBE packet for the given topic filter and `QoS`.
    ///
    /// Fixed header byte `0x82` (SUBSCRIBE, reserved bits = 0010).
    /// Uses packet ID 1.
    #[must_use]
    pub fn subscribe(topic: &str, qos: u8) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(1);
        body.put_u8(0);
        put_mqtt_string(&mut body, topic);
        body.put_u8(qos & 0x03);
        wrap_fixed_header(0x82, &body)
    }

    /// Builds a SUBSCRIBE packet with a configurable packet ID.
    ///
    /// Fixed header byte `0x82` (SUBSCRIBE, reserved bits = 0010).
    #[must_use]
    pub fn subscribe_with_packet_id(topic: &str, qos: u8, packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        body.put_u8(0);
        put_mqtt_string(&mut body, topic);
        body.put_u8(qos & 0x03);
        wrap_fixed_header(0x82, &body)
    }

    /// Builds a SUBSCRIBE packet with multiple topic filters.
    ///
    /// Each entry in `filters` is `(topic_filter, qos_byte)`.
    /// Fixed header byte `0x82`.
    #[must_use]
    pub fn subscribe_multiple(filters: &[(&str, u8)], packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        body.put_u8(0);
        for (topic, qos) in filters {
            put_mqtt_string(&mut body, topic);
            body.put_u8(*qos & 0x03);
        }
        wrap_fixed_header(0x82, &body)
    }

    /// Builds a SUBSCRIBE packet with invalid fixed header flags.
    ///
    /// Fixed header byte `0x80` (flags = `0x00` instead of required `0x02`).
    /// Violates `[MQTT-3.8.1-1]`.
    #[must_use]
    pub fn subscribe_invalid_flags(topic: &str, qos: u8) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(1);
        body.put_u8(0);
        put_mqtt_string(&mut body, topic);
        body.put_u8(qos & 0x03);
        wrap_fixed_header(0x80, &body)
    }

    /// Builds a SUBSCRIBE packet with no topic filters (empty payload after
    /// packet ID and properties).
    ///
    /// Violates `[MQTT-3.8.3-3]`.
    #[must_use]
    pub fn subscribe_empty_payload(packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        body.put_u8(0);
        wrap_fixed_header(0x82, &body)
    }

    /// Builds a SUBSCRIBE for a shared subscription with `NoLocal=1`.
    ///
    /// The options byte encodes: `QoS` in bits 0-1, `NoLocal` in bit 2.
    /// Topic filter is `$share/{group}/{topic}`.
    /// Violates `[MQTT-3.8.3-4]`.
    #[must_use]
    pub fn subscribe_shared_no_local(group: &str, topic: &str, qos: u8, packet_id: u16) -> Vec<u8> {
        let filter = format!("$share/{group}/{topic}");
        let options_byte = (qos & 0x03) | 0x04;
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        body.put_u8(0);
        put_mqtt_string(&mut body, &filter);
        body.put_u8(options_byte);
        wrap_fixed_header(0x82, &body)
    }

    /// Builds an UNSUBSCRIBE packet for a single topic filter.
    ///
    /// Fixed header byte `0xA2` (UNSUBSCRIBE, reserved bits = 0010).
    #[must_use]
    pub fn unsubscribe(topic: &str, packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        body.put_u8(0);
        put_mqtt_string(&mut body, topic);
        wrap_fixed_header(0xA2, &body)
    }

    /// Builds an UNSUBSCRIBE packet with multiple topic filters.
    ///
    /// Fixed header byte `0xA2`.
    #[must_use]
    pub fn unsubscribe_multiple(filters: &[&str], packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        body.put_u8(0);
        for filter in filters {
            put_mqtt_string(&mut body, filter);
        }
        wrap_fixed_header(0xA2, &body)
    }

    /// Builds an UNSUBSCRIBE packet with invalid fixed header flags.
    ///
    /// Fixed header byte `0xA0` (flags = `0x00` instead of required `0x02`).
    /// Violates `[MQTT-3.10.1-1]`.
    #[must_use]
    pub fn unsubscribe_invalid_flags(topic: &str, packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        body.put_u8(0);
        put_mqtt_string(&mut body, topic);
        wrap_fixed_header(0xA0, &body)
    }

    /// Builds an UNSUBSCRIBE packet with no topic filters (empty payload
    /// after packet ID and properties).
    ///
    /// Violates `[MQTT-3.10.3-2]`.
    #[must_use]
    pub fn unsubscribe_empty_payload(packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        body.put_u8(0);
        wrap_fixed_header(0xA2, &body)
    }

    /// Builds a `QoS` 2 PUBLISH with the given topic, payload, and packet ID.
    ///
    /// Fixed header byte `0x34` (PUBLISH, DUP=0, QoS=2, RETAIN=0).
    #[must_use]
    pub fn publish_qos2(topic: &str, payload: &[u8], packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        put_mqtt_string(&mut body, topic);
        body.put_u16(packet_id);
        body.put_u8(0);
        body.put_slice(payload);
        wrap_fixed_header(0x34, &body)
    }

    /// Builds a PUBREC packet with the given packet ID and implicit Success reason.
    ///
    /// Fixed header byte `0x50`.
    #[must_use]
    pub fn pubrec(packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        wrap_fixed_header(0x50, &body)
    }

    /// Builds a PUBREL packet with the correct fixed header flags (`0x02`).
    ///
    /// Fixed header byte `0x62` (PUBREL with reserved flags = 0010).
    #[must_use]
    pub fn pubrel(packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        wrap_fixed_header(0x62, &body)
    }

    /// Builds a malformed PUBREL packet with flags = `0x00` instead of `0x02`.
    ///
    /// Fixed header byte `0x60` (invalid — flags MUST be `0x02`).
    /// Violates `[MQTT-3.6.1-1]`.
    #[must_use]
    pub fn pubrel_invalid_flags(packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        wrap_fixed_header(0x60, &body)
    }

    /// Builds a PUBCOMP packet with the given packet ID and implicit Success reason.
    ///
    /// Fixed header byte `0x70`.
    #[must_use]
    pub fn pubcomp(packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        wrap_fixed_header(0x70, &body)
    }

    /// Builds a PINGREQ packet (`0xC0 0x00`).
    #[must_use]
    pub fn pingreq() -> Vec<u8> {
        vec![0xC0, 0x00]
    }

    /// Builds a CONNECT packet with a configurable keep-alive value.
    ///
    /// Same as [`valid_connect`](Self::valid_connect) but uses `keepalive_secs`
    /// instead of the default 60s.
    #[must_use]
    pub fn connect_with_keepalive(client_id: &str, keepalive_secs: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x02);
        body.put_u16(keepalive_secs);
        body.put_u8(0);
        put_mqtt_string(&mut body, client_id);

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a CONNECT packet with Password Flag set but Username Flag clear.
    ///
    /// Connect flags `0x42` = Password (bit 6) + `clean_start` (bit 1).
    /// MQTT v5.0 allows password without username, so this is a valid packet.
    #[must_use]
    pub fn connect_with_password_no_username() -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x42);
        body.put_u16(60);
        body.put_u8(0);
        put_mqtt_string(&mut body, "test-pw-client");
        put_mqtt_string(&mut body, "secret");

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a `QoS` 0 PUBLISH with a Topic Alias property that registers the alias.
    ///
    /// The topic field is non-empty and the Topic Alias property (`0x23`) maps
    /// `alias` to `topic`. Fixed header byte `0x30`.
    #[must_use]
    pub fn publish_qos0_with_topic_alias(topic: &str, payload: &[u8], alias: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        put_mqtt_string(&mut body, topic);
        let mut props = BytesMut::new();
        props.put_u8(0x23);
        props.put_u16(alias);
        encode_variable_int(&mut body, props.len() as u32);
        body.put(props);
        body.put_slice(payload);
        wrap_fixed_header(0x30, &body)
    }

    /// Builds a `QoS` 0 PUBLISH that reuses a previously registered Topic Alias.
    ///
    /// The topic field is empty (zero-length string) and the Topic Alias property
    /// (`0x23`) references `alias`. Fixed header byte `0x30`.
    #[must_use]
    pub fn publish_qos0_alias_only(payload: &[u8], alias: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        put_mqtt_string(&mut body, "");
        let mut props = BytesMut::new();
        props.put_u8(0x23);
        props.put_u16(alias);
        encode_variable_int(&mut body, props.len() as u32);
        body.put(props);
        body.put_slice(payload);
        wrap_fixed_header(0x30, &body)
    }

    /// Builds a `QoS` 1 PUBLISH with DUP=1 flag set.
    ///
    /// Fixed header byte `0x3A` (PUBLISH, DUP=1, QoS=1, RETAIN=0).
    #[must_use]
    pub fn publish_qos1_with_dup(topic: &str, payload: &[u8], packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        put_mqtt_string(&mut body, topic);
        body.put_u16(packet_id);
        body.put_u8(0);
        body.put_slice(payload);
        wrap_fixed_header(0x3A, &body)
    }

    /// Builds a normal DISCONNECT packet with no reason code (implies 0x00).
    ///
    /// Fixed header `0xE0`, remaining length `0x00`.
    #[must_use]
    pub fn disconnect_normal() -> Vec<u8> {
        vec![0xE0, 0x00]
    }

    /// Builds a DISCONNECT packet with an explicit reason code byte.
    ///
    /// Fixed header `0xE0`, remaining length 2, reason byte, no properties.
    #[must_use]
    pub fn disconnect_with_reason(reason_code: u8) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u8(reason_code);
        body.put_u8(0);
        wrap_fixed_header(0xE0, &body)
    }

    /// Builds a CONNECT packet with Will Flag=1 (`QoS` 0) and a configurable keepalive.
    ///
    /// Will topic is `"will/{client_id}"`, will payload is `"offline"`.
    /// Connect flags: Will Flag (bit 2) + `clean_start` (bit 1) = `0x06`.
    #[must_use]
    pub fn connect_with_will_and_keepalive(client_id: &str, keepalive_secs: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x06);
        body.put_u16(keepalive_secs);
        body.put_u8(0);
        put_mqtt_string(&mut body, client_id);
        body.put_u8(0);
        let will_topic = format!("will/{client_id}");
        put_mqtt_string(&mut body, &will_topic);
        put_mqtt_string(&mut body, "offline");
        wrap_fixed_header(0x10, &body)
    }
}

fn build_connect_with_will(client_id: &str, connect_flags: u8) -> Vec<u8> {
    let mut body = BytesMut::new();
    body.put_u16(4);
    body.put_slice(b"MQTT");
    body.put_u8(5);
    body.put_u8(connect_flags);
    body.put_u16(60);
    body.put_u8(0);
    put_mqtt_string(&mut body, client_id);
    body.put_u8(0);
    put_mqtt_string(&mut body, "will/topic");
    put_mqtt_string(&mut body, "will-payload");
    wrap_fixed_header(0x10, &body)
}

fn parse_ack_packet(data: &[u8]) -> Option<(u16, u8)> {
    let mut idx = 1;
    let mut remaining_len: u32 = 0;
    let mut shift = 0;
    loop {
        if idx >= data.len() {
            return None;
        }
        let byte = data[idx];
        idx += 1;
        remaining_len |= u32::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 21 {
            return None;
        }
    }
    if data.len() < idx + remaining_len as usize || remaining_len < 2 {
        return None;
    }
    let packet_id = u16::from_be_bytes([data[idx], data[idx + 1]]);
    let reason_code = if remaining_len >= 3 {
        data[idx + 2]
    } else {
        0x00
    };
    Some((packet_id, reason_code))
}

fn put_mqtt_string(buf: &mut BytesMut, s: &str) {
    let bytes = s.as_bytes();
    buf.put_u16(bytes.len() as u16);
    buf.put_slice(bytes);
}

fn encode_variable_int(buf: &mut BytesMut, mut value: u32) {
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value > 0 {
            byte |= 0x80;
        }
        buf.put_u8(byte);
        if value == 0 {
            break;
        }
    }
}

fn wrap_fixed_header(first_byte: u8, body: &[u8]) -> Vec<u8> {
    let mut packet = BytesMut::new();
    packet.put_u8(first_byte);
    encode_variable_int(&mut packet, body.len() as u32);
    packet.put_slice(body);
    packet.to_vec()
}
