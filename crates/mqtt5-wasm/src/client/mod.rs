mod callbacks;
mod connectivity;
mod handlers;
mod keepalive;
mod packet;
mod qos;
mod reader;
mod reconnect;
mod state;

use crate::config::{
    WasmConnectOptions, WasmPublishOptions, WasmReconnectOptions, WasmSubscribeOptions,
};
use crate::decoder::read_packet;
use crate::transport::{WasmReader, WasmTransportType};
use bytes::BytesMut;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::subscribe::SubscribePacket;
use mqtt5_protocol::packet::unsubscribe::UnsubscribePacket;
use mqtt5_protocol::packet::Packet;
use mqtt5_protocol::protocol::v5::properties::Properties;
use mqtt5_protocol::strip_shared_subscription_prefix;
use mqtt5_protocol::QoS;
use mqtt5_protocol::Transport;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::MessagePort;

use callbacks::{drain_pending_callbacks, trigger_disconnect_callback};
use keepalive::spawn_keepalive_task;
use packet::encode_packet;
use qos::{await_ack_promises, create_ack_promises, spawn_qos2_cleanup_task};
use reader::spawn_packet_reader;
use state::{ClientState, StoredConnectOptions};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = "setTimeout")]
    fn set_timeout(handler: &js_sys::Function, timeout: i32) -> i32;
}

pub async fn sleep_ms(millis: u32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        set_timeout(&resolve, i32::try_from(millis).unwrap_or(i32::MAX));
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

#[wasm_bindgen]
pub struct WasmMqttClient {
    state: Rc<RefCell<ClientState>>,
}

#[wasm_bindgen]
impl WasmMqttClient {
    #[wasm_bindgen(constructor)]
    #[allow(clippy::must_use_candidate)]
    pub fn new(client_id: String) -> Self {
        console_error_panic_hook::set_once();

        let state = Rc::new(RefCell::new(ClientState::new(client_id)));
        let (online_fn, offline_fn) = connectivity::register_connectivity_listeners(&state);
        {
            let mut s = state.borrow_mut();
            s.online_listener_fn = Some(online_fn);
            s.offline_listener_fn = Some(offline_fn);
        }

        Self { state }
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
        let lower = url.to_ascii_lowercase();
        if !lower.starts_with("ws://") && !lower.starts_with("wss://") {
            return Err(JsValue::from_str("URL must start with ws:// or wss://"));
        }
        if lower.starts_with("ws://") && (config.username.is_some() || config.password.is_some()) {
            web_sys::console::warn_1(&"Credentials sent over unencrypted ws:// connection".into());
        }

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

    async fn connect_with_transport_and_config(
        &self,
        mut transport: WasmTransportType,
        config: &WasmConnectOptions,
    ) -> Result<(), JsValue> {
        {
            let state_ref = self.state.borrow();
            if state_ref.connected {
                return Err(JsValue::from_str("Already connected"));
            }
            if state_ref.reconnecting {
                return Err(JsValue::from_str("Reconnection in progress"));
            }
        }

        transport
            .connect()
            .await
            .map_err(|e| JsValue::from_str(&format!("Transport connection failed: {e}")))?;

        let client_id = self.state.borrow().client_id.clone();

        {
            let mut state = self.state.borrow_mut();
            state.keep_alive = config.keep_alive;
            state.protocol_version = config.protocol_version;
            state.last_options = Some(StoredConnectOptions::from(config));
            state.user_initiated_disconnect = false;
            state.reconnect_attempt = 0;
            #[cfg(feature = "codec")]
            {
                state.codec_registry.clone_from(&config.codec_registry);
            }
        }

        let packet = Packet::Connect(Box::new(build_connect_packet(client_id, config)));
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

        let (reader, writer) = transport
            .into_split()
            .map_err(|e| JsValue::from_str(&format!("Transport split failed: {e}")))?;

        let writer_rc = Rc::new(RefCell::new(writer));
        self.state.borrow_mut().writer = Some(Rc::clone(&writer_rc));

        self.handle_connect_response(reader).await
    }

    async fn handle_connect_response(&self, mut reader: WasmReader) -> Result<(), JsValue> {
        loop {
            let packet = read_packet(&mut reader)
                .await
                .map_err(|e| JsValue::from_str(&format!("Packet read failed: {e}")))?;

            match packet {
                Packet::ConnAck(connack) => {
                    let reason_code = connack.reason_code as u8;
                    let session_present = connack.session_present;

                    if reason_code != 0 {
                        return Err(JsValue::from_str(&format!(
                            "Connection rejected: {}",
                            connack_error_description(reason_code)
                        )));
                    }

                    {
                        let mut state_mut = self.state.borrow_mut();
                        state_mut.connected = true;
                        state_mut.connection_generation =
                            state_mut.connection_generation.wrapping_add(1);
                    }

                    spawn_packet_reader(Rc::clone(&self.state), reader);
                    spawn_keepalive_task(Rc::clone(&self.state));
                    spawn_qos2_cleanup_task(Rc::clone(&self.state));

                    let callback = self.state.borrow().on_connect.clone();
                    if let Some(callback) = callback {
                        let reason_code_js = JsValue::from_f64(f64::from(reason_code));
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
        self.ensure_connected().await?;

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

        self.send_packet(&Packet::Publish(publish_packet))
    }

    /// # Errors
    /// Returns an error if not connected or publish fails.
    pub async fn publish_with_options(
        &self,
        topic: &str,
        payload: &[u8],
        options: &WasmPublishOptions,
    ) -> Result<(), JsValue> {
        self.ensure_connected().await?;

        let qos = options.to_qos();
        let packet_id = if qos == QoS::AtMostOnce {
            None
        } else {
            Some(self.state.borrow_mut().packet_id.next())
        };

        let (puback_promise, pubcomp_promise) = create_ack_promises(&self.state, qos, packet_id);

        #[cfg(feature = "codec")]
        let (final_payload, codec_content_type) = {
            let registry = self.state.borrow().codec_registry.clone();
            if let Some(ref reg) = registry {
                match reg.encode_with_default(payload) {
                    Ok((encoded, ct)) => (encoded, ct),
                    Err(_) => (payload.to_vec(), None),
                }
            } else {
                (payload.to_vec(), None)
            }
        };

        #[cfg(not(feature = "codec"))]
        let (final_payload, codec_content_type): (Vec<u8>, Option<String>) =
            (payload.to_vec(), None);

        let protocol_version = self.state.borrow().protocol_version;
        let mut properties = if protocol_version == 5 {
            options.to_properties()
        } else {
            Properties::default()
        };

        if let Some(ct) = codec_content_type {
            if properties
                .add(
                    mqtt5_protocol::protocol::v5::properties::PropertyId::ContentType,
                    mqtt5_protocol::protocol::v5::properties::PropertyValue::Utf8String(ct),
                )
                .is_err()
            {
                web_sys::console::warn_1(&"Failed to add codec content type property".into());
            }
        }

        let publish_packet = PublishPacket {
            dup: false,
            qos,
            retain: options.retain,
            topic_name: topic.to_string(),
            packet_id,
            properties,
            payload: final_payload.into(),
            protocol_version,
        };

        self.send_packet(&Packet::Publish(publish_packet))?;
        await_ack_promises(puback_promise, pubcomp_promise).await
    }

    /// # Errors
    /// Returns an error if not connected or publish fails.
    pub async fn publish_qos1(
        &self,
        topic: &str,
        payload: &[u8],
        callback: js_sys::Function,
    ) -> Result<u16, JsValue> {
        self.ensure_connected().await?;

        let packet_id = self.state.borrow_mut().packet_id.next();
        self.state
            .borrow_mut()
            .pending_pubacks
            .insert(packet_id, callback);

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

        self.send_packet(&Packet::Publish(publish_packet))?;
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
        self.ensure_connected().await?;

        let packet_id = self.state.borrow_mut().packet_id.next();
        let now = js_sys::Date::now();
        self.state
            .borrow_mut()
            .pending_pubcomps
            .insert(packet_id, (callback, now));

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

        self.send_packet(&Packet::Publish(publish_packet))?;
        Ok(packet_id)
    }

    /// # Errors
    /// Returns an error if not connected or subscribe fails.
    #[allow(clippy::unused_async)]
    pub async fn subscribe(&self, topic: &str) -> Result<u16, JsValue> {
        if !self.state.borrow().connected {
            return Err(JsValue::from_str("Not connected"));
        }

        let packet_id = self.state.borrow_mut().packet_id.next();

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

        self.send_packet(&Packet::Subscribe(subscribe_packet))?;
        Ok(packet_id)
    }

    /// # Errors
    /// Returns an error if not connected or subscribe fails.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub async fn subscribe_with_options(
        &self,
        topic: &str,
        callback: js_sys::Function,
        options: &WasmSubscribeOptions,
    ) -> Result<u16, JsValue> {
        if !self.state.borrow().connected {
            return Err(JsValue::from_str("Not connected"));
        }

        let packet_id = self.state.borrow_mut().packet_id.next();
        let actual_filter = strip_shared_subscription_prefix(topic);
        self.state
            .borrow_mut()
            .subscriptions
            .insert(actual_filter.to_string(), callback);

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

        self.send_packet(&Packet::Subscribe(subscribe_packet))?;

        let state = Rc::clone(&self.state);
        let promise = js_sys::Promise::new(&mut move |resolve, _reject| {
            state
                .borrow_mut()
                .pending_subacks
                .insert(packet_id, resolve);
        });

        let result = JsFuture::from(promise).await?;
        let reason_codes = js_sys::Array::from(&result);

        if reason_codes.length() > 0 {
            let first_code = reason_codes.get(0).as_f64().unwrap_or(0.0) as u8;
            if first_code >= 0x80 {
                let actual_filter = strip_shared_subscription_prefix(topic);
                self.state.borrow_mut().subscriptions.remove(actual_filter);
                return Err(JsValue::from_str(&format!(
                    "Subscribe rejected with reason code: {first_code}"
                )));
            }
        }

        Ok(packet_id)
    }

    /// # Errors
    /// Returns an error if not connected or subscribe fails.
    #[allow(clippy::unused_async)]
    pub async fn subscribe_with_callback(
        &self,
        topic: &str,
        callback: js_sys::Function,
    ) -> Result<u16, JsValue> {
        if !self.state.borrow().connected {
            return Err(JsValue::from_str("Not connected"));
        }

        let packet_id = self.state.borrow_mut().packet_id.next();
        let actual_filter = strip_shared_subscription_prefix(topic);
        self.state
            .borrow_mut()
            .subscriptions
            .insert(actual_filter.to_string(), callback);

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

        self.send_packet(&Packet::Subscribe(subscribe_packet))?;
        Ok(packet_id)
    }

    /// # Errors
    /// Returns an error if not connected or unsubscribe fails.
    #[allow(clippy::unused_async)]
    pub async fn unsubscribe(&self, topic: &str) -> Result<u16, JsValue> {
        if !self.state.borrow().connected {
            return Err(JsValue::from_str("Not connected"));
        }

        let packet_id = self.state.borrow_mut().packet_id.next();
        self.state.borrow_mut().subscriptions.remove(topic);

        let protocol_version = self.state.borrow().protocol_version;
        let unsubscribe_packet = UnsubscribePacket {
            packet_id,
            properties: Properties::default(),
            filters: vec![topic.to_string()],
            protocol_version,
        };

        self.send_packet(&Packet::Unsubscribe(unsubscribe_packet))?;
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
                    state.connection_generation = state.connection_generation.wrapping_add(1);
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

        drain_pending_callbacks(&mut self.state.borrow_mut());
        trigger_disconnect_callback(&self.state);
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

    pub fn on_connectivity_change(&self, callback: js_sys::Function) {
        self.state.borrow_mut().on_connectivity_change = Some(callback);
    }

    #[must_use]
    pub fn is_browser_online(&self) -> bool {
        connectivity::is_browser_online()
    }

    pub fn destroy(&self) {
        let state = self.state.borrow();
        if let (Some(online_fn), Some(offline_fn)) =
            (&state.online_listener_fn, &state.offline_listener_fn)
        {
            connectivity::remove_connectivity_listeners(online_fn, offline_fn);
        }
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

        self.send_packet(&Packet::Auth(auth_packet))
    }

    async fn ensure_connected(&self) -> Result<(), JsValue> {
        loop {
            match self.state.try_borrow() {
                Ok(state) => {
                    if !state.connected {
                        return Err(JsValue::from_str("Not connected"));
                    }
                    return Ok(());
                }
                Err(_) => {
                    sleep_ms(10).await;
                }
            }
        }
    }

    fn send_packet(&self, packet: &Packet) -> Result<(), JsValue> {
        let mut buf = BytesMut::new();
        encode_packet(packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {e}")))?;

        let writer_rc = self
            .state
            .borrow()
            .writer
            .clone()
            .ok_or_else(|| JsValue::from_str("Writer disconnected"))?;

        let result = writer_rc
            .borrow_mut()
            .write(&buf)
            .map_err(|e| JsValue::from_str(&format!("Write failed: {e}")));
        result
    }
}

fn build_connect_packet(client_id: String, config: &WasmConnectOptions) -> ConnectPacket {
    let (will, will_properties) = if let Some(will_config) = &config.will {
        let will_msg = will_config.to_will_message();
        let will_props = will_msg.properties.clone().into();
        (Some(will_msg), will_props)
    } else {
        (None, Properties::default())
    };

    let (properties, will_properties) = if config.protocol_version == 5 {
        (config.to_properties(), will_properties)
    } else {
        (Properties::default(), Properties::default())
    };

    ConnectPacket {
        protocol_version: config.protocol_version,
        clean_start: config.clean_start,
        keep_alive: config.keep_alive,
        client_id,
        username: config.username.clone(),
        password: config.password.clone(),
        will,
        properties,
        will_properties,
    }
}

fn connack_error_description(reason_code: u8) -> &'static str {
    match reason_code {
        0x80 => "Unspecified error",
        0x81 => "Malformed packet",
        0x82 => "Protocol error",
        0x83 => "Implementation specific error",
        0x84 => "Unsupported protocol version",
        0x85 => "Client identifier not valid",
        0x86 => "Bad username or password",
        0x87 => "Not authorized",
        0x88 => "Server unavailable",
        0x89 => "Server busy",
        0x8A => "Banned",
        0x8C => "Bad authentication method",
        0x90 => "Topic name invalid",
        0x97 => "Quota exceeded",
        0x9F => "Connection rate exceeded",
        _ => "Unknown error",
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
    #[allow(clippy::unused_async)]
    pub async fn subscribe_with_callback_internal_opts(
        &self,
        topic: &str,
        qos: QoS,
        no_local: bool,
        callback: Box<dyn Fn(RustMessage)>,
    ) -> Result<u16, JsValue> {
        if !self.state.borrow().connected {
            return Err(JsValue::from_str("Not connected"));
        }

        let packet_id = self.state.borrow_mut().packet_id.next();
        let actual_filter = strip_shared_subscription_prefix(topic);
        self.state
            .borrow_mut()
            .rust_subscriptions
            .insert(actual_filter.to_string(), Rc::new(callback));

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

        self.send_packet(&Packet::Subscribe(subscribe_packet))?;
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
        if !self.state.borrow().connected {
            return Err(JsValue::from_str("Not connected"));
        }

        let packet_id = if qos == QoS::AtMostOnce {
            None
        } else {
            Some(self.state.borrow_mut().packet_id.next())
        };

        let (puback_promise, pubcomp_promise) = create_ack_promises(&self.state, qos, packet_id);

        let mut publish_packet = PublishPacket::new(topic.to_string(), payload.to_vec(), qos);
        publish_packet.packet_id = packet_id;

        self.send_packet(&Packet::Publish(publish_packet))?;
        await_ack_promises(puback_promise, pubcomp_promise).await
    }
}
