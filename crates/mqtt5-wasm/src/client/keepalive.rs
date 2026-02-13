use bytes::BytesMut;
use mqtt5_protocol::packet::Packet;
use mqtt5_protocol::time::Duration;
use mqtt5_protocol::u128_to_u32_saturating;
use mqtt5_protocol::KeepaliveConfig;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen_futures::spawn_local;

use super::callbacks::handle_connection_lost;
use super::packet::encode_packet;
use super::sleep_ms;
use super::state::ClientState;

pub fn spawn_keepalive_task(state: Rc<RefCell<ClientState>>) {
    let keepalive_config = KeepaliveConfig::conservative();
    let generation = state.borrow().connection_generation;

    spawn_local(async move {
        loop {
            let (keepalive_duration, connected) = if let Ok(state_ref) = state.try_borrow() {
                let duration = Duration::from_secs(u64::from(state_ref.keep_alive));
                let conn = state_ref.connected;
                (duration, conn)
            } else {
                sleep_ms(100).await;
                continue;
            };

            if !connected {
                break;
            }

            let ping_interval = keepalive_config.ping_interval(keepalive_duration);
            let sleep_duration = u128_to_u32_saturating(ping_interval.as_millis());
            sleep_ms(sleep_duration).await;

            if state.borrow().connection_generation != generation {
                break;
            }

            let timeout_duration = keepalive_config.timeout_duration(keepalive_duration);
            let timeout_ms = crate::utils::u128_to_f64_saturating(timeout_duration.as_millis());

            let should_disconnect = if let Ok(state_ref) = state.try_borrow() {
                if !state_ref.connected {
                    break;
                }

                let now = js_sys::Date::now();

                if let Some(last_ping) = state_ref.last_ping_sent {
                    let pong_received = state_ref
                        .last_pong_received
                        .is_some_and(|pong| pong > last_ping);
                    !pong_received && (now - last_ping) > timeout_ms
                } else {
                    false
                }
            } else {
                sleep_ms(100).await;
                continue;
            };

            if should_disconnect {
                handle_connection_lost(&state, "Keepalive timeout");
                break;
            }

            let packet = Packet::PingReq;
            let mut buf = BytesMut::new();
            if let Err(e) = encode_packet(&packet, &mut buf) {
                web_sys::console::error_1(&format!("Ping encode error: {e}").into());
                continue;
            }

            state.borrow_mut().last_ping_sent = Some(js_sys::Date::now());

            let writer_rc = {
                let state_ref = state.borrow();
                state_ref.writer.as_ref().map(Rc::clone)
            };

            match writer_rc {
                Some(writer_rc) => match writer_rc.borrow_mut().write(&buf) {
                    Ok(()) => {}
                    Err(e) => {
                        let error_msg = format!("Ping send error: {e}");
                        handle_connection_lost(&state, &error_msg);
                        break;
                    }
                },
                None => {
                    break;
                }
            }
        }
    });
}
