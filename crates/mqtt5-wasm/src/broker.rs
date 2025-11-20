use crate::client_handler::WasmClientHandler;
use mqtt5::broker::auth::PasswordAuthProvider;
use mqtt5::broker::config::BrokerConfig;
use mqtt5::broker::resource_monitor::{ResourceLimits, ResourceMonitor};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::storage::{DynamicStorage, MemoryBackend};
use mqtt5::broker::sys_topics::BrokerStats;
use mqtt5::time::Duration;
use std::sync::Arc;
use wasm_bindgen::prelude::*;
use web_sys::MessagePort;

#[wasm_bindgen]
pub struct WasmBrokerConfig {
    max_clients: u32,
    session_expiry_interval_secs: u32,
    max_packet_size: u32,
    topic_alias_maximum: u16,
    retain_available: bool,
    maximum_qos: u8,
    wildcard_subscription_available: bool,
    subscription_identifier_available: bool,
    shared_subscription_available: bool,
    server_keep_alive_secs: Option<u32>,
}

#[wasm_bindgen]
impl WasmBrokerConfig {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            max_clients: 1000,
            session_expiry_interval_secs: 3600,
            max_packet_size: 268_435_456,
            topic_alias_maximum: 65535,
            retain_available: true,
            maximum_qos: 2,
            wildcard_subscription_available: true,
            subscription_identifier_available: true,
            shared_subscription_available: true,
            server_keep_alive_secs: None,
        }
    }

    #[wasm_bindgen(setter)]
    pub fn set_max_clients(&mut self, value: u32) {
        self.max_clients = value;
    }

    #[wasm_bindgen(setter)]
    pub fn set_session_expiry_interval_secs(&mut self, value: u32) {
        self.session_expiry_interval_secs = value;
    }

    #[wasm_bindgen(setter)]
    pub fn set_max_packet_size(&mut self, value: u32) {
        self.max_packet_size = value;
    }

    #[wasm_bindgen(setter)]
    pub fn set_topic_alias_maximum(&mut self, value: u16) {
        self.topic_alias_maximum = value;
    }

    #[wasm_bindgen(setter)]
    pub fn set_retain_available(&mut self, value: bool) {
        self.retain_available = value;
    }

    #[wasm_bindgen(setter)]
    pub fn set_maximum_qos(&mut self, value: u8) {
        self.maximum_qos = value.min(2);
    }

    #[wasm_bindgen(setter)]
    pub fn set_wildcard_subscription_available(&mut self, value: bool) {
        self.wildcard_subscription_available = value;
    }

    #[wasm_bindgen(setter)]
    pub fn set_subscription_identifier_available(&mut self, value: bool) {
        self.subscription_identifier_available = value;
    }

    #[wasm_bindgen(setter)]
    pub fn set_shared_subscription_available(&mut self, value: bool) {
        self.shared_subscription_available = value;
    }

    #[wasm_bindgen(setter)]
    pub fn set_server_keep_alive_secs(&mut self, value: Option<u32>) {
        self.server_keep_alive_secs = value;
    }

    fn to_broker_config(&self) -> BrokerConfig {
        let mut config = BrokerConfig::default();
        config.max_clients = self.max_clients as usize;
        config.session_expiry_interval =
            Duration::from_secs(u64::from(self.session_expiry_interval_secs));
        config.max_packet_size = self.max_packet_size as usize;
        config.topic_alias_maximum = self.topic_alias_maximum;
        config.retain_available = self.retain_available;
        config.maximum_qos = self.maximum_qos;
        config.wildcard_subscription_available = self.wildcard_subscription_available;
        config.subscription_identifier_available = self.subscription_identifier_available;
        config.shared_subscription_available = self.shared_subscription_available;
        config.server_keep_alive = self
            .server_keep_alive_secs
            .map(|s| Duration::from_secs(u64::from(s)));
        config
    }
}

impl Default for WasmBrokerConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
pub struct WasmBroker {
    config: Arc<BrokerConfig>,
    router: Arc<MessageRouter>,
    auth_provider: Arc<PasswordAuthProvider>,
    storage: Arc<DynamicStorage>,
    stats: Arc<BrokerStats>,
    resource_monitor: Arc<ResourceMonitor>,
}

#[wasm_bindgen]
impl WasmBroker {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<WasmBroker, JsValue> {
        Self::with_config(WasmBrokerConfig::new())
    }

    #[wasm_bindgen]
    pub fn with_config(wasm_config: WasmBrokerConfig) -> Result<WasmBroker, JsValue> {
        let config = Arc::new(wasm_config.to_broker_config());

        let storage = Arc::new(DynamicStorage::Memory(MemoryBackend::new()));
        let router = Arc::new(MessageRouter::with_storage(Arc::clone(&storage)));

        let auth_provider = Arc::new(PasswordAuthProvider::new().with_anonymous(true));

        let stats = Arc::new(BrokerStats::new());

        let limits = ResourceLimits {
            max_connections: config.max_clients,
            ..Default::default()
        };
        let resource_monitor = Arc::new(ResourceMonitor::new(limits));

        Ok(WasmBroker {
            config,
            router,
            auth_provider,
            storage,
            stats,
            resource_monitor,
        })
    }

    #[wasm_bindgen]
    pub async fn add_user(&self, username: String, password: String) -> Result<(), JsValue> {
        self.auth_provider
            .add_user(username, &password)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen]
    pub async fn add_user_with_hash(&self, username: String, password_hash: String) {
        self.auth_provider
            .add_user_with_hash(username, password_hash)
            .await;
    }

    #[wasm_bindgen]
    pub async fn remove_user(&self, username: &str) -> bool {
        self.auth_provider.remove_user(username).await
    }

    #[wasm_bindgen]
    pub async fn has_user(&self, username: &str) -> bool {
        self.auth_provider.has_user(username).await
    }

    #[wasm_bindgen]
    pub async fn user_count(&self) -> usize {
        self.auth_provider.user_count().await
    }

    #[wasm_bindgen]
    pub fn set_allow_anonymous(&mut self, allow: bool) {
        if let Some(provider) = Arc::get_mut(&mut self.auth_provider) {
            provider.set_allow_anonymous(allow);
        }
    }

    #[wasm_bindgen]
    pub fn hash_password(password: &str) -> Result<String, JsValue> {
        PasswordAuthProvider::hash_password(password).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    pub fn create_client_port(&self) -> Result<MessagePort, JsValue> {
        let channel = web_sys::MessageChannel::new()?;

        let client_port = channel.port1();
        let broker_port = channel.port2();

        WasmClientHandler::new(
            broker_port,
            Arc::clone(&self.config),
            Arc::clone(&self.router),
            Arc::clone(&self.auth_provider) as _,
            Arc::clone(&self.storage),
            Arc::clone(&self.stats),
            Arc::clone(&self.resource_monitor),
        );

        Ok(client_port)
    }
}
