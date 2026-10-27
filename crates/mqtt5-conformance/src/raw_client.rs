//! Raw transport-level client for sending hand-crafted MQTT packets.
//!
//! The normal [`MqttClient`](mqtt5::MqttClient) API enforces well-formed packets,
//! making it impossible to test how the broker handles malformed input.
//! [`RawMqttClient`] operates at the byte level over any [`Transport`], and
//! [`RawPacketBuilder`] constructs both valid and deliberately invalid MQTT
//! v5.0 packets for conformance edge-case testing.

use crate::packet_parser::decode_variable_int;
use crate::transport::{TcpTransport, Transport};
use bytes::{BufMut, BytesMut};
use mqtt5_protocol::packet::connack::ConnAckPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::FixedHeader;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use ulid::Ulid;

pub(crate) fn parse_ack_reason_codes(data: &[u8], packet_type: u8) -> Option<(u16, Vec<u8>)> {
    if data.first() != Some(&packet_type) {
        return None;
    }
    let (remaining_len, body_start) = decode_variable_int(data, 1)?;
    let end = body_start + remaining_len as usize;
    if remaining_len < 3 || data.len() < end {
        return None;
    }
    let packet_id = u16::from_be_bytes([data[body_start], data[body_start + 1]]);
    let (props_len, props_start) = decode_variable_int(data, body_start + 2)?;
    let codes_start = props_start + props_len as usize;
    if codes_start > end {
        return None;
    }
    Some((packet_id, data[codes_start..end].to_vec()))
}

/// Generates a unique client ID with the given prefix using a ULID suffix.
#[must_use]
pub fn unique_client_id(prefix: &str) -> String {
    format!("conf-{prefix}-{}", Ulid::new())
}

/// A raw transport-level client for sending arbitrary bytes to an MQTT broker.
///
/// Unlike [`MqttClient`](mqtt5::MqttClient), this client performs no packet
/// validation or encoding — it writes raw bytes directly to the underlying
/// transport. Use it for testing broker behavior with malformed, truncated, or
/// protocol-violating packets.
///
/// The default transport is [`TcpTransport`]; future phases add TLS and
/// WebSocket variants so the same test code can drive a broker over every
/// protocol it speaks.
pub struct RawMqttClient<T: Transport = TcpTransport> {
    stream: T,
    buf: BytesMut,
}

impl RawMqttClient<TcpTransport> {
    /// # Errors
    ///
    /// Returns the I/O error if the TCP connection cannot be opened.
    pub async fn connect_tcp(addr: SocketAddr) -> std::io::Result<Self> {
        let stream = TcpTransport::connect(addr).await?;
        Ok(Self {
            stream,
            buf: BytesMut::new(),
        })
    }
}

impl<T: Transport> RawMqttClient<T> {
    pub fn from_transport(stream: T) -> Self {
        Self {
            stream,
            buf: BytesMut::new(),
        }
    }

    pub fn into_transport(self) -> T {
        self.stream
    }

    /// Sends raw bytes over the transport.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the write fails.
    pub async fn send_raw(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.stream.write_all(data).await
    }

    pub async fn read_packet_bytes(&mut self, timeout_dur: Duration) -> Option<Vec<u8>> {
        match self
            .read_until(tokio::time::Instant::now() + timeout_dur)
            .await
        {
            ReadOutcome::Packet(pkt) => Some(pkt),
            ReadOutcome::Closed | ReadOutcome::TimedOut => None,
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
        let (remaining_len, idx) = decode_variable_int(&data, 1)?;
        if data.len() < idx + remaining_len as usize || remaining_len < 2 {
            return None;
        }
        Some((data[idx], data[idx + 1]))
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
    /// Convenience wrapper combining [`Self::send_raw`] with [`Self::expect_connack`].
    ///
    /// # Panics
    ///
    /// Panics if the CONNECT cannot be sent or CONNACK is not received.
    pub async fn connect_and_establish(
        &mut self,
        client_id: &str,
        timeout_dur: Duration,
    ) -> (u8, u8) {
        self.send_raw(&RawPacketBuilder::valid_connect(client_id))
            .await
            .expect("failed to send CONNECT");
        self.expect_connack(timeout_dur)
            .await
            .expect("CONNACK not received")
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
        let (first_byte, packet_id, _, _) = parse_publish(&data)?;
        ((first_byte >> 1) & 0x03 == 2).then_some(packet_id)
    }

    /// Reads one packet and parses it as a SUBACK, returning
    /// `(packet_id, reason_codes)`.
    ///
    /// Returns `None` if the packet is not a valid SUBACK or the timeout elapses.
    pub async fn expect_suback(&mut self, timeout_dur: Duration) -> Option<(u16, Vec<u8>)> {
        let data = self.read_packet_bytes(timeout_dur).await?;
        parse_ack_reason_codes(&data, 0x90)
    }

    /// Reads one packet and parses it as an UNSUBACK, returning
    /// `(packet_id, reason_codes)`.
    ///
    /// Returns `None` if the packet is not a valid UNSUBACK or the timeout elapses.
    pub async fn expect_unsuback(&mut self, timeout_dur: Duration) -> Option<(u16, Vec<u8>)> {
        let data = self.read_packet_bytes(timeout_dur).await?;
        parse_ack_reason_codes(&data, 0xB0)
    }

    /// Reads and parses a PUBLISH packet from the server, returning
    /// `(qos, topic, payload)`.
    ///
    /// Returns `None` if the packet is not a PUBLISH or the timeout elapses.
    pub async fn expect_publish(&mut self, timeout_dur: Duration) -> Option<(u8, String, Vec<u8>)> {
        let data = self.read_packet_bytes(timeout_dur).await?;
        let (first_byte, _, topic, payload) = parse_publish(&data)?;
        Some(((first_byte >> 1) & 0x03, topic, payload))
    }

    /// Reads and parses a PUBLISH packet, returning `(first_byte, qos, topic, payload)`.
    ///
    /// Same as [`Self::expect_publish`] but also returns the raw first byte so callers
    /// can inspect DUP (bit 3) and RETAIN (bit 0) flags.
    /// Returns `None` if the packet is not a PUBLISH or the timeout elapses.
    pub async fn expect_publish_raw_header(
        &mut self,
        timeout_dur: Duration,
    ) -> Option<(u8, u8, String, Vec<u8>)> {
        let data = self.read_packet_bytes(timeout_dur).await?;
        let (first_byte, _, topic, payload) = parse_publish(&data)?;
        Some((first_byte, (first_byte >> 1) & 0x03, topic, payload))
    }

    /// Reads and parses a PUBLISH packet, returning `(first_byte, packet_id, qos, topic, payload)`.
    ///
    /// For `QoS` 0 packets, `packet_id` is 0. Returns the raw first byte so callers
    /// can inspect DUP (bit 3) and RETAIN (bit 0) flags, plus the packet identifier
    /// for `QoS` 1/2 acknowledgement flows.
    /// Returns `None` if the packet is not a PUBLISH or the timeout elapses.
    pub async fn expect_publish_with_id(
        &mut self,
        timeout_dur: Duration,
    ) -> Option<(u8, u16, u8, String, Vec<u8>)> {
        let data = self.read_packet_bytes(timeout_dur).await?;
        let (first_byte, packet_id, topic, payload) = parse_publish(&data)?;
        Some((
            first_byte,
            packet_id,
            (first_byte >> 1) & 0x03,
            topic,
            payload,
        ))
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
        if data.first() != Some(&0xE0) {
            return None;
        }
        let (remaining_len, idx) = decode_variable_int(&data, 1)?;
        if remaining_len == 0 {
            return Some(0x00);
        }
        if data.len() < idx + remaining_len as usize {
            return None;
        }
        Some(data[idx])
    }

    /// Reads raw bytes and returns them if the first byte is `0xE0` (DISCONNECT).
    pub async fn expect_disconnect_raw(&mut self, timeout_dur: Duration) -> Option<Vec<u8>> {
        let data = self.read_packet_bytes(timeout_dur).await?;
        if data.is_empty() || data[0] != 0xE0 {
            return None;
        }
        Some(data)
    }

    /// Reads an AUTH packet and returns `(reason_code, auth_method)`.
    pub async fn expect_auth_packet(
        &mut self,
        timeout_dur: Duration,
    ) -> Option<(u8, Option<String>)> {
        let data = self.read_packet_bytes(timeout_dur).await?;
        if data.first() != Some(&0xF0) {
            return None;
        }
        let (remaining_len, idx) = decode_variable_int(&data, 1)?;
        if remaining_len == 0 {
            return Some((0x00, None));
        }
        if data.len() < idx + remaining_len as usize {
            return None;
        }
        Some((data[idx], extract_auth_method_property(&data, idx + 1)))
    }

    pub async fn expect_disconnect(&mut self, timeout_dur: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout_dur;
        loop {
            match self.read_until(deadline).await {
                ReadOutcome::Packet(_) => {}
                ReadOutcome::Closed => return true,
                ReadOutcome::TimedOut => return false,
            }
        }
    }

    fn try_extract_packet(buf: &mut BytesMut) -> Option<Vec<u8>> {
        if buf.is_empty() {
            return None;
        }
        match decode_variable_int(buf, 1) {
            Some((remaining_len, body_start)) => {
                let total = body_start + remaining_len as usize;
                (buf.len() >= total).then(|| buf.split_to(total).to_vec())
            }
            None if buf.len() >= 5 => Some(buf.split().to_vec()),
            None => None,
        }
    }

    async fn read_until(&mut self, deadline: tokio::time::Instant) -> ReadOutcome {
        loop {
            if let Some(pkt) = Self::try_extract_packet(&mut self.buf) {
                return ReadOutcome::Packet(pkt);
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return ReadOutcome::TimedOut;
            }
            let mut tmp = [0u8; 8192];
            match tokio::time::timeout(remaining, self.stream.read(&mut tmp)).await {
                Ok(Ok(0) | Err(_)) if self.buf.is_empty() => return ReadOutcome::Closed,
                Ok(Ok(0) | Err(_)) => return ReadOutcome::Packet(self.buf.split().to_vec()),
                Ok(Ok(n)) => self.buf.extend_from_slice(&tmp[..n]),
                Err(_) => return ReadOutcome::TimedOut,
            }
        }
    }
}

enum ReadOutcome {
    Packet(Vec<u8>),
    Closed,
    TimedOut,
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
        put_mqtt_string(&mut body, &unique_client_id("test-version-client"));

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
        put_mqtt_string(&mut body, &unique_client_id("test-proto-client"));

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
        put_mqtt_string(&mut body, &unique_client_id("test-reserved-client"));

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a CONNECT packet with Will Flag set but no Will Topic or
    /// Will Payload in the payload.
    ///
    /// Connect flags `0x06` = Will Flag (bit 2) + `clean_start` (bit 1).
    /// Used to test `[MQTT-3.1.2-9]`: Will Topic and Will Payload MUST be
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
        put_mqtt_string(&mut body, &unique_client_id("test-will-client"));

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

    /// Builds a complete CONNECT packet whose properties length (10) runs
    /// past the end of the packet, which holds only a 3-byte payload.
    #[must_use]
    pub fn connect_with_overlong_properties() -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x02);
        body.put_u16(60);
        body.put_u8(10);
        put_mqtt_string(&mut body, "a");
        wrap_fixed_header(0x10, &body)
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

        encode_variable_int(&mut body, props.len());
        body.put(props);
        put_mqtt_string(&mut body, &unique_client_id("test-dup-prop-client"));

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a CONNECT packet with fixed header flags byte `0x11` instead
    /// of the required `0x10`.
    ///
    /// The lower 4 bits of a CONNECT fixed header MUST be `0x00`.
    /// Used to test `[MQTT-2.1.3-1]`.
    #[must_use]
    pub fn connect_with_invalid_fixed_header_flags() -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x02);
        body.put_u16(60);
        body.put_u8(0);
        put_mqtt_string(&mut body, &unique_client_id("test-flags-client"));

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
        encode_variable_int(&mut body, props.len());
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
        encode_variable_int(&mut props, variable_int_value(sub_id));
        encode_variable_int(&mut body, props.len());
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

    /// Builds a SUBSCRIBE packet with a Subscription Identifier property.
    ///
    /// Property ID `0x0B` encodes the subscription identifier as a variable-length
    /// integer. Used for overlapping subscription tests `[MQTT-3.3.4-3]`/`[MQTT-3.3.4-5]`.
    #[must_use]
    pub fn subscribe_with_sub_id(topic: &str, qos: u8, packet_id: u16, sub_id: u32) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        let mut props = BytesMut::new();
        props.put_u8(0x0B);
        encode_variable_int(&mut props, variable_int_value(sub_id));
        encode_variable_int(&mut body, props.len());
        body.put(props);
        put_mqtt_string(&mut body, topic);
        body.put_u8(qos & 0x03);
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
    /// Violates `[MQTT-3.8.3-2]`.
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

    /// Builds a `QoS` 2 PUBLISH with DUP=1 flag set.
    ///
    /// Fixed header byte `0x3C` (PUBLISH, DUP=1, QoS=2, RETAIN=0).
    #[must_use]
    pub fn publish_qos2_with_dup(topic: &str, payload: &[u8], packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        put_mqtt_string(&mut body, topic);
        body.put_u16(packet_id);
        body.put_u8(0);
        body.put_slice(payload);
        wrap_fixed_header(0x3C, &body)
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
        put_mqtt_string(&mut body, &unique_client_id("test-pw-client"));
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
        encode_variable_int(&mut body, props.len());
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
        encode_variable_int(&mut body, props.len());
        body.put(props);
        body.put_slice(payload);
        wrap_fixed_header(0x30, &body)
    }

    /// Builds a `QoS` 0 PUBLISH with a Response Topic property.
    ///
    /// Property ID `0x08` (Response Topic) is encoded as a UTF-8 string.
    /// Use with wildcard characters in `response_topic` to test `[MQTT-3.3.2-14]`.
    #[must_use]
    pub fn publish_qos0_with_response_topic(
        topic: &str,
        payload: &[u8],
        response_topic: &str,
    ) -> Vec<u8> {
        let mut body = BytesMut::new();
        put_mqtt_string(&mut body, topic);
        let mut props = BytesMut::new();
        props.put_u8(0x08);
        put_mqtt_string(&mut props, response_topic);
        encode_variable_int(&mut body, props.len());
        body.put(props);
        body.put_slice(payload);
        wrap_fixed_header(0x30, &body)
    }

    /// Builds a `QoS` 1 PUBLISH with DUP=1 flag set.
    ///
    /// Fixed header byte `0x3A` (PUBLISH, DUP=1, QoS=1, RETAIN=0).
    #[must_use]
    pub fn publish_qos1_with_dup(topic: &str, payload: &[u8], packet_id: u16) -> Vec<u8> {
        qos1_publish(0x3A, topic, payload, packet_id)
    }

    #[must_use]
    pub fn publish_qos1_retained(topic: &str, payload: &[u8], packet_id: u16) -> Vec<u8> {
        qos1_publish(0x33, topic, payload, packet_id)
    }

    #[must_use]
    pub fn connect_resume(client_id: &str, session_expiry: u32) -> Vec<u8> {
        connect_with_flags_and_session_expiry(0x00, client_id, session_expiry)
    }

    /// Builds a CONNECT packet with a Receive Maximum property.
    ///
    /// Same as [`valid_connect`](Self::valid_connect) but includes the Receive
    /// Maximum property (ID `0x21`, `u16`) so the broker limits its outbound
    /// inflight window for this client.
    #[must_use]
    pub fn connect_with_receive_maximum(client_id: &str, receive_maximum: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x02);
        body.put_u16(60);

        let mut props = BytesMut::new();
        props.put_u8(0x21);
        props.put_u16(receive_maximum);
        encode_variable_int(&mut body, props.len());
        body.put(props);

        put_mqtt_string(&mut body, client_id);

        wrap_fixed_header(0x10, &body)
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

    /// Builds a CONNECT with `clean_start=true` and Session Expiry Interval property.
    ///
    /// Property ID `0x11` encodes the session expiry as a `u32`.
    #[must_use]
    pub fn connect_with_session_expiry(client_id: &str, session_expiry: u32) -> Vec<u8> {
        connect_with_flags_and_session_expiry(0x02, client_id, session_expiry)
    }

    /// Builds a CONNECT with `clean_start=true` and Maximum Packet Size property.
    ///
    /// Property ID `0x27` encodes the maximum packet size as a `u32`.
    #[must_use]
    pub fn connect_with_max_packet_size(client_id: &str, max_packet_size: u32) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x02);
        body.put_u16(60);

        let mut props = BytesMut::new();
        props.put_u8(0x27);
        props.put_u32(max_packet_size);
        encode_variable_int(&mut body, props.len());
        body.put(props);

        put_mqtt_string(&mut body, client_id);

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a CONNECT with Will Flag=1, `QoS` 0, and configurable Will Retain.
    ///
    /// Will topic is `"will/{client_id}"`, will payload is `"offline"`.
    /// Connect flags: `clean_start` (bit 1) | Will Flag (bit 2) | Will Retain (bit 5 if true).
    #[must_use]
    pub fn connect_with_will_retain_flag(client_id: &str, will_retain: bool) -> Vec<u8> {
        let mut flags: u8 = 0x06;
        if will_retain {
            flags |= 0x20;
        }

        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(flags);
        body.put_u16(60);
        body.put_u8(0);
        put_mqtt_string(&mut body, client_id);
        body.put_u8(0);
        let will_topic = format!("will/{client_id}");
        put_mqtt_string(&mut body, &will_topic);
        put_mqtt_string(&mut body, "offline");

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a CONNECT with Will Flag=1, `QoS` 0, and Will Delay Interval property.
    ///
    /// Will Delay Interval property ID `0x18` is encoded in the Will Properties.
    /// Session Expiry Interval is set to 300. Will topic is `"will/{client_id}"`,
    /// will payload is `"offline"`.
    #[must_use]
    pub fn connect_with_will_delay(
        client_id: &str,
        will_delay_secs: u32,
        keepalive_secs: u16,
    ) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x06);
        body.put_u16(keepalive_secs);

        let mut connect_props = BytesMut::new();
        connect_props.put_u8(0x11);
        connect_props.put_u32(300);
        encode_variable_int(&mut body, connect_props.len());
        body.put(connect_props);

        put_mqtt_string(&mut body, client_id);

        let mut will_props = BytesMut::new();
        will_props.put_u8(0x18);
        will_props.put_u32(will_delay_secs);
        encode_variable_int(&mut body, will_props.len());
        body.put(will_props);

        let will_topic = format!("will/{client_id}");
        put_mqtt_string(&mut body, &will_topic);
        put_mqtt_string(&mut body, "offline");

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a CONNECT with Will Flag=1, `QoS` 0, and Will User Properties.
    ///
    /// Each user property (ID `0x26`) is a repeated UTF-8 string pair in the
    /// Will Properties. Will topic is `"will/{client_id}"`, will payload is `"offline"`.
    #[must_use]
    pub fn connect_with_will_user_properties(
        client_id: &str,
        user_props: &[(&str, &str)],
    ) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x06);
        body.put_u16(60);
        body.put_u8(0);
        put_mqtt_string(&mut body, client_id);

        let mut will_props = BytesMut::new();
        for (key, value) in user_props {
            will_props.put_u8(0x26);
            put_mqtt_string(&mut will_props, key);
            put_mqtt_string(&mut will_props, value);
        }
        encode_variable_int(&mut body, will_props.len());
        body.put(will_props);

        let will_topic = format!("will/{client_id}");
        put_mqtt_string(&mut body, &will_topic);
        put_mqtt_string(&mut body, "offline");

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a CONNECT with Request Response Information property.
    ///
    /// Property ID `0x19` encodes the request as a single byte.
    #[must_use]
    pub fn connect_with_request_response_info(client_id: &str, request_response: u8) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x02);
        body.put_u16(60);

        let mut props = BytesMut::new();
        props.put_u8(0x19);
        props.put_u8(request_response);
        encode_variable_int(&mut body, props.len());
        body.put(props);

        put_mqtt_string(&mut body, client_id);

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a CONNECT with Request Problem Information property.
    ///
    /// Property ID `0x17` encodes the request as a single byte.
    #[must_use]
    pub fn connect_with_request_problem_info(client_id: &str, request_problem: u8) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x02);
        body.put_u16(60);

        let mut props = BytesMut::new();
        props.put_u8(0x17);
        props.put_u8(request_problem);
        encode_variable_int(&mut body, props.len());
        body.put(props);

        put_mqtt_string(&mut body, client_id);

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a CONNECT with Username Flag (bit 7) and `clean_start` (bit 1).
    ///
    /// Connect flags `0x82`. Payload contains client ID then username.
    #[must_use]
    pub fn connect_with_username(client_id: &str, username: &str) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x82);
        body.put_u16(60);
        body.put_u8(0);
        put_mqtt_string(&mut body, client_id);
        put_mqtt_string(&mut body, username);

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a CONNECT with Password Flag (bit 6) and `clean_start` (bit 1).
    ///
    /// Connect flags `0x42`. Payload contains client ID then password
    /// (binary data with `u16` length prefix).
    #[must_use]
    pub fn connect_with_password(client_id: &str, password: &str) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x42);
        body.put_u16(60);
        body.put_u8(0);
        put_mqtt_string(&mut body, client_id);
        let pw_bytes = password.as_bytes();
        body.put_u16(length_u16(pw_bytes.len()));
        body.put_slice(pw_bytes);

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a PUBACK packet with the given packet ID and implicit Success reason.
    ///
    /// Fixed header byte `0x40`.
    #[must_use]
    pub fn puback(packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        wrap_fixed_header(0x40, &body)
    }

    /// Builds a PUBREC packet with an explicit reason code.
    ///
    /// Fixed header byte `0x50`. Body: packet ID (`u16`) + reason code + properties
    /// length (0).
    #[must_use]
    pub fn pubrec_with_reason(packet_id: u16, reason_code: u8) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        body.put_u8(reason_code);
        body.put_u8(0);
        wrap_fixed_header(0x50, &body)
    }

    /// Builds a PUBACK packet with an explicit reason code.
    ///
    /// Fixed header byte `0x40`. Body: packet ID (`u16`) + reason code + properties
    /// length (0).
    #[must_use]
    pub fn puback_with_reason(packet_id: u16, reason_code: u8) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        body.put_u8(reason_code);
        body.put_u8(0);
        wrap_fixed_header(0x40, &body)
    }

    /// Builds a SUBSCRIBE packet with a raw options byte.
    ///
    /// Fixed header byte `0x82`. Allows setting reserved bits for protocol
    /// violation testing.
    #[must_use]
    pub fn subscribe_with_options(topic: &str, options_byte: u8, packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        body.put_u8(0);
        put_mqtt_string(&mut body, topic);
        body.put_u8(options_byte);
        wrap_fixed_header(0x82, &body)
    }

    /// Builds a SUBSCRIBE packet with invalid UTF-8 bytes in the topic filter.
    ///
    /// Violates `[MQTT-3.8.3-1]`: topic filters must be valid UTF-8.
    #[must_use]
    pub fn subscribe_invalid_utf8(packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        body.put_u8(0);
        let invalid_topic: &[u8] = &[0xFF, 0xFE];
        body.put_u16(length_u16(invalid_topic.len()));
        body.put_slice(invalid_topic);
        body.put_u8(0x00);
        wrap_fixed_header(0x82, &body)
    }

    /// Builds an UNSUBSCRIBE packet with invalid UTF-8 bytes in the topic filter.
    ///
    /// Violates `[MQTT-3.10.3-1]`: topic filters must be valid UTF-8.
    #[must_use]
    pub fn unsubscribe_invalid_utf8(packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        body.put_u8(0);
        let invalid_topic: &[u8] = &[0xFF, 0xFE];
        body.put_u16(length_u16(invalid_topic.len()));
        body.put_slice(invalid_topic);
        wrap_fixed_header(0xA2, &body)
    }

    /// Builds an AUTH packet with a reason code and no properties.
    ///
    /// Fixed header byte `0xF0`.
    #[must_use]
    pub fn auth_packet(reason_code: u8) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u8(reason_code);
        body.put_u8(0);
        wrap_fixed_header(0xF0, &body)
    }

    /// Builds a CONNECT packet with invalid UTF-8 bytes in the will topic.
    ///
    /// Connect flags `0x06` (will flag + clean start), will topic is `[0xFF, 0xFE]`.
    /// Violates `[MQTT-3.1.3-9]`.
    #[must_use]
    pub fn connect_with_invalid_utf8_will_topic(client_id: &str) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x06);
        body.put_u16(60);
        body.put_u8(0);
        put_mqtt_string(&mut body, client_id);
        body.put_u8(0);
        let invalid_topic: &[u8] = &[0xFF, 0xFE];
        body.put_u16(length_u16(invalid_topic.len()));
        body.put_slice(invalid_topic);
        put_mqtt_string(&mut body, "will-payload");
        wrap_fixed_header(0x10, &body)
    }

    /// Builds a CONNECT with invalid UTF-8 bytes in the username field.
    #[must_use]
    pub fn connect_with_invalid_utf8_username(client_id: &str) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x82);
        body.put_u16(60);
        body.put_u8(0);
        put_mqtt_string(&mut body, client_id);
        let invalid_username: &[u8] = &[0xFF, 0xFE];
        body.put_u16(length_u16(invalid_username.len()));
        body.put_slice(invalid_username);
        wrap_fixed_header(0x10, &body)
    }

    /// Builds a `QoS` 2 PUBLISH with a Message Expiry Interval property.
    #[must_use]
    pub fn publish_qos2_with_message_expiry(
        topic: &str,
        payload: &[u8],
        packet_id: u16,
        expiry_secs: u32,
    ) -> Vec<u8> {
        let mut body = BytesMut::new();
        put_mqtt_string(&mut body, topic);
        body.put_u16(packet_id);
        let mut props = BytesMut::new();
        props.put_u8(0x02);
        props.put_u32(expiry_secs);
        encode_variable_int(&mut body, props.len());
        body.put(props);
        body.put_slice(payload);
        wrap_fixed_header(0x34, &body)
    }

    /// Builds an AUTH packet with non-zero reserved flags (`0xF1` instead of `0xF0`).
    #[must_use]
    pub fn auth_with_invalid_flags() -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u8(0x00);
        body.put_u8(0);
        wrap_fixed_header(0xF1, &body)
    }

    /// Builds a `QoS` 0 PUBLISH with the RETAIN flag set.
    ///
    /// Fixed header byte `0x31` (PUBLISH, DUP=0, QoS=0, RETAIN=1).
    #[must_use]
    pub fn publish_qos0_retained(topic: &str, payload: &[u8]) -> Vec<u8> {
        let mut body = BytesMut::new();
        put_mqtt_string(&mut body, topic);
        body.put_u8(0);
        body.put_slice(payload);
        wrap_fixed_header(0x31, &body)
    }

    /// Builds a CONNECT packet with a UTF-16 surrogate codepoint in the client ID.
    ///
    /// The client ID contains the byte sequence `0xED 0xA0 0x80` (U+D800),
    /// which is an invalid UTF-8 encoding of a surrogate half.
    /// Violates `[MQTT-1.5.4-1]`.
    #[must_use]
    pub fn connect_with_surrogate_utf8() -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x02);
        body.put_u16(60);
        body.put_u8(0);
        let client_id_bytes: &[u8] = &[0xED, 0xA0, 0x80];
        body.put_u16(length_u16(client_id_bytes.len()));
        body.put_slice(client_id_bytes);
        wrap_fixed_header(0x10, &body)
    }

    /// Builds a CONNECT-like packet with a 5-byte variable byte integer in the
    /// remaining length field.
    ///
    /// MQTT variable byte integers use at most 4 bytes (max value 268,435,455).
    /// Five continuation bytes violate the encoding maximum.
    /// Violates `[MQTT-1.5.5-1]`.
    #[must_use]
    pub fn connect_with_non_minimal_varint() -> Vec<u8> {
        let mut packet = BytesMut::new();
        packet.put_u8(0x10);
        packet.put_u8(0x80);
        packet.put_u8(0x80);
        packet.put_u8(0x80);
        packet.put_u8(0x80);
        packet.put_u8(0x01);
        packet.to_vec()
    }

    /// Builds a `QoS` 0 PUBLISH with a user property key containing invalid
    /// UTF-8 bytes.
    ///
    /// The user property key is `[0xFF, 0xFE]` which is not valid UTF-8.
    /// Violates `[MQTT-1.5.7-1]`.
    #[must_use]
    pub fn publish_with_invalid_utf8_user_property(topic: &str, payload: &[u8]) -> Vec<u8> {
        let mut body = BytesMut::new();
        put_mqtt_string(&mut body, topic);
        let mut props = BytesMut::new();
        props.put_u8(0x26);
        let invalid_key: &[u8] = &[0xFF, 0xFE];
        props.put_u16(length_u16(invalid_key.len()));
        props.put_slice(invalid_key);
        put_mqtt_string(&mut props, "value");
        encode_variable_int(&mut body, props.len());
        body.put(props);
        body.put_slice(payload);
        wrap_fixed_header(0x30, &body)
    }

    /// Builds a `QoS` 0 PUBLISH with a topic of 65535 'A' characters.
    ///
    /// The 2-byte UTF-8 string length prefix has maximum value 65535, so this
    /// is the largest valid topic encoding. The packet is self-consistent:
    /// remaining length matches the actual body. The broker may reject this
    /// due to max packet size limits.
    /// Tests `[MQTT-4.7.3-3]`.
    #[must_use]
    pub fn publish_with_oversized_topic() -> Vec<u8> {
        let topic_len: usize = 65_535;
        let topic_bytes = vec![b'A'; topic_len];
        let mut body = BytesMut::new();
        body.put_u16(length_u16(topic_len));
        body.put_slice(&topic_bytes);
        body.put_u8(0);
        body.put_slice(b"payload");
        wrap_fixed_header(0x30, &body)
    }

    /// Builds a SUBSCRIBE with raw bytes in the topic filter (including BOM prefix).
    ///
    /// Used for testing `[MQTT-1.5.4-3]`: BOM characters should not be stripped.
    #[must_use]
    pub fn subscribe_raw_topic(topic_bytes: &[u8], qos: u8, packet_id: u16) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(packet_id);
        body.put_u8(0);
        body.put_u16(length_u16(topic_bytes.len()));
        body.put_slice(topic_bytes);
        body.put_u8(qos & 0x03);
        wrap_fixed_header(0x82, &body)
    }

    /// Builds a `QoS` 0 PUBLISH with raw bytes as the topic name.
    ///
    /// Used for testing `[MQTT-1.5.4-3]`: BOM characters should not be stripped.
    #[must_use]
    pub fn publish_qos0_raw_topic(topic_bytes: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(length_u16(topic_bytes.len()));
        body.put_slice(topic_bytes);
        body.put_u8(0);
        body.put_slice(payload);
        wrap_fixed_header(0x30, &body)
    }

    /// Builds a CONNECT with an Authentication Method property.
    #[must_use]
    pub fn connect_with_auth_method(client_id: &str, auth_method: &str) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x02);
        body.put_u16(60);

        let mut props = BytesMut::new();
        props.put_u8(0x15);
        put_mqtt_string(&mut props, auth_method);
        encode_variable_int(&mut body, props.len());
        body.put(props);

        put_mqtt_string(&mut body, client_id);

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a CONNECT with an Authentication Method property and a Maximum
    /// Packet Size property (`0x27`, `u32`).
    #[must_use]
    pub fn connect_with_auth_method_and_max_packet_size(
        client_id: &str,
        auth_method: &str,
        max_packet_size: u32,
    ) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x02);
        body.put_u16(60);

        let mut props = BytesMut::new();
        props.put_u8(0x27);
        props.put_u32(max_packet_size);
        props.put_u8(0x15);
        put_mqtt_string(&mut props, auth_method);
        encode_variable_int(&mut body, props.len());
        body.put(props);

        put_mqtt_string(&mut body, client_id);

        wrap_fixed_header(0x10, &body)
    }

    /// Builds a CONNECT with Authentication Method and Authentication Data properties.
    #[must_use]
    pub fn connect_with_auth_method_and_data(
        client_id: &str,
        auth_method: &str,
        auth_data: &[u8],
    ) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u16(4);
        body.put_slice(b"MQTT");
        body.put_u8(5);
        body.put_u8(0x02);
        body.put_u16(60);

        let mut props = BytesMut::new();
        props.put_u8(0x15);
        put_mqtt_string(&mut props, auth_method);
        props.put_u8(0x16);
        props.put_u16(length_u16(auth_data.len()));
        props.put_slice(auth_data);
        encode_variable_int(&mut body, props.len());
        body.put(props);

        put_mqtt_string(&mut body, client_id);

        wrap_fixed_header(0x10, &body)
    }

    /// Builds an AUTH packet with the given reason code and Authentication Method.
    #[must_use]
    pub fn auth_with_method(reason_code: u8, auth_method: &str) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u8(reason_code);

        let mut props = BytesMut::new();
        props.put_u8(0x15);
        put_mqtt_string(&mut props, auth_method);
        encode_variable_int(&mut body, props.len());
        body.put(props);

        wrap_fixed_header(0xF0, &body)
    }

    /// Builds an AUTH packet with reason code, Authentication Method, and Authentication Data.
    #[must_use]
    pub fn auth_with_method_and_data(
        reason_code: u8,
        auth_method: &str,
        auth_data: &[u8],
    ) -> Vec<u8> {
        let mut body = BytesMut::new();
        body.put_u8(reason_code);

        let mut props = BytesMut::new();
        props.put_u8(0x15);
        put_mqtt_string(&mut props, auth_method);
        props.put_u8(0x16);
        props.put_u16(length_u16(auth_data.len()));
        props.put_slice(auth_data);
        encode_variable_int(&mut body, props.len());
        body.put(props);

        wrap_fixed_header(0xF0, &body)
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
    let (remaining_len, idx) = decode_variable_int(data, 1)?;
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

fn parse_publish(data: &[u8]) -> Option<(u8, u16, String, Vec<u8>)> {
    let first_byte = *data.first()?;
    if first_byte & 0xF0 != 0x30 {
        return None;
    }
    let (remaining_len, body_start) = decode_variable_int(data, 1)?;
    let end = body_start + remaining_len as usize;
    if data.len() < end || body_start + 2 > end {
        return None;
    }
    let topic_start = body_start + 2;
    let topic_end =
        topic_start + u16::from_be_bytes([data[body_start], data[body_start + 1]]) as usize;
    if topic_end > end {
        return None;
    }
    let topic = String::from_utf8_lossy(&data[topic_start..topic_end]).to_string();
    let (packet_id, props_len_start) = if (first_byte >> 1) & 0x03 > 0 {
        if topic_end + 2 > end {
            return None;
        }
        (
            u16::from_be_bytes([data[topic_end], data[topic_end + 1]]),
            topic_end + 2,
        )
    } else {
        (0, topic_end)
    };
    let (props_len, props_start) = decode_variable_int(&data[..end], props_len_start)?;
    let payload_start = props_start + props_len as usize;
    if payload_start > end {
        return None;
    }
    Some((
        first_byte,
        packet_id,
        topic,
        data[payload_start..end].to_vec(),
    ))
}

pub fn put_mqtt_string(buf: &mut BytesMut, s: &str) {
    let bytes = s.as_bytes();
    buf.put_u16(length_u16(bytes.len()));
    buf.put_slice(bytes);
}

fn qos1_publish(header: u8, topic: &str, payload: &[u8], packet_id: u16) -> Vec<u8> {
    let mut body = BytesMut::new();
    put_mqtt_string(&mut body, topic);
    body.put_u16(packet_id);
    body.put_u8(0);
    body.put_slice(payload);
    wrap_fixed_header(header, &body)
}

fn connect_with_flags_and_session_expiry(
    flags: u8,
    client_id: &str,
    session_expiry: u32,
) -> Vec<u8> {
    let mut body = BytesMut::new();
    body.put_u16(4);
    body.put_slice(b"MQTT");
    body.put_u8(5);
    body.put_u8(flags);
    body.put_u16(60);
    let mut props = BytesMut::new();
    props.put_u8(0x11);
    props.put_u32(session_expiry);
    encode_variable_int(&mut body, props.len());
    body.put(props);
    put_mqtt_string(&mut body, client_id);
    wrap_fixed_header(0x10, &body)
}

fn variable_int_value(value: u32) -> usize {
    usize::try_from(value).expect("variable byte integer fits in usize")
}

fn length_u16(length: usize) -> u16 {
    u16::try_from(length).expect("length fits in a two byte integer")
}

pub fn encode_variable_int(buf: &mut BytesMut, mut value: usize) {
    loop {
        let mut byte = value.to_le_bytes()[0] & 0x7F;
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

#[must_use]
pub fn wrap_fixed_header(first_byte: u8, body: &[u8]) -> Vec<u8> {
    let mut packet = BytesMut::new();
    packet.put_u8(first_byte);
    encode_variable_int(&mut packet, body.len());
    packet.put_slice(body);
    packet.to_vec()
}

fn extract_auth_method_property(data: &[u8], props_start: usize) -> Option<String> {
    let (props_len, mut idx) = decode_variable_int(data, props_start)?;
    let props_end = idx + props_len as usize;
    while idx < props_end && idx < data.len() {
        let prop_id = data[idx];
        idx += 1;
        if prop_id == 0x15 {
            if idx + 2 > data.len() {
                return None;
            }
            let str_len = u16::from_be_bytes([data[idx], data[idx + 1]]) as usize;
            idx += 2;
            if idx + str_len > data.len() {
                return None;
            }
            return String::from_utf8(data[idx..idx + str_len].to_vec()).ok();
        }
        match prop_id {
            0x1F | 0x26 => {
                if idx + 2 > data.len() {
                    return None;
                }
                let len = u16::from_be_bytes([data[idx], data[idx + 1]]) as usize;
                idx += 2 + len;
                if prop_id == 0x26 {
                    if idx + 2 > data.len() {
                        return None;
                    }
                    let vlen = u16::from_be_bytes([data[idx], data[idx + 1]]) as usize;
                    idx += 2 + vlen;
                }
            }
            0x16 => {
                if idx + 2 > data.len() {
                    return None;
                }
                let len = u16::from_be_bytes([data[idx], data[idx + 1]]) as usize;
                idx += 2 + len;
            }
            _ => {
                return None;
            }
        }
    }
    None
}

#[cfg(test)]
mod framing_tests {
    use super::RawMqttClient;
    use std::time::Duration;
    use tokio::io::{AsyncWriteExt, DuplexStream};

    const TIMEOUT: Duration = Duration::from_millis(200);
    const SUBACK: [u8; 6] = [0x90, 0x04, 0x00, 0x01, 0x00, 0x00];
    const PUBLISH: [u8; 7] = [0x30, 0x05, 0x00, 0x01, b't', 0x00, b'x'];
    const PUBACK: [u8; 4] = [0x40, 0x02, 0x00, 0x07];
    const PINGRESP: [u8; 2] = [0xD0, 0x00];
    const DISCONNECT: [u8; 3] = [0xE0, 0x01, 0x8D];

    async fn client_receiving(chunks: &[&[u8]]) -> (RawMqttClient<DuplexStream>, DuplexStream) {
        let (client, mut server) = tokio::io::duplex(4096);
        server
            .write_all(&chunks.concat())
            .await
            .expect("server write");
        (RawMqttClient::from_transport(client), server)
    }

    #[tokio::test]
    async fn publish_buffered_behind_suback_is_read() {
        let (mut client, _server) = client_receiving(&[&SUBACK, &PUBLISH]).await;
        assert!(client.expect_suback(TIMEOUT).await.is_some());
        assert_eq!(
            client.expect_publish(TIMEOUT).await,
            Some((0, "t".to_owned(), b"x".to_vec())),
            "a PUBLISH that arrived in the same read as the SUBACK must stay buffered"
        );
    }

    #[tokio::test]
    async fn coalesced_packets_are_read_one_at_a_time() {
        let (mut client, _server) = client_receiving(&[&PUBACK, &PINGRESP]).await;
        assert_eq!(client.expect_puback(TIMEOUT).await, Some((7, 0)));
        assert!(
            client.expect_pingresp(TIMEOUT).await,
            "a PINGRESP that arrived in the same read as the PUBACK must stay buffered"
        );
    }

    #[tokio::test]
    async fn buffered_disconnect_then_close_is_a_disconnect() {
        let (mut client, server) = client_receiving(&[&SUBACK, &DISCONNECT]).await;
        assert!(client.expect_suback(TIMEOUT).await.is_some());
        drop(server);
        assert!(
            client.expect_disconnect(TIMEOUT).await,
            "a DISCONNECT buffered behind the SUBACK, then a close, must count as a disconnect"
        );
    }

    #[tokio::test]
    async fn disconnect_on_open_connection_is_not_a_close() {
        let (mut client, _server) = client_receiving(&[&DISCONNECT]).await;
        assert!(
            !client.expect_disconnect(TIMEOUT).await,
            "a DISCONNECT on a connection that stays open is not a close"
        );
    }

    #[tokio::test]
    async fn open_connection_without_disconnect_is_not_a_disconnect() {
        let (mut client, _server) = client_receiving(&[&PUBLISH]).await;
        assert!(
            !client.expect_disconnect(TIMEOUT).await,
            "a PUBLISH on a connection that stays open is not a disconnect"
        );
    }

    #[tokio::test]
    async fn closed_connection_is_a_disconnect() {
        let (mut client, server) = client_receiving(&[&PUBLISH]).await;
        drop(server);
        assert!(client.expect_disconnect(TIMEOUT).await);
    }

    #[tokio::test]
    async fn malformed_remaining_length_is_returned() {
        let malformed = [0x20, 0xFF, 0xFF, 0xFF, 0xFF, 0x01, 0x00, 0x00];
        let (mut client, _server) = client_receiving(&[&malformed]).await;
        assert_eq!(
            client.read_packet_bytes(TIMEOUT).await,
            Some(malformed.to_vec()),
            "bytes with a Remaining Length over 4 bytes must reach the caller, not look like silence"
        );
    }

    #[tokio::test]
    async fn truncated_packet_before_close_is_returned() {
        let (mut client, server) = client_receiving(&[&PUBLISH[..4]]).await;
        drop(server);
        assert_eq!(
            client.read_packet_bytes(TIMEOUT).await,
            Some(PUBLISH[..4].to_vec()),
            "a partial packet before the close must reach the caller, not look like silence"
        );
    }
}
