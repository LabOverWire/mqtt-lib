mod callbacks;
mod connection;
mod connectivity;
mod handlers;
mod keepalive;
mod outbound;
mod packet;
mod qos;
mod reader;
mod reconnect;
mod state;

use crate::config::{
    WasmConnectOptions, WasmPublishOptions, WasmReconnectOptions, WasmSubscribeOptions,
};
use crate::transport::WasmTransportType;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::disconnect::DisconnectPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::subscribe::{SubscribePacket, TopicFilter};
use mqtt5_protocol::packet::unsubscribe::UnsubscribePacket;
use mqtt5_protocol::packet::Packet;
use mqtt5_protocol::protocol::v5::properties::{Properties, PropertyId, PropertyValue};
use mqtt5_protocol::strip_shared_subscription_prefix;
use mqtt5_protocol::QoS;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::MessagePort;

use callbacks::{close_network_connection, end_connection_state, trigger_disconnect_callback};
use connection::establish;
use outbound::{check_publish, check_subscribe, check_unsubscribe};
use packet::write_packet;
use qos::{abandon_flight, await_ack_promises, create_ack_promises, reserve_flight};
use state::{ClientState, StoredConnectOptions};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = "setTimeout")]
    fn set_timeout(handler: &js_sys::Function, timeout: i32) -> JsValue;
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

enum AckSink {
    Promise,
    Callback(js_sys::Function),
}

fn js_error(message: impl AsRef<str>) -> JsValue {
    JsValue::from_str(message.as_ref())
}

#[wasm_bindgen(js_name = "MqttClient")]
pub struct WasmMqttClient {
    state: Rc<RefCell<ClientState>>,
}

#[wasm_bindgen(js_class = "MqttClient")]
impl WasmMqttClient {
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(#[wasm_bindgen(js_name = clientId)] client_id: String) -> Self {
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
    #[wasm_bindgen(js_name = "connectWithOptions")]
    pub async fn connect_with_options(
        &self,
        url: &str,
        config: &WasmConnectOptions,
    ) -> Result<(), JsValue> {
        let lower = url.to_ascii_lowercase();
        if !lower.starts_with("ws://") && !lower.starts_with("wss://") {
            return Err(js_error("URL must start with ws:// or wss://"));
        }
        if lower.starts_with("ws://") && (config.username.is_some() || config.password.is_some()) {
            tracing::warn!("credentials sent over an unencrypted ws:// connection");
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
    #[wasm_bindgen(js_name = "connectMessagePort")]
    pub async fn connect_message_port(&self, port: MessagePort) -> Result<(), JsValue> {
        let config = WasmConnectOptions::default();
        self.connect_message_port_with_options(port, &config).await
    }

    /// # Errors
    /// Returns an error if connection fails.
    #[wasm_bindgen(js_name = "connectMessagePortWithOptions")]
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
    #[wasm_bindgen(js_name = "connectBroadcastChannel")]
    pub async fn connect_broadcast_channel(
        &self,
        #[wasm_bindgen(js_name = channelName)] channel_name: &str,
    ) -> Result<(), JsValue> {
        let config = WasmConnectOptions::default();
        let transport = WasmTransportType::BroadcastChannel(
            crate::transport::broadcast::BroadcastChannelTransport::new(channel_name),
        );
        self.connect_with_transport_and_config(transport, &config)
            .await
    }

    async fn connect_with_transport_and_config(
        &self,
        transport: WasmTransportType,
        config: &WasmConnectOptions,
    ) -> Result<(), JsValue> {
        let stored = StoredConnectOptions::from(config);
        let client_id = {
            let mut state = self.state.borrow_mut();
            if state.connected {
                return Err(js_error("Already connected"));
            }
            if state.reconnecting {
                return Err(js_error("Reconnection in progress"));
            }
            state.last_options = Some(stored.clone());
            state.user_initiated_disconnect = false;
            state.reconnect_attempt = 0;
            state.client_id.clone()
        };

        let connect = build_connect_packet(client_id, config);
        establish(&self.state, transport, connect, &stored)
            .await
            .map(|_| ())
            .map_err(JsValue::from)
    }

    /// # Errors
    /// Returns an error if not connected or publish fails.
    pub async fn publish(&self, topic: &str, payload: &[u8]) -> Result<(), JsValue> {
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
            stream_id: None,
        };
        self.dispatch_publish(publish_packet, AckSink::Promise)
            .await
            .map(|_| ())
    }

    /// # Errors
    /// Returns an error if not connected or publish fails.
    #[wasm_bindgen(js_name = "publishWithOptions")]
    pub async fn publish_with_options(
        &self,
        topic: &str,
        payload: &[u8],
        options: &WasmPublishOptions,
    ) -> Result<(), JsValue> {
        let qos = options.to_qos();

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
                .add(PropertyId::ContentType, PropertyValue::Utf8String(ct))
                .is_err()
            {
                tracing::warn!("failed to add codec content type property");
            }
        }

        let publish_packet = PublishPacket {
            dup: false,
            qos,
            retain: options.retain,
            topic_name: topic.to_string(),
            packet_id: None,
            properties,
            payload: final_payload.into(),
            protocol_version,
            stream_id: None,
        };

        let packet_id = self
            .dispatch_publish(publish_packet, AckSink::Promise)
            .await?;
        let (puback_promise, pubcomp_promise) = create_ack_promises(&self.state, qos, packet_id);
        await_ack_promises(puback_promise, pubcomp_promise).await
    }

    /// # Errors
    /// Returns an error if not connected or publish fails.
    #[wasm_bindgen(js_name = "publishQos1")]
    pub async fn publish_qos1(
        &self,
        topic: &str,
        payload: &[u8],
        callback: js_sys::Function,
    ) -> Result<u16, JsValue> {
        self.publish_with_callback(topic, payload, QoS::AtLeastOnce, callback)
            .await
    }

    /// # Errors
    /// Returns an error if not connected or publish fails.
    #[wasm_bindgen(js_name = "publishQos2")]
    pub async fn publish_qos2(
        &self,
        topic: &str,
        payload: &[u8],
        callback: js_sys::Function,
    ) -> Result<u16, JsValue> {
        self.publish_with_callback(topic, payload, QoS::ExactlyOnce, callback)
            .await
    }

    /// # Errors
    /// Returns an error if not connected or subscribe fails.
    pub async fn subscribe(&self, topic: &str) -> Result<u16, JsValue> {
        let filter = TopicFilter::new(topic, QoS::AtMostOnce);
        self.send_subscribe(filter, Properties::default(), None)
            .await
    }

    /// # Errors
    /// Returns an error if not connected or subscribe fails.
    #[wasm_bindgen(js_name = "subscribeWithOptions")]
    pub async fn subscribe_with_options(
        &self,
        topic: &str,
        callback: js_sys::Function,
        options: &WasmSubscribeOptions,
    ) -> Result<u16, JsValue> {
        let mut topic_filter = TopicFilter::new(topic, options.to_qos());
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
                    PropertyId::SubscriptionIdentifier,
                    PropertyValue::VariableByteInteger(id),
                )
                .is_err()
            {
                tracing::warn!("failed to add subscription identifier property");
            }
        }

        let packet_id = self
            .send_subscribe(topic_filter, properties, Some(callback))
            .await?;

        let state = Rc::clone(&self.state);
        let promise = js_sys::Promise::new(&mut move |resolve, _reject| {
            if let Some(slot) = state.borrow_mut().pending_subacks.get_mut(&packet_id) {
                *slot = Some(resolve);
            }
        });

        let result = JsFuture::from(promise).await?;
        let reason_codes = js_sys::Array::from(&result);
        let first_code = reason_codes.get(0).as_f64().unwrap_or(0.0);
        if first_code >= 128.0 {
            let actual_filter = strip_shared_subscription_prefix(topic);
            self.state.borrow_mut().subscriptions.remove(actual_filter);
            return Err(js_error(format!(
                "Subscribe rejected with reason code: {first_code}"
            )));
        }

        Ok(packet_id)
    }

    /// # Errors
    /// Returns an error if not connected or subscribe fails.
    #[wasm_bindgen(js_name = "subscribeWithCallback")]
    pub async fn subscribe_with_callback(
        &self,
        topic: &str,
        callback: js_sys::Function,
    ) -> Result<u16, JsValue> {
        let filter = TopicFilter::new(topic, QoS::AtMostOnce);
        self.send_subscribe(filter, Properties::default(), Some(callback))
            .await
    }

    /// # Errors
    /// Returns an error if not connected or unsubscribe fails.
    pub async fn unsubscribe(&self, topic: &str) -> Result<u16, JsValue> {
        self.ensure_connected().await?;

        let protocol_version = self.state.borrow().protocol_version;
        let mut unsubscribe_packet = UnsubscribePacket {
            packet_id: 0,
            properties: Properties::default(),
            filters: vec![topic.to_string()],
            protocol_version,
        };
        check_unsubscribe(&self.state.borrow(), &unsubscribe_packet).map_err(js_error)?;

        let packet_id = self.reserve_packet_id()?;
        unsubscribe_packet.packet_id = packet_id;
        self.state.borrow_mut().pending_unsubacks.insert(packet_id);

        if let Err(e) = write_packet(&self.state, &Packet::Unsubscribe(unsubscribe_packet)) {
            self.state.borrow_mut().pending_unsubacks.remove(&packet_id);
            return Err(js_error(e));
        }
        self.state
            .borrow_mut()
            .subscriptions
            .remove(strip_shared_subscription_prefix(topic));
        Ok(packet_id)
    }

    /// # Errors
    /// Returns an error if disconnect fails.
    pub async fn disconnect(&self) -> Result<(), JsValue> {
        self.ensure_not_borrowed().await;
        let connected = {
            let mut state = self.state.borrow_mut();
            state.user_initiated_disconnect = true;
            state.connected
        };
        if connected {
            let disconnect = DisconnectPacket::normal();
            if let Err(e) = write_packet(&self.state, &Packet::Disconnect(disconnect)) {
                tracing::warn!(error = %e, "DISCONNECT not sent");
            }
        }
        close_network_connection(&self.state);
        end_connection_state(&self.state);
        trigger_disconnect_callback(&self.state);
        Ok(())
    }

    #[must_use]
    #[wasm_bindgen(js_name = "isConnected")]
    pub fn is_connected(&self) -> bool {
        self.state.borrow().connected
    }

    #[wasm_bindgen(js_name = "onConnect")]
    pub fn on_connect(&self, callback: js_sys::Function) {
        self.state.borrow_mut().on_connect = Some(callback);
    }

    #[wasm_bindgen(js_name = "onDisconnect")]
    pub fn on_disconnect(&self, callback: js_sys::Function) {
        self.state.borrow_mut().on_disconnect = Some(callback);
    }

    #[wasm_bindgen(js_name = "onError")]
    pub fn on_error(&self, callback: js_sys::Function) {
        self.state.borrow_mut().on_error = Some(callback);
    }

    #[wasm_bindgen(js_name = "onAuthChallenge")]
    pub fn on_auth_challenge(&self, callback: js_sys::Function) {
        self.state.borrow_mut().on_auth_challenge = Some(callback);
    }

    #[wasm_bindgen(js_name = "onReconnecting")]
    pub fn on_reconnecting(&self, callback: js_sys::Function) {
        self.state.borrow_mut().on_reconnecting = Some(callback);
    }

    #[wasm_bindgen(js_name = "onReconnectFailed")]
    pub fn on_reconnect_failed(&self, callback: js_sys::Function) {
        self.state.borrow_mut().on_reconnect_failed = Some(callback);
    }

    #[wasm_bindgen(js_name = "onConnectivityChange")]
    pub fn on_connectivity_change(&self, callback: js_sys::Function) {
        self.state.borrow_mut().on_connectivity_change = Some(callback);
    }

    #[must_use]
    #[wasm_bindgen(js_name = "isBrowserOnline")]
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

    #[wasm_bindgen(js_name = "setReconnectOptions")]
    pub fn set_reconnect_options(&self, options: &WasmReconnectOptions) {
        self.state.borrow_mut().reconnect_config = options.to_reconnect_config();
    }

    #[wasm_bindgen(js_name = "enableAutoReconnect")]
    pub fn enable_auto_reconnect(&self, enabled: bool) {
        self.state.borrow_mut().reconnect_config.enabled = enabled;
    }

    #[must_use]
    #[wasm_bindgen(js_name = "isReconnecting")]
    pub fn is_reconnecting(&self) -> bool {
        self.state.borrow().reconnecting
    }

    /// # Errors
    /// Returns an error if no auth method is set or send fails.
    #[wasm_bindgen(js_name = "respondAuth")]
    pub fn respond_auth(
        &self,
        #[wasm_bindgen(js_name = authData)] auth_data: &[u8],
    ) -> Result<(), JsValue> {
        let auth_method = self
            .state
            .borrow()
            .auth_method
            .clone()
            .ok_or_else(|| js_error("No auth method set"))?;

        let mut auth_packet = mqtt5_protocol::packet::auth::AuthPacket::new(
            mqtt5_protocol::protocol::v5::reason_codes::ReasonCode::ContinueAuthentication,
        );
        auth_packet
            .properties
            .set_authentication_method(auth_method);
        auth_packet
            .properties
            .set_authentication_data(auth_data.to_vec().into());

        write_packet(&self.state, &Packet::Auth(auth_packet)).map_err(js_error)
    }

    async fn ensure_not_borrowed(&self) {
        while self.state.try_borrow_mut().is_err() {
            sleep_ms(10).await;
        }
    }

    async fn ensure_connected(&self) -> Result<(), JsValue> {
        self.ensure_not_borrowed().await;
        if self.state.borrow().connected {
            Ok(())
        } else {
            Err(js_error("Not connected"))
        }
    }

    fn reserve_packet_id(&self) -> Result<u16, JsValue> {
        self.state
            .borrow()
            .allocate_packet_id()
            .ok_or_else(|| js_error("No packet identifier available"))
    }

    async fn publish_with_callback(
        &self,
        topic: &str,
        payload: &[u8],
        qos: QoS,
        callback: js_sys::Function,
    ) -> Result<u16, JsValue> {
        let protocol_version = self.state.borrow().protocol_version;
        let publish_packet = PublishPacket {
            dup: false,
            qos,
            retain: false,
            topic_name: topic.to_string(),
            packet_id: None,
            properties: Properties::default(),
            payload: payload.to_vec().into(),
            protocol_version,
            stream_id: None,
        };
        self.dispatch_publish(publish_packet, AckSink::Callback(callback))
            .await?
            .ok_or_else(|| js_error("QoS 0 publish has no packet identifier"))
    }

    async fn dispatch_publish(
        &self,
        mut publish: PublishPacket,
        sink: AckSink,
    ) -> Result<Option<u16>, JsValue> {
        self.ensure_connected().await?;
        check_publish(&self.state.borrow(), &publish).map_err(js_error)?;

        let packet_id = if publish.qos == QoS::AtMostOnce {
            None
        } else {
            let packet_id = reserve_flight(&self.state, &publish).await?;
            if let Err(e) = check_publish(&self.state.borrow(), &publish) {
                abandon_flight(&self.state, packet_id);
                return Err(js_error(e));
            }
            Some(packet_id)
        };
        publish.packet_id = packet_id;

        if let (Some(packet_id), AckSink::Callback(callback)) = (packet_id, sink) {
            let mut state = self.state.borrow_mut();
            if publish.qos == QoS::ExactlyOnce {
                state
                    .pending_pubcomps
                    .insert(packet_id, (callback, js_sys::Date::now()));
            } else {
                state.pending_pubacks.insert(packet_id, callback);
            }
        }

        let alias_mapping = publish
            .topic_alias()
            .filter(|_| !publish.topic_name.is_empty())
            .map(|alias| (alias, publish.topic_name.clone()));

        if let Err(e) = write_packet(&self.state, &Packet::Publish(publish)) {
            if let Some(packet_id) = packet_id {
                {
                    let mut state = self.state.borrow_mut();
                    state.pending_pubacks.remove(&packet_id);
                    state.pending_pubcomps.remove(&packet_id);
                }
                abandon_flight(&self.state, packet_id);
            }
            return Err(js_error(e));
        }

        if let Some((alias, topic)) = alias_mapping {
            if let Err(e) = self
                .state
                .borrow_mut()
                .outbound_aliases
                .register_alias(alias, &topic)
            {
                tracing::warn!(alias, topic, error = %e, "outbound Topic Alias not recorded");
            }
        }

        Ok(packet_id)
    }

    async fn send_subscribe(
        &self,
        filter: TopicFilter,
        properties: Properties,
        callback: Option<js_sys::Function>,
    ) -> Result<u16, JsValue> {
        self.ensure_connected().await?;

        let protocol_version = self.state.borrow().protocol_version;
        let properties = if protocol_version == 5 {
            properties
        } else {
            Properties::default()
        };
        let topic = filter.filter.clone();
        let mut subscribe_packet = SubscribePacket {
            packet_id: 0,
            properties,
            filters: vec![filter],
            protocol_version,
        };
        check_subscribe(&self.state.borrow(), &subscribe_packet).map_err(js_error)?;

        let packet_id = self.reserve_packet_id()?;
        subscribe_packet.packet_id = packet_id;
        {
            let mut state = self.state.borrow_mut();
            state.pending_subacks.insert(packet_id, None);
            if let Some(callback) = callback {
                state.subscriptions.insert(
                    strip_shared_subscription_prefix(&topic).to_string(),
                    callback,
                );
            }
        }

        if let Err(e) = write_packet(&self.state, &Packet::Subscribe(subscribe_packet)) {
            self.state.borrow_mut().pending_subacks.remove(&packet_id);
            return Err(js_error(e));
        }
        Ok(packet_id)
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
        let mut options = mqtt5_protocol::packet::subscribe::SubscriptionOptions::new(qos);
        options.no_local = no_local;
        let packet_id = self
            .send_subscribe(
                TopicFilter::with_options(topic, options),
                Properties::default(),
                None,
            )
            .await?;
        self.state.borrow_mut().rust_subscriptions.insert(
            strip_shared_subscription_prefix(topic).to_string(),
            Rc::new(callback),
        );
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
        let publish_packet = PublishPacket::new(topic.to_string(), payload.to_vec(), qos);
        let packet_id = self
            .dispatch_publish(publish_packet, AckSink::Promise)
            .await?;
        let (puback_promise, pubcomp_promise) = create_ack_promises(&self.state, qos, packet_id);
        await_ack_promises(puback_promise, pubcomp_promise).await
    }
}
