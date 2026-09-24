use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen_futures::spawn_local;

use crate::decoder::read_frame;
use crate::transport::WasmReader;
use mqtt5_protocol::error::MqttError;
use mqtt5_protocol::packet::{FixedHeader, Packet};
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;

use super::callbacks::{fail_connection, handle_connection_lost};
use super::handlers::{handle_incoming_packet, Violation};
use super::state::ClientState;

enum ReadOutcome {
    Packet(Packet),
    Violation(Violation),
    Lost(String),
}

pub fn classify_read_error(error: MqttError) -> Result<Violation, String> {
    match error {
        MqttError::PacketTooLarge { size, max } => Ok(Violation::new(
            ReasonCode::PacketTooLarge,
            format!("Inbound packet of {size} bytes exceeds Maximum Packet Size {max}"),
        )),
        MqttError::ProtocolError(message) => Ok(Violation::new(ReasonCode::ProtocolError, message)),
        MqttError::ConnectionClosedByPeer
        | MqttError::ClientClosed
        | MqttError::NotConnected
        | MqttError::Io(_)
        | MqttError::ConnectionError(_) => Err(format!("Packet read error: {error}")),
        other => Ok(Violation::new(
            ReasonCode::MalformedPacket,
            other.to_string(),
        )),
    }
}

pub fn decode_frame(
    fixed_header: &FixedHeader,
    body: &[u8],
    protocol_version: u8,
) -> Result<Packet, Violation> {
    let mut cursor = body;
    Packet::decode_from_body_with_version(
        fixed_header.packet_type,
        fixed_header,
        &mut cursor,
        protocol_version,
    )
    .and_then(|packet| match packet {
        Packet::Publish(_) | Packet::Subscribe(_) | Packet::SubAck(_) | Packet::Unsubscribe(_)
            if !fixed_header.validate_flags() =>
        {
            Err(MqttError::MalformedPacket(format!(
                "Invalid fixed header flags 0x{:02X}",
                fixed_header.flags
            )))
        }
        packet => Ok(packet),
    })
    .map_err(|e| {
        classify_read_error(e)
            .unwrap_or_else(|message| Violation::new(ReasonCode::MalformedPacket, message))
    })
}

async fn next_packet(state: &Rc<RefCell<ClientState>>, reader: &mut WasmReader) -> ReadOutcome {
    let (maximum_packet_size, protocol_version) = {
        let state_ref = state.borrow();
        (
            state_ref.client_limits.maximum_packet_size,
            state_ref.protocol_version,
        )
    };
    match read_frame(reader, maximum_packet_size).await {
        Ok((fixed_header, body)) => match decode_frame(&fixed_header, &body, protocol_version) {
            Ok(packet) => ReadOutcome::Packet(packet),
            Err(violation) => ReadOutcome::Violation(violation),
        },
        Err(error) => match classify_read_error(error) {
            Ok(violation) => ReadOutcome::Violation(violation),
            Err(message) => ReadOutcome::Lost(message),
        },
    }
}

pub async fn read_connect_response(
    state: &Rc<RefCell<ClientState>>,
    reader: &mut WasmReader,
) -> Result<Packet, String> {
    match next_packet(state, reader).await {
        ReadOutcome::Packet(packet) => Ok(packet),
        ReadOutcome::Violation(violation) => Err(format!(
            "Invalid packet during connect ({:?}): {}",
            violation.reason_code, violation.message
        )),
        ReadOutcome::Lost(message) => Err(message),
    }
}

pub fn spawn_packet_reader(state: Rc<RefCell<ClientState>>, mut reader: WasmReader) {
    let generation = state.borrow().connection_generation;
    spawn_local(async move {
        loop {
            let outcome = next_packet(&state, &mut reader).await;
            if state.borrow().connection_generation != generation {
                return;
            }
            match outcome {
                ReadOutcome::Packet(Packet::Disconnect(disconnect)) => {
                    let reason = format!("Server sent DISCONNECT: {:?}", disconnect.reason_code);
                    handle_connection_lost(&state, &reason);
                    return;
                }
                ReadOutcome::Packet(packet) => {
                    if let Err(violation) = handle_incoming_packet(&state, packet) {
                        fail_connection(&state, &violation);
                        return;
                    }
                }
                ReadOutcome::Violation(violation) => {
                    fail_connection(&state, &violation);
                    return;
                }
                ReadOutcome::Lost(message) => {
                    handle_connection_lost(&state, &message);
                    return;
                }
            }
        }
    });
}
