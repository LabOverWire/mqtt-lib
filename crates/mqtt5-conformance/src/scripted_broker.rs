use crate::packet_parser::decode_variable_int;
use crate::raw_client::{encode_variable_int, wrap_fixed_header, RawMqttClient, ReadOutcome};
use bytes::BytesMut;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};

pub const CONNECT: u8 = 1;
pub const PUBLISH: u8 = 3;
pub const PUBACK: u8 = 4;
pub const PUBREC: u8 = 5;
pub const PUBREL: u8 = 6;
pub const PUBCOMP: u8 = 7;
pub const SUBSCRIBE: u8 = 8;
pub const UNSUBSCRIBE: u8 = 10;
pub const PINGREQ: u8 = 12;
pub const DISCONNECT: u8 = 14;
pub const AUTH: u8 = 15;

pub const PROP_ASSIGNED_CLIENT_IDENTIFIER: u8 = 0x12;
pub const PROP_SERVER_KEEP_ALIVE: u8 = 0x13;
pub const PROP_AUTHENTICATION_METHOD: u8 = 0x15;
pub const PROP_REASON_STRING: u8 = 0x1F;
pub const PROP_TOPIC_ALIAS_MAXIMUM: u8 = 0x22;
pub const PROP_MAXIMUM_QOS: u8 = 0x24;
pub const PROP_RETAIN_AVAILABLE: u8 = 0x25;
pub const PROP_USER_PROPERTY: u8 = 0x26;
pub const PROP_MAXIMUM_PACKET_SIZE: u8 = 0x27;

#[derive(Debug, Clone)]
pub struct Packet {
    bytes: Vec<u8>,
    body_start: usize,
}

impl Packet {
    fn from_bytes(bytes: Vec<u8>) -> Option<Self> {
        let (remaining_len, body_start) = decode_variable_int(&bytes, 1)?;
        (bytes.len() == body_start + remaining_len as usize).then_some(Self { bytes, body_start })
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn packet_type(&self) -> u8 {
        self.bytes[0] >> 4
    }

    #[must_use]
    pub fn flags(&self) -> u8 {
        self.bytes[0] & 0x0F
    }

    #[must_use]
    pub fn remaining_length_bytes(&self) -> &[u8] {
        &self.bytes[1..self.body_start]
    }

    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.bytes[self.body_start..]
    }

    #[must_use]
    pub fn wire_len(&self) -> usize {
        self.bytes.len()
    }

    #[must_use]
    pub fn leading_packet_id(&self) -> Option<u16> {
        Some(u16::from_be_bytes(*self.body().first_chunk()?))
    }
}

pub enum Next {
    Packet(Packet),
    Malformed,
    Closed,
    Silent,
}

pub struct CloseObservation {
    pub closed: bool,
    pub disconnect_reason: Option<u8>,
}

pub struct ScriptedBroker {
    listener: TcpListener,
}

impl ScriptedBroker {
    pub async fn bind() -> std::io::Result<Self> {
        Ok(Self {
            listener: TcpListener::bind("127.0.0.1:0").await?,
        })
    }

    pub fn url(&self) -> std::io::Result<String> {
        Ok(format!("mqtt://{}", self.listener.local_addr()?))
    }

    pub async fn accept(&self, wait: Duration) -> Option<BrokerConnection> {
        let (stream, _) = tokio::time::timeout(wait, self.listener.accept())
            .await
            .ok()?
            .ok()?;
        Some(BrokerConnection {
            peer: RawMqttClient::from_transport(stream),
        })
    }
}

pub struct BrokerConnection {
    peer: RawMqttClient<TcpStream>,
}

impl BrokerConnection {
    pub async fn send(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.peer.send_raw(bytes).await
    }

    pub async fn next(&mut self, wait: Duration) -> Next {
        match self.peer.read_packet(wait).await {
            ReadOutcome::Packet(bytes) => {
                Packet::from_bytes(bytes).map_or(Next::Malformed, Next::Packet)
            }
            ReadOutcome::Closed => Next::Closed,
            ReadOutcome::TimedOut => Next::Silent,
        }
    }

    pub async fn next_answering_pings(&mut self, wait: Duration) -> Next {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match self.next(left).await {
                Next::Packet(packet) if packet.packet_type() == PINGREQ => {
                    let _ = self.send(&[0xD0, 0x00]).await;
                }
                other => return other,
            }
        }
    }

    pub async fn next_of_type(&mut self, packet_type: u8, wait: Duration) -> Option<Packet> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match self.next(left).await {
                Next::Packet(packet) if packet.packet_type() == packet_type => return Some(packet),
                Next::Packet(packet) if packet.packet_type() == PINGREQ => {
                    let _ = self.send(&[0xD0, 0x00]).await;
                }
                Next::Packet(_) => {}
                Next::Malformed | Next::Closed | Next::Silent => return None,
            }
        }
    }

    pub async fn observe_close(&mut self, wait: Duration) -> CloseObservation {
        let deadline = tokio::time::Instant::now() + wait;
        let mut disconnect_reason = None;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match self.next_answering_pings(left).await {
                Next::Packet(packet) if packet.packet_type() == DISCONNECT => {
                    disconnect_reason = Some(packet.body().first().copied().unwrap_or(0));
                }
                Next::Packet(_) | Next::Malformed => {}
                Next::Closed => {
                    return CloseObservation {
                        closed: true,
                        disconnect_reason,
                    }
                }
                Next::Silent => {
                    return CloseObservation {
                        closed: false,
                        disconnect_reason,
                    }
                }
            }
        }
    }
}

#[must_use]
pub fn variable_int(value: usize) -> Vec<u8> {
    let mut buf = BytesMut::new();
    encode_variable_int(&mut buf, value);
    buf.to_vec()
}

#[must_use]
pub fn mqtt_string(value: &[u8]) -> Vec<u8> {
    let mut out = u16::try_from(value.len())
        .expect("string longer than 65535 bytes")
        .to_be_bytes()
        .to_vec();
    out.extend_from_slice(value);
    out
}

#[must_use]
pub fn property_u8(id: u8, value: u8) -> Vec<u8> {
    vec![id, value]
}

#[must_use]
pub fn property_u16(id: u8, value: u16) -> Vec<u8> {
    let mut out = vec![id];
    out.extend(value.to_be_bytes());
    out
}

#[must_use]
pub fn property_u32(id: u8, value: u32) -> Vec<u8> {
    let mut out = vec![id];
    out.extend(value.to_be_bytes());
    out
}

#[must_use]
pub fn property_string(id: u8, value: &[u8]) -> Vec<u8> {
    let mut out = vec![id];
    out.extend(mqtt_string(value));
    out
}

#[must_use]
pub fn user_property(key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut out = vec![PROP_USER_PROPERTY];
    out.extend(mqtt_string(key));
    out.extend(mqtt_string(value));
    out
}

#[must_use]
pub fn properties(props: &[u8]) -> Vec<u8> {
    let mut out = variable_int(props.len());
    out.extend_from_slice(props);
    out
}

#[must_use]
pub fn connack(session_present: bool, props: &[u8]) -> Vec<u8> {
    let mut body = vec![u8::from(session_present), 0x00];
    body.extend(properties(props));
    wrap_fixed_header(0x20, &body)
}

#[must_use]
pub fn publish(
    first_byte: u8,
    topic: &[u8],
    packet_id: Option<u16>,
    props: &[u8],
    payload: &[u8],
) -> Vec<u8> {
    let mut body = mqtt_string(topic);
    if let Some(id) = packet_id {
        body.extend(id.to_be_bytes());
    }
    body.extend(properties(props));
    body.extend_from_slice(payload);
    wrap_fixed_header(first_byte, &body)
}

#[must_use]
pub fn ack(first_byte: u8, packet_id: u16) -> Vec<u8> {
    wrap_fixed_header(first_byte, &packet_id.to_be_bytes())
}

#[must_use]
pub fn ack_with_properties(first_byte: u8, packet_id: u16, reason: u8, props: &[u8]) -> Vec<u8> {
    let mut body = packet_id.to_be_bytes().to_vec();
    body.push(reason);
    body.extend(properties(props));
    wrap_fixed_header(first_byte, &body)
}

#[must_use]
pub fn subscription_ack(first_byte: u8, packet_id: u16, props: &[u8], reasons: &[u8]) -> Vec<u8> {
    let mut body = packet_id.to_be_bytes().to_vec();
    body.extend(properties(props));
    body.extend_from_slice(reasons);
    wrap_fixed_header(first_byte, &body)
}

#[must_use]
pub fn auth(reason: u8, method: &[u8], extra_props: &[u8]) -> Vec<u8> {
    let mut props = property_string(PROP_AUTHENTICATION_METHOD, method);
    props.extend_from_slice(extra_props);
    let mut body = vec![reason];
    body.extend(properties(&props));
    wrap_fixed_header(0xF0, &body)
}
