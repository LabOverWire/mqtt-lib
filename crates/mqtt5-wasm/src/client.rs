use crate::config::{
    WasmConnectOptions, WasmMessageProperties, WasmPublishOptions, WasmReconnectOptions,
    WasmSubscribeOptions,
};
use crate::decoder::read_packet;
use crate::transport::{WasmReader, WasmTransportType, WasmWriter};
use bytes::BytesMut;
use mqtt5_protocol::connection::ReconnectConfig;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::subscribe::SubscribePacket;
use mqtt5_protocol::packet::unsubscribe::UnsubscribePacket;
use mqtt5_protocol::packet::{MqttPacket, Packet};
use mqtt5_protocol::packet_id::PacketIdGenerator;
use mqtt5_protocol::protocol::v5::properties::Properties;
use mqtt5_protocol::strip_shared_subscription_prefix;
use mqtt5_protocol::time::Duration;
use mqtt5_protocol::KeepaliveConfig;
use mqtt5_protocol::QoS;
use mqtt5_protocol::Transport;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::MessagePort;

async fn sleep_ms(millis: u32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let window = web_sys::window().expect("no global window");
        window
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                &resolve,
                i32::try_from(millis).unwrap_or(i32::MAX),
            )
            .expect("setTimeout failed");
    });
    JsFuture::from(promise).await.ok();
}

pub struct RustMessage {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: QoS,
    pub retain: bool,
    pub properties: mqtt5_protocol::types::MessageProperties,
}

type RustCallback = Rc<dyn Fn(RustMessage)>;

struct ClientState {
    client_id: String,
    writer: Option<Rc<RefCell<WasmWriter>>>,
    packet_id: PacketIdGenerator,
    connected: bool,
    protocol_version: u8,
    subscriptions: HashMap<String, js_sys::Function>,
    rust_subscriptions: HashMap<String, RustCallback>,
    pending_subacks: HashMap<u16, js_sys::Function>,
    pending_pubacks: HashMap<u16, js_sys::Function>,
    pending_pubcomps: HashMap<u16, (js_sys::Function, f64)>,
    pending_pubrecs: HashMap<u16, f64>,
    received_qos2: HashMap<u16, f64>,
    keep_alive: u16,
    last_ping_sent: Option<f64>,
    last_pong_received: Option<f64>,
    on_connect: Option<js_sys::Function>,
    on_disconnect: Option<js_sys::Function>,
    on_error: Option<js_sys::Function>,
    on_auth_challenge: Option<js_sys::Function>,
    on_reconnecting: Option<js_sys::Function>,
    on_reconnect_failed: Option<js_sys::Function>,
    auth_method: Option<String>,
    reconnect_config: ReconnectConfig,
    reconnect_attempt: u32,
    reconnecting: bool,
    last_url: Option<String>,
    last_options: Option<StoredConnectOptions>,
    user_initiated_disconnect: bool,
    current_broker_index: usize,
}

#[derive(Clone)]
struct StoredConnectOptions {
    keep_alive: u16,
    username: Option<String>,
    password: Option<Vec<u8>>,
    session_expiry_interval: Option<u32>,
    receive_maximum: Option<u16>,
    maximum_packet_size: Option<u32>,
    topic_alias_maximum: Option<u16>,
    request_response_information: Option<bool>,
    request_problem_information: Option<bool>,
    authentication_method: Option<String>,
    authentication_data: Option<Vec<u8>>,
    user_properties: Vec<(String, String)>,
    protocol_version: u8,
    backup_urls: Vec<String>,
}

impl From<&WasmConnectOptions> for StoredConnectOptions {
    fn from(opts: &WasmConnectOptions) -> Self {
        Self {
            keep_alive: opts.keep_alive,
            username: opts.username.clone(),
            password: opts.password.clone(),
            session_expiry_interval: opts.session_expiry_interval,
            receive_maximum: opts.receive_maximum,
            maximum_packet_size: opts.maximum_packet_size,
            topic_alias_maximum: opts.topic_alias_maximum,
            request_response_information: opts.request_response_information,
            request_problem_information: opts.request_problem_information,
            authentication_method: opts.authentication_method.clone(),
            authentication_data: opts.authentication_data.clone(),
            user_properties: opts.user_properties.clone(),
            protocol_version: opts.protocol_version,
            backup_urls: opts.backup_urls.clone(),
        }
    }
}

impl ClientState {
    fn new(client_id: String) -> Self {
        Self {
            client_id,
            writer: None,
            packet_id: PacketIdGenerator::new(),
            connected: false,
            protocol_version: 5,
            subscriptions: HashMap::new(),
            rust_subscriptions: HashMap::new(),
            pending_subacks: HashMap::new(),
            pending_pubacks: HashMap::new(),
            pending_pubcomps: HashMap::new(),
            pending_pubrecs: HashMap::new(),
            received_qos2: HashMap::new(),
            keep_alive: 60,
            last_ping_sent: None,
            last_pong_received: None,
            on_connect: None,
            on_disconnect: None,
            on_error: None,
            on_auth_challenge: None,
            on_reconnecting: None,
            on_reconnect_failed: None,
            auth_method: None,
            reconnect_config: ReconnectConfig::disabled(),
            reconnect_attempt: 0,
            reconnecting: false,
            last_url: None,
            last_options: None,
            user_initiated_disconnect: false,
            current_broker_index: 0,
        }
    }
}

fn encode_packet(packet: &Packet, buf: &mut BytesMut) -> mqtt5_protocol::error::Result<()> {
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

#[wasm_bindgen]
pub struct WasmMqttClient {
    state: Rc<RefCell<ClientState>>,
}

impl WasmMqttClient {
    fn trigger_disconnect_callback(state: &Rc<RefCell<ClientState>>) {
        let callback = state.borrow().on_disconnect.clone();
        if let Some(callback) = callback {
            if let Err(e) = callback.call0(&JsValue::NULL) {
                web_sys::console::error_1(&format!("onDisconnect callback error: {e:?}").into());
            }
        }
    }

    fn trigger_error_callback(state: &Rc<RefCell<ClientState>>, error_msg: &str) {
        let callback = state.borrow().on_error.clone();
        if let Some(callback) = callback {
            let error_js = JsValue::from_str(error_msg);
            if let Err(e) = callback.call1(&JsValue::NULL, &error_js) {
                web_sys::console::error_1(&format!("onError callback error: {e:?}").into());
            }
        }
    }

    fn trigger_reconnecting_callback(
        state: &Rc<RefCell<ClientState>>,
        attempt: u32,
        delay_ms: u32,
    ) {
        let callback = state.borrow().on_reconnecting.clone();
        if let Some(callback) = callback {
            let attempt_js = JsValue::from_f64(f64::from(attempt));
            let delay_js = JsValue::from_f64(f64::from(delay_ms));
            if let Err(e) = callback.call2(&JsValue::NULL, &attempt_js, &delay_js) {
                web_sys::console::error_1(&format!("onReconnecting callback error: {e:?}").into());
            }
        }
    }

    fn trigger_reconnect_failed_callback(state: &Rc<RefCell<ClientState>>, error_msg: &str) {
        let callback = state.borrow().on_reconnect_failed.clone();
        if let Some(callback) = callback {
            let error_js = JsValue::from_str(error_msg);
            if let Err(e) = callback.call1(&JsValue::NULL, &error_js) {
                web_sys::console::error_1(
                    &format!("onReconnectFailed callback error: {e:?}").into(),
                );
            }
        }
    }

    fn spawn_reconnection_task(state: Rc<RefCell<ClientState>>) {
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
                    let delay = state_ref.reconnect_config.calculate_delay(attempt);
                    #[allow(clippy::cast_possible_truncation)]
                    let delay_ms = delay.as_millis() as u32;
                    let should_continue = state_ref.reconnect_config.should_retry(attempt);
                    let url = state_ref.last_url.clone();
                    let options = state_ref.last_options.clone();
                    (attempt, delay_ms, should_continue, url, options)
                };

                if !should_continue {
                    Self::trigger_reconnect_failed_callback(
                        &state,
                        "Max reconnection attempts exceeded",
                    );
                    state.borrow_mut().reconnecting = false;
                    return;
                }

                let (primary_url, options) = match (primary_url, options) {
                    (Some(u), Some(o)) => (u, o),
                    _ => {
                        Self::trigger_reconnect_failed_callback(
                            &state,
                            "No stored connection parameters",
                        );
                        state.borrow_mut().reconnecting = false;
                        return;
                    }
                };

                Self::trigger_reconnecting_callback(&state, attempt + 1, delay);

                sleep_ms(delay).await;

                if state.borrow().user_initiated_disconnect {
                    state.borrow_mut().reconnecting = false;
                    return;
                }

                let mut all_urls = vec![primary_url.clone()];
                all_urls.extend(options.backup_urls.clone());

                let mut connected = false;
                for (idx, url) in all_urls.iter().enumerate() {
                    match Self::attempt_reconnect(&state, url, &options).await {
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
        }

        let properties = Self::build_properties_from_stored(options);

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

        let (mut reader, writer) = transport
            .into_split()
            .map_err(|e| format!("Transport split failed: {e}"))?;

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
                    state_mut.writer = Some(Rc::clone(&writer_rc));
                }

                Self::spawn_packet_reader_internal(Rc::clone(state), reader);
                Self::spawn_keepalive_task_internal(Rc::clone(state));
                Self::spawn_qos2_cleanup_task_internal(Rc::clone(state));

                let callback = state.borrow().on_connect.clone();
                if let Some(callback) = callback {
                    let reason_code_js = JsValue::from_f64(f64::from(connack.reason_code as u8));
                    let session_present_js = JsValue::from_bool(connack.session_present);
                    let _ = callback.call2(&JsValue::NULL, &reason_code_js, &session_present_js);
                }

                Ok(())
            }
            _ => Err(format!("Expected CONNACK, received: {packet:?}")),
        }
    }

    fn build_properties_from_stored(options: &StoredConnectOptions) -> Properties {
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

    fn spawn_packet_reader_internal(state: Rc<RefCell<ClientState>>, mut reader: WasmReader) {
        spawn_local(async move {
            loop {
                let packet_result = read_packet(&mut reader).await;

                match packet_result {
                    Ok(packet) => {
                        Self::handle_incoming_packet(&state, packet);
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
                            Self::trigger_error_callback(&state, &error_msg);
                            Self::trigger_disconnect_callback(&state);

                            if should_reconnect {
                                Self::spawn_reconnection_task(Rc::clone(&state));
                            }
                        }
                        break;
                    }
                }
            }
        });
    }

    fn spawn_keepalive_task_internal(state: Rc<RefCell<ClientState>>) {
        let keepalive_config = KeepaliveConfig::conservative();

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
                #[allow(clippy::cast_possible_truncation)]
                let sleep_duration = ping_interval.as_millis() as u32;
                sleep_ms(sleep_duration).await;

                let timeout_duration = keepalive_config.timeout_duration(keepalive_duration);
                #[allow(clippy::cast_precision_loss)]
                let timeout_ms = timeout_duration.as_millis() as f64;

                let should_disconnect = if let Ok(state_ref) = state.try_borrow() {
                    if !state_ref.connected {
                        break;
                    }

                    let now = js_sys::Date::now();

                    if let Some(last_ping) = state_ref.last_ping_sent {
                        let pong_received = state_ref
                            .last_pong_received
                            .is_some_and(|pong| pong > last_ping);
                        if !pong_received && (now - last_ping) > timeout_ms {
                            web_sys::console::error_1(&"Keepalive timeout".into());
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    sleep_ms(100).await;
                    continue;
                };

                if should_disconnect {
                    let should_reconnect = {
                        let mut state_ref = state.borrow_mut();
                        state_ref.connected = false;
                        state_ref.reconnect_config.enabled
                            && !state_ref.user_initiated_disconnect
                            && !state_ref.reconnecting
                            && state_ref.last_url.is_some()
                    };

                    Self::trigger_error_callback(&state, "Keepalive timeout");
                    Self::trigger_disconnect_callback(&state);

                    if should_reconnect {
                        Self::spawn_reconnection_task(Rc::clone(&state));
                    }
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
                            web_sys::console::error_1(&error_msg.clone().into());
                            let should_reconnect = {
                                let mut state_ref = state.borrow_mut();
                                state_ref.connected = false;
                                state_ref.reconnect_config.enabled
                                    && !state_ref.user_initiated_disconnect
                                    && !state_ref.reconnecting
                                    && state_ref.last_url.is_some()
                            };

                            Self::trigger_error_callback(&state, &error_msg);
                            Self::trigger_disconnect_callback(&state);

                            if should_reconnect {
                                Self::spawn_reconnection_task(Rc::clone(&state));
                            }
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

    fn spawn_qos2_cleanup_task_internal(state: Rc<RefCell<ClientState>>) {
        spawn_local(async move {
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

    fn spawn_packet_reader(&self, reader: WasmReader) {
        Self::spawn_packet_reader_internal(Rc::clone(&self.state), reader);
    }

    fn spawn_keepalive_task(&self) {
        Self::spawn_keepalive_task_internal(Rc::clone(&self.state));
    }

    fn spawn_qos2_cleanup_task(&self) {
        Self::spawn_qos2_cleanup_task_internal(Rc::clone(&self.state));
    }

    #[allow(clippy::too_many_lines)]
    fn handle_incoming_packet(state: &Rc<RefCell<ClientState>>, packet: Packet) {
        match packet {
            Packet::ConnAck(connack) => {
                let callback = state.borrow().on_connect.clone();
                if let Some(callback) = callback {
                    let reason_code = JsValue::from_f64(f64::from(connack.reason_code as u8));
                    let session_present = JsValue::from_bool(connack.session_present);

                    if let Err(e) = callback.call2(&JsValue::NULL, &reason_code, &session_present) {
                        web_sys::console::error_1(
                            &format!("onConnect callback error: {e:?}").into(),
                        );
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
                        let actions = mqtt5_protocol::qos2::handle_incoming_publish_qos2(
                            packet_id,
                            is_duplicate,
                        );

                        for action in actions {
                            match action {
                                mqtt5_protocol::qos2::QoS2Action::DeliverMessage {
                                    packet_id: _,
                                } => {
                                    let subscriptions = state.borrow().subscriptions.clone();
                                    let rust_subscriptions =
                                        state.borrow().rust_subscriptions.clone();

                                    for (filter, callback) in &subscriptions {
                                        if mqtt5_protocol::validation::topic_matches_filter(
                                            &topic, filter,
                                        ) {
                                            let topic_js = JsValue::from_str(&topic);
                                            let payload_array =
                                                js_sys::Uint8Array::from(&payload[..]);
                                            let props_js: WasmMessageProperties =
                                                properties.clone().into();

                                            if let Err(e) = callback.call3(
                                                &JsValue::NULL,
                                                &topic_js,
                                                &payload_array.into(),
                                                &props_js.into(),
                                            ) {
                                                web_sys::console::error_1(
                                                    &format!("Callback error: {e:?}").into(),
                                                );
                                            }
                                        }
                                    }

                                    for (filter, callback) in &rust_subscriptions {
                                        if mqtt5_protocol::validation::topic_matches_filter(
                                            &topic, filter,
                                        ) {
                                            let msg = RustMessage {
                                                topic: topic.clone(),
                                                payload: payload.to_vec(),
                                                qos,
                                                retain,
                                                properties: properties.clone(),
                                            };
                                            callback(msg);
                                        }
                                    }
                                }
                                mqtt5_protocol::qos2::QoS2Action::SendPubRec {
                                    packet_id,
                                    reason_code,
                                } => {
                                    let pubrec =
                                        mqtt5_protocol::packet::pubrec::PubRecPacket::new_with_reason(
                                            packet_id,
                                            reason_code,
                                        );
                                    let mut buf = BytesMut::new();
                                    if let Err(e) = encode_packet(
                                        &mqtt5_protocol::packet::Packet::PubRec(pubrec),
                                        &mut buf,
                                    ) {
                                        web_sys::console::error_1(
                                            &format!("PUBREC encode error: {e}").into(),
                                        );
                                        continue;
                                    }

                                    let writer_rc = state.borrow().writer.clone();
                                    if let Some(writer_rc) = writer_rc {
                                        spawn_local(async move {
                                            if let Err(e) = writer_rc.borrow_mut().write(&buf) {
                                                web_sys::console::error_1(
                                                    &format!("PUBREC send error: {e}").into(),
                                                );
                                            }
                                        });
                                    }
                                }
                                mqtt5_protocol::qos2::QoS2Action::TrackIncomingPubRec {
                                    packet_id,
                                } => {
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
                    let subscriptions = state.borrow().subscriptions.clone();
                    let rust_subscriptions = state.borrow().rust_subscriptions.clone();

                    for (filter, callback) in &subscriptions {
                        if mqtt5_protocol::validation::topic_matches_filter(&topic, filter) {
                            let topic_js = JsValue::from_str(&topic);
                            let payload_array = js_sys::Uint8Array::from(&payload[..]);
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
                        if mqtt5_protocol::validation::topic_matches_filter(&topic, filter) {
                            let msg = RustMessage {
                                topic: topic.clone(),
                                payload: payload.to_vec(),
                                qos,
                                retain,
                                properties: properties.clone(),
                            };
                            callback(msg);
                        }
                    }
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
                let has_pending = state
                    .borrow()
                    .pending_pubcomps
                    .contains_key(&pubrec.packet_id);
                let actions = mqtt5_protocol::qos2::handle_incoming_pubrec(
                    pubrec.packet_id,
                    pubrec.reason_code,
                    has_pending,
                );

                for action in actions {
                    match action {
                        mqtt5_protocol::qos2::QoS2Action::SendPubRel { packet_id } => {
                            let pubrel_packet =
                                mqtt5_protocol::packet::pubrel::PubRelPacket::new(packet_id);
                            let mut buf = BytesMut::new();
                            if let Err(e) = encode_packet(
                                &mqtt5_protocol::packet::Packet::PubRel(pubrel_packet),
                                &mut buf,
                            ) {
                                web_sys::console::error_1(
                                    &format!("PUBREL encode error: {e}").into(),
                                );
                                continue;
                            }

                            let writer_rc = state.borrow().writer.clone();
                            if let Some(writer_rc) = writer_rc {
                                spawn_local(async move {
                                    if let Err(e) = writer_rc.borrow_mut().write(&buf) {
                                        web_sys::console::error_1(
                                            &format!("PUBREL send error: {e}").into(),
                                        );
                                    }
                                });
                            }
                        }
                        mqtt5_protocol::qos2::QoS2Action::ErrorFlow {
                            packet_id,
                            reason_code,
                        } => {
                            if let Some((callback, _)) =
                                state.borrow_mut().pending_pubcomps.remove(&packet_id)
                            {
                                let reason_code_js =
                                    JsValue::from_f64(f64::from(reason_code as u8));
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
            Packet::PubComp(pubcomp) => {
                let has_pending = state
                    .borrow()
                    .pending_pubcomps
                    .contains_key(&pubcomp.packet_id);
                let actions = mqtt5_protocol::qos2::handle_incoming_pubcomp(
                    pubcomp.packet_id,
                    pubcomp.reason_code,
                    has_pending,
                );

                for action in actions {
                    match action {
                        mqtt5_protocol::qos2::QoS2Action::CompleteFlow { packet_id } => {
                            if let Some((callback, _)) =
                                state.borrow_mut().pending_pubcomps.remove(&packet_id)
                            {
                                let reason_code_js =
                                    JsValue::from_f64(f64::from(pubcomp.reason_code as u8));
                                if let Err(e) = callback.call1(&JsValue::NULL, &reason_code_js) {
                                    web_sys::console::error_1(
                                        &format!("PUBCOMP callback error: {e:?}").into(),
                                    );
                                }
                            }
                        }
                        mqtt5_protocol::qos2::QoS2Action::ErrorFlow {
                            packet_id,
                            reason_code,
                        } => {
                            if let Some((callback, _)) =
                                state.borrow_mut().pending_pubcomps.remove(&packet_id)
                            {
                                let reason_code_js =
                                    JsValue::from_f64(f64::from(reason_code as u8));
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
            Packet::PubRel(pubrel) => {
                let has_pubrec = state
                    .borrow()
                    .pending_pubrecs
                    .contains_key(&pubrel.packet_id);
                let actions =
                    mqtt5_protocol::qos2::handle_incoming_pubrel(pubrel.packet_id, has_pubrec);

                for action in actions {
                    match action {
                        mqtt5_protocol::qos2::QoS2Action::RemoveIncomingPubRec { packet_id } => {
                            state.borrow_mut().pending_pubrecs.remove(&packet_id);
                        }
                        mqtt5_protocol::qos2::QoS2Action::SendPubComp {
                            packet_id,
                            reason_code,
                        } => {
                            let pubcomp =
                                mqtt5_protocol::packet::pubcomp::PubCompPacket::new_with_reason(
                                    packet_id,
                                    reason_code,
                                );
                            let mut buf = BytesMut::new();
                            if let Err(e) = encode_packet(
                                &mqtt5_protocol::packet::Packet::PubComp(pubcomp),
                                &mut buf,
                            ) {
                                web_sys::console::error_1(
                                    &format!("PUBCOMP encode error: {e}").into(),
                                );
                                continue;
                            }

                            let writer_rc = state.borrow().writer.clone();
                            if let Some(writer_rc) = writer_rc {
                                spawn_local(async move {
                                    if let Err(e) = writer_rc.borrow_mut().write(&buf) {
                                        web_sys::console::error_1(
                                            &format!("PUBCOMP send error: {e}").into(),
                                        );
                                    }
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
            Packet::Auth(auth) => {
                let reason_code = auth.reason_code;

                if reason_code == mqtt5_protocol::protocol::v5::reason_codes::ReasonCode::ContinueAuthentication {
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
                } else if reason_code == mqtt5_protocol::protocol::v5::reason_codes::ReasonCode::Success {
                    web_sys::console::log_1(&"Authentication successful".into());
                }
            }
            _ => {
                web_sys::console::warn_1(&format!("Unhandled packet type: {packet:?}").into());
            }
        }
    }
}

#[wasm_bindgen]
impl WasmMqttClient {
    #[wasm_bindgen(constructor)]
    #[allow(clippy::must_use_candidate)]
    pub fn new(client_id: String) -> Self {
        console_error_panic_hook::set_once();

        Self {
            state: Rc::new(RefCell::new(ClientState::new(client_id))),
        }
    }

    /// # Errors
    /// Returns an error if connection fails.
    pub async fn connect(&self, url: &str) -> Result<(), JsValue> {
        let config = WasmConnectOptions::default();
        self.connect_with_options(url, &config).await
    }

    /// # Errors
    /// Returns an error if connection fails.
    pub async fn connect_with_options(
        &self,
        url: &str,
        config: &WasmConnectOptions,
    ) -> Result<(), JsValue> {
        self.state.borrow_mut().last_url = Some(url.to_string());
        let transport = WasmTransportType::WebSocket(
            crate::transport::websocket::WasmWebSocketTransport::new(url),
        );
        self.connect_with_transport_and_config(transport, config)
            .await
    }

    /// # Errors
    /// Returns an error if connection fails.
    pub async fn connect_message_port(&self, port: MessagePort) -> Result<(), JsValue> {
        let config = WasmConnectOptions::default();
        self.connect_message_port_with_options(port, &config).await
    }

    /// # Errors
    /// Returns an error if connection fails.
    pub async fn connect_message_port_with_options(
        &self,
        port: MessagePort,
        config: &WasmConnectOptions,
    ) -> Result<(), JsValue> {
        let transport = WasmTransportType::MessagePort(
            crate::transport::message_port::MessagePortTransport::new(port),
        );
        self.connect_with_transport_and_config(transport, config)
            .await
    }

    /// # Errors
    /// Returns an error if connection fails.
    pub async fn connect_broadcast_channel(&self, channel_name: &str) -> Result<(), JsValue> {
        let config = WasmConnectOptions::default();
        let transport = WasmTransportType::BroadcastChannel(
            crate::transport::broadcast::BroadcastChannelTransport::new(channel_name),
        );
        self.connect_with_transport_and_config(transport, &config)
            .await
    }

    #[allow(clippy::too_many_lines)]
    async fn connect_with_transport_and_config(
        &self,
        mut transport: WasmTransportType,
        config: &WasmConnectOptions,
    ) -> Result<(), JsValue> {
        transport
            .connect()
            .await
            .map_err(|e| JsValue::from_str(&format!("Transport connection failed: {e}")))?;

        let client_id = self.state.borrow().client_id.clone();
        let protocol_version = config.protocol_version;

        {
            let mut state = self.state.borrow_mut();
            state.keep_alive = config.keep_alive;
            state.protocol_version = protocol_version;
            state.last_options = Some(StoredConnectOptions::from(config));
            state.user_initiated_disconnect = false;
            state.reconnect_attempt = 0;
        }

        let (will, will_properties) = if let Some(will_config) = &config.will {
            let will_msg = will_config.to_will_message();
            let will_props = will_msg.properties.clone().into();
            (Some(will_msg), will_props)
        } else {
            (None, Properties::default())
        };

        let (properties, will_properties) = if protocol_version == 5 {
            (config.to_properties(), will_properties)
        } else {
            (Properties::default(), Properties::default())
        };

        let connect_packet = ConnectPacket {
            protocol_version,
            clean_start: config.clean_start,
            keep_alive: config.keep_alive,
            client_id,
            username: config.username.clone(),
            password: config.password.clone(),
            will,
            properties,
            will_properties,
        };

        let packet = Packet::Connect(Box::new(connect_packet));
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {e}")))?;

        transport
            .write(&buf)
            .await
            .map_err(|e| JsValue::from_str(&format!("Write failed: {e}")))?;

        if let Some(method) = &config.authentication_method {
            self.state.borrow_mut().auth_method = Some(method.clone());
        }

        let (mut reader, writer) = transport
            .into_split()
            .map_err(|e| JsValue::from_str(&format!("Transport split failed: {e}")))?;

        let writer_rc = Rc::new(RefCell::new(writer));
        self.state.borrow_mut().writer = Some(Rc::clone(&writer_rc));

        loop {
            let packet = read_packet(&mut reader)
                .await
                .map_err(|e| JsValue::from_str(&format!("Packet read failed: {e}")))?;

            match packet {
                Packet::ConnAck(connack) => {
                    let reason_code = connack.reason_code;
                    let session_present = connack.session_present;

                    self.state.borrow_mut().connected = true;

                    self.spawn_packet_reader(reader);
                    self.spawn_keepalive_task();
                    self.spawn_qos2_cleanup_task();

                    let callback = self.state.borrow().on_connect.clone();
                    if let Some(callback) = callback {
                        let reason_code_js = JsValue::from_f64(f64::from(reason_code as u8));
                        let session_present_js = JsValue::from_bool(session_present);

                        if let Err(e) =
                            callback.call2(&JsValue::NULL, &reason_code_js, &session_present_js)
                        {
                            web_sys::console::error_1(
                                &format!("onConnect callback error: {e:?}").into(),
                            );
                        }
                    }

                    return Ok(());
                }
                Packet::Auth(auth) => {
                    let auth_reason = auth.reason_code;
                    if auth_reason
                        == mqtt5_protocol::protocol::v5::reason_codes::ReasonCode::ContinueAuthentication
                    {
                        let callback = self.state.borrow().on_auth_challenge.clone();
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
                        } else {
                            return Err(JsValue::from_str(
                                "AUTH challenge received but no on_auth_challenge callback set",
                            ));
                        }
                    } else {
                        return Err(JsValue::from_str(&format!(
                            "Unexpected AUTH reason code: {auth_reason:?}"
                        )));
                    }
                }
                _ => {
                    return Err(JsValue::from_str(&format!(
                        "Expected CONNACK or AUTH, received: {packet:?}"
                    )));
                }
            }
        }
    }

    /// # Errors
    /// Returns an error if not connected or publish fails.
    pub async fn publish(&self, topic: &str, payload: &[u8]) -> Result<(), JsValue> {
        loop {
            match self.state.try_borrow() {
                Ok(state) => {
                    if !state.connected {
                        return Err(JsValue::from_str("Not connected"));
                    }
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        }

        let protocol_version = self.state.borrow().protocol_version;
        let publish_packet = PublishPacket {
            dup: false,
            qos: QoS::AtMostOnce,
            retain: false,
            topic_name: topic.to_string(),
            packet_id: None,
            properties: Properties::default(),
            payload: payload.to_vec().into(),
            protocol_version,
        };

        let packet = Packet::Publish(publish_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {e}")))?;

        let writer_rc = self
            .state
            .borrow()
            .writer
            .clone()
            .ok_or_else(|| JsValue::from_str("Writer disconnected"))?;

        writer_rc
            .borrow_mut()
            .write(&buf)
            .map_err(|e| JsValue::from_str(&format!("Write failed: {e}")))?;

        Ok(())
    }

    /// # Errors
    /// Returns an error if not connected or publish fails.
    pub async fn publish_with_options(
        &self,
        topic: &str,
        payload: &[u8],
        options: &WasmPublishOptions,
    ) -> Result<(), JsValue> {
        loop {
            match self.state.try_borrow() {
                Ok(state) => {
                    if !state.connected {
                        return Err(JsValue::from_str("Not connected"));
                    }
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        }

        let qos = options.to_qos();
        let packet_id = if qos == QoS::AtMostOnce {
            None
        } else {
            Some(loop {
                match self.state.try_borrow_mut() {
                    Ok(state) => break state.packet_id.next(),
                    Err(_) => {
                        sleep_ms(10).await;
                    }
                }
            })
        };

        if qos == QoS::ExactlyOnce {
            if let Some(pid) = packet_id {
                let now = js_sys::Date::now();
                let noop_callback = js_sys::Function::new_no_args("");
                loop {
                    match self.state.try_borrow_mut() {
                        Ok(mut state) => {
                            state.pending_pubcomps.insert(pid, (noop_callback, now));
                            break;
                        }
                        Err(_) => {
                            sleep_ms(10).await;
                        }
                    }
                }
            }
        }

        let protocol_version = self.state.borrow().protocol_version;
        let properties = if protocol_version == 5 {
            options.to_properties()
        } else {
            Properties::default()
        };
        let publish_packet = PublishPacket {
            dup: false,
            qos,
            retain: options.retain,
            topic_name: topic.to_string(),
            packet_id,
            properties,
            payload: payload.to_vec().into(),
            protocol_version,
        };

        let packet = Packet::Publish(publish_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {e}")))?;

        let writer_rc = self
            .state
            .borrow()
            .writer
            .clone()
            .ok_or_else(|| JsValue::from_str("Writer disconnected"))?;

        writer_rc
            .borrow_mut()
            .write(&buf)
            .map_err(|e| JsValue::from_str(&format!("Write failed: {e}")))?;

        Ok(())
    }

    /// # Errors
    /// Returns an error if not connected or publish fails.
    pub async fn publish_qos1(
        &self,
        topic: &str,
        payload: &[u8],
        callback: js_sys::Function,
    ) -> Result<u16, JsValue> {
        loop {
            match self.state.try_borrow() {
                Ok(state) => {
                    if !state.connected {
                        return Err(JsValue::from_str("Not connected"));
                    }
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        }

        let packet_id = loop {
            match self.state.try_borrow_mut() {
                Ok(state) => break state.packet_id.next(),
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        };

        loop {
            match self.state.try_borrow_mut() {
                Ok(mut state) => {
                    state.pending_pubacks.insert(packet_id, callback);
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        }

        let protocol_version = self.state.borrow().protocol_version;
        let publish_packet = PublishPacket {
            dup: false,
            qos: QoS::AtLeastOnce,
            retain: false,
            topic_name: topic.to_string(),
            packet_id: Some(packet_id),
            properties: Properties::default(),
            payload: payload.to_vec().into(),
            protocol_version,
        };

        let packet = Packet::Publish(publish_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {e}")))?;

        let writer_rc = self
            .state
            .borrow()
            .writer
            .clone()
            .ok_or_else(|| JsValue::from_str("Writer disconnected"))?;

        writer_rc
            .borrow_mut()
            .write(&buf)
            .map_err(|e| JsValue::from_str(&format!("Write failed: {e}")))?;

        Ok(packet_id)
    }

    /// # Errors
    /// Returns an error if not connected or publish fails.
    pub async fn publish_qos2(
        &self,
        topic: &str,
        payload: &[u8],
        callback: js_sys::Function,
    ) -> Result<u16, JsValue> {
        loop {
            match self.state.try_borrow() {
                Ok(state) => {
                    if !state.connected {
                        return Err(JsValue::from_str("Not connected"));
                    }
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        }

        let packet_id = loop {
            match self.state.try_borrow_mut() {
                Ok(state) => break state.packet_id.next(),
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        };

        let now = js_sys::Date::now();
        loop {
            match self.state.try_borrow_mut() {
                Ok(mut state) => {
                    state.pending_pubcomps.insert(packet_id, (callback, now));
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        }

        let protocol_version = self.state.borrow().protocol_version;
        let publish_packet = PublishPacket {
            dup: false,
            qos: QoS::ExactlyOnce,
            retain: false,
            topic_name: topic.to_string(),
            packet_id: Some(packet_id),
            properties: Properties::default(),
            payload: payload.to_vec().into(),
            protocol_version,
        };

        let packet = Packet::Publish(publish_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {e}")))?;

        let writer_rc = self
            .state
            .borrow()
            .writer
            .clone()
            .ok_or_else(|| JsValue::from_str("Writer disconnected"))?;

        writer_rc
            .borrow_mut()
            .write(&buf)
            .map_err(|e| JsValue::from_str(&format!("Write failed: {e}")))?;

        Ok(packet_id)
    }

    /// # Errors
    /// Returns an error if not connected or subscribe fails.
    pub async fn subscribe(&self, topic: &str) -> Result<u16, JsValue> {
        loop {
            match self.state.try_borrow() {
                Ok(state) => {
                    if !state.connected {
                        return Err(JsValue::from_str("Not connected"));
                    }
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        }

        let packet_id = loop {
            match self.state.try_borrow_mut() {
                Ok(state) => break state.packet_id.next(),
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        };

        let protocol_version = self.state.borrow().protocol_version;
        let subscribe_packet = SubscribePacket {
            packet_id,
            properties: Properties::default(),
            filters: vec![mqtt5_protocol::packet::subscribe::TopicFilter::new(
                topic,
                QoS::AtMostOnce,
            )],
            protocol_version,
        };

        let packet = Packet::Subscribe(subscribe_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {e}")))?;

        let writer_rc = self
            .state
            .borrow()
            .writer
            .clone()
            .ok_or_else(|| JsValue::from_str("Writer disconnected"))?;

        writer_rc
            .borrow_mut()
            .write(&buf)
            .map_err(|e| JsValue::from_str(&format!("Write failed: {e}")))?;

        Ok(packet_id)
    }

    /// # Errors
    /// Returns an error if not connected or subscribe fails.
    pub async fn subscribe_with_options(
        &self,
        topic: &str,
        callback: js_sys::Function,
        options: &WasmSubscribeOptions,
    ) -> Result<u16, JsValue> {
        loop {
            match self.state.try_borrow() {
                Ok(state) => {
                    if !state.connected {
                        return Err(JsValue::from_str("Not connected"));
                    }
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        }

        let packet_id = loop {
            match self.state.try_borrow_mut() {
                Ok(state) => break state.packet_id.next(),
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        };

        loop {
            match self.state.try_borrow_mut() {
                Ok(mut state) => {
                    let actual_filter = strip_shared_subscription_prefix(topic);
                    state
                        .subscriptions
                        .insert(actual_filter.to_string(), callback);
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        }

        let mut topic_filter =
            mqtt5_protocol::packet::subscribe::TopicFilter::new(topic, options.to_qos());
        topic_filter.options.no_local = options.no_local;
        topic_filter.options.retain_as_published = options.retain_as_published;
        topic_filter.options.retain_handling = match options.retain_handling {
            1 => mqtt5_protocol::packet::subscribe::RetainHandling::SendAtSubscribeIfNew,
            2 => mqtt5_protocol::packet::subscribe::RetainHandling::DoNotSend,
            _ => mqtt5_protocol::packet::subscribe::RetainHandling::SendAtSubscribe,
        };

        let mut properties = Properties::default();
        if let Some(id) = options.subscription_identifier {
            if properties
                .add(
                    mqtt5_protocol::protocol::v5::properties::PropertyId::SubscriptionIdentifier,
                    mqtt5_protocol::protocol::v5::properties::PropertyValue::VariableByteInteger(
                        id,
                    ),
                )
                .is_err()
            {
                web_sys::console::warn_1(&"Failed to add subscription identifier property".into());
            }
        }

        let protocol_version = self.state.borrow().protocol_version;
        let properties = if protocol_version == 5 {
            properties
        } else {
            Properties::default()
        };
        let subscribe_packet = SubscribePacket {
            packet_id,
            properties,
            filters: vec![topic_filter],
            protocol_version,
        };

        let packet = Packet::Subscribe(subscribe_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {e}")))?;

        let writer_rc = self
            .state
            .borrow()
            .writer
            .clone()
            .ok_or_else(|| JsValue::from_str("Writer disconnected"))?;

        writer_rc
            .borrow_mut()
            .write(&buf)
            .map_err(|e| JsValue::from_str(&format!("Write failed: {e}")))?;

        Ok(packet_id)
    }

    /// # Errors
    /// Returns an error if not connected or subscribe fails.
    pub async fn subscribe_with_callback(
        &self,
        topic: &str,
        callback: js_sys::Function,
    ) -> Result<u16, JsValue> {
        loop {
            match self.state.try_borrow() {
                Ok(state) => {
                    if !state.connected {
                        return Err(JsValue::from_str("Not connected"));
                    }
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        }

        let packet_id = loop {
            match self.state.try_borrow_mut() {
                Ok(state) => break state.packet_id.next(),
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        };

        loop {
            match self.state.try_borrow_mut() {
                Ok(mut state) => {
                    let actual_filter = strip_shared_subscription_prefix(topic);
                    state
                        .subscriptions
                        .insert(actual_filter.to_string(), callback);
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        }

        let protocol_version = self.state.borrow().protocol_version;
        let subscribe_packet = SubscribePacket {
            packet_id,
            properties: Properties::default(),
            filters: vec![mqtt5_protocol::packet::subscribe::TopicFilter::new(
                topic,
                QoS::AtMostOnce,
            )],
            protocol_version,
        };

        let packet = Packet::Subscribe(subscribe_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {e}")))?;

        let writer_rc = self
            .state
            .borrow()
            .writer
            .clone()
            .ok_or_else(|| JsValue::from_str("Writer disconnected"))?;

        writer_rc
            .borrow_mut()
            .write(&buf)
            .map_err(|e| JsValue::from_str(&format!("Write failed: {e}")))?;

        Ok(packet_id)
    }

    /// # Errors
    /// Returns an error if not connected or unsubscribe fails.
    pub async fn unsubscribe(&self, topic: &str) -> Result<u16, JsValue> {
        loop {
            match self.state.try_borrow() {
                Ok(state) => {
                    if !state.connected {
                        return Err(JsValue::from_str("Not connected"));
                    }
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        }

        let packet_id = loop {
            match self.state.try_borrow_mut() {
                Ok(state) => break state.packet_id.next(),
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        };

        loop {
            match self.state.try_borrow_mut() {
                Ok(mut state) => {
                    state.subscriptions.remove(topic);
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        }

        let protocol_version = self.state.borrow().protocol_version;
        let unsubscribe_packet = UnsubscribePacket {
            packet_id,
            properties: Properties::default(),
            filters: vec![topic.to_string()],
            protocol_version,
        };

        let packet = Packet::Unsubscribe(unsubscribe_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {e}")))?;

        let writer_rc = self
            .state
            .borrow()
            .writer
            .clone()
            .ok_or_else(|| JsValue::from_str("Writer disconnected"))?;

        writer_rc
            .borrow_mut()
            .write(&buf)
            .map_err(|e| JsValue::from_str(&format!("Write failed: {e}")))?;

        Ok(packet_id)
    }

    /// # Errors
    /// Returns an error if disconnect fails.
    pub async fn disconnect(&self) -> Result<(), JsValue> {
        let disconnect_packet = mqtt5_protocol::packet::disconnect::DisconnectPacket {
            reason_code: mqtt5_protocol::protocol::v5::reason_codes::ReasonCode::Success,
            properties: Properties::default(),
        };
        let packet = Packet::Disconnect(disconnect_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("DISCONNECT packet encoding failed: {e}")))?;

        let writer_rc = loop {
            match self.state.try_borrow_mut() {
                Ok(mut state) => {
                    state.connected = false;
                    state.user_initiated_disconnect = true;
                    break state.writer.take();
                }
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        };

        if let Some(writer_rc) = writer_rc {
            let mut writer = writer_rc.borrow_mut();
            writer
                .write(&buf)
                .map_err(|e| JsValue::from_str(&format!("DISCONNECT packet send failed: {e}")))?;
            writer
                .close()
                .map_err(|e| JsValue::from_str(&format!("Close failed: {e}")))?;
        }

        Self::trigger_disconnect_callback(&self.state);
        Ok(())
    }

    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.state.borrow().connected
    }

    pub fn on_connect(&self, callback: js_sys::Function) {
        self.state.borrow_mut().on_connect = Some(callback);
    }

    pub fn on_disconnect(&self, callback: js_sys::Function) {
        self.state.borrow_mut().on_disconnect = Some(callback);
    }

    pub fn on_error(&self, callback: js_sys::Function) {
        self.state.borrow_mut().on_error = Some(callback);
    }

    pub fn on_auth_challenge(&self, callback: js_sys::Function) {
        self.state.borrow_mut().on_auth_challenge = Some(callback);
    }

    pub fn on_reconnecting(&self, callback: js_sys::Function) {
        self.state.borrow_mut().on_reconnecting = Some(callback);
    }

    pub fn on_reconnect_failed(&self, callback: js_sys::Function) {
        self.state.borrow_mut().on_reconnect_failed = Some(callback);
    }

    pub fn set_reconnect_options(&self, options: &WasmReconnectOptions) {
        self.state.borrow_mut().reconnect_config = options.to_reconnect_config();
    }

    pub fn enable_auto_reconnect(&self, enabled: bool) {
        self.state.borrow_mut().reconnect_config.enabled = enabled;
    }

    #[must_use]
    pub fn is_reconnecting(&self) -> bool {
        self.state.borrow().reconnecting
    }

    /// # Errors
    /// Returns an error if no auth method is set or send fails.
    pub fn respond_auth(&self, auth_data: &[u8]) -> Result<(), JsValue> {
        let auth_method = self
            .state
            .borrow()
            .auth_method
            .clone()
            .ok_or_else(|| JsValue::from_str("No auth method set"))?;

        let mut auth_packet = mqtt5_protocol::packet::auth::AuthPacket::new(
            mqtt5_protocol::protocol::v5::reason_codes::ReasonCode::ContinueAuthentication,
        );
        auth_packet
            .properties
            .set_authentication_method(auth_method);
        auth_packet
            .properties
            .set_authentication_data(auth_data.to_vec().into());

        let packet = Packet::Auth(auth_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("AUTH packet encoding failed: {e}")))?;

        let writer_rc = self
            .state
            .borrow()
            .writer
            .clone()
            .ok_or_else(|| JsValue::from_str("Writer disconnected"))?;

        writer_rc
            .borrow_mut()
            .write(&buf)
            .map_err(|e| JsValue::from_str(&format!("AUTH send failed: {e}")))?;

        Ok(())
    }
}

impl WasmMqttClient {
    /// # Errors
    /// Returns an error if not connected or subscribe fails.
    pub async fn subscribe_with_callback_internal(
        &self,
        topic: &str,
        qos: QoS,
        callback: Box<dyn Fn(RustMessage)>,
    ) -> Result<u16, JsValue> {
        self.subscribe_with_callback_internal_opts(topic, qos, false, callback)
            .await
    }

    /// # Errors
    /// Returns an error if not connected or subscribe fails.
    pub async fn subscribe_with_callback_internal_opts(
        &self,
        topic: &str,
        qos: QoS,
        no_local: bool,
        callback: Box<dyn Fn(RustMessage)>,
    ) -> Result<u16, JsValue> {
        loop {
            match self.state.try_borrow() {
                Ok(state) => {
                    if !state.connected {
                        return Err(JsValue::from_str("Not connected"));
                    }
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        }

        let packet_id = loop {
            match self.state.try_borrow_mut() {
                Ok(state) => break state.packet_id.next(),
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        };

        loop {
            match self.state.try_borrow_mut() {
                Ok(mut state) => {
                    let actual_filter = strip_shared_subscription_prefix(topic);
                    state
                        .rust_subscriptions
                        .insert(actual_filter.to_string(), Rc::new(callback));
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        }

        let mut options = mqtt5_protocol::packet::subscribe::SubscriptionOptions::new(qos);
        options.no_local = no_local;

        let protocol_version = self.state.borrow().protocol_version;
        let subscribe_packet = SubscribePacket {
            packet_id,
            properties: Properties::default(),
            filters: vec![
                mqtt5_protocol::packet::subscribe::TopicFilter::with_options(topic, options),
            ],
            protocol_version,
        };

        let packet = Packet::Subscribe(subscribe_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {e}")))?;

        let writer_rc = self
            .state
            .borrow()
            .writer
            .clone()
            .ok_or_else(|| JsValue::from_str("Writer disconnected"))?;

        writer_rc
            .borrow_mut()
            .write(&buf)
            .map_err(|e| JsValue::from_str(&format!("Write failed: {e}")))?;

        Ok(packet_id)
    }

    /// # Errors
    /// Returns an error if not connected or publish fails.
    pub async fn publish_internal(
        &self,
        topic: &str,
        payload: &[u8],
        qos: QoS,
    ) -> Result<(), JsValue> {
        loop {
            match self.state.try_borrow() {
                Ok(state) => {
                    if !state.connected {
                        return Err(JsValue::from_str("Not connected"));
                    }
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        }

        let packet_id = if qos == QoS::AtMostOnce {
            None
        } else {
            Some(loop {
                match self.state.try_borrow_mut() {
                    Ok(state) => break state.packet_id.next(),
                    Err(_) => {
                        sleep_ms(10).await;
                    }
                }
            })
        };

        if qos == QoS::ExactlyOnce {
            if let Some(pid) = packet_id {
                let now = js_sys::Date::now();
                let noop_callback = js_sys::Function::new_no_args("");
                loop {
                    match self.state.try_borrow_mut() {
                        Ok(mut state) => {
                            state.pending_pubcomps.insert(pid, (noop_callback, now));
                            break;
                        }
                        Err(_) => {
                            sleep_ms(10).await;
                        }
                    }
                }
            }
        }

        let mut publish_packet = PublishPacket::new(topic.to_string(), payload.to_vec(), qos);
        publish_packet.packet_id = packet_id;

        let packet = Packet::Publish(publish_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("PUBLISH packet encoding failed: {e}")))?;

        let writer_rc = self
            .state
            .borrow()
            .writer
            .clone()
            .ok_or_else(|| JsValue::from_str("Writer disconnected"))?;

        writer_rc
            .borrow_mut()
            .write(&buf)
            .map_err(|e| JsValue::from_str(&format!("PUBLISH send failed: {e}")))?;

        Ok(())
    }
}
