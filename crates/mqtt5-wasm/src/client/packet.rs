use bytes::BytesMut;
use mqtt5_protocol::error::Result;
use mqtt5_protocol::packet::{MqttPacket, Packet};

pub fn encode_packet(packet: &Packet, buf: &mut BytesMut) -> Result<()> {
    match packet {
        Packet::Connect(p) => p.encode(buf),
        Packet::Publish(p) => p.encode(buf),
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
