mod loop_prevention;

use crate::client::{RustMessage, WasmMqttClient};
use loop_prevention::WasmLoopPrevention;
use mqtt5::broker::router::MessageRouter;
use mqtt5::packet::publish::PublishPacket;
use mqtt5_protocol::{evaluate_forwarding, BridgeDirection, BridgeStats, QoS, TopicMappingCore};
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

impl From<WasmBridgeDirection> for BridgeDirection {
    fn from(dir: WasmBridgeDirection) -> Self {
        match dir {
            WasmBridgeDirection::In => BridgeDirection::In,
            WasmBridgeDirection::Out => BridgeDirection::Out,
            WasmBridgeDirection::Both => BridgeDirection::Both,
        }
    }
}

impl From<BridgeDirection> for WasmBridgeDirection {
    fn from(dir: BridgeDirection) -> Self {
        match dir {
            BridgeDirection::In => WasmBridgeDirection::In,
            BridgeDirection::Out => WasmBridgeDirection::Out,
            BridgeDirection::Both => WasmBridgeDirection::Both,
        }
    }
}

#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct WasmTopicMapping {
    core: TopicMappingCore,
}

#[wasm_bindgen]
impl WasmTopicMapping {
    #[wasm_bindgen(constructor)]
    #[allow(clippy::must_use_candidate)]
    pub fn new(pattern: String, direction: WasmBridgeDirection) -> Self {
        Self {
            core: TopicMappingCore::new(pattern, direction.into()),
        }
    }

    #[wasm_bindgen(setter)]
    pub fn set_qos(&mut self, qos: u8) {
        self.core.qos = QoS::from(qos.min(2));
    }

    #[wasm_bindgen(setter)]
    pub fn set_local_prefix(&mut self, prefix: Option<String>) {
        self.core.local_prefix = prefix;
    }

    #[wasm_bindgen(setter)]
    pub fn set_remote_prefix(&mut self, prefix: Option<String>) {
        self.core.remote_prefix = prefix;
    }
}

impl WasmTopicMapping {
    fn direction(&self) -> WasmBridgeDirection {
        self.core.direction.into()
    }

    fn pattern(&self) -> &str {
        &self.core.pattern
    }

    fn qos_level(&self) -> QoS {
        self.core.qos
    }

    fn to_core(&self) -> TopicMappingCore {
        self.core.clone()
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
    loop_prevention_ttl_secs: u64,
    loop_prevention_cache_size: usize,
}

#[wasm_bindgen]
impl WasmBridgeConfig {
    #[wasm_bindgen(constructor)]
    #[allow(clippy::must_use_candidate)]
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
            loop_prevention_ttl_secs: 0,
            loop_prevention_cache_size: 10000,
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

    /// Sets how long message fingerprints are remembered for loop detection.
    ///
    /// Messages with the same fingerprint seen within this window are blocked.
    /// Set to 0 to disable loop prevention (default).
    #[wasm_bindgen(setter)]
    pub fn set_loop_prevention_ttl_secs(&mut self, secs: u64) {
        self.loop_prevention_ttl_secs = secs;
    }

    /// Sets the maximum number of message fingerprints to cache.
    ///
    /// When exceeded, expired entries are cleaned up. Default: 10000.
    #[wasm_bindgen(setter)]
    pub fn set_loop_prevention_cache_size(&mut self, size: usize) {
        self.loop_prevention_cache_size = size;
    }

    #[wasm_bindgen]
    pub fn add_topic(&mut self, mapping: WasmTopicMapping) {
        self.topics.push(mapping);
    }

    /// # Errors
    /// Returns an error if the bridge configuration is invalid.
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
            if topic.pattern().is_empty() {
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
    stats: Rc<RefCell<BridgeStats>>,
}

impl WasmBridgeConnection {
    /// # Errors
    /// Returns an error if the bridge configuration is invalid.
    pub fn new(config: WasmBridgeConfig, router: Arc<MessageRouter>) -> Result<Self, String> {
        config.validate()?;

        let client = Rc::new(WasmMqttClient::new(config.client_id.clone()));

        Ok(Self {
            config,
            client,
            router,
            running: Rc::new(RefCell::new(false)),
            stats: Rc::new(RefCell::new(BridgeStats::new())),
        })
    }

    /// # Errors
    /// Returns an error if connection or subscription setup fails.
    pub async fn connect(&self, port: MessagePort) -> Result<(), JsValue> {
        self.client.connect_message_port(port).await?;
        *self.running.borrow_mut() = true;
        self.setup_subscriptions().await?;
        Ok(())
    }

    async fn setup_subscriptions(&self) -> Result<(), JsValue> {
        for mapping in &self.config.topics {
            let direction = mapping.direction();
            match direction {
                WasmBridgeDirection::In | WasmBridgeDirection::Both => {
                    let remote_topic = mapping.core.apply_remote_prefix(mapping.pattern());
                    let router = self.router.clone();
                    let local_prefix = mapping.core.local_prefix.clone();
                    let stats = self.stats.clone();
                    let qos = mapping.qos_level();

                    self.client
                        .subscribe_with_callback_internal_opts(
                            &remote_topic,
                            qos,
                            true,
                            Box::new(move |msg: RustMessage| {
                                let router = router.clone();
                                let local_prefix = local_prefix.clone();

                                stats.borrow_mut().record_received();

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
                                packet.properties.inject_sender(None);
                                packet.properties.inject_client_id(None);

                                wasm_bindgen_futures::spawn_local(async move {
                                    router.route_message_local_only(&packet, None).await;
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

    /// # Errors
    /// Returns an error if publishing the message fails.
    pub async fn forward_message(&self, packet: &PublishPacket) -> Result<(), JsValue> {
        if !*self.running.borrow() {
            return Ok(());
        }

        let core_mappings: Vec<TopicMappingCore> = self
            .config
            .topics
            .iter()
            .map(WasmTopicMapping::to_core)
            .collect();

        if let Some(decision) = evaluate_forwarding(&packet.topic_name, &core_mappings, true) {
            self.client
                .publish_internal(&decision.transformed_topic, &packet.payload, decision.qos)
                .await?;

            self.stats.borrow_mut().record_sent();
        }
        Ok(())
    }

    /// # Errors
    /// Returns an error if disconnection fails.
    pub async fn stop(&self) -> Result<(), JsValue> {
        *self.running.borrow_mut() = false;
        self.client.disconnect().await
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.config.name
    }

    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.client.is_connected()
    }

    #[must_use]
    pub fn messages_sent(&self) -> u64 {
        self.stats.borrow().messages_sent
    }

    #[must_use]
    pub fn messages_received(&self) -> u64 {
        self.stats.borrow().messages_received
    }
}

#[derive(Clone)]
pub struct WasmBridgeManager {
    bridges: Rc<RefCell<HashMap<String, Rc<WasmBridgeConnection>>>>,
    router: Arc<MessageRouter>,
    loop_prevention: Rc<RefCell<Option<Rc<WasmLoopPrevention>>>>,
}

impl WasmBridgeManager {
    #[allow(clippy::must_use_candidate)]
    pub fn new(router: Arc<MessageRouter>) -> Self {
        Self {
            bridges: Rc::new(RefCell::new(HashMap::new())),
            router,
            loop_prevention: Rc::new(RefCell::new(None)),
        }
    }

    pub fn set_loop_prevention(&self, ttl_secs: u64, cache_size: usize) {
        let lp = Rc::new(WasmLoopPrevention::new(ttl_secs, cache_size));
        *self.loop_prevention.borrow_mut() = Some(lp);
        tracing::info!(
            ttl_secs = ttl_secs,
            cache_size = cache_size,
            "Loop prevention configured at manager level"
        );
    }

    fn get_or_init_loop_prevention(&self, config: &WasmBridgeConfig) {
        if config.loop_prevention_ttl_secs == 0 {
            return;
        }

        let mut guard = self.loop_prevention.borrow_mut();
        if let Some(ref lp) = *guard {
            let existing_ttl = lp.ttl_secs();
            let existing_cache_size = lp.max_cache_size();
            if config.loop_prevention_ttl_secs != existing_ttl
                || config.loop_prevention_cache_size != existing_cache_size
            {
                tracing::warn!(
                    bridge = %config.name,
                    bridge_ttl_secs = config.loop_prevention_ttl_secs,
                    bridge_cache_size = config.loop_prevention_cache_size,
                    active_ttl_secs = existing_ttl,
                    active_cache_size = existing_cache_size,
                    "Bridge has different loop prevention settings than active config; using active config"
                );
            }
            return;
        }
        let lp = Rc::new(WasmLoopPrevention::new(
            config.loop_prevention_ttl_secs,
            config.loop_prevention_cache_size,
        ));
        *guard = Some(lp.clone());
    }

    /// # Errors
    /// Returns an error if the bridge already exists or connection fails.
    pub async fn add_bridge(
        &self,
        config: WasmBridgeConfig,
        port: MessagePort,
    ) -> Result<(), JsValue> {
        let name = config.name.clone();

        if self.bridges.borrow().contains_key(&name) {
            return Err(JsValue::from_str(&format!(
                "Bridge '{name}' already exists"
            )));
        }

        self.get_or_init_loop_prevention(&config);

        let connection = WasmBridgeConnection::new(config, self.router.clone())
            .map_err(|e| JsValue::from_str(&e))?;
        connection.connect(port).await?;

        self.bridges.borrow_mut().insert(name, Rc::new(connection));
        Ok(())
    }

    /// # Errors
    /// Returns an error if the bridge is not found or disconnection fails.
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

        if let Some(lp) = self.loop_prevention.borrow().as_ref() {
            if !lp.check_message(
                &packet.topic_name,
                &packet.payload,
                packet.qos,
                packet.retain,
            ) {
                return;
            }
        }

        let bridges: Vec<_> = self.bridges.borrow().values().cloned().collect();
        for bridge in bridges {
            let _ = bridge.forward_message(packet).await;
        }
    }

    #[must_use]
    pub fn list_bridges(&self) -> Vec<String> {
        self.bridges.borrow().keys().cloned().collect()
    }

    pub async fn stop_all(&self) {
        let bridges: Vec<_> = self.bridges.borrow_mut().drain().collect();
        for (_, bridge) in bridges {
            let _ = bridge.stop().await;
        }
    }

    pub fn clear_loop_prevention_cache(&self) {
        if let Some(lp) = self.loop_prevention.borrow().as_ref() {
            lp.clear_cache();
        }
    }

    #[must_use]
    pub fn loop_prevention_cache_size(&self) -> usize {
        self.loop_prevention
            .borrow()
            .as_ref()
            .map_or(0, |lp| lp.cache_size())
    }
}
