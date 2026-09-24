use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::pubrel::PubRelPacket;
use mqtt5_protocol::packet::Packet;
use mqtt5_protocol::QoS;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use super::outbound::check_server_limits;
use super::packet::write_packet;
use super::sleep_ms;
use super::state::ClientState;

const QOS2_CALLBACK_TIMEOUT_MS: f64 = 10_000.0;
const INDETERMINATE: &str = "indeterminate: may have been delivered";
const SESSION_LOST: &str = "indeterminate: session lost; may have been delivered";

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

pub async fn await_ack_promises(
    puback_promise: Option<js_sys::Promise>,
    pubcomp_promise: Option<js_sys::Promise>,
) -> Result<(), JsValue> {
    for promise in [puback_promise, pubcomp_promise].into_iter().flatten() {
        let result = JsFuture::from(promise).await?;
        match result.as_f64() {
            Some(reason_code) if reason_code >= 128.0 => {
                return Err(JsValue::from_str(&format!(
                    "Publish rejected with reason code: {reason_code}"
                )));
            }
            Some(_) => {}
            None => {
                return Err(JsValue::from_str(&format!(
                    "Publish not acknowledged: {}",
                    result.as_string().unwrap_or_default()
                )));
            }
        }
    }
    Ok(())
}

fn stored_copy(state: &ClientState, publish: &PublishPacket) -> PublishPacket {
    let mut stored = publish.clone();
    if let Some(alias) = stored.topic_alias() {
        if stored.topic_name.is_empty() {
            if let Some(topic) = state.outbound_aliases.get_topic(alias) {
                stored.topic_name = topic.to_string();
            }
        }
        stored.properties.remove_topic_alias();
    }
    stored
}

pub async fn reserve_flight(
    state: &Rc<RefCell<ClientState>>,
    publish: &PublishPacket,
    generation: u32,
) -> Result<u16, JsValue> {
    loop {
        {
            let mut state_mut = state.borrow_mut();
            if !state_mut.connected {
                return Err(JsValue::from_str("Not connected"));
            }
            if state_mut.connection_generation != generation {
                return Err(JsValue::from_str(
                    "Connection changed before the message was sent",
                ));
            }
            if state_mut.send_quota > 0 {
                let packet_id = state_mut
                    .allocate_packet_id()
                    .ok_or_else(|| JsValue::from_str("No packet identifier available"))?;
                state_mut.send_quota -= 1;
                let mut stored = stored_copy(&state_mut, publish);
                stored.packet_id = Some(packet_id);
                state_mut.record_flight(packet_id, stored);
                return Ok(packet_id);
            }
        }
        let waiting_state = Rc::clone(state);
        let promise = js_sys::Promise::new(&mut move |resolve, _reject| {
            waiting_state.borrow_mut().quota_waiters.push_back(resolve);
        });
        JsFuture::from(promise).await?;
    }
}

pub fn abandon_flight(state: &Rc<RefCell<ClientState>>, packet_id: u16) {
    let removed = state.borrow_mut().outbound.remove(&packet_id).is_some();
    if removed {
        release_quota(state);
    }
}

pub fn release_quota(state: &Rc<RefCell<ClientState>>) {
    let (resend, waiter) = {
        let mut state_mut = state.borrow_mut();
        if state_mut.quota_debt > 0 {
            state_mut.quota_debt -= 1;
            return;
        }
        state_mut.send_quota = state_mut
            .send_quota
            .saturating_add(1)
            .min(state_mut.server.receive_maximum);
        let mut resend = None;
        while let Some(packet_id) = state_mut.pending_resends.pop_front() {
            if let Some(flight) = state_mut.outbound.get_mut(&packet_id) {
                resend = Some(flight.publish.clone().with_dup(flight.transmitted));
                flight.transmitted = true;
                break;
            }
        }
        match resend {
            Some(publish) => {
                state_mut.send_quota -= 1;
                (Some(publish), None)
            }
            None => (None, state_mut.quota_waiters.pop_front()),
        }
    };
    if let Some(publish) = resend {
        if let Err(e) = write_packet(state, &Packet::Publish(publish)) {
            tracing::warn!(error = %e, "resending PUBLISH failed");
        }
    }
    if let Some(waiter) = waiter {
        if let Err(e) = waiter.call0(&JsValue::NULL) {
            tracing::warn!(error = ?e, "send quota waiter failed");
        }
    }
}

pub fn wake_quota_waiters(state: &Rc<RefCell<ClientState>>) {
    let waiters: Vec<js_sys::Function> = state.borrow_mut().quota_waiters.drain(..).collect();
    for waiter in waiters {
        if let Err(e) = waiter.call0(&JsValue::NULL) {
            tracing::warn!(error = ?e, "send quota waiter failed");
        }
    }
}

pub fn resume_session(state: &Rc<RefCell<ClientState>>, session_present: bool) {
    let settlements = {
        let mut state_mut = state.borrow_mut();
        if !session_present {
            state_mut.awaiting_pubrel.clear();
            state_mut.quarantined.clear();
        }
        let mut settlements = Vec::new();
        for packet_id in state_mut.flights_in_order() {
            let Some(verdict) = session_verdict(&mut state_mut, packet_id, session_present) else {
                continue;
            };
            state_mut.outbound.remove(&packet_id);
            if verdict.quarantine {
                state_mut.quarantined.insert(packet_id);
            }
            tracing::warn!(packet_id, reason = %verdict.message, "unacknowledged PUBLISH abandoned");
            if let Some(callback) = state_mut.take_ack_callback(packet_id) {
                settlements.push((callback, verdict.message));
            }
        }
        settlements
    };
    resend_session(state);
    for (callback, message) in settlements {
        if let Err(e) = callback.call1(&JsValue::NULL, &JsValue::from_str(&message)) {
            tracing::warn!(error = ?e, "acknowledgement callback failed");
        }
    }
}

struct Abandon {
    message: String,
    quarantine: bool,
}

fn session_verdict(
    state: &mut ClientState,
    packet_id: u16,
    session_present: bool,
) -> Option<Abandon> {
    let flight = state.outbound.get_mut(&packet_id)?;
    let not_resent = |reason: String, prefix: &str| {
        format!(
            "{prefix}; not resent because the new connection's limits do not allow it ({reason})"
        )
    };
    match (session_present, flight.publish.qos, flight.released) {
        (true, _, true) => None,
        (true, qos, false) => check_server_limits(&state.server, &flight.publish)
            .err()
            .map(|reason| Abandon {
                message: not_resent(reason, INDETERMINATE),
                quarantine: qos == QoS::ExactlyOnce,
            }),
        (false, QoS::ExactlyOnce, _) => Some(Abandon {
            message: SESSION_LOST.to_string(),
            quarantine: false,
        }),
        (false, _, _) => {
            flight.transmitted = false;
            check_server_limits(&state.server, &flight.publish)
                .err()
                .map(|reason| Abandon {
                    message: not_resent(reason, SESSION_LOST),
                    quarantine: false,
                })
        }
    }
}

fn resend_session(state: &Rc<RefCell<ClientState>>) {
    let packets = {
        let mut state_mut = state.borrow_mut();
        let mut packets = Vec::with_capacity(state_mut.outbound.len());
        for packet_id in state_mut.flights_in_order() {
            let mut quota = state_mut.send_quota;
            let Some(flight) = state_mut.outbound.get_mut(&packet_id) else {
                continue;
            };
            if flight.released {
                packets.push(Packet::PubRel(PubRelPacket::new(packet_id)));
                if quota > 0 {
                    quota -= 1;
                } else {
                    state_mut.quota_debt = state_mut.quota_debt.saturating_add(1);
                }
            } else if quota > 0 {
                packets.push(Packet::Publish(
                    flight.publish.clone().with_dup(flight.transmitted),
                ));
                flight.transmitted = true;
                quota -= 1;
            } else {
                state_mut.pending_resends.push_back(packet_id);
            }
            state_mut.send_quota = quota;
        }
        packets
    };
    for packet in packets {
        if let Err(e) = write_packet(state, &packet) {
            tracing::warn!(error = %e, "resending session state failed");
        }
    }
}

pub fn spawn_qos2_cleanup_task(state: Rc<RefCell<ClientState>>) {
    let generation = state.borrow().connection_generation;
    wasm_bindgen_futures::spawn_local(async move {
        loop {
            sleep_ms(5000).await;

            let timed_out = match state.try_borrow_mut() {
                Ok(mut state_ref) => {
                    if state_ref.connection_generation != generation || !state_ref.connected {
                        break;
                    }
                    let now = js_sys::Date::now();
                    let expired: Vec<u16> = state_ref
                        .pending_pubcomps
                        .iter()
                        .filter(|(_, (_, timestamp))| now - timestamp > QOS2_CALLBACK_TIMEOUT_MS)
                        .map(|(packet_id, _)| *packet_id)
                        .collect();
                    expired
                        .into_iter()
                        .filter_map(|packet_id| {
                            state_ref
                                .pending_pubcomps
                                .remove(&packet_id)
                                .map(|(callback, _)| (packet_id, callback))
                        })
                        .collect::<Vec<_>>()
                }
                Err(_) => continue,
            };

            for (packet_id, callback) in timed_out {
                tracing::warn!(packet_id, "QoS 2 publish acknowledgement timed out");
                if let Err(e) = callback.call1(&JsValue::NULL, &JsValue::from_str("Timeout")) {
                    tracing::warn!(error = ?e, "QoS 2 timeout callback failed");
                }
            }
        }
    });
}
