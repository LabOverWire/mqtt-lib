use crate::bridge::{WasmBridgeConfig, WasmBridgeManager};
use crate::client_handler::WasmClientHandler;
use mqtt5::broker::acl::{AclRule, Permission};
use mqtt5::broker::auth::{ComprehensiveAuthProvider, PasswordAuthProvider};
use mqtt5::broker::config::BrokerConfig;
use mqtt5::broker::resource_monitor::{ResourceLimits, ResourceMonitor};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::storage::{DynamicStorage, MemoryBackend};
use mqtt5::broker::sys_topics::{BrokerStats, SysTopicsProvider};
use mqtt5::time::Duration;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use wasm_bindgen::prelude::*;
use web_sys::MessagePort;

#[wasm_bindgen]
#[allow(clippy::struct_excessive_bools)]
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
    allow_anonymous: bool,
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
            allow_anonymous: false,
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

    #[wasm_bindgen(setter)]
    pub fn set_allow_anonymous(&mut self, value: bool) {
        self.allow_anonymous = value;
    }

    fn to_broker_config(&self) -> BrokerConfig {
        BrokerConfig {
            max_clients: self.max_clients as usize,
            session_expiry_interval: Duration::from_secs(u64::from(
                self.session_expiry_interval_secs,
            )),
            max_packet_size: self.max_packet_size as usize,
            topic_alias_maximum: self.topic_alias_maximum,
            retain_available: self.retain_available,
            maximum_qos: self.maximum_qos,
            wildcard_subscription_available: self.wildcard_subscription_available,
            subscription_identifier_available: self.subscription_identifier_available,
            shared_subscription_available: self.shared_subscription_available,
            server_keep_alive: self
                .server_keep_alive_secs
                .map(|s| Duration::from_secs(u64::from(s))),
            ..Default::default()
        }
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
    auth_provider: Arc<ComprehensiveAuthProvider>,
    storage: Arc<DynamicStorage>,
    stats: Arc<BrokerStats>,
    resource_monitor: Arc<ResourceMonitor>,
    bridge_manager: Rc<RefCell<WasmBridgeManager>>,
}

#[wasm_bindgen]
impl WasmBroker {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<WasmBroker, JsValue> {
        Self::with_config(WasmBrokerConfig::new())
    }

    #[wasm_bindgen]
    #[allow(clippy::needless_pass_by_value)]
    pub fn with_config(wasm_config: WasmBrokerConfig) -> Result<WasmBroker, JsValue> {
        let allow_anonymous = wasm_config.allow_anonymous;
        let config = Arc::new(wasm_config.to_broker_config());

        let storage = Arc::new(DynamicStorage::Memory(MemoryBackend::new()));
        let router = Arc::new(MessageRouter::with_storage(Arc::clone(&storage)));

        let password_provider = PasswordAuthProvider::new().with_anonymous(allow_anonymous);
        let acl_manager = mqtt5::broker::acl::AclManager::allow_all();
        let auth_provider = Arc::new(ComprehensiveAuthProvider::with_providers(
            password_provider,
            acl_manager,
        ));

        let stats = Arc::new(BrokerStats::new());

        let limits = ResourceLimits {
            max_connections: config.max_clients,
            ..Default::default()
        };
        let resource_monitor = Arc::new(ResourceMonitor::new(limits));

        let bridge_manager = Rc::new(RefCell::new(WasmBridgeManager::new(Arc::clone(&router))));

        let broker = WasmBroker {
            config,
            router,
            auth_provider,
            storage,
            stats,
            resource_monitor,
            bridge_manager,
        };

        broker.setup_bridge_callback();

        Ok(broker)
    }

    #[wasm_bindgen]
    pub async fn add_user(&self, username: String, password: String) -> Result<(), JsValue> {
        self.auth_provider
            .password_provider()
            .add_user(username, &password)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen]
    pub async fn add_user_with_hash(&self, username: String, password_hash: String) {
        self.auth_provider
            .password_provider()
            .add_user_with_hash(username, password_hash)
            .await;
    }

    #[wasm_bindgen]
    pub async fn remove_user(&self, username: &str) -> bool {
        self.auth_provider
            .password_provider()
            .remove_user(username)
            .await
    }

    #[wasm_bindgen]
    pub async fn has_user(&self, username: &str) -> bool {
        self.auth_provider
            .password_provider()
            .has_user(username)
            .await
    }

    #[wasm_bindgen]
    pub async fn user_count(&self) -> usize {
        self.auth_provider.password_provider().user_count().await
    }

    #[wasm_bindgen]
    pub fn hash_password(password: &str) -> Result<String, JsValue> {
        PasswordAuthProvider::hash_password(password).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen]
    pub async fn add_acl_rule(
        &self,
        username: String,
        topic_pattern: String,
        permission: String,
    ) -> Result<(), JsValue> {
        let perm: Permission = permission
            .parse()
            .map_err(|e: mqtt5::error::MqttError| JsValue::from_str(&e.to_string()))?;
        self.auth_provider
            .acl_manager()
            .add_rule(AclRule::new(username, topic_pattern, perm))
            .await;
        Ok(())
    }

    #[wasm_bindgen]
    pub async fn clear_acl_rules(&self) {
        self.auth_provider.acl_manager().clear_rules().await;
    }

    #[wasm_bindgen]
    pub async fn acl_rule_count(&self) -> usize {
        self.auth_provider.acl_manager().rule_count().await
    }

    #[wasm_bindgen]
    pub async fn add_role(&self, name: String) {
        self.auth_provider.acl_manager().add_role(name).await;
    }

    #[wasm_bindgen]
    pub async fn remove_role(&self, name: &str) -> bool {
        self.auth_provider.acl_manager().remove_role(name).await
    }

    #[wasm_bindgen]
    pub async fn list_roles(&self) -> Vec<String> {
        self.auth_provider.acl_manager().list_roles().await
    }

    #[wasm_bindgen]
    pub async fn role_count(&self) -> usize {
        self.auth_provider.acl_manager().role_count().await
    }

    #[wasm_bindgen]
    pub async fn add_role_rule(
        &self,
        role_name: String,
        topic_pattern: String,
        permission: String,
    ) -> Result<(), JsValue> {
        let perm: Permission = permission
            .parse()
            .map_err(|e: mqtt5::error::MqttError| JsValue::from_str(&e.to_string()))?;
        self.auth_provider
            .acl_manager()
            .add_role_rule(&role_name, topic_pattern, perm)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen]
    pub async fn assign_role(&self, username: String, role_name: String) -> Result<(), JsValue> {
        self.auth_provider
            .acl_manager()
            .assign_role(&username, &role_name)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen]
    pub async fn unassign_role(&self, username: &str, role_name: &str) -> bool {
        self.auth_provider
            .acl_manager()
            .unassign_role(username, role_name)
            .await
    }

    #[wasm_bindgen]
    pub async fn get_user_roles(&self, username: &str) -> Vec<String> {
        self.auth_provider
            .acl_manager()
            .get_user_roles(username)
            .await
    }

    #[wasm_bindgen]
    pub async fn clear_roles(&self) {
        self.auth_provider.acl_manager().clear_roles().await;
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

    #[wasm_bindgen]
    pub async fn add_bridge(
        &self,
        config: WasmBridgeConfig,
        remote_port: MessagePort,
    ) -> Result<(), JsValue> {
        let manager = self.bridge_manager.borrow().clone();
        manager.add_bridge(config, remote_port).await
    }

    #[wasm_bindgen]
    pub async fn remove_bridge(&self, name: &str) -> Result<(), JsValue> {
        let manager = self.bridge_manager.borrow().clone();
        manager.remove_bridge(name).await
    }

    #[wasm_bindgen]
    pub fn list_bridges(&self) -> Vec<String> {
        self.bridge_manager.borrow().list_bridges()
    }

    #[wasm_bindgen]
    pub async fn stop_all_bridges(&self) {
        let manager = self.bridge_manager.borrow().clone();
        manager.stop_all().await;
    }

    #[wasm_bindgen]
    pub fn start_sys_topics(&self) {
        self.start_sys_topics_with_interval_secs(10);
    }

    #[wasm_bindgen]
    pub fn start_sys_topics_with_interval_secs(&self, interval_secs: u32) {
        let provider = SysTopicsProvider::new(Arc::clone(&self.router), Arc::clone(&self.stats));
        let interval_ms = u64::from(interval_secs) * 1000;

        wasm_bindgen_futures::spawn_local(async move {
            gloo_timers::future::sleep(std::time::Duration::from_millis(interval_ms)).await;
            provider.publish_static_topics().await;

            loop {
                gloo_timers::future::sleep(std::time::Duration::from_millis(interval_ms)).await;
                provider.publish_dynamic_topics().await;
            }
        });
    }

    fn setup_bridge_callback(&self) {
        let bridge_manager = self.bridge_manager.clone();
        let router = Arc::clone(&self.router);

        wasm_bindgen_futures::spawn_local(async move {
            router
                .set_wasm_bridge_callback(move |packet| {
                    let manager = bridge_manager.borrow().clone();
                    let packet = packet.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        manager.forward_to_bridges(&packet).await;
                    });
                })
                .await;
        });
    }
}
