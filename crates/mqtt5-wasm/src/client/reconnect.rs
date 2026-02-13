use bytes::BytesMut;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::Packet;
use mqtt5_protocol::protocol::v5::properties::Properties;
use mqtt5_protocol::u128_to_u32_saturating;
use mqtt5_protocol::Transport;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen_futures::spawn_local;

use crate::decoder::read_packet;
use crate::transport::WasmTransportType;

use super::callbacks::{trigger_reconnect_failed_callback, trigger_reconnecting_callback};
use super::keepalive::spawn_keepalive_task;
use super::packet::encode_packet;
use super::qos::spawn_qos2_cleanup_task;
use super::reader::spawn_packet_reader;
use super::sleep_ms;
use super::state::{ClientState, StoredConnectOptions};

pub fn spawn_reconnection_task(state: Rc<RefCell<ClientState>>) {
    spawn_local(async move {
        {
            let mut state_ref = state.borrow_mut();
            state_ref.reconnecting = true;
            state_ref.reconnect_attempt = 0;
        }

        loop {
            let (attempt, delay, should_continue, primary_url, options) = {
                let state_ref = state.borrow();
                let attempt = state_ref.reconnect_attempt;
                let base_delay = state_ref.reconnect_config.calculate_delay(attempt);
                let base_delay_ms = u128_to_u32_saturating(base_delay.as_millis());
                let jitter_f64 = js_sys::Math::random() * f64::from(base_delay_ms / 4);
                let jitter = if jitter_f64 >= f64::from(u32::MAX) {
                    u32::MAX
                } else {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let result = jitter_f64 as u32;
                    result
                };
                let delay_ms = base_delay_ms.saturating_add(jitter);
                let should_continue = state_ref.reconnect_config.should_retry(attempt);
                let url = state_ref.last_url.clone();
                let options = state_ref.last_options.clone();
                (attempt, delay_ms, should_continue, url, options)
            };

            if !should_continue {
                trigger_reconnect_failed_callback(&state, "Max reconnection attempts exceeded");
                state.borrow_mut().reconnecting = false;
                return;
            }

            let (Some(primary_url), Some(options)) = (primary_url, options) else {
                trigger_reconnect_failed_callback(&state, "No stored connection parameters");
                state.borrow_mut().reconnecting = false;
                return;
            };

            trigger_reconnecting_callback(&state, attempt + 1, delay);

            sleep_ms(delay).await;

            if state.borrow().user_initiated_disconnect {
                state.borrow_mut().reconnecting = false;
                return;
            }

            let mut all_urls = vec![primary_url.clone()];
            all_urls.extend(options.backup_urls.clone());

            let mut connected = false;
            for (idx, url) in all_urls.iter().enumerate() {
                match attempt_reconnect(&state, url, &options).await {
                    Ok(()) => {
                        {
                            let mut state_ref = state.borrow_mut();
                            state_ref.reconnecting = false;
                            state_ref.reconnect_attempt = 0;
                            state_ref.current_broker_index = idx;
                        }
                        if idx == 0 {
                            web_sys::console::log_1(
                                &format!(
                                    "Reconnected to primary after {0} attempt(s)",
                                    attempt + 1
                                )
                                .into(),
                            );
                        } else {
                            web_sys::console::log_1(
                                &format!(
                                    "Reconnected to backup {idx} after {0} attempt(s)",
                                    attempt + 1
                                )
                                .into(),
                            );
                        }
                        connected = true;
                        break;
                    }
                    Err(e) => {
                        if idx == 0 {
                            web_sys::console::warn_1(
                                &format!("Primary connection failed: {e}").into(),
                            );
                        } else {
                            web_sys::console::warn_1(
                                &format!("Backup {idx} connection failed: {e}").into(),
                            );
                        }
                    }
                }
            }

            if connected {
                return;
            }

            web_sys::console::warn_1(
                &format!("All brokers failed on attempt {0}", attempt + 1).into(),
            );
            state.borrow_mut().reconnect_attempt = attempt + 1;
        }
    });
}

async fn attempt_reconnect(
    state: &Rc<RefCell<ClientState>>,
    url: &str,
    options: &StoredConnectOptions,
) -> Result<(), String> {
    let mut transport = WasmTransportType::WebSocket(
        crate::transport::websocket::WasmWebSocketTransport::new(url),
    );

    transport
        .connect()
        .await
        .map_err(|e| format!("Transport connection failed: {e}"))?;

    let client_id = state.borrow().client_id.clone();

    {
        let mut state_mut = state.borrow_mut();
        state_mut.keep_alive = options.keep_alive;
        state_mut.protocol_version = options.protocol_version;
        #[cfg(feature = "codec")]
        {
            state_mut.codec_registry.clone_from(&options.codec_registry);
        }
    }

    let properties = build_properties_from_stored(options);

    let connect_packet = ConnectPacket {
        protocol_version: options.protocol_version,
        clean_start: false,
        keep_alive: options.keep_alive,
        client_id,
        username: options.username.clone(),
        password: options.password.clone(),
        will: None,
        properties,
        will_properties: Properties::default(),
    };

    let packet = Packet::Connect(Box::new(connect_packet));
    let mut buf = BytesMut::new();
    encode_packet(&packet, &mut buf).map_err(|e| format!("Packet encoding failed: {e}"))?;

    transport
        .write(&buf)
        .await
        .map_err(|e| format!("Write failed: {e}"))?;

    if let Some(method) = &options.authentication_method {
        state.borrow_mut().auth_method = Some(method.clone());
    }

    let (reader, writer) = transport
        .into_split()
        .map_err(|e| format!("Transport split failed: {e}"))?;

    let mut reader = reader;
    let packet = read_packet(&mut reader)
        .await
        .map_err(|e| format!("Packet read failed: {e}"))?;

    match packet {
        Packet::ConnAck(connack) => {
            let reason_code = connack.reason_code as u8;
            if reason_code != 0 {
                return Err(format!("CONNACK error: {reason_code}"));
            }

            let writer_rc = Rc::new(RefCell::new(writer));
            {
                let mut state_mut = state.borrow_mut();
                state_mut.connected = true;
                state_mut.connection_generation = state_mut.connection_generation.wrapping_add(1);
                state_mut.writer = Some(Rc::clone(&writer_rc));
            }

            spawn_packet_reader(Rc::clone(state), reader);
            spawn_keepalive_task(Rc::clone(state));
            spawn_qos2_cleanup_task(Rc::clone(state));

            let callback = state.borrow().on_connect.clone();
            if let Some(callback) = callback {
                let reason_code_js =
                    wasm_bindgen::JsValue::from_f64(f64::from(connack.reason_code as u8));
                let session_present_js = wasm_bindgen::JsValue::from_bool(connack.session_present);
                let _ = callback.call2(
                    &wasm_bindgen::JsValue::NULL,
                    &reason_code_js,
                    &session_present_js,
                );
            }

            Ok(())
        }
        _ => Err(format!("Expected CONNACK, received: {packet:?}")),
    }
}

pub fn build_properties_from_stored(options: &StoredConnectOptions) -> Properties {
    let mut properties = Properties::default();

    if let Some(interval) = options.session_expiry_interval {
        let _ = properties.add(
            mqtt5_protocol::protocol::v5::properties::PropertyId::SessionExpiryInterval,
            mqtt5_protocol::protocol::v5::properties::PropertyValue::FourByteInteger(interval),
        );
    }

    if let Some(max) = options.receive_maximum {
        let _ = properties.add(
            mqtt5_protocol::protocol::v5::properties::PropertyId::ReceiveMaximum,
            mqtt5_protocol::protocol::v5::properties::PropertyValue::TwoByteInteger(max),
        );
    }

    if let Some(size) = options.maximum_packet_size {
        let _ = properties.add(
            mqtt5_protocol::protocol::v5::properties::PropertyId::MaximumPacketSize,
            mqtt5_protocol::protocol::v5::properties::PropertyValue::FourByteInteger(size),
        );
    }

    if let Some(max) = options.topic_alias_maximum {
        let _ = properties.add(
            mqtt5_protocol::protocol::v5::properties::PropertyId::TopicAliasMaximum,
            mqtt5_protocol::protocol::v5::properties::PropertyValue::TwoByteInteger(max),
        );
    }

    if let Some(req) = options.request_response_information {
        let _ = properties.add(
            mqtt5_protocol::protocol::v5::properties::PropertyId::RequestResponseInformation,
            mqtt5_protocol::protocol::v5::properties::PropertyValue::Byte(u8::from(req)),
        );
    }

    if let Some(req) = options.request_problem_information {
        let _ = properties.add(
            mqtt5_protocol::protocol::v5::properties::PropertyId::RequestProblemInformation,
            mqtt5_protocol::protocol::v5::properties::PropertyValue::Byte(u8::from(req)),
        );
    }

    if let Some(method) = &options.authentication_method {
        let _ = properties.add(
            mqtt5_protocol::protocol::v5::properties::PropertyId::AuthenticationMethod,
            mqtt5_protocol::protocol::v5::properties::PropertyValue::Utf8String(method.clone()),
        );
    }

    if let Some(data) = &options.authentication_data {
        let _ = properties.add(
            mqtt5_protocol::protocol::v5::properties::PropertyId::AuthenticationData,
            mqtt5_protocol::protocol::v5::properties::PropertyValue::BinaryData(
                data.clone().into(),
            ),
        );
    }

    for (key, value) in &options.user_properties {
        let _ = properties.add(
            mqtt5_protocol::protocol::v5::properties::PropertyId::UserProperty,
            mqtt5_protocol::protocol::v5::properties::PropertyValue::Utf8StringPair(
                key.clone(),
                value.clone(),
            ),
        );
    }

    properties
}
