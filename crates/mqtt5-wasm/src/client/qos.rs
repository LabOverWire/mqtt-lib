use mqtt5_protocol::QoS;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use super::sleep_ms;
use super::state::ClientState;

pub fn create_ack_promises(
    state: &Rc<RefCell<ClientState>>,
    qos: QoS,
    packet_id: Option<u16>,
) -> (Option<js_sys::Promise>, Option<js_sys::Promise>) {
    let puback_promise = if qos == QoS::AtLeastOnce {
        packet_id.map(|pid| {
            let state = Rc::clone(state);
            js_sys::Promise::new(&mut move |resolve, _reject| {
                state.borrow_mut().pending_pubacks.insert(pid, resolve);
            })
        })
    } else {
        None
    };

    let pubcomp_promise = if qos == QoS::ExactlyOnce {
        packet_id.map(|pid| {
            let now = js_sys::Date::now();
            let state = Rc::clone(state);
            js_sys::Promise::new(&mut move |resolve, _reject| {
                state
                    .borrow_mut()
                    .pending_pubcomps
                    .insert(pid, (resolve, now));
            })
        })
    } else {
        None
    };

    (puback_promise, pubcomp_promise)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub async fn await_ack_promises(
    puback_promise: Option<js_sys::Promise>,
    pubcomp_promise: Option<js_sys::Promise>,
) -> Result<(), JsValue> {
    if let Some(promise) = puback_promise {
        let result = JsFuture::from(promise).await?;
        let reason_code = result.as_f64().unwrap_or(0.0) as u8;
        if reason_code >= 0x80 {
            return Err(JsValue::from_str(&format!(
                "Publish rejected with reason code: {reason_code}"
            )));
        }
    }

    if let Some(promise) = pubcomp_promise {
        let result = JsFuture::from(promise).await?;
        let reason_code = result.as_f64().unwrap_or(0.0) as u8;
        if reason_code >= 0x80 {
            return Err(JsValue::from_str(&format!(
                "Publish rejected with reason code: {reason_code}"
            )));
        }
    }

    Ok(())
}

pub fn spawn_qos2_cleanup_task(state: Rc<RefCell<ClientState>>) {
    wasm_bindgen_futures::spawn_local(async move {
        loop {
            sleep_ms(5000).await;

            let connected = match state.try_borrow() {
                Ok(state_ref) => state_ref.connected,
                Err(_) => continue,
            };

            if !connected {
                break;
            }

            let now = js_sys::Date::now();
            let timeout_ms = 10000.0;
            let cleanup_ms = 30000.0;

            if let Ok(mut state_ref) = state.try_borrow_mut() {
                let mut timed_out_pubcomps = Vec::new();

                for (packet_id, (callback, timestamp)) in &state_ref.pending_pubcomps {
                    if now - timestamp > timeout_ms {
                        timed_out_pubcomps.push((*packet_id, callback.clone()));
                    }
                }

                for (packet_id, callback) in timed_out_pubcomps {
                    state_ref.pending_pubcomps.remove(&packet_id);
                    web_sys::console::warn_1(
                        &format!("QoS 2 publish timeout for packet {packet_id}").into(),
                    );
                    let error = JsValue::from_str("Timeout");
                    if let Err(e) = callback.call1(&JsValue::NULL, &error) {
                        web_sys::console::error_1(
                            &format!("QoS 2 timeout callback error: {e:?}").into(),
                        );
                    }
                }

                state_ref
                    .pending_pubrecs
                    .retain(|_packet_id, timestamp| now - *timestamp <= cleanup_ms);

                state_ref
                    .received_qos2
                    .retain(|_packet_id, timestamp| now - *timestamp <= cleanup_ms);
            }
        }
    });
}
