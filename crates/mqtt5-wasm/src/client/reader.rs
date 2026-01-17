use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen_futures::spawn_local;

use crate::decoder::read_packet;
use crate::transport::WasmReader;

use super::callbacks::{trigger_disconnect_callback, trigger_error_callback};
use super::handlers::handle_incoming_packet;
use super::reconnect::spawn_reconnection_task;
use super::sleep_ms;
use super::state::ClientState;

pub fn spawn_packet_reader(state: Rc<RefCell<ClientState>>, mut reader: WasmReader) {
    spawn_local(async move {
        loop {
            let packet_result = read_packet(&mut reader).await;

            match packet_result {
                Ok(packet) => {
                    handle_incoming_packet(&state, packet);
                }
                Err(e) => {
                    let (was_connected, should_reconnect) = loop {
                        match state.try_borrow_mut() {
                            Ok(mut state_ref) => {
                                let connected = state_ref.connected;
                                state_ref.connected = false;
                                let should = state_ref.reconnect_config.enabled
                                    && !state_ref.user_initiated_disconnect
                                    && !state_ref.reconnecting
                                    && state_ref.last_url.is_some();
                                break (connected, should);
                            }
                            Err(_) => {
                                sleep_ms(10).await;
                            }
                        }
                    };

                    if was_connected {
                        let error_msg = format!("Packet read error: {e}");
                        web_sys::console::error_1(&error_msg.clone().into());
                        trigger_error_callback(&state, &error_msg);
                        trigger_disconnect_callback(&state);

                        if should_reconnect {
                            spawn_reconnection_task(Rc::clone(&state));
                        }
                    }
                    break;
                }
            }
        }
    });
}
