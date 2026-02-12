use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen_futures::spawn_local;

use crate::decoder::read_packet;
use crate::transport::WasmReader;
use mqtt5_protocol::packet::Packet;

use super::callbacks::handle_connection_lost;
use super::handlers::handle_incoming_packet;
use super::state::ClientState;

pub fn spawn_packet_reader(state: Rc<RefCell<ClientState>>, mut reader: WasmReader) {
    let generation = state.borrow().connection_generation;
    spawn_local(async move {
        let disconnect_reason = loop {
            match read_packet(&mut reader).await {
                Ok(packet) => {
                    if state.borrow().connection_generation != generation {
                        break None;
                    }
                    if matches!(&packet, Packet::Disconnect(_)) {
                        break Some("Server sent DISCONNECT".to_string());
                    }
                    handle_incoming_packet(&state, packet);
                }
                Err(e) => {
                    break Some(format!("Packet read error: {e}"));
                }
            }
        };

        if state.borrow().connection_generation != generation {
            return;
        }

        if let Some(reason) = disconnect_reason {
            handle_connection_lost(&state, &reason);
        }
    });
}
