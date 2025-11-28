use crate::client::{RustMessage, WasmMqttClient};
use mqtt5::broker::router::MessageRouter;
use mqtt5::packet::publish::PublishPacket;
use mqtt5::validation::topic_matches_filter;
use mqtt5_protocol::QoS;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use wasm_bindgen::prelude::*;
use web_sys::MessagePort;

#[wasm_bindgen]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WasmBridgeDirection {
    In,
    Out,
    Both,
}

#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct WasmTopicMapping {
    pattern: String,
    direction: WasmBridgeDirection,
    qos: u8,
    local_prefix: Option<String>,
    remote_prefix: Option<String>,
}

#[wasm_bindgen]
impl WasmTopicMapping {
    #[wasm_bindgen(constructor)]
    pub fn new(pattern: String, direction: WasmBridgeDirection) -> Self {
        Self {
            pattern,
            direction,
            qos: 0,
            local_prefix: None,
            remote_prefix: None,
        }
    }

    #[wasm_bindgen(setter)]
    pub fn set_qos(&mut self, qos: u8) {
        self.qos = qos.min(2);
    }

    #[wasm_bindgen(setter)]
    pub fn set_local_prefix(&mut self, prefix: Option<String>) {
        self.local_prefix = prefix;
    }

    #[wasm_bindgen(setter)]
    pub fn set_remote_prefix(&mut self, prefix: Option<String>) {
        self.remote_prefix = prefix;
    }

    fn qos_level(&self) -> QoS {
        match self.qos {
            0 => QoS::AtMostOnce,
            1 => QoS::AtLeastOnce,
            _ => QoS::ExactlyOnce,
        }
    }
}

#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct WasmBridgeConfig {
    name: String,
    client_id: String,
    clean_start: bool,
    keep_alive_secs: u16,
    username: Option<String>,
    password: Option<String>,
    topics: Vec<WasmTopicMapping>,
}

#[wasm_bindgen]
impl WasmBridgeConfig {
    #[wasm_bindgen(constructor)]
    pub fn new(name: String) -> Self {
        let client_id = format!("bridge-{name}");
        Self {
            name,
            client_id,
            clean_start: true,
            keep_alive_secs: 60,
            username: None,
            password: None,
            topics: Vec::new(),
        }
    }

    #[wasm_bindgen(setter)]
    pub fn set_client_id(&mut self, client_id: String) {
        self.client_id = client_id;
    }

    #[wasm_bindgen(setter)]
    pub fn set_clean_start(&mut self, clean_start: bool) {
        self.clean_start = clean_start;
    }

    #[wasm_bindgen(setter)]
    pub fn set_keep_alive_secs(&mut self, secs: u16) {
        self.keep_alive_secs = secs;
    }

    #[wasm_bindgen(setter)]
    pub fn set_username(&mut self, username: Option<String>) {
        self.username = username;
    }

    #[wasm_bindgen(setter)]
    pub fn set_password(&mut self, password: Option<String>) {
        self.password = password;
    }

    #[wasm_bindgen]
    pub fn add_topic(&mut self, mapping: WasmTopicMapping) {
        self.topics.push(mapping);
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() {
            return Err("Bridge name cannot be empty".into());
        }
        if self.client_id.is_empty() {
            return Err("Client ID cannot be empty".into());
        }
        if self.topics.is_empty() {
            return Err("Bridge must have at least one topic mapping".into());
        }
        for topic in &self.topics {
            if topic.pattern.is_empty() {
                return Err("Topic pattern cannot be empty".into());
            }
        }
        Ok(())
    }
}

pub struct WasmBridgeConnection {
    config: WasmBridgeConfig,
    client: Rc<WasmMqttClient>,
    router: Arc<MessageRouter>,
    running: Rc<RefCell<bool>>,
    messages_sent: Rc<RefCell<u64>>,
    messages_received: Rc<RefCell<u64>>,
}

impl WasmBridgeConnection {
    pub fn new(config: WasmBridgeConfig, router: Arc<MessageRouter>) -> Result<Self, String> {
        config.validate()?;

        let client = Rc::new(WasmMqttClient::new(config.client_id.clone()));

        Ok(Self {
            config,
            client,
            router,
            running: Rc::new(RefCell::new(false)),
            messages_sent: Rc::new(RefCell::new(0)),
            messages_received: Rc::new(RefCell::new(0)),
        })
    }

    pub async fn connect(&self, port: MessagePort) -> Result<(), JsValue> {
        self.client.connect_message_port(port).await?;
        *self.running.borrow_mut() = true;
        self.setup_subscriptions().await?;
        Ok(())
    }

    async fn setup_subscriptions(&self) -> Result<(), JsValue> {
        for mapping in &self.config.topics {
            match mapping.direction {
                WasmBridgeDirection::In | WasmBridgeDirection::Both => {
                    let remote_topic = self.apply_remote_prefix(&mapping.pattern);
                    let router = self.router.clone();
                    let local_prefix = mapping.local_prefix.clone();
                    let messages_received = self.messages_received.clone();
                    let qos = mapping.qos_level();

                    self.client
                        .subscribe_with_callback_internal(
                            &remote_topic,
                            qos,
                            Box::new(move |msg: RustMessage| {
                                let router = router.clone();
                                let local_prefix = local_prefix.clone();

                                *messages_received.borrow_mut() += 1;

                                let local_topic = if let Some(ref prefix) = local_prefix {
                                    format!("{prefix}{}", msg.topic)
                                } else {
                                    msg.topic.clone()
                                };

                                let mut packet =
                                    PublishPacket::new(local_topic, msg.payload.clone(), msg.qos);
                                let pub_props: mqtt5::types::PublishProperties =
                                    msg.properties.clone().into();
                                packet.properties = pub_props.into();
                                packet.retain = msg.retain;

                                wasm_bindgen_futures::spawn_local(async move {
                                    router.route_message(&packet, None).await;
                                });
                            }),
                        )
                        .await?;
                }
                WasmBridgeDirection::Out => {}
            }
        }
        Ok(())
    }

    fn apply_remote_prefix(&self, topic: &str) -> String {
        for mapping in &self.config.topics {
            if mapping.pattern == topic {
                if let Some(ref prefix) = mapping.remote_prefix {
                    return format!("{prefix}{topic}");
                }
            }
        }
        topic.to_string()
    }

    pub async fn forward_message(&self, packet: &PublishPacket) -> Result<(), JsValue> {
        if !*self.running.borrow() {
            return Ok(());
        }

        for mapping in &self.config.topics {
            match mapping.direction {
                WasmBridgeDirection::Out | WasmBridgeDirection::Both => {
                    if topic_matches_filter(&packet.topic_name, &mapping.pattern) {
                        let remote_topic = if let Some(ref prefix) = mapping.remote_prefix {
                            format!("{prefix}{}", packet.topic_name)
                        } else {
                            packet.topic_name.clone()
                        };

                        self.client
                            .publish_internal(&remote_topic, &packet.payload, mapping.qos_level())
                            .await?;

                        *self.messages_sent.borrow_mut() += 1;
                        break;
                    }
                }
                WasmBridgeDirection::In => {}
            }
        }
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), JsValue> {
        *self.running.borrow_mut() = false;
        self.client.disconnect().await
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    pub fn is_connected(&self) -> bool {
        self.client.is_connected()
    }

    pub fn messages_sent(&self) -> u64 {
        *self.messages_sent.borrow()
    }

    pub fn messages_received(&self) -> u64 {
        *self.messages_received.borrow()
    }
}

#[derive(Clone)]
pub struct WasmBridgeManager {
    bridges: Rc<RefCell<HashMap<String, Rc<WasmBridgeConnection>>>>,
    router: Arc<MessageRouter>,
}

impl WasmBridgeManager {
    pub fn new(router: Arc<MessageRouter>) -> Self {
        Self {
            bridges: Rc::new(RefCell::new(HashMap::new())),
            router,
        }
    }

    pub async fn add_bridge(
        &self,
        config: WasmBridgeConfig,
        port: MessagePort,
    ) -> Result<(), JsValue> {
        let name = config.name.clone();

        if self.bridges.borrow().contains_key(&name) {
            return Err(JsValue::from_str(&format!("Bridge '{name}' already exists")));
        }

        let connection =
            WasmBridgeConnection::new(config, self.router.clone()).map_err(|e| JsValue::from_str(&e))?;
        connection.connect(port).await?;

        self.bridges
            .borrow_mut()
            .insert(name, Rc::new(connection));
        Ok(())
    }

    pub async fn remove_bridge(&self, name: &str) -> Result<(), JsValue> {
        let bridge = self.bridges.borrow_mut().remove(name);
        if let Some(bridge) = bridge {
            bridge.stop().await?;
            Ok(())
        } else {
            Err(JsValue::from_str(&format!("Bridge '{name}' not found")))
        }
    }

    pub async fn forward_to_bridges(&self, packet: &PublishPacket) {
        if packet.topic_name.starts_with("$SYS/") {
            return;
        }

        let bridges: Vec<_> = self.bridges.borrow().values().cloned().collect();
        for bridge in bridges {
            let _ = bridge.forward_message(packet).await;
        }
    }

    pub fn list_bridges(&self) -> Vec<String> {
        self.bridges.borrow().keys().cloned().collect()
    }

    pub async fn stop_all(&self) {
        let bridges: Vec<_> = self.bridges.borrow_mut().drain().collect();
        for (_, bridge) in bridges {
            let _ = bridge.stop().await;
        }
    }
}
