use bytes::BytesMut;
use mqtt5_protocol::error::Result;
use mqtt5_protocol::packet::{MqttPacket, Packet};
use std::cell::RefCell;
use std::rc::Rc;

use super::state::ClientState;

pub fn encode_packet(packet: &Packet, buf: &mut BytesMut) -> Result<()> {
    match packet {
        Packet::Connect(p) => p.encode(buf),
        Packet::Publish(p) => p.encode(buf),
        Packet::PubAck(p) => p.encode(buf),
        Packet::PubRec(p) => p.encode(buf),
        Packet::PubRel(p) => p.encode(buf),
        Packet::PubComp(p) => p.encode(buf),
        Packet::Subscribe(p) => p.encode(buf),
        Packet::PingReq => mqtt5_protocol::packet::pingreq::PingReqPacket::default().encode(buf),
        Packet::Disconnect(p) => p.encode(buf),
        Packet::Unsubscribe(p) => p.encode(buf),
        Packet::Auth(p) => p.encode(buf),
        _ => Err(mqtt5_protocol::error::MqttError::ProtocolError(format!(
            "Encoding not yet implemented for packet type: {packet:?}"
        ))),
    }
}

pub fn encode_checked(
    packet: &Packet,
    maximum_packet_size: Option<u32>,
) -> std::result::Result<BytesMut, String> {
    let mut buf = BytesMut::new();
    encode_packet(packet, &mut buf).map_err(|e| format!("Packet encoding failed: {e}"))?;
    match maximum_packet_size {
        Some(max) if u32::try_from(buf.len()).map_or(true, |len| len > max) => Err(format!(
            "{} of {} bytes exceeds the server Maximum Packet Size {max}",
            packet.packet_type_name(),
            buf.len()
        )),
        _ => Ok(buf),
    }
}

pub fn write_packet(
    state: &Rc<RefCell<ClientState>>,
    packet: &Packet,
) -> std::result::Result<(), String> {
    let (writer, maximum_packet_size) = {
        let state_ref = state.borrow();
        (
            state_ref.writer.clone(),
            state_ref.server.maximum_packet_size,
        )
    };
    let buf = encode_checked(packet, maximum_packet_size)?;
    let writer = writer.ok_or_else(|| "Not connected".to_string())?;
    let result = writer
        .borrow_mut()
        .write(&buf)
        .map_err(|e| format!("Write failed: {e}"));
    result
}
