use mqtt5_protocol::packet::puback::PubAckPacket;
use mqtt5_protocol::packet::pubcomp::PubCompPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::pubrec::PubRecPacket;
use mqtt5_protocol::packet::pubrel::PubRelPacket;
use mqtt5_protocol::packet::Packet;
use mqtt5_protocol::protocol::v5::properties::{Properties, PropertyId};
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use mqtt5_protocol::QoS;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;

use crate::config::WasmMessageProperties;

use super::packet::write_packet;
use super::qos::release_quota;
use super::state::ClientState;
use super::RustMessage;

#[cfg(feature = "codec")]
use crate::codec::WasmCodecRegistry;

#[derive(Debug)]
pub struct Violation {
    pub reason_code: ReasonCode,
    pub message: String,
}

impl Violation {
    pub fn new(reason_code: ReasonCode, message: impl Into<String>) -> Self {
        Self {
            reason_code,
            message: message.into(),
        }
    }

    fn protocol(message: impl Into<String>) -> Self {
        Self::new(ReasonCode::ProtocolError, message)
    }
}

pub fn handle_incoming_packet(
    state: &Rc<RefCell<ClientState>>,
    packet: Packet,
) -> Result<(), Violation> {
    check_problem_information(state, &packet)?;
    match packet {
        Packet::Publish(publish) => handle_publish(state, &publish),
        Packet::SubAck(suback) => {
            let callback = state.borrow_mut().pending_subacks.remove(&suback.packet_id);
            if let Some(Some(callback)) = callback {
                let reason_codes = suback
                    .reason_codes
                    .iter()
                    .map(|rc| JsValue::from_f64(f64::from(*rc as u8)))
                    .collect::<js_sys::Array>();
                if let Err(e) = callback.call1(&JsValue::NULL, &reason_codes.into()) {
                    tracing::warn!(error = ?e, "SUBACK callback failed");
                }
            }
            Ok(())
        }
        Packet::UnsubAck(unsuback) => {
            state
                .borrow_mut()
                .pending_unsubacks
                .remove(&unsuback.packet_id);
            Ok(())
        }
        Packet::PingResp => {
            state.borrow_mut().last_pong_received = Some(js_sys::Date::now());
            Ok(())
        }
        Packet::PubAck(puback) => {
            handle_puback(state, puback.packet_id, puback.reason_code);
            Ok(())
        }
        Packet::PubRec(pubrec) => {
            handle_pubrec(state, pubrec.packet_id, pubrec.reason_code);
            Ok(())
        }
        Packet::PubComp(pubcomp) => {
            handle_pubcomp(state, pubcomp.packet_id, pubcomp.reason_code);
            Ok(())
        }
        Packet::PubRel(pubrel) => {
            handle_pubrel(state, pubrel.packet_id);
            Ok(())
        }
        Packet::Auth(auth) => handle_auth(state, &auth),
        other => Err(Violation::protocol(format!(
            "A client must not receive {}",
            other.packet_type_name()
        ))),
    }
}

fn check_problem_information(
    state: &Rc<RefCell<ClientState>>,
    packet: &Packet,
) -> Result<(), Violation> {
    if state.borrow().client_limits.request_problem_information {
        return Ok(());
    }
    let properties = match packet {
        Packet::PubAck(p) => &p.properties,
        Packet::PubRec(p) => &p.properties,
        Packet::PubRel(p) => &p.properties,
        Packet::PubComp(p) => &p.properties,
        Packet::SubAck(p) => &p.properties,
        Packet::UnsubAck(p) => &p.properties,
        Packet::Auth(p) => &p.properties,
        _ => return Ok(()),
    };
    if carries_problem_information(properties) {
        return Err(Violation::protocol(format!(
            "{} carries a Reason String or User Property although Request Problem Information is 0",
            packet.packet_type_name()
        )));
    }
    Ok(())
}

fn carries_problem_information(properties: &Properties) -> bool {
    properties.contains(PropertyId::ReasonString) || properties.contains(PropertyId::UserProperty)
}

pub fn handle_auth(
    state: &Rc<RefCell<ClientState>>,
    auth: &mqtt5_protocol::packet::auth::AuthPacket,
) -> Result<(), Violation> {
    let (auth_method, callback) = {
        let state_ref = state.borrow();
        (
            state_ref.auth_method.clone(),
            state_ref.on_auth_challenge.clone(),
        )
    };
    let Some(auth_method) = auth_method else {
        return Err(Violation::protocol(
            "AUTH received but CONNECT carried no Authentication Method",
        ));
    };
    if auth.properties.get_authentication_method() != Some(&auth_method) {
        return Err(Violation::protocol(
            "AUTH Authentication Method differs from CONNECT",
        ));
    }
    match auth.reason_code {
        ReasonCode::ContinueAuthentication => {
            let Some(callback) = callback else {
                return Err(Violation::new(
                    ReasonCode::ImplementationSpecificError,
                    "AUTH challenge received but no onAuthChallenge callback set",
                ));
            };
            let data_js = auth
                .properties
                .get_authentication_data()
                .map_or(JsValue::NULL, |data| js_sys::Uint8Array::from(data).into());
            if let Err(e) =
                callback.call2(&JsValue::NULL, &JsValue::from_str(&auth_method), &data_js)
            {
                tracing::warn!(error = ?e, "onAuthChallenge callback failed");
            }
            Ok(())
        }
        ReasonCode::Success => {
            tracing::debug!("re-authentication succeeded");
            Ok(())
        }
        other => Err(Violation::protocol(format!(
            "Unexpected AUTH reason code {other:?}"
        ))),
    }
}

fn resolve_topic(
    state: &Rc<RefCell<ClientState>>,
    publish: &PublishPacket,
) -> Result<String, Violation> {
    let mut state_mut = state.borrow_mut();
    match publish.topic_alias() {
        None if publish.topic_name.is_empty() => Err(Violation::protocol(
            "PUBLISH without Topic Name or Topic Alias",
        )),
        None => Ok(publish.topic_name.clone()),
        Some(alias) if alias == 0 || alias > state_mut.client_limits.topic_alias_maximum => {
            Err(Violation::new(
                ReasonCode::TopicAliasInvalid,
                format!(
                    "Topic Alias {alias} outside 1..={}",
                    state_mut.client_limits.topic_alias_maximum
                ),
            ))
        }
        Some(alias) if publish.topic_name.is_empty() => state_mut
            .inbound_aliases
            .get_topic(alias)
            .map(str::to_string)
            .ok_or_else(|| Violation::protocol(format!("Topic Alias {alias} has no mapping"))),
        Some(alias) => {
            state_mut
                .inbound_aliases
                .register_alias(alias, &publish.topic_name)
                .map_err(|e| Violation::new(ReasonCode::TopicAliasInvalid, e.to_string()))?;
            Ok(publish.topic_name.clone())
        }
    }
}

fn send_ack(state: &Rc<RefCell<ClientState>>, packet: &Packet) {
    if let Err(e) = write_packet(state, packet) {
        tracing::warn!(error = %e, packet = packet.packet_type_name(), "acknowledgement not sent");
    }
}

fn handle_publish(
    state: &Rc<RefCell<ClientState>>,
    publish: &PublishPacket,
) -> Result<(), Violation> {
    let topic = resolve_topic(state, publish)?;
    let properties: mqtt5_protocol::types::MessageProperties = publish.properties.clone().into();

    match (publish.qos, publish.packet_id) {
        (QoS::AtMostOnce, _) => {
            deliver_message(
                state,
                &topic,
                &publish.payload,
                publish.qos,
                publish.retain,
                &properties,
            );
            Ok(())
        }
        (QoS::AtLeastOnce, Some(packet_id)) => {
            deliver_message(
                state,
                &topic,
                &publish.payload,
                publish.qos,
                publish.retain,
                &properties,
            );
            send_ack(state, &Packet::PubAck(PubAckPacket::new(packet_id)));
            Ok(())
        }
        (QoS::ExactlyOnce, Some(packet_id)) => {
            let is_new = {
                let mut state_mut = state.borrow_mut();
                if state_mut.awaiting_pubrel.contains(&packet_id) {
                    false
                } else if state_mut.awaiting_pubrel.len()
                    >= usize::from(state_mut.client_limits.receive_maximum)
                {
                    return Err(Violation::new(
                        ReasonCode::ReceiveMaximumExceeded,
                        "Server exceeded the client Receive Maximum",
                    ));
                } else {
                    state_mut.awaiting_pubrel.insert(packet_id);
                    true
                }
            };
            if is_new {
                deliver_message(
                    state,
                    &topic,
                    &publish.payload,
                    publish.qos,
                    publish.retain,
                    &properties,
                );
            }
            send_ack(state, &Packet::PubRec(PubRecPacket::new(packet_id)));
            Ok(())
        }
        (_, None) => Err(Violation::new(
            ReasonCode::MalformedPacket,
            "QoS > 0 PUBLISH without Packet Identifier",
        )),
    }
}

fn deliver_message(
    state: &Rc<RefCell<ClientState>>,
    topic: &str,
    payload: &[u8],
    qos: QoS,
    retain: bool,
    properties: &mqtt5_protocol::types::MessageProperties,
) {
    #[cfg(feature = "codec")]
    let decoded_payload = {
        let registry = state.borrow().codec_registry.clone();
        decode_payload_if_needed(
            payload,
            properties.content_type.as_deref(),
            registry.as_ref(),
        )
    };

    #[cfg(not(feature = "codec"))]
    let decoded_payload = payload.to_vec();

    let subscriptions = state.borrow().subscriptions.clone();
    let rust_subscriptions = state.borrow().rust_subscriptions.clone();

    for (filter, callback) in &subscriptions {
        if mqtt5_protocol::validation::topic_matches_filter(topic, filter) {
            let topic_js = JsValue::from_str(topic);
            let payload_array = js_sys::Uint8Array::from(decoded_payload.as_slice());
            let props_js: WasmMessageProperties = properties.clone().into();

            if let Err(e) = callback.call3(
                &JsValue::NULL,
                &topic_js,
                &payload_array.into(),
                &props_js.into(),
            ) {
                tracing::warn!(error = ?e, "message callback failed");
            }
        }
    }

    for (filter, callback) in &rust_subscriptions {
        if mqtt5_protocol::validation::topic_matches_filter(topic, filter) {
            let msg = RustMessage {
                topic: topic.to_string(),
                payload: decoded_payload.clone(),
                qos,
                retain,
                properties: properties.clone(),
            };
            callback(msg);
        }
    }
}

#[cfg(feature = "codec")]
fn decode_payload_if_needed(
    payload: &[u8],
    content_type: Option<&str>,
    registry: Option<&Rc<WasmCodecRegistry>>,
) -> Vec<u8> {
    if let Some(reg) = registry {
        reg.decode_if_needed(payload, content_type)
            .unwrap_or_else(|_| payload.to_vec())
    } else {
        payload.to_vec()
    }
}

fn complete_flight(
    state: &Rc<RefCell<ClientState>>,
    packet_id: u16,
    reason_code: ReasonCode,
    callback: Option<js_sys::Function>,
) {
    release_quota(state);
    if let Some(callback) = callback {
        let reason_code_js = JsValue::from_f64(f64::from(u8::from(reason_code)));
        if let Err(e) = callback.call1(&JsValue::NULL, &reason_code_js) {
            tracing::warn!(error = ?e, packet_id, "publish acknowledgement callback failed");
        }
    }
}

fn handle_puback(state: &Rc<RefCell<ClientState>>, packet_id: u16, reason_code: ReasonCode) {
    let callback = {
        let mut state_mut = state.borrow_mut();
        let acknowledged = state_mut
            .outbound
            .get(&packet_id)
            .is_some_and(|flight| flight.publish.qos == QoS::AtLeastOnce);
        if !acknowledged {
            tracing::debug!(packet_id, "PUBACK for unknown packet identifier");
            return;
        }
        state_mut.outbound.remove(&packet_id);
        state_mut.pending_pubacks.remove(&packet_id)
    };
    complete_flight(state, packet_id, reason_code, callback);
}

enum PubRecOutcome {
    Release,
    Failed(Option<js_sys::Function>),
    Unknown,
}

fn handle_pubrec(state: &Rc<RefCell<ClientState>>, packet_id: u16, reason_code: ReasonCode) {
    let outcome = {
        let mut state_mut = state.borrow_mut();
        let failed = u8::from(reason_code) >= 0x80;
        match state_mut.outbound.get_mut(&packet_id) {
            Some(flight) if flight.publish.qos == QoS::ExactlyOnce && !failed => {
                flight.released = true;
                PubRecOutcome::Release
            }
            Some(flight) if flight.publish.qos == QoS::ExactlyOnce && !flight.released => {
                state_mut.outbound.remove(&packet_id);
                PubRecOutcome::Failed(
                    state_mut
                        .pending_pubcomps
                        .remove(&packet_id)
                        .map(|(callback, _)| callback),
                )
            }
            _ => PubRecOutcome::Unknown,
        }
    };
    match outcome {
        PubRecOutcome::Release => send_ack(state, &Packet::PubRel(PubRelPacket::new(packet_id))),
        PubRecOutcome::Failed(callback) => complete_flight(state, packet_id, reason_code, callback),
        PubRecOutcome::Unknown => send_ack(
            state,
            &Packet::PubRel(PubRelPacket::new_with_reason(
                packet_id,
                ReasonCode::PacketIdentifierNotFound,
            )),
        ),
    }
}

fn handle_pubcomp(state: &Rc<RefCell<ClientState>>, packet_id: u16, reason_code: ReasonCode) {
    let callback = {
        let mut state_mut = state.borrow_mut();
        let released = state_mut
            .outbound
            .get(&packet_id)
            .is_some_and(|flight| flight.released);
        if !released {
            tracing::debug!(packet_id, "PUBCOMP for unknown packet identifier");
            return;
        }
        state_mut.outbound.remove(&packet_id);
        state_mut
            .pending_pubcomps
            .remove(&packet_id)
            .map(|(callback, _)| callback)
    };
    complete_flight(state, packet_id, reason_code, callback);
}

fn handle_pubrel(state: &Rc<RefCell<ClientState>>, packet_id: u16) {
    let known = state.borrow_mut().awaiting_pubrel.remove(&packet_id);
    let reason_code = if known {
        ReasonCode::Success
    } else {
        ReasonCode::PacketIdentifierNotFound
    };
    send_ack(
        state,
        &Packet::PubComp(PubCompPacket::new_with_reason(packet_id, reason_code)),
    );
}
