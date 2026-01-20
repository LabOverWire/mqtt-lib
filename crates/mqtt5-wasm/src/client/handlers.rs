use bytes::BytesMut;
use mqtt5_protocol::packet::Packet;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::config::WasmMessageProperties;

use super::packet::encode_packet;
use super::state::ClientState;
use super::RustMessage;

#[cfg(feature = "codec")]
use crate::codec::WasmCodecRegistry;

#[allow(clippy::too_many_lines)]
pub fn handle_incoming_packet(state: &Rc<RefCell<ClientState>>, packet: Packet) {
    match packet {
        Packet::ConnAck(connack) => {
            let callback = state.borrow().on_connect.clone();
            if let Some(callback) = callback {
                let reason_code = JsValue::from_f64(f64::from(connack.reason_code as u8));
                let session_present = JsValue::from_bool(connack.session_present);

                if let Err(e) = callback.call2(&JsValue::NULL, &reason_code, &session_present) {
                    web_sys::console::error_1(&format!("onConnect callback error: {e:?}").into());
                }
            }
        }
        Packet::Publish(publish) => {
            let topic = publish.topic_name.clone();
            let payload = publish.payload.clone();
            let qos = publish.qos;
            let retain = publish.retain;
            let properties: mqtt5_protocol::types::MessageProperties =
                publish.properties.clone().into();

            if qos == mqtt5_protocol::QoS::ExactlyOnce {
                if let Some(packet_id) = publish.packet_id {
                    let is_duplicate = state.borrow().received_qos2.contains_key(&packet_id);
                    let actions =
                        mqtt5_protocol::qos2::handle_incoming_publish_qos2(packet_id, is_duplicate);

                    for action in actions {
                        match action {
                            mqtt5_protocol::qos2::QoS2Action::DeliverMessage { packet_id: _ } => {
                                deliver_message(state, &topic, &payload, qos, retain, &properties);
                            }
                            mqtt5_protocol::qos2::QoS2Action::SendPubRec {
                                packet_id,
                                reason_code,
                            } => {
                                send_pubrec(state, packet_id, reason_code);
                            }
                            mqtt5_protocol::qos2::QoS2Action::TrackIncomingPubRec { packet_id } => {
                                let now = js_sys::Date::now();
                                state.borrow_mut().pending_pubrecs.insert(packet_id, now);
                                state.borrow_mut().received_qos2.insert(packet_id, now);
                            }
                            _ => {}
                        }
                    }
                } else {
                    web_sys::console::error_1(&"QoS 2 PUBLISH missing packet_id".into());
                }
            } else {
                deliver_message(state, &topic, &payload, qos, retain, &properties);
            }
        }
        Packet::SubAck(suback) => {
            let callback = state.borrow_mut().pending_subacks.remove(&suback.packet_id);
            if let Some(callback) = callback {
                let reason_codes = suback
                    .reason_codes
                    .iter()
                    .map(|rc| JsValue::from_f64(f64::from(*rc as u8)))
                    .collect::<js_sys::Array>();

                if let Err(e) = callback.call1(&JsValue::NULL, &reason_codes.into()) {
                    web_sys::console::error_1(&format!("SUBACK callback error: {e:?}").into());
                }
            }
        }
        Packet::UnsubAck(_unsuback) => {}
        Packet::PingResp => {
            state.borrow_mut().last_pong_received = Some(js_sys::Date::now());
        }
        Packet::PubAck(puback) => {
            let callback = state.borrow_mut().pending_pubacks.remove(&puback.packet_id);
            if let Some(callback) = callback {
                let reason_code = JsValue::from_f64(f64::from(puback.reason_code as u8));
                if let Err(e) = callback.call1(&JsValue::NULL, &reason_code) {
                    web_sys::console::error_1(&format!("PUBACK callback error: {e:?}").into());
                }
            }
        }
        Packet::PubRec(pubrec) => {
            handle_pubrec(state, pubrec.packet_id, pubrec.reason_code);
        }
        Packet::PubComp(pubcomp) => {
            handle_pubcomp(state, pubcomp.packet_id, pubcomp.reason_code);
        }
        Packet::PubRel(pubrel) => {
            handle_pubrel(state, pubrel.packet_id);
        }
        Packet::Auth(auth) => {
            let reason_code = auth.reason_code;

            if reason_code
                == mqtt5_protocol::protocol::v5::reason_codes::ReasonCode::ContinueAuthentication
            {
                let callback = state.borrow().on_auth_challenge.clone();
                if let Some(callback) = callback {
                    let auth_method = auth
                        .properties
                        .get_authentication_method()
                        .cloned()
                        .unwrap_or_default();
                    let auth_data = auth.properties.get_authentication_data();

                    let method_js = JsValue::from_str(&auth_method);
                    let data_js = if let Some(data) = auth_data {
                        js_sys::Uint8Array::from(data).into()
                    } else {
                        JsValue::NULL
                    };

                    if let Err(e) = callback.call2(&JsValue::NULL, &method_js, &data_js) {
                        web_sys::console::error_1(
                            &format!("onAuthChallenge callback error: {e:?}").into(),
                        );
                    }
                }
            } else if reason_code == mqtt5_protocol::protocol::v5::reason_codes::ReasonCode::Success
            {
                web_sys::console::log_1(&"Authentication successful".into());
            }
        }
        _ => {
            web_sys::console::warn_1(&format!("Unhandled packet type: {packet:?}").into());
        }
    }
}

fn deliver_message(
    state: &Rc<RefCell<ClientState>>,
    topic: &str,
    payload: &[u8],
    qos: mqtt5_protocol::QoS,
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
                web_sys::console::error_1(&format!("Callback error: {e:?}").into());
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

fn send_pubrec(
    state: &Rc<RefCell<ClientState>>,
    packet_id: u16,
    reason_code: mqtt5_protocol::protocol::v5::reason_codes::ReasonCode,
) {
    let pubrec =
        mqtt5_protocol::packet::pubrec::PubRecPacket::new_with_reason(packet_id, reason_code);
    let mut buf = BytesMut::new();
    if let Err(e) = encode_packet(&mqtt5_protocol::packet::Packet::PubRec(pubrec), &mut buf) {
        web_sys::console::error_1(&format!("PUBREC encode error: {e}").into());
        return;
    }

    let writer_rc = state.borrow().writer.clone();
    if let Some(writer_rc) = writer_rc {
        spawn_local(async move {
            if let Err(e) = writer_rc.borrow_mut().write(&buf) {
                web_sys::console::error_1(&format!("PUBREC send error: {e}").into());
            }
        });
    }
}

fn handle_pubrec(
    state: &Rc<RefCell<ClientState>>,
    packet_id: u16,
    reason_code: mqtt5_protocol::protocol::v5::reason_codes::ReasonCode,
) {
    let has_pending = state.borrow().pending_pubcomps.contains_key(&packet_id);
    let actions = mqtt5_protocol::qos2::handle_incoming_pubrec(packet_id, reason_code, has_pending);

    for action in actions {
        match action {
            mqtt5_protocol::qos2::QoS2Action::SendPubRel { packet_id } => {
                let pubrel_packet = mqtt5_protocol::packet::pubrel::PubRelPacket::new(packet_id);
                let mut buf = BytesMut::new();
                if let Err(e) = encode_packet(
                    &mqtt5_protocol::packet::Packet::PubRel(pubrel_packet),
                    &mut buf,
                ) {
                    web_sys::console::error_1(&format!("PUBREL encode error: {e}").into());
                    continue;
                }

                let writer_rc = state.borrow().writer.clone();
                if let Some(writer_rc) = writer_rc {
                    spawn_local(async move {
                        if let Err(e) = writer_rc.borrow_mut().write(&buf) {
                            web_sys::console::error_1(&format!("PUBREL send error: {e}").into());
                        }
                    });
                }
            }
            mqtt5_protocol::qos2::QoS2Action::ErrorFlow {
                packet_id,
                reason_code,
            } => {
                if let Some((callback, _)) = state.borrow_mut().pending_pubcomps.remove(&packet_id)
                {
                    let reason_code_js = JsValue::from_f64(f64::from(reason_code as u8));
                    if let Err(e) = callback.call1(&JsValue::NULL, &reason_code_js) {
                        web_sys::console::error_1(
                            &format!("QoS 2 error callback error: {e:?}").into(),
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

fn handle_pubcomp(
    state: &Rc<RefCell<ClientState>>,
    packet_id: u16,
    reason_code: mqtt5_protocol::protocol::v5::reason_codes::ReasonCode,
) {
    let has_pending = state.borrow().pending_pubcomps.contains_key(&packet_id);
    let actions =
        mqtt5_protocol::qos2::handle_incoming_pubcomp(packet_id, reason_code, has_pending);

    for action in actions {
        match action {
            mqtt5_protocol::qos2::QoS2Action::CompleteFlow { packet_id } => {
                if let Some((callback, _)) = state.borrow_mut().pending_pubcomps.remove(&packet_id)
                {
                    let reason_code_js = JsValue::from_f64(f64::from(reason_code as u8));
                    if let Err(e) = callback.call1(&JsValue::NULL, &reason_code_js) {
                        web_sys::console::error_1(&format!("PUBCOMP callback error: {e:?}").into());
                    }
                }
            }
            mqtt5_protocol::qos2::QoS2Action::ErrorFlow {
                packet_id,
                reason_code,
            } => {
                if let Some((callback, _)) = state.borrow_mut().pending_pubcomps.remove(&packet_id)
                {
                    let reason_code_js = JsValue::from_f64(f64::from(reason_code as u8));
                    if let Err(e) = callback.call1(&JsValue::NULL, &reason_code_js) {
                        web_sys::console::error_1(
                            &format!("QoS 2 error callback error: {e:?}").into(),
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

fn handle_pubrel(state: &Rc<RefCell<ClientState>>, packet_id: u16) {
    let has_pubrec = state.borrow().pending_pubrecs.contains_key(&packet_id);
    let actions = mqtt5_protocol::qos2::handle_incoming_pubrel(packet_id, has_pubrec);

    for action in actions {
        match action {
            mqtt5_protocol::qos2::QoS2Action::RemoveIncomingPubRec { packet_id } => {
                state.borrow_mut().pending_pubrecs.remove(&packet_id);
            }
            mqtt5_protocol::qos2::QoS2Action::SendPubComp {
                packet_id,
                reason_code,
            } => {
                let pubcomp = mqtt5_protocol::packet::pubcomp::PubCompPacket::new_with_reason(
                    packet_id,
                    reason_code,
                );
                let mut buf = BytesMut::new();
                if let Err(e) =
                    encode_packet(&mqtt5_protocol::packet::Packet::PubComp(pubcomp), &mut buf)
                {
                    web_sys::console::error_1(&format!("PUBCOMP encode error: {e}").into());
                    continue;
                }

                let writer_rc = state.borrow().writer.clone();
                if let Some(writer_rc) = writer_rc {
                    spawn_local(async move {
                        if let Err(e) = writer_rc.borrow_mut().write(&buf) {
                            web_sys::console::error_1(&format!("PUBCOMP send error: {e}").into());
                        }
                    });
                }
            }
            _ => {}
        }
    }
}
