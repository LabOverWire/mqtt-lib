use crate::bridge::{WasmBridgeConfig, WasmBridgeManager};
use crate::client_handler::{BrokerContext, WasmClientHandler};
use crate::config::WasmConnectOptions;
use mqtt5::broker::acl::{AclRule, Permission};
use mqtt5::broker::auth::{AuthProvider, ComprehensiveAuthProvider, PasswordAuthProvider};
use mqtt5::broker::config::{BrokerConfig, ChangeOnlyDeliveryConfig, LoadBalancerConfig};
use mqtt5::broker::resource_monitor::{ResourceLimits, ResourceMonitor};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::storage::{DynamicStorage, MemoryBackend};
use mqtt5::broker::sys_topics::{BrokerStats, SysTopicsProvider};
use mqtt5::time::Duration;
use mqtt5_protocol::{u64_to_f64_saturating, u64_to_u32_saturating};
use std::cell::{Cell, RefCell};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::{Arc, RwLock};
use wasm_bindgen::prelude::*;
use web_sys::MessagePort;

#[derive(Clone, Default)]
pub struct WasmEventCallbacks {
    pub on_client_connect: Rc<RefCell<Option<js_sys::Function>>>,
    pub on_client_disconnect: Rc<RefCell<Option<js_sys::Function>>>,
    pub on_client_publish: Rc<RefCell<Option<js_sys::Function>>>,
    pub on_client_subscribe: Rc<RefCell<Option<js_sys::Function>>>,
    pub on_client_unsubscribe: Rc<RefCell<Option<js_sys::Function>>>,
    pub on_message_delivered: Rc<RefCell<Option<js_sys::Function>>>,
}

#[derive(Hash)]
#[allow(clippy::struct_excessive_bools)]
struct ConfigHashFields {
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
    change_only_delivery_enabled: bool,
    change_only_delivery_patterns: Vec<String>,
    echo_suppression_enabled: bool,
    echo_suppression_property_key: Option<String>,
    max_outbound_rate_per_client: u32,
    max_message_rate_per_client: u32,
    max_bandwidth_per_client: u64,
    load_balancer_backends: Vec<String>,
    session_sweep_interval_secs: u32,
}

#[wasm_bindgen(js_name = "BrokerConfig")]
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
    change_only_delivery_enabled: bool,
    change_only_delivery_patterns: Vec<String>,
    echo_suppression_enabled: bool,
    echo_suppression_property_key: Option<String>,
    max_outbound_rate_per_client: u32,
    max_message_rate_per_client: u32,
    max_bandwidth_per_client: u64,
    load_balancer_backends: Vec<String>,
    session_sweep_interval_secs: u32,
}

#[wasm_bindgen(js_class = "BrokerConfig")]
impl WasmBrokerConfig {
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_clients: 1000,
            session_expiry_interval_secs: u32::MAX,
            max_packet_size: 268_435_456,
            topic_alias_maximum: 65535,
            retain_available: true,
            maximum_qos: 2,
            wildcard_subscription_available: true,
            subscription_identifier_available: true,
            shared_subscription_available: true,
            server_keep_alive_secs: None,
            allow_anonymous: false,
            change_only_delivery_enabled: false,
            change_only_delivery_patterns: Vec::new(),
            echo_suppression_enabled: false,
            echo_suppression_property_key: None,
            max_outbound_rate_per_client: 0,
            max_message_rate_per_client: 0,
            max_bandwidth_per_client: 0,
            load_balancer_backends: Vec::new(),
            session_sweep_interval_secs: 3600,
        }
    }

    #[wasm_bindgen(setter, js_name = "maxClients")]
    pub fn set_max_clients(&mut self, value: u32) {
        self.max_clients = value;
    }

    #[wasm_bindgen(setter, js_name = "sessionExpiryIntervalSecs")]
    pub fn set_session_expiry_interval_secs(&mut self, value: u32) {
        self.session_expiry_interval_secs = value;
    }

    #[wasm_bindgen(setter, js_name = "maxPacketSize")]
    pub fn set_max_packet_size(&mut self, value: u32) {
        self.max_packet_size = value;
    }

    #[wasm_bindgen(setter, js_name = "topicAliasMaximum")]
    pub fn set_topic_alias_maximum(&mut self, value: u16) {
        self.topic_alias_maximum = value;
    }

    #[wasm_bindgen(setter, js_name = "retainAvailable")]
    pub fn set_retain_available(&mut self, value: bool) {
        self.retain_available = value;
    }

    #[wasm_bindgen(setter, js_name = "maximumQos")]
    pub fn set_maximum_qos(&mut self, value: u8) {
        self.maximum_qos = value.min(2);
    }

    #[wasm_bindgen(setter, js_name = "wildcardSubscriptionAvailable")]
    pub fn set_wildcard_subscription_available(&mut self, value: bool) {
        self.wildcard_subscription_available = value;
    }

    #[wasm_bindgen(setter, js_name = "subscriptionIdentifierAvailable")]
    pub fn set_subscription_identifier_available(&mut self, value: bool) {
        self.subscription_identifier_available = value;
    }

    #[wasm_bindgen(setter, js_name = "sharedSubscriptionAvailable")]
    pub fn set_shared_subscription_available(&mut self, value: bool) {
        self.shared_subscription_available = value;
    }

    #[wasm_bindgen(setter, js_name = "serverKeepAliveSecs")]
    pub fn set_server_keep_alive_secs(&mut self, value: Option<u32>) {
        self.server_keep_alive_secs = value;
    }

    #[wasm_bindgen(setter, js_name = "allowAnonymous")]
    pub fn set_allow_anonymous(&mut self, value: bool) {
        self.allow_anonymous = value;
    }

    #[wasm_bindgen(setter, js_name = "changeOnlyDeliveryEnabled")]
    pub fn set_change_only_delivery_enabled(&mut self, value: bool) {
        self.change_only_delivery_enabled = value;
    }

    #[wasm_bindgen(js_name = "addChangeOnlyDeliveryPattern")]
    pub fn add_change_only_delivery_pattern(&mut self, pattern: String) {
        self.change_only_delivery_patterns.push(pattern);
    }

    #[wasm_bindgen(js_name = "clearChangeOnlyDeliveryPatterns")]
    pub fn clear_change_only_delivery_patterns(&mut self) {
        self.change_only_delivery_patterns.clear();
    }

    #[wasm_bindgen(setter, js_name = "echoSuppressionEnabled")]
    pub fn set_echo_suppression_enabled(&mut self, value: bool) {
        self.echo_suppression_enabled = value;
    }

    #[wasm_bindgen(setter, js_name = "echoSuppressionPropertyKey")]
    pub fn set_echo_suppression_property_key(&mut self, value: Option<String>) {
        self.echo_suppression_property_key = value;
    }

    #[wasm_bindgen(setter, js_name = "maxOutboundRatePerClient")]
    pub fn set_max_outbound_rate_per_client(&mut self, value: u32) {
        self.max_outbound_rate_per_client = value;
    }

    #[wasm_bindgen(setter, js_name = "maxMessageRatePerClient")]
    pub fn set_max_message_rate_per_client(&mut self, value: u32) {
        self.max_message_rate_per_client = value;
    }

    #[wasm_bindgen(setter, js_name = "maxBandwidthPerClient")]
    pub fn set_max_bandwidth_per_client(&mut self, value: u64) {
        self.max_bandwidth_per_client = value;
    }

    #[wasm_bindgen(js_name = "addLoadBalancerBackend")]
    pub fn add_load_balancer_backend(&mut self, backend: String) {
        self.load_balancer_backends.push(backend);
    }

    #[wasm_bindgen(js_name = "clearLoadBalancerBackends")]
    pub fn clear_load_balancer_backends(&mut self) {
        self.load_balancer_backends.clear();
    }

    #[wasm_bindgen(setter, js_name = "sessionSweepIntervalSecs")]
    pub fn set_session_sweep_interval_secs(&mut self, value: u32) {
        self.session_sweep_interval_secs = value;
    }

    fn into_broker_config(self) -> BrokerConfig {
        let mut config = BrokerConfig {
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
            change_only_delivery_config: ChangeOnlyDeliveryConfig {
                enabled: self.change_only_delivery_enabled,
                topic_patterns: self.change_only_delivery_patterns,
            },
            echo_suppression_config: mqtt5::broker::config::EchoSuppressionConfig {
                enabled: self.echo_suppression_enabled,
                property_key: self
                    .echo_suppression_property_key
                    .unwrap_or_else(|| "x-origin-client-id".to_string()),
            },
            max_outbound_rate_per_client: self.max_outbound_rate_per_client,
            max_message_rate_per_client: self.max_message_rate_per_client,
            max_bandwidth_per_client: self.max_bandwidth_per_client,
            load_balancer: if self.load_balancer_backends.is_empty() {
                None
            } else {
                Some(LoadBalancerConfig::new(self.load_balancer_backends))
            },
            ..Default::default()
        };
        config.storage_config.cleanup_interval =
            Duration::from_secs(u64::from(self.session_sweep_interval_secs.max(1)));
        config
    }

    fn calculate_hash(&self) -> u64 {
        let fields = ConfigHashFields {
            max_clients: self.max_clients,
            session_expiry_interval_secs: self.session_expiry_interval_secs,
            max_packet_size: self.max_packet_size,
            topic_alias_maximum: self.topic_alias_maximum,
            retain_available: self.retain_available,
            maximum_qos: self.maximum_qos,
            wildcard_subscription_available: self.wildcard_subscription_available,
            subscription_identifier_available: self.subscription_identifier_available,
            shared_subscription_available: self.shared_subscription_available,
            server_keep_alive_secs: self.server_keep_alive_secs,
            allow_anonymous: self.allow_anonymous,
            change_only_delivery_enabled: self.change_only_delivery_enabled,
            change_only_delivery_patterns: self.change_only_delivery_patterns.clone(),
            echo_suppression_enabled: self.echo_suppression_enabled,
            echo_suppression_property_key: self.echo_suppression_property_key.clone(),
            max_outbound_rate_per_client: self.max_outbound_rate_per_client,
            max_message_rate_per_client: self.max_message_rate_per_client,
            max_bandwidth_per_client: self.max_bandwidth_per_client,
            load_balancer_backends: self.load_balancer_backends.clone(),
            session_sweep_interval_secs: self.session_sweep_interval_secs,
        };
        let mut hasher = DefaultHasher::new();
        fields.hash(&mut hasher);
        hasher.finish()
    }
}

impl Default for WasmBrokerConfig {
    fn default() -> Self {
        Self::new()
    }
}

fn resource_limits_from(config: &BrokerConfig) -> ResourceLimits {
    ResourceLimits {
        max_connections: config.max_clients,
        max_message_rate_per_client: config.max_message_rate_per_client,
        max_bandwidth_per_client: config.max_bandwidth_per_client,
        rate_limit_window: Duration::from_secs(1),
        ..Default::default()
    }
}

const SESSION_SWEEP_INTERVAL_MS: u32 = 60_000;

#[wasm_bindgen(js_name = "Broker")]
pub struct WasmBroker {
    config: Arc<RwLock<BrokerConfig>>,
    router: Arc<MessageRouter>,
    auth_provider: Arc<ComprehensiveAuthProvider>,
    storage: Arc<DynamicStorage>,
    stats: Arc<BrokerStats>,
    resource_monitor: Arc<ResourceMonitor>,
    bridge_manager: Rc<RefCell<WasmBridgeManager>>,
    sys_topics_running: Rc<Cell<bool>>,
    config_hash: Rc<Cell<u64>>,
    on_config_change: Rc<RefCell<Option<js_sys::Function>>>,
    event_callbacks: WasmEventCallbacks,
}

impl WasmBroker {
    fn handler_context(&self, auth_provider: Arc<dyn AuthProvider>) -> BrokerContext {
        BrokerContext {
            config: Arc::clone(&self.config),
            router: Arc::clone(&self.router),
            auth_provider,
            storage: Arc::clone(&self.storage),
            stats: Arc::clone(&self.stats),
            resource_monitor: Arc::clone(&self.resource_monitor),
            event_callbacks: self.event_callbacks.clone(),
        }
    }
}

#[wasm_bindgen(js_class = "Broker")]
impl WasmBroker {
    /// # Errors
    /// Returns an error if broker initialization fails.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<WasmBroker, JsValue> {
        Self::with_config(WasmBrokerConfig::new())
    }

    /// # Errors
    /// Returns an error if broker initialization fails.
    #[wasm_bindgen(js_name = "withConfig")]
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn with_config(options: WasmBrokerConfig) -> Result<WasmBroker, JsValue> {
        let allow_anonymous = options.allow_anonymous;
        let config_hash = options.calculate_hash();
        let config = Arc::new(RwLock::new(options.into_broker_config()));

        let storage = Arc::new(DynamicStorage::Memory(MemoryBackend::new()));
        let broker_config = config
            .read()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let base_router = MessageRouter::with_storage(Arc::clone(&storage));
        let with_echo = if broker_config.echo_suppression_config.enabled {
            base_router.with_echo_suppression_key(
                broker_config.echo_suppression_config.property_key.clone(),
            )
        } else {
            base_router
        };
        let router = if broker_config.max_outbound_rate_per_client > 0 {
            Arc::new(with_echo.with_max_outbound_rate(broker_config.max_outbound_rate_per_client))
        } else {
            Arc::new(with_echo)
        };
        drop(broker_config);

        let password_provider = PasswordAuthProvider::new().with_anonymous(allow_anonymous);
        let acl_manager = mqtt5::broker::acl::AclManager::allow_all();
        let auth_provider = Arc::new(ComprehensiveAuthProvider::with_providers(
            password_provider,
            acl_manager,
        ));

        let stats = Arc::new(BrokerStats::new());

        let limits = config
            .read()
            .map_or_else(|_| ResourceLimits::default(), |c| resource_limits_from(&c));
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
            sys_topics_running: Rc::new(Cell::new(false)),
            config_hash: Rc::new(Cell::new(config_hash)),
            on_config_change: Rc::new(RefCell::new(None)),
            event_callbacks: WasmEventCallbacks::default(),
        };

        broker.setup_bridge_callback();
        broker.spawn_session_sweep();

        Ok(broker)
    }

    fn spawn_session_sweep(&self) {
        let router = Arc::downgrade(&self.router);
        let storage = Arc::downgrade(&self.storage);
        let interval = self
            .config
            .read()
            .map_or(SESSION_SWEEP_INTERVAL_MS, |config| {
                u32::try_from(config.storage_config.cleanup_interval.as_millis())
                    .unwrap_or(SESSION_SWEEP_INTERVAL_MS)
                    .max(1)
            });
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                gloo_timers::future::TimeoutFuture::new(interval).await;
                let (Some(router), Some(storage)) = (router.upgrade(), storage.upgrade()) else {
                    break;
                };
                router.sweep_sessions().await;
                if let Err(e) =
                    mqtt5::broker::storage::StorageBackend::cleanup_expired(&*storage).await
                {
                    tracing::warn!("Storage cleanup error: {e}");
                }
            }
        });
    }

    /// # Errors
    /// Returns an error if adding the user fails.
    #[wasm_bindgen(js_name = "addUser")]
    pub fn add_user(&self, username: String, password: &str) -> Result<(), JsValue> {
        self.auth_provider
            .password_provider()
            .add_user(username, password)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = "addUserWithHash")]
    pub fn add_user_with_hash(
        &self,
        username: String,
        #[wasm_bindgen(js_name = "passwordHash")] password_hash: String,
    ) {
        self.auth_provider
            .password_provider()
            .add_user_with_hash(username, password_hash);
    }

    #[wasm_bindgen(js_name = "removeUser")]
    #[must_use]
    pub fn remove_user(&self, username: &str) -> bool {
        self.auth_provider.password_provider().remove_user(username)
    }

    #[wasm_bindgen(js_name = "hasUser")]
    #[must_use]
    pub fn has_user(&self, username: &str) -> bool {
        self.auth_provider.password_provider().has_user(username)
    }

    #[wasm_bindgen(js_name = "userCount")]
    #[must_use]
    pub fn user_count(&self) -> usize {
        self.auth_provider.password_provider().user_count()
    }

    /// # Errors
    /// Returns an error if password hashing fails.
    #[wasm_bindgen(js_name = "hashPassword")]
    pub fn hash_password(password: &str) -> Result<String, JsValue> {
        PasswordAuthProvider::hash_password(password).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// # Errors
    /// Returns an error if the permission string is invalid.
    #[wasm_bindgen(js_name = "addAclRule")]
    pub async fn add_acl_rule(
        &self,
        username: String,
        #[wasm_bindgen(js_name = "topicPattern")] topic_pattern: String,
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

    #[wasm_bindgen(js_name = "clearAclRules")]
    pub async fn clear_acl_rules(&self) {
        self.auth_provider.acl_manager().clear_rules().await;
    }

    #[wasm_bindgen(js_name = "aclRuleCount")]
    pub async fn acl_rule_count(&self) -> usize {
        self.auth_provider.acl_manager().rule_count().await
    }

    #[wasm_bindgen(js_name = "addRole")]
    pub async fn add_role(&self, name: String) {
        self.auth_provider.acl_manager().add_role(name).await;
    }

    #[wasm_bindgen(js_name = "removeRole")]
    pub async fn remove_role(&self, name: &str) -> bool {
        self.auth_provider.acl_manager().remove_role(name).await
    }

    #[wasm_bindgen(js_name = "listRoles")]
    pub async fn list_roles(&self) -> Vec<String> {
        self.auth_provider.acl_manager().list_roles().await
    }

    #[wasm_bindgen(js_name = "roleCount")]
    pub async fn role_count(&self) -> usize {
        self.auth_provider.acl_manager().role_count().await
    }

    /// # Errors
    /// Returns an error if the permission string is invalid or role does not exist.
    #[wasm_bindgen(js_name = "addRoleRule")]
    pub async fn add_role_rule(
        &self,
        #[wasm_bindgen(js_name = "roleName")] role_name: String,
        #[wasm_bindgen(js_name = "topicPattern")] topic_pattern: String,
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

    /// # Errors
    /// Returns an error if the role does not exist.
    #[wasm_bindgen(js_name = "assignRole")]
    pub async fn assign_role(
        &self,
        username: String,
        #[wasm_bindgen(js_name = "roleName")] role_name: String,
    ) -> Result<(), JsValue> {
        self.auth_provider
            .acl_manager()
            .assign_role(&username, &role_name)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = "unassignRole")]
    pub async fn unassign_role(
        &self,
        username: &str,
        #[wasm_bindgen(js_name = "roleName")] role_name: &str,
    ) -> bool {
        self.auth_provider
            .acl_manager()
            .unassign_role(username, role_name)
            .await
    }

    #[wasm_bindgen(js_name = "getUserRoles")]
    pub async fn get_user_roles(&self, username: &str) -> Vec<String> {
        self.auth_provider
            .acl_manager()
            .get_user_roles(username)
            .await
    }

    #[wasm_bindgen(js_name = "clearRoles")]
    pub async fn clear_roles(&self) {
        self.auth_provider.acl_manager().clear_roles().await;
    }

    #[wasm_bindgen(js_name = "setAclDefaultDeny")]
    pub async fn set_acl_default_deny(&self) {
        self.auth_provider
            .acl_manager()
            .set_default_permission(Permission::Deny)
            .await;
    }

    #[wasm_bindgen(js_name = "setAclDefaultAllow")]
    pub async fn set_acl_default_allow(&self) {
        self.auth_provider
            .acl_manager()
            .set_default_permission(Permission::ReadWrite)
            .await;
    }

    /// # Errors
    /// Returns an error if the `MessageChannel` cannot be created.
    #[wasm_bindgen(js_name = "createClientPort")]
    pub fn create_client_port(&self) -> Result<MessagePort, JsValue> {
        let channel = web_sys::MessageChannel::new()?;

        let client_port = channel.port1();
        let broker_port = channel.port2();

        WasmClientHandler::spawn(
            broker_port,
            self.handler_context(Arc::clone(&self.auth_provider) as _),
        );

        Ok(client_port)
    }

    /// # Errors
    /// Returns an error if the bridge cannot be added.
    #[wasm_bindgen(js_name = "addBridge")]
    pub async fn add_bridge(
        &self,
        config: WasmBridgeConfig,
        #[wasm_bindgen(js_name = "remotePort")] remote_port: MessagePort,
    ) -> Result<(), JsValue> {
        let manager = self.bridge_manager.borrow().clone();
        manager.add_bridge(config, remote_port).await
    }

    /// # Errors
    /// Returns an error if the bridge cannot be added or WebSocket connection fails.
    #[wasm_bindgen(js_name = "addBridgeWebSocket")]
    pub async fn add_bridge_ws(
        &self,
        config: WasmBridgeConfig,
        url: &str,
        opts: &WasmConnectOptions,
    ) -> Result<(), JsValue> {
        let manager = self.bridge_manager.borrow().clone();
        manager.add_bridge_ws(config, url, opts).await
    }

    /// # Errors
    /// Returns an error if the bridge cannot be removed.
    #[wasm_bindgen(js_name = "removeBridge")]
    pub async fn remove_bridge(&self, name: &str) -> Result<(), JsValue> {
        let manager = self.bridge_manager.borrow().clone();
        manager.remove_bridge(name).await
    }

    #[must_use]
    #[wasm_bindgen(js_name = "listBridges")]
    pub fn list_bridges(&self) -> Vec<String> {
        self.bridge_manager.borrow().list_bridges()
    }

    #[wasm_bindgen(js_name = "stopAllBridges")]
    pub async fn stop_all_bridges(&self) {
        let manager = self.bridge_manager.borrow().clone();
        manager.stop_all().await;
    }

    #[wasm_bindgen(js_name = "startSysTopics")]
    pub fn start_sys_topics(&self) {
        self.start_sys_topics_with_interval_secs(10);
    }

    #[wasm_bindgen(js_name = "startSysTopicsWithIntervalSecs")]
    pub fn start_sys_topics_with_interval_secs(
        &self,
        #[wasm_bindgen(js_name = "intervalSecs")] interval_secs: u32,
    ) {
        if self.sys_topics_running.get() {
            return;
        }
        self.sys_topics_running.set(true);

        let provider = SysTopicsProvider::new(Arc::clone(&self.router), Arc::clone(&self.stats));
        let interval_ms = u64::from(interval_secs) * 1000;
        let running = Rc::clone(&self.sys_topics_running);

        wasm_bindgen_futures::spawn_local(async move {
            gloo_timers::future::sleep(std::time::Duration::from_millis(interval_ms)).await;
            provider.publish_static_topics().await;

            while running.get() {
                gloo_timers::future::sleep(std::time::Duration::from_millis(interval_ms)).await;
                if !running.get() {
                    break;
                }
                provider.publish_dynamic_topics().await;
            }
        });
    }

    #[wasm_bindgen(js_name = "stopSysTopics")]
    pub fn stop_sys_topics(&self) {
        self.sys_topics_running.set(false);
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

    /// # Errors
    /// Returns an error if the config write lock cannot be acquired.
    #[wasm_bindgen(js_name = "updateConfig")]
    pub fn update_config(
        &self,
        #[wasm_bindgen(js_name = "newConfig")] new_config: WasmBrokerConfig,
    ) -> Result<(), JsValue> {
        let new_hash = new_config.calculate_hash();
        let old_hash = self.config_hash.get();

        if new_hash == old_hash {
            return Ok(());
        }

        let broker_config = new_config.into_broker_config();

        let echo_key = if broker_config.echo_suppression_config.enabled {
            Some(broker_config.echo_suppression_config.property_key.clone())
        } else {
            None
        };
        let max_rate = broker_config.max_outbound_rate_per_client;
        let new_limits = resource_limits_from(&broker_config);

        if let Ok(mut config) = self.config.try_write() {
            *config = broker_config;
        } else {
            web_sys::console::error_1(
                &"Config update failed: lock contention (config in use)".into(),
            );
            return Err(JsValue::from_str(
                "Failed to acquire config write lock: resource busy",
            ));
        }

        if !self.router.try_update_echo_suppression_key(echo_key) {
            web_sys::console::warn_1(
                &"Echo suppression key update skipped: router lock contention".into(),
            );
        }

        self.router.update_max_outbound_rate(max_rate);
        self.resource_monitor.update_limits(new_limits);

        self.config_hash.set(new_hash);

        if let Some(callback) = self.on_config_change.borrow().as_ref() {
            let old_hash_js = JsValue::from_f64(u64_to_f64_saturating(old_hash));
            let new_hash_js = JsValue::from_f64(u64_to_f64_saturating(new_hash));
            if let Err(e) = callback.call2(&JsValue::NULL, &old_hash_js, &new_hash_js) {
                web_sys::console::error_1(&format!("Config change callback error: {e:?}").into());
            }
        }

        web_sys::console::log_1(&"Broker config updated".into());
        Ok(())
    }

    #[wasm_bindgen(js_name = "getConfigHash")]
    #[must_use]
    pub fn get_config_hash(&self) -> f64 {
        u64_to_f64_saturating(self.config_hash.get())
    }

    #[wasm_bindgen(js_name = "onConfigChange")]
    pub fn on_config_change(&self, callback: js_sys::Function) {
        *self.on_config_change.borrow_mut() = Some(callback);
    }

    #[wasm_bindgen(js_name = "onClientConnect")]
    pub fn on_client_connect(&self, callback: js_sys::Function) {
        *self.event_callbacks.on_client_connect.borrow_mut() = Some(callback);
    }

    #[wasm_bindgen(js_name = "onClientDisconnect")]
    pub fn on_client_disconnect(&self, callback: js_sys::Function) {
        *self.event_callbacks.on_client_disconnect.borrow_mut() = Some(callback);
    }

    #[wasm_bindgen(js_name = "onClientPublish")]
    pub fn on_client_publish(&self, callback: js_sys::Function) {
        *self.event_callbacks.on_client_publish.borrow_mut() = Some(callback);
    }

    #[wasm_bindgen(js_name = "onClientSubscribe")]
    pub fn on_client_subscribe(&self, callback: js_sys::Function) {
        *self.event_callbacks.on_client_subscribe.borrow_mut() = Some(callback);
    }

    #[wasm_bindgen(js_name = "onClientUnsubscribe")]
    pub fn on_client_unsubscribe(&self, callback: js_sys::Function) {
        *self.event_callbacks.on_client_unsubscribe.borrow_mut() = Some(callback);
    }

    #[wasm_bindgen(js_name = "onMessageDelivered")]
    pub fn on_message_delivered(&self, callback: js_sys::Function) {
        *self.event_callbacks.on_message_delivered.borrow_mut() = Some(callback);
    }

    #[wasm_bindgen(js_name = "getMaxClients")]
    #[must_use]
    pub fn get_max_clients(&self) -> u32 {
        self.config.read().map_or_else(
            |_| {
                web_sys::console::warn_1(&"Config read failed, using default max_clients".into());
                1000
            },
            |c| u32::try_from(c.max_clients).unwrap_or(u32::MAX),
        )
    }

    #[wasm_bindgen(js_name = "getMaxPacketSize")]
    #[must_use]
    pub fn get_max_packet_size(&self) -> u32 {
        self.config.read().map_or_else(
            |_| {
                web_sys::console::warn_1(
                    &"Config read failed, using default max_packet_size".into(),
                );
                268_435_456
            },
            |c| u32::try_from(c.max_packet_size).unwrap_or(u32::MAX),
        )
    }

    #[wasm_bindgen(js_name = "getSessionExpiryIntervalSecs")]
    #[must_use]
    pub fn get_session_expiry_interval_secs(&self) -> u32 {
        self.config.read().map_or_else(
            |_| {
                web_sys::console::warn_1(
                    &"Config read failed, using default session_expiry_interval".into(),
                );
                u32::MAX
            },
            |c| u64_to_u32_saturating(c.session_expiry_interval.as_secs()),
        )
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::{WasmBroker, WasmBrokerConfig};
    use crate::client_handler::WasmClientHandler;
    use bytes::BytesMut;
    use mqtt5::broker::auth::{AuthProvider, AuthResult, EnhancedAuthResult};
    use mqtt5::broker::storage::StorageBackend;
    use mqtt5_protocol::error::Result;
    use mqtt5_protocol::packet::auth::AuthPacket;
    use mqtt5_protocol::packet::connect::ConnectPacket;
    use mqtt5_protocol::packet::disconnect::DisconnectPacket;
    use mqtt5_protocol::packet::MqttPacket;
    use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
    use mqtt5_protocol::types::{ConnectOptions, WillMessage};
    use mqtt5_protocol::QoS;
    use std::cell::RefCell;
    use std::future::Future;
    use std::net::SocketAddr;
    use std::pin::Pin;
    use std::rc::Rc;
    use std::sync::Arc;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_test::wasm_bindgen_test;
    use web_sys::{MessageEvent, MessagePort};

    async fn sleep(ms: u32) {
        gloo_timers::future::TimeoutFuture::new(ms).await;
    }

    fn send(port: &MessagePort, packet: &impl MqttPacket) {
        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();
        port.post_message(&js_sys::Uint8Array::from(&buf[..]).buffer())
            .unwrap();
    }

    async fn connect_with_will(broker: &WasmBroker, client_id: &str) -> MessagePort {
        let port = broker.create_client_port().unwrap();
        let inbox = Rc::new(RefCell::new(Vec::new()));
        let inbox_in = Rc::clone(&inbox);
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            inbox_in
                .borrow_mut()
                .extend(js_sys::Uint8Array::new(&event.data()).to_vec());
        });
        port.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref())
            .unwrap();
        on_message.forget();
        port.start();
        let options = ConnectOptions::new(client_id)
            .with_session_expiry_interval(60)
            .with_will(WillMessage::new(format!("will/{client_id}"), "offline"));
        send(&port, &ConnectPacket::new(options));
        for _ in 0..200 {
            if inbox.borrow().len() >= 4 {
                break;
            }
            sleep(5).await;
        }
        assert_eq!(inbox.borrow().first(), Some(&0x20), "expected CONNACK");
        port
    }

    async fn stored_will_present(broker: &WasmBroker, client_id: &str) -> bool {
        broker
            .storage
            .get_session(client_id)
            .await
            .unwrap()
            .expect("session is kept for its expiry interval")
            .will_message
            .is_some()
    }

    fn broker() -> WasmBroker {
        let mut config = WasmBrokerConfig::new();
        config.set_allow_anonymous(true);
        WasmBroker::with_config(config).unwrap()
    }

    #[wasm_bindgen_test]
    async fn normal_disconnect_removes_stored_will() {
        let broker = broker();
        let port = connect_with_will(&broker, "stored-will-normal").await;
        assert!(stored_will_present(&broker, "stored-will-normal").await);

        send(&port, &DisconnectPacket::new(ReasonCode::Success));
        port.close();
        sleep(300).await;

        assert!(!stored_will_present(&broker, "stored-will-normal").await);
    }

    #[wasm_bindgen_test]
    async fn published_will_is_removed_from_stored_session() {
        let broker = broker();
        let port = connect_with_will(&broker, "stored-will-published").await;
        assert!(stored_will_present(&broker, "stored-will-published").await);

        port.close();
        sleep(300).await;

        assert!(!stored_will_present(&broker, "stored-will-published").await);
    }

    struct Client {
        port: MessagePort,
        inbox: Rc<RefCell<Vec<u8>>>,
    }

    impl Client {
        fn open(port: MessagePort) -> Self {
            let inbox = Rc::new(RefCell::new(Vec::new()));
            let inbox_in = Rc::clone(&inbox);
            let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                inbox_in
                    .borrow_mut()
                    .extend(js_sys::Uint8Array::new(&event.data()).to_vec());
            });
            port.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref())
                .unwrap();
            on_message.forget();
            port.start();
            Self { port, inbox }
        }

        fn send(&self, packet: &impl MqttPacket) {
            send(&self.port, packet);
        }

        fn send_raw(&self, bytes: &[u8]) {
            self.port
                .post_message(&js_sys::Uint8Array::from(bytes).buffer())
                .unwrap();
        }

        async fn wait_for(&self, len: usize) -> Vec<u8> {
            for _ in 0..300 {
                if self.inbox.borrow().len() >= len {
                    break;
                }
                sleep(10).await;
            }
            self.inbox.borrow().clone()
        }

        fn received(&self) -> Vec<u8> {
            self.inbox.borrow().clone()
        }

        fn clear(&self) {
            self.inbox.borrow_mut().clear();
        }
    }

    fn connect_options(client_id: &str, clean_start: bool, expiry: u32) -> ConnectOptions {
        ConnectOptions::new(client_id)
            .with_clean_start(clean_start)
            .with_session_expiry_interval(expiry)
    }

    fn start_connect(broker: &WasmBroker, options: ConnectOptions) -> Client {
        let client = Client::open(broker.create_client_port().unwrap());
        client.send(&ConnectPacket::new(options));
        client
    }

    async fn connected(broker: &WasmBroker, options: ConnectOptions) -> Client {
        let client = start_connect(broker, options);
        let connack = client.wait_for(4).await;
        assert_eq!(connack.first(), Some(&0x20), "expected CONNACK");
        assert_eq!(connack[3], 0x00, "CONNACK must be Success");
        client.clear();
        client
    }

    fn subscribe_bytes(topic: &str) -> Vec<u8> {
        let topic_len = u8::try_from(topic.len()).unwrap();
        let mut bytes = vec![0x82, 6 + topic_len, 0x00, 0x01, 0x00, 0x00, topic_len];
        bytes.extend_from_slice(topic.as_bytes());
        bytes.push(0x01);
        bytes
    }

    fn unsubscribe_bytes(topic: &str) -> Vec<u8> {
        let topic_len = u8::try_from(topic.len()).unwrap();
        let mut bytes = vec![0xA2, 5 + topic_len, 0x00, 0x02, 0x00, 0x00, topic_len];
        bytes.extend_from_slice(topic.as_bytes());
        bytes
    }

    fn subscribe_bytes_at(topic: &str, qos: u8) -> Vec<u8> {
        let mut bytes = subscribe_bytes(topic);
        if let Some(options) = bytes.last_mut() {
            *options = qos;
        }
        bytes
    }

    /// The first byte and body of each packet in `bytes`, all shorter than 128 bytes.
    fn frames(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut frames = Vec::new();
        let mut rest = bytes;
        while let [first_byte, length, tail @ ..] = rest {
            assert!(*length < 128);
            let (body, after) = tail.split_at(usize::from(*length));
            frames.push((*first_byte, body.to_vec()));
            rest = after;
        }
        frames
    }

    fn publish_frames(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
        frames(bytes)
            .into_iter()
            .filter(|(first_byte, _)| first_byte >> 4 == 3)
            .collect()
    }

    async fn instance(broker: &WasmBroker, client_id: &str, topic_filter: &str) -> u64 {
        broker
            .storage
            .get_session(client_id)
            .await
            .unwrap()
            .unwrap()
            .subscriptions[topic_filter]
            .instance
    }

    async fn route(broker: &WasmBroker, topic: &str, payload: &[u8], qos: QoS) {
        let publish =
            mqtt5_protocol::packet::publish::PublishPacket::new(topic, payload.to_vec(), qos);
        broker.router.route_message(&publish, None).await;
    }

    fn pipelined_resume(broker: &WasmBroker, client_id: &str, topic: &str) -> Client {
        let client = Client::open(broker.create_client_port().unwrap());
        let mut connect = BytesMut::new();
        ConnectPacket::new(connect_options(client_id, false, 3600))
            .encode(&mut connect)
            .unwrap();
        let mut pipelined = connect.to_vec();
        pipelined.extend_from_slice(&subscribe_bytes(topic));
        client.send_raw(&pipelined);
        client
    }

    async fn subscribed(client: &Client, topic: &str) {
        client.send_raw(&subscribe_bytes(topic));
        assert_eq!(
            client.wait_for(1).await.first(),
            Some(&0x90),
            "expected SUBACK"
        );
        client.clear();
    }

    #[wasm_bindgen_test]
    async fn claim_waits_for_the_session_slot() {
        let broker = broker();
        let slot = broker.router.lock_session("slot-claim").await;
        let client = start_connect(&broker, connect_options("slot-claim", true, 60));
        sleep(200).await;
        assert!(
            client.received().is_empty(),
            "CONNACK sent while the slot was held"
        );
        assert!(
            broker
                .storage
                .get_session("slot-claim")
                .await
                .unwrap()
                .is_none(),
            "the claim wrote the session without holding the slot"
        );
        assert!(
            !broker.router.is_connected("slot-claim").await,
            "the claim registered without holding the slot"
        );
        drop(slot);
        assert_eq!(client.wait_for(4).await.first(), Some(&0x20));
        assert!(broker.router.is_connected("slot-claim").await);
    }

    #[wasm_bindgen_test]
    async fn displaced_subscribe_installs_no_route() {
        let broker = broker();
        let first = connected(&broker, connect_options("dsub", true, 60)).await;
        let slot = broker.router.lock_session("dsub").await;
        let second = start_connect(&broker, connect_options("dsub", true, 60));
        sleep(50).await;
        first.send_raw(&subscribe_bytes("dsub/t"));
        sleep(50).await;
        drop(slot);
        assert_eq!(second.wait_for(4).await.first(), Some(&0x20));
        sleep(200).await;
        assert!(
            !broker.router.has_subscription("dsub", "dsub/t").await,
            "a displaced connection's SUBSCRIBE installed a route for the successor"
        );
        let stored = broker.storage.get_session("dsub").await.unwrap().unwrap();
        assert!(!stored.subscriptions.contains_key("dsub/t"));
    }

    #[wasm_bindgen_test]
    async fn displaced_unsubscribe_keeps_the_successors_route() {
        let broker = broker();
        let first = connected(&broker, connect_options("dunsub", true, 60)).await;
        subscribed(&first, "keep/x").await;
        let slot = broker.router.lock_session("dunsub").await;
        let second = start_connect(&broker, connect_options("dunsub", false, 60));
        sleep(50).await;
        first.send_raw(&unsubscribe_bytes("keep/x"));
        sleep(50).await;
        drop(slot);
        let connack = second.wait_for(4).await;
        assert_eq!(connack.first(), Some(&0x20));
        assert_eq!(connack[2] & 1, 1, "session present");
        sleep(200).await;
        assert!(
            broker.router.has_subscription("dunsub", "keep/x").await,
            "a displaced connection's UNSUBSCRIBE removed the successor's route"
        );
        let stored = broker.storage.get_session("dunsub").await.unwrap().unwrap();
        assert!(stored.subscriptions.contains_key("keep/x"));
    }

    #[wasm_bindgen_test]
    async fn resume_drops_a_revoked_subscription_from_session_and_routes() {
        let broker = broker();
        let first = connected(&broker, connect_options("revoked", true, 3600)).await;
        subscribed(&first, "secret/x").await;
        first.send(&DisconnectPacket::new(ReasonCode::Success));
        first.port.close();
        sleep(200).await;
        assert!(broker.router.has_subscription("revoked", "secret/x").await);

        broker.set_acl_default_deny().await;
        let second = start_connect(&broker, connect_options("revoked", false, 3600));
        let connack = second.wait_for(4).await;
        assert_eq!(connack.first(), Some(&0x20));
        assert_eq!(connack[2] & 1, 1, "session present");
        assert!(
            !broker.router.has_subscription("revoked", "secret/x").await,
            "a subscription no longer authorized must not stay routed after resume"
        );
        let stored = broker
            .storage
            .get_session("revoked")
            .await
            .unwrap()
            .unwrap();
        assert!(!stored.subscriptions.contains_key("secret/x"));
    }

    #[wasm_bindgen_test]
    async fn resume_withholds_what_was_queued_for_a_revoked_subscription() {
        let broker = broker();
        let first = connected(&broker, connect_options("revoked-q", true, 3600)).await;
        subscribed(&first, "secret/x").await;
        first.send(&DisconnectPacket::new(ReasonCode::Success));
        first.port.close();
        sleep(200).await;

        let publish = mqtt5_protocol::packet::publish::PublishPacket::new(
            "secret/x",
            &b"hi"[..],
            QoS::AtLeastOnce,
        );
        broker.router.route_message(&publish, None).await;
        assert_eq!(
            broker.router.queue_handle("revoked-q").count(),
            1,
            "queued for the session"
        );

        broker.set_acl_default_deny().await;
        let second = start_connect(&broker, connect_options("revoked-q", false, 3600));
        let connack = second.wait_for(4).await;
        assert_eq!(connack.first(), Some(&0x20));
        assert_eq!(connack[2] & 1, 1, "session present");
        sleep(500).await;
        let received = second.received();
        let after_connack = &received[2 + usize::from(received[1])..];
        assert!(
            after_connack.is_empty(),
            "something followed CONNACK, a message queued for a subscription dropped on resume: {after_connack:?}"
        );
        assert_eq!(
            broker.router.queue_handle("revoked-q").count(),
            0,
            "the queue drained"
        );
    }

    #[wasm_bindgen_test]
    async fn the_forwarder_withholds_what_a_lane_carries_for_a_revoked_subscription() {
        let broker = broker();
        let first = connected(&broker, connect_options("revoked-l", true, 3600)).await;
        subscribed(&first, "secret/x").await;
        first.send(&DisconnectPacket::new(ReasonCode::Success));
        first.port.close();
        sleep(200).await;
        let routed_for = instance(&broker, "revoked-l", "secret/x").await;

        broker.set_acl_default_deny().await;
        let second = connected(&broker, connect_options("revoked-l", false, 3600)).await;
        second.clear();
        broker
            .router
            .subscribe(
                mqtt5::broker::router::SubscriptionRequest::new(
                    "revoked-l",
                    "secret/x",
                    QoS::AtMostOnce,
                )
                .with_instance(routed_for),
            )
            .await
            .unwrap();
        let publish = mqtt5_protocol::packet::publish::PublishPacket::new(
            "secret/x",
            &b"hi"[..],
            QoS::AtMostOnce,
        );
        broker.router.route_message(&publish, None).await;
        sleep(300).await;
        assert!(
            second.received().is_empty(),
            "the forwarder sent a message for a subscription dropped on resume: {:?}",
            second.received()
        );
    }

    #[wasm_bindgen_test]
    async fn resubscribing_does_not_release_what_was_queued_for_a_revoked_subscription() {
        let broker = broker();
        let first = connected(&broker, connect_options("rw1", true, 3600)).await;
        subscribed(&first, "secret/#").await;
        first.send(&DisconnectPacket::new(ReasonCode::Success));
        first.port.close();
        sleep(200).await;
        let publish = mqtt5_protocol::packet::publish::PublishPacket::new(
            "secret/x",
            &b"hi"[..],
            QoS::AtLeastOnce,
        );
        broker.router.route_message(&publish, None).await;
        assert_eq!(broker.router.queue_handle("rw1").count(), 1);

        broker.set_acl_default_deny().await;
        broker
            .add_acl_rule(
                "anonymous".to_string(),
                "secret/x".to_string(),
                "readwrite".to_string(),
            )
            .await
            .unwrap();
        let client = Client::open(broker.create_client_port().unwrap());
        let mut connect = BytesMut::new();
        ConnectPacket::new(connect_options("rw1", false, 3600))
            .encode(&mut connect)
            .unwrap();
        let mut pipelined = connect.to_vec();
        pipelined.extend_from_slice(&subscribe_bytes("secret/x"));
        client.send_raw(&pipelined);
        sleep(500).await;
        let stored = broker.storage.get_session("rw1").await.unwrap().unwrap();
        assert!(!stored.subscriptions.contains_key("secret/#"));
        assert!(stored.subscriptions.contains_key("secret/x"));
        let received = client.received();
        let mut rest = received.as_slice();
        let mut types = Vec::new();
        while let [first_byte, length, tail @ ..] = rest {
            types.push(*first_byte);
            rest = &tail[usize::from(*length)..];
        }
        assert!(
            !types.iter().any(|first_byte| first_byte >> 4 == 3),
            "queued during the withdrawal, sent after the re-subscribe: {types:02x?}"
        );
    }

    #[wasm_bindgen_test]
    async fn resubscribing_once_authorized_again_delivers_again() {
        let broker = broker();
        let first = connected(&broker, connect_options("revoked-r", true, 3600)).await;
        subscribed(&first, "secret/x").await;
        first.send(&DisconnectPacket::new(ReasonCode::Success));
        first.port.close();
        sleep(200).await;
        broker.set_acl_default_deny().await;
        let second = connected(&broker, connect_options("revoked-r", false, 3600)).await;

        broker.set_acl_default_allow().await;
        subscribed(&second, "secret/x").await;
        let publish = mqtt5_protocol::packet::publish::PublishPacket::new(
            "secret/x",
            &b"hi"[..],
            QoS::AtMostOnce,
        );
        broker.router.route_message(&publish, None).await;
        assert_eq!(
            second.wait_for(1).await.first().map(|byte| byte >> 4),
            Some(3),
            "a subscription made again once authorized again delivers"
        );
    }

    #[wasm_bindgen_test]
    async fn a_plain_resubscribe_keeps_what_was_queued_in_order() {
        let broker = broker();
        let first = connected(&broker, connect_options("plain-w", true, 3600)).await;
        subscribed(&first, "open/x").await;
        first.send(&DisconnectPacket::new(ReasonCode::Success));
        first.port.close();
        sleep(200).await;
        for payload in ["q0", "q1", "q2"] {
            route(&broker, "open/x", payload.as_bytes(), QoS::AtLeastOnce).await;
        }
        assert_eq!(broker.router.queue_handle("plain-w").count(), 3);

        let client = pipelined_resume(&broker, "plain-w", "open/x");
        sleep(500).await;
        let payloads: Vec<Vec<u8>> = publish_frames(&client.received())
            .into_iter()
            .map(|(_, body)| body[body.len() - 2..].to_vec())
            .collect();
        assert_eq!(
            payloads,
            [b"q0".to_vec(), b"q1".to_vec(), b"q2".to_vec()],
            "a SUBSCRIBE refreshing a held subscription dropped or reordered what was queued before it"
        );
    }

    #[wasm_bindgen_test]
    async fn subscribes_neither_reorder_nor_drop_messages_on_other_topics() {
        let broker = broker();
        let first = connected(&broker, connect_options("ordered-w", true, 3600)).await;
        subscribed(&first, "secret/x").await;
        subscribed(&first, "open/x").await;
        first.send(&DisconnectPacket::new(ReasonCode::Success));
        first.port.close();
        sleep(200).await;
        broker.set_acl_default_deny().await;
        broker
            .add_acl_rule(
                "anonymous".to_string(),
                "open/x".to_string(),
                "readwrite".to_string(),
            )
            .await
            .unwrap();
        let client = connected(&broker, connect_options("ordered-w", false, 3600)).await;
        broker
            .add_acl_rule(
                "anonymous".to_string(),
                "other/#".to_string(),
                "readwrite".to_string(),
            )
            .await
            .unwrap();
        for index in 0u32..500 {
            route(&broker, "open/x", &index.to_be_bytes(), QoS::AtLeastOnce).await;
            if index % 50 == 0 {
                client.send_raw(&subscribe_bytes(&format!("other/{index}")));
                sleep(1).await;
            }
        }
        sleep(500).await;
        let received: Vec<u32> = publish_frames(&client.received())
            .into_iter()
            .map(|(_, body)| u32::from_be_bytes(body[body.len() - 4..].try_into().unwrap()))
            .collect();
        assert_eq!(received, (0u32..500).collect::<Vec<_>>());
    }

    async fn shared_group_after_one_member_lost_it(revoke: bool) -> (Vec<String>, usize) {
        let broker = broker();
        let member = connected(&broker, connect_options("shared-w", true, 3600)).await;
        subscribed(&member, "$share/g/secret/x").await;
        subscribed(&member, "open/x").await;
        member.send(&DisconnectPacket::new(ReasonCode::Success));
        member.port.close();
        sleep(200).await;
        route(&broker, "secret/x", b"q", QoS::AtLeastOnce).await;
        route(&broker, "open/x", b"q", QoS::AtLeastOnce).await;
        assert_eq!(broker.router.queue_handle("shared-w").count(), 2);

        let other = connected(&broker, connect_options("shared-w-other", true, 3600)).await;
        subscribed(&other, "$share/g/secret/x").await;
        if revoke {
            broker.set_acl_default_deny().await;
            broker
                .add_acl_rule(
                    "anonymous".to_string(),
                    "open/x".to_string(),
                    "readwrite".to_string(),
                )
                .await
                .unwrap();
        }
        let member = start_connect(&broker, connect_options("shared-w", false, 3600));
        sleep(300).await;
        route(&broker, "secret/x", b"l", QoS::AtLeastOnce).await;
        route(&broker, "secret/x", b"l", QoS::AtLeastOnce).await;
        sleep(300).await;
        let member_topics = publish_frames(&member.received())
            .into_iter()
            .map(|(_, body)| {
                let length = usize::from(u16::from_be_bytes([body[0], body[1]]));
                String::from_utf8_lossy(&body[2..2 + length]).into_owned()
            })
            .collect();
        (member_topics, publish_frames(&other.received()).len())
    }

    #[wasm_bindgen_test]
    async fn a_member_that_lost_its_shared_subscription_gets_none_of_its_messages() {
        let (member, other) = shared_group_after_one_member_lost_it(true).await;
        assert_eq!(member, ["open/x"]);
        assert_eq!(other, 2, "the other member gets what is published after");
    }

    #[wasm_bindgen_test]
    async fn control_a_member_keeping_its_shared_subscription_gets_its_queued_message() {
        let (member, other) = shared_group_after_one_member_lost_it(false).await;
        assert!(member.contains(&"secret/x".to_string()), "{member:?}");
        assert!(member.contains(&"open/x".to_string()), "{member:?}");
        assert_eq!(
            member.iter().filter(|topic| *topic == "secret/x").count() + other,
            3
        );
    }

    async fn started_exactly_once_delivery_after(revoke: bool) -> Vec<u8> {
        let broker = broker();
        let first = connected(&broker, connect_options("started-w", true, 3600)).await;
        first.send_raw(&subscribe_bytes_at("secret/x", 2));
        assert_eq!(first.wait_for(1).await.first(), Some(&0x90));
        first.clear();
        route(&broker, "secret/x", b"s", QoS::ExactlyOnce).await;
        assert_eq!(
            publish_frames(&first.wait_for(1).await)
                .first()
                .map(|(first_byte, _)| *first_byte),
            Some(0x34)
        );
        first.port.close();
        sleep(200).await;
        if revoke {
            broker.set_acl_default_deny().await;
        }
        let second = start_connect(&broker, connect_options("started-w", false, 3600));
        sleep(500).await;
        publish_frames(&second.received())
            .into_iter()
            .map(|(first_byte, _)| first_byte)
            .collect()
    }

    #[wasm_bindgen_test]
    async fn an_exactly_once_delivery_already_started_is_completed_after_a_revocation() {
        assert_eq!(started_exactly_once_delivery_after(true).await, [0x3C]);
    }

    #[wasm_bindgen_test]
    async fn control_an_exactly_once_delivery_already_started_is_completed() {
        assert_eq!(started_exactly_once_delivery_after(false).await, [0x3C]);
    }

    #[wasm_bindgen_test]
    async fn a_deny_rule_refuses_a_shared_subscription_to_its_filter() {
        let broker = broker();
        broker
            .add_acl_rule("*".to_string(), "secret/#".to_string(), "deny".to_string())
            .await
            .unwrap();
        let client = connected(&broker, connect_options("shared-denied", true, 0)).await;
        client.send_raw(&subscribe_bytes("$share/g/secret/x"));
        let suback = client.wait_for(1).await;
        assert_eq!(suback.first(), Some(&0x90), "expected SUBACK");
        assert_eq!(suback.last(), Some(&0x87), "expected Not authorized");
    }

    #[wasm_bindgen_test]
    async fn a_deny_rule_under_a_wildcard_keeps_its_topics_from_a_subscriber() {
        let broker = broker();
        let publish = |topic: &'static str, retain: bool| {
            let mut publish = mqtt5_protocol::packet::publish::PublishPacket::new(
                topic,
                &b"hi"[..],
                QoS::AtMostOnce,
            );
            publish.retain = retain;
            publish
        };
        broker
            .router
            .route_message(&publish("data/secret/retained", true), None)
            .await;
        broker
            .add_acl_rule(
                "*".to_string(),
                "data/secret/#".to_string(),
                "deny".to_string(),
            )
            .await
            .unwrap();
        let client = connected(&broker, connect_options("wildcard-denied", true, 0)).await;
        subscribed(&client, "data/#").await;
        client.clear();
        broker
            .router
            .route_message(&publish("data/secret/live", false), None)
            .await;
        broker
            .router
            .route_message(&publish("data/open", false), None)
            .await;
        sleep(200).await;
        let received = client.received();
        let contains = |needle: &[u8]| received.windows(needle.len()).any(|w| w == needle);
        assert!(!contains(b"data/secret"), "{received:?}");
        assert!(contains(b"data/open"), "{received:?}");
    }

    #[wasm_bindgen_test]
    async fn sweep_removes_an_expired_offline_session() {
        let mut config = WasmBrokerConfig::new();
        config.set_allow_anonymous(true);
        config.set_session_sweep_interval_secs(1);
        let broker = WasmBroker::with_config(config).unwrap();
        let client = connected(&broker, connect_options("swept", true, 1)).await;
        subscribed(&client, "swept/t").await;
        client.send(&DisconnectPacket::new(ReasonCode::Success));
        client.port.close();
        sleep(200).await;
        assert!(broker.router.has_subscription("swept", "swept/t").await);
        sleep(3500).await;
        assert_eq!(
            broker.router.subscription_count_for_client("swept").await,
            0,
            "the periodic sweep must strip an expired session's routes"
        );
        assert!(broker.storage.get_session("swept").await.unwrap().is_none());
    }

    #[wasm_bindgen_test]
    async fn release_waits_for_the_session_slot() {
        let broker = broker();
        let client = connected(&broker, connect_options("slot-release", true, 60)).await;
        let slot = broker.router.lock_session("slot-release").await;
        client.port.close();
        sleep(200).await;
        assert!(
            broker.router.is_connected("slot-release").await,
            "the router entry was released without holding the slot"
        );
        drop(slot);
        sleep(200).await;
        assert!(!broker.router.is_connected("slot-release").await);
    }

    #[wasm_bindgen_test]
    async fn disconnect_expiry_is_stored_before_the_release() {
        let broker = broker();
        let client = connected(&broker, connect_options("disc-zero", true, 3600)).await;
        let slot = broker.router.lock_session("disc-zero").await;
        let mut disconnect = DisconnectPacket::new(ReasonCode::Success);
        disconnect.properties.set_session_expiry_interval(0);
        client.send(&disconnect);
        sleep(100).await;
        let next = broker.router.lock_session("disc-zero");
        futures::pin_mut!(next);
        assert!(futures::poll!(next.as_mut()).is_pending());
        drop(slot);
        let observed = next.await;
        let owner_live = broker.router.is_connected("disc-zero").await;
        let stored = broker
            .storage
            .get_session("disc-zero")
            .await
            .unwrap()
            .map(|session| session.expiry_interval);
        drop(observed);
        assert!(
            owner_live,
            "the release ran before the DISCONNECT expiry reached the store"
        );
        assert_eq!(
            stored,
            Some(Some(0)),
            "between DISCONNECT and release a claim must already see expiry 0"
        );
        let resumed = start_connect(&broker, connect_options("disc-zero", false, 3600));
        let connack = resumed.wait_for(4).await;
        assert_eq!(connack[2] & 1, 0, "an ended session must not be resumed");
    }

    struct Challenge;

    impl AuthProvider for Challenge {
        fn authenticate<'a>(
            &'a self,
            _connect: &'a ConnectPacket,
            _client_addr: SocketAddr,
        ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
            Box::pin(async move { Ok(AuthResult::success()) })
        }

        fn authorize_publish<'a>(
            &'a self,
            _client_id: &str,
            _user_id: Option<&'a str>,
            _topic: &'a str,
        ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
            Box::pin(async move { true })
        }

        fn authorize_subscribe<'a>(
            &'a self,
            _client_id: &str,
            _user_id: Option<&'a str>,
            _topic_filter: &'a str,
        ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
            Box::pin(async move { true })
        }

        fn supports_enhanced_auth(&self) -> bool {
            true
        }

        fn authenticate_enhanced<'a>(
            &'a self,
            auth_method: &'a str,
            auth_data: Option<&'a [u8]>,
            _client_id: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
            let method = auth_method.to_string();
            Box::pin(async move {
                Ok(match auth_data {
                    None => EnhancedAuthResult::continue_auth(method, Some(b"c".to_vec())),
                    Some(_) => EnhancedAuthResult::success(method),
                })
            })
        }
    }

    #[wasm_bindgen_test]
    async fn multi_step_enhanced_auth_rejects_an_unsupported_will_qos() {
        let mut config = WasmBrokerConfig::new();
        config.set_maximum_qos(1);
        let broker = WasmBroker::with_config(config).unwrap();
        let channel = web_sys::MessageChannel::new().unwrap();
        WasmClientHandler::spawn(channel.port2(), broker.handler_context(Arc::new(Challenge)));
        let client = Client::open(channel.port1());
        let will = WillMessage::new("will/scram", "offline").with_qos(QoS::ExactlyOnce);
        client.send(&ConnectPacket::new(
            ConnectOptions::new("scram")
                .with_authentication_method("X")
                .with_will(will),
        ));
        assert_eq!(
            client.wait_for(1).await.first(),
            Some(&0xF0),
            "expected AUTH"
        );
        client.clear();
        client.send(
            &AuthPacket::continue_authentication("X".to_string(), Some(b"r".to_vec())).unwrap(),
        );
        let connack = client.wait_for(4).await;
        assert_eq!(connack.first(), Some(&0x20), "expected CONNACK");
        assert_eq!(
            connack[3],
            u8::from(ReasonCode::QoSNotSupported),
            "a Will QoS above the server maximum must be rejected after multi-step auth too"
        );
        assert!(!broker.router.is_connected("scram").await);
        assert!(broker.storage.get_session("scram").await.unwrap().is_none());
    }

    #[wasm_bindgen_test]
    async fn messages_routed_while_a_subscribe_waits_are_delivered() {
        let broker = broker();
        let client = connected(&broker, connect_options("busy", true, 0)).await;
        subscribed(&client, "open/x").await;
        let slot = broker.router.lock_session("busy").await;
        client.send_raw(&subscribe_bytes("other/y"));
        sleep(100).await;
        for index in 0u32..5 {
            route(&broker, "open/x", &index.to_be_bytes(), QoS::AtLeastOnce).await;
        }
        sleep(200).await;
        drop(slot);
        sleep(300).await;
        let frames = frames(&client.received());
        let publishes = frames.iter().filter(|(b, _)| b >> 4 == 3).count();
        let subacks = frames.iter().filter(|(b, _)| *b == 0x90).count();
        assert_eq!((publishes, subacks), (5, 1));
    }

    fn puback(packet_id: &[u8]) -> Vec<u8> {
        vec![0x40, 0x02, packet_id[0], packet_id[1]]
    }

    /// The packet identifier of a `QoS` 1 or 2 PUBLISH body, whose topic is at its start.
    fn publish_packet_id(body: &[u8]) -> Vec<u8> {
        let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
        body[2 + topic_len..4 + topic_len].to_vec()
    }

    async fn route_retained(broker: &WasmBroker, topic: &str, qos: QoS) {
        let mut publish =
            mqtt5_protocol::packet::publish::PublishPacket::new(topic, b"kept".to_vec(), qos);
        publish.retain = true;
        broker.router.route_message(&publish, None).await;
    }

    #[wasm_bindgen_test]
    async fn a_burst_over_the_lane_stays_in_order() {
        let broker = broker();
        let client = connected(&broker, connect_options("burst", true, 0)).await;
        client.send_raw(&subscribe_bytes_at("burst/x", 1));
        assert_eq!(client.wait_for(1).await.first(), Some(&0x90));
        client.clear();
        for index in 0u32..300 {
            route(&broker, "burst/x", &index.to_be_bytes(), QoS::AtLeastOnce).await;
        }
        for _ in 0..300 {
            if publish_frames(&client.received()).len() >= 300 {
                break;
            }
            sleep(10).await;
        }
        let received: Vec<u32> = publish_frames(&client.received())
            .iter()
            .map(|(_, body)| u32::from_be_bytes(body[body.len() - 4..].try_into().unwrap()))
            .collect();
        assert_eq!(received, (0u32..300).collect::<Vec<u32>>());
    }

    #[wasm_bindgen_test]
    async fn a_retained_message_at_qos_1_reaches_the_subscriber() {
        let broker = broker();
        route_retained(&broker, "kept/one", QoS::AtLeastOnce).await;
        let client = connected(&broker, connect_options("retained-1", true, 0)).await;
        client.send_raw(&subscribe_bytes_at("kept/one", 1));
        sleep(300).await;
        let frames = frames(&client.received());
        assert!(frames.iter().any(|(first_byte, _)| *first_byte == 0x90));
        let publish = frames
            .iter()
            .find(|(first_byte, _)| *first_byte == 0x33)
            .expect("a retained QoS 1 PUBLISH");
        assert_ne!(publish_packet_id(&publish.1), [0, 0]);
    }

    #[wasm_bindgen_test]
    async fn a_retained_message_is_capped_at_the_granted_qos() {
        let broker = broker();
        route_retained(&broker, "kept/two", QoS::ExactlyOnce).await;
        let client = connected(&broker, connect_options("retained-2", true, 0)).await;
        client.send_raw(&subscribe_bytes_at("kept/two", 0));
        sleep(300).await;
        let first_bytes: Vec<u8> = publish_frames(&client.received())
            .into_iter()
            .map(|(first_byte, _)| first_byte)
            .collect();
        assert_eq!(first_bytes, [0x31]);
    }

    async fn retained_on_a_second_subscribe(options: u8) -> usize {
        let broker = broker();
        route_retained(&broker, "kept/rh", QoS::AtMostOnce).await;
        let client = connected(&broker, connect_options("retained-rh", true, 0)).await;
        client.send_raw(&subscribe_bytes_at("kept/rh", options));
        sleep(200).await;
        assert_eq!(publish_frames(&client.received()).len(), 1);
        client.clear();
        client.send_raw(&subscribe_bytes_at("kept/rh", options));
        sleep(200).await;
        publish_frames(&client.received()).len()
    }

    #[wasm_bindgen_test]
    async fn retain_handling_1_skips_a_subscription_that_exists() {
        assert_eq!(retained_on_a_second_subscribe(0x10).await, 0);
    }

    #[wasm_bindgen_test]
    async fn control_retain_handling_0_resends_on_every_subscribe() {
        assert_eq!(retained_on_a_second_subscribe(0x00).await, 1);
    }

    async fn delivered_at_qos_1_then_resumed(acknowledge: bool) -> Vec<(u8, Vec<u8>)> {
        let broker = broker();
        let first = connected(&broker, connect_options("resend-1", true, 3600)).await;
        first.send_raw(&subscribe_bytes_at("resend/x", 1));
        assert_eq!(first.wait_for(1).await.first(), Some(&0x90));
        first.clear();
        for payload in [b"a", b"b", b"c"] {
            route(&broker, "resend/x", payload, QoS::AtLeastOnce).await;
        }
        sleep(200).await;
        let sent = publish_frames(&first.received());
        assert_eq!(sent.len(), 3);
        if acknowledge {
            for (_, body) in &sent {
                first.send_raw(&puback(&publish_packet_id(body)));
            }
            sleep(100).await;
        }
        first.port.close();
        sleep(200).await;
        let second = start_connect(&broker, connect_options("resend-1", false, 3600));
        sleep(400).await;
        publish_frames(&second.received())
    }

    #[wasm_bindgen_test]
    async fn an_unacknowledged_qos_1_delivery_is_resent_in_order_on_resume() {
        let resent = delivered_at_qos_1_then_resumed(false).await;
        let payloads: Vec<u8> = resent
            .iter()
            .map(|(_, body)| body[body.len() - 1])
            .collect();
        assert!(resent.iter().all(|(first_byte, _)| *first_byte == 0x3A));
        assert_eq!(payloads, *b"abc");
    }

    #[wasm_bindgen_test]
    async fn an_acknowledged_qos_1_delivery_is_not_resent() {
        assert!(delivered_at_qos_1_then_resumed(true).await.is_empty());
    }

    #[wasm_bindgen_test]
    async fn a_displaced_connection_is_told_and_its_next_packet_is_not_handled() {
        let broker = broker();
        let watcher = connected(&broker, connect_options("takeover-watch", true, 0)).await;
        subscribed(&watcher, "takeover/x").await;
        let first = connected(&broker, connect_options("takeover", true, 3600)).await;
        first.clear();
        let second = connected(&broker, connect_options("takeover", false, 3600)).await;
        sleep(200).await;
        let told = frames(&first.received());
        assert_eq!(
            told.first()
                .map(|(first_byte, body)| (*first_byte, body[0])),
            Some((0xE0, u8::from(ReasonCode::SessionTakenOver)))
        );
        first.clear();
        let publish = mqtt5_protocol::packet::publish::PublishPacket::new(
            "takeover/x",
            b"late".to_vec(),
            QoS::AtLeastOnce,
        )
        .with_packet_id(9);
        first.send(&publish);
        sleep(200).await;
        assert!(first.received().is_empty(), "no PUBACK");
        assert!(watcher.received().is_empty(), "not routed");
        drop(second);
    }

    #[wasm_bindgen_test]
    async fn nothing_is_sent_after_the_server_disconnect() {
        let broker = broker();
        let client = connected(&broker, connect_options("after-disconnect", true, 0)).await;
        subscribed(&client, "after/x").await;
        let slot = broker.router.lock_session("after-disconnect").await;
        client.send(
            &AuthPacket::continue_authentication("X".to_string(), Some(b"r".to_vec())).unwrap(),
        );
        assert_eq!(client.wait_for(1).await.first(), Some(&0xE0));
        client.clear();
        route(&broker, "after/x", b"late", QoS::AtLeastOnce).await;
        sleep(200).await;
        drop(slot);
        assert!(client.received().is_empty());
    }

    struct Reauth;

    impl AuthProvider for Reauth {
        fn authenticate<'a>(
            &'a self,
            _connect: &'a ConnectPacket,
            _client_addr: SocketAddr,
        ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
            Box::pin(async move { Ok(AuthResult::success()) })
        }

        fn authorize_publish<'a>(
            &'a self,
            _client_id: &str,
            _user_id: Option<&'a str>,
            _topic: &'a str,
        ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
            Box::pin(async move { true })
        }

        fn authorize_subscribe<'a>(
            &'a self,
            _client_id: &str,
            user_id: Option<&'a str>,
            topic_filter: &'a str,
        ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
            Box::pin(
                async move { user_id == Some("alice") || !topic_filter.starts_with("secret/") },
            )
        }

        fn checks_delivery(&self) -> bool {
            true
        }

        fn authorize_delivery<'a>(
            &'a self,
            _client_id: &str,
            user_id: Option<&'a str>,
            topic: &'a str,
        ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
            Box::pin(async move { user_id == Some("alice") || !topic.starts_with("open/private") })
        }

        fn supports_enhanced_auth(&self) -> bool {
            true
        }

        fn authenticate_enhanced<'a>(
            &'a self,
            auth_method: &'a str,
            auth_data: Option<&'a [u8]>,
            _client_id: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
            let method = auth_method.to_string();
            let user = auth_data.map(|data| String::from_utf8_lossy(data).to_string());
            Box::pin(async move {
                Ok(match user {
                    None => EnhancedAuthResult::continue_auth(method, Some(b"c".to_vec())),
                    Some(user) => EnhancedAuthResult::success_with_user(method, user),
                })
            })
        }

        fn reauthenticate<'a>(
            &'a self,
            auth_method: &'a str,
            auth_data: Option<&'a [u8]>,
            client_id: &'a str,
            _user_id: Option<&'a str>,
        ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
            self.authenticate_enhanced(auth_method, auth_data, client_id)
        }
    }

    async fn authenticated_as_alice(broker: &WasmBroker, client_id: &str) -> Client {
        let channel = web_sys::MessageChannel::new().unwrap();
        WasmClientHandler::spawn(channel.port2(), broker.handler_context(Arc::new(Reauth)));
        let client = Client::open(channel.port1());
        client.send(&ConnectPacket::new(
            ConnectOptions::new(client_id)
                .with_session_expiry_interval(3600)
                .with_authentication_method("X"),
        ));
        assert_eq!(client.wait_for(1).await.first(), Some(&0xF0));
        client.clear();
        client.send(
            &AuthPacket::continue_authentication("X".to_string(), Some(b"alice".to_vec())).unwrap(),
        );
        assert_eq!(client.wait_for(4).await.first(), Some(&0x20));
        client.clear();
        client
    }

    async fn reauthenticate_as(client: &Client, user: &[u8]) {
        let mut auth = AuthPacket::new(ReasonCode::ReAuthenticate);
        auth.properties.set_authentication_method("X".to_string());
        auth.properties
            .set_authentication_data(user.to_vec().into());
        client.send(&auth);
        assert_eq!(client.wait_for(1).await.first(), Some(&0xF0));
        client.clear();
    }

    async fn delivered_after_reauthenticating_as(
        user: &[u8],
        topic: &str,
    ) -> (usize, Option<String>) {
        let broker = broker();
        let client = authenticated_as_alice(&broker, "reauth").await;
        subscribed(&client, topic).await;
        reauthenticate_as(&client, user).await;
        route(&broker, topic, b"m", QoS::AtLeastOnce).await;
        sleep(200).await;
        let stored = broker.storage.get_session("reauth").await.unwrap().unwrap();
        (publish_frames(&client.received()).len(), stored.user_id)
    }

    #[wasm_bindgen_test]
    async fn reauthenticating_as_another_user_drops_what_that_user_cannot_have() {
        let (delivered, owner) = delivered_after_reauthenticating_as(b"bob", "secret/x").await;
        assert_eq!((delivered, owner.as_deref()), (0, Some("bob")));
    }

    #[wasm_bindgen_test]
    async fn delivery_is_checked_for_the_reauthenticated_user() {
        let (delivered, owner) = delivered_after_reauthenticating_as(b"bob", "open/private").await;
        assert_eq!((delivered, owner.as_deref()), (0, Some("bob")));
    }

    #[wasm_bindgen_test]
    async fn control_reauthenticating_as_the_same_user_keeps_its_subscriptions() {
        let (delivered, owner) = delivered_after_reauthenticating_as(b"alice", "secret/x").await;
        assert_eq!((delivered, owner.as_deref()), (1, Some("alice")));
    }

    fn encoded(packet: &impl MqttPacket) -> Vec<u8> {
        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();
        buf.to_vec()
    }

    fn last_bytes(frames: &[(u8, Vec<u8>)]) -> String {
        frames
            .iter()
            .map(|(_, body)| char::from(body[body.len() - 1]))
            .collect()
    }

    #[wasm_bindgen_test]
    async fn lane_contents_survive_the_end_of_the_connection() {
        let broker = broker();
        let first = connected(&broker, connect_options("lane-end", true, 3600)).await;
        first.send_raw(&subscribe_bytes_at("lane/x", 1));
        assert_eq!(first.wait_for(1).await.first(), Some(&0x90));
        first.clear();
        let mut chunk = Vec::new();
        for (index, payload) in [b"a", b"b", b"c"].iter().enumerate() {
            let publish = mqtt5_protocol::packet::publish::PublishPacket::new(
                "lane/x",
                payload.to_vec(),
                QoS::AtLeastOnce,
            )
            .with_packet_id(u16::try_from(index).unwrap() + 1);
            chunk.extend(encoded(&publish));
        }
        chunk.extend_from_slice(&[0xE0, 0x00]);
        first.send_raw(&chunk);
        sleep(200).await;
        let acked = frames(&first.received())
            .iter()
            .filter(|(first_byte, _)| first_byte >> 4 == 4)
            .count();
        let second = start_connect(&broker, connect_options("lane-end", false, 3600));
        sleep(300).await;
        let resumed = publish_frames(&second.received());
        assert_eq!((acked, last_bytes(&resumed).as_str()), (3, "abc"));
    }

    async fn retained_count_over_the_lane(qos: QoS, requested: u8) -> (usize, usize) {
        let broker = broker();
        for index in 0..150 {
            route_retained(&broker, &format!("over/{index}"), qos).await;
        }
        let client = connected(&broker, connect_options("retained-over", true, 0)).await;
        client.send_raw(&subscribe_bytes_at("over/#", requested));
        sleep(500).await;
        let all = publish_frames(&client.received());
        let retain_bits = all.iter().filter(|(b, _)| b & 1 == 1).count();
        (all.len(), retain_bits)
    }

    #[wasm_bindgen_test]
    async fn all_retained_qos_0_arrive() {
        assert_eq!(
            retained_count_over_the_lane(QoS::AtMostOnce, 0).await,
            (150, 150)
        );
    }

    #[wasm_bindgen_test]
    async fn all_retained_qos_1_arrive() {
        assert_eq!(
            retained_count_over_the_lane(QoS::AtLeastOnce, 1).await,
            (150, 150)
        );
    }

    #[wasm_bindgen_test]
    async fn retained_qos_1_capped_to_0_all_arrive() {
        assert_eq!(
            retained_count_over_the_lane(QoS::AtLeastOnce, 0).await,
            (150, 150)
        );
    }

    #[wasm_bindgen_test]
    async fn resend_keeps_send_order_when_identifiers_are_reused() {
        while js_sys::Date::now() % 1000.0 > 100.0 {
            sleep(5).await;
        }
        let broker = broker();
        let first = connected(&broker, connect_options("reuse", true, 3600)).await;
        first.send_raw(&subscribe_bytes_at("reuse/x", 1));
        assert_eq!(first.wait_for(1).await.first(), Some(&0x90));
        first.clear();
        for payload in [b"a", b"b", b"c"] {
            route(&broker, "reuse/x", payload, QoS::AtLeastOnce).await;
        }
        sleep(10).await;
        let sent = publish_frames(&first.received());
        for (_, body) in &sent[..2] {
            first.send_raw(&puback(&publish_packet_id(body)));
        }
        sleep(10).await;
        let second = start_connect(&broker, connect_options("reuse", false, 3600));
        sleep(20).await;
        for payload in [b"d", b"e", b"f"] {
            route(&broker, "reuse/x", payload, QoS::AtLeastOnce).await;
        }
        sleep(10).await;
        let on_second = publish_frames(&second.received());
        let third = start_connect(&broker, connect_options("reuse", false, 3600));
        sleep(20).await;
        let elapsed_ms = js_sys::Date::now() % 1000.0;
        let resent = publish_frames(&third.received());
        assert_eq!(
            (
                last_bytes(&on_second).as_str(),
                last_bytes(&resent).as_str()
            ),
            ("cdef", "cdef"),
            "ms into the second at the end: {elapsed_ms}"
        );
    }

    struct Gated(Arc<tokio::sync::Semaphore>);

    impl AuthProvider for Gated {
        fn authenticate<'a>(
            &'a self,
            _connect: &'a ConnectPacket,
            _client_addr: SocketAddr,
        ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
            Box::pin(async move { Ok(AuthResult::success()) })
        }
        fn authorize_publish<'a>(
            &'a self,
            _client_id: &str,
            _user_id: Option<&'a str>,
            _topic: &'a str,
        ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
            Box::pin(async move { true })
        }
        fn authorize_subscribe<'a>(
            &'a self,
            _client_id: &str,
            _user_id: Option<&'a str>,
            _topic_filter: &'a str,
        ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
            Box::pin(async move { true })
        }
        fn checks_delivery(&self) -> bool {
            true
        }
        fn authorize_delivery<'a>(
            &'a self,
            _client_id: &str,
            _user_id: Option<&'a str>,
            _topic: &'a str,
        ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
            let gate = Arc::clone(&self.0);
            Box::pin(async move {
                gate.acquire()
                    .await
                    .map(tokio::sync::SemaphorePermit::forget)
                    .is_ok()
            })
        }
    }

    fn open_with(broker: &WasmBroker, provider: Arc<dyn AuthProvider>) -> Client {
        let channel = web_sys::MessageChannel::new().unwrap();
        WasmClientHandler::spawn(channel.port2(), broker.handler_context(provider));
        Client::open(channel.port1())
    }

    #[wasm_bindgen_test]
    async fn takeover_with_a_slow_delivery_check_loses_nothing() {
        let broker = broker();
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let provider: Arc<dyn AuthProvider> = Arc::new(Gated(Arc::clone(&gate)));
        let first = open_with(&broker, Arc::clone(&provider));
        first.send(&ConnectPacket::new(connect_options("slow", true, 3600)));
        assert_eq!(first.wait_for(4).await.first(), Some(&0x20));
        first.clear();
        first.send_raw(&subscribe_bytes_at("slow/x", 1));
        assert_eq!(first.wait_for(1).await.first(), Some(&0x90));
        for payload in [b"a", b"b", b"c"] {
            route(&broker, "slow/x", payload, QoS::AtLeastOnce).await;
        }
        sleep(50).await;
        let second = open_with(&broker, provider);
        second.send(&ConnectPacket::new(connect_options("slow", false, 3600)));
        assert_eq!(second.wait_for(4).await.first(), Some(&0x20));
        second.clear();
        sleep(50).await;
        gate.add_permits(1000);
        sleep(200).await;
        let before = last_bytes(&publish_frames(&second.received()));
        route(&broker, "slow/x", b"d", QoS::AtLeastOnce).await;
        sleep(200).await;
        let after = last_bytes(&publish_frames(&second.received()));
        assert_eq!((before.as_str(), after.as_str()), ("abc", "abcd"));
    }

    #[wasm_bindgen_test]
    async fn two_step_reauthentication_completes() {
        let broker = broker();
        let client = authenticated_as_alice(&broker, "reauth-2").await;
        let mut auth = AuthPacket::new(ReasonCode::ReAuthenticate);
        auth.properties.set_authentication_method("X".to_string());
        client.send(&auth);
        assert_eq!(client.wait_for(1).await.first(), Some(&0xF0));
        client.clear();
        client.send(
            &AuthPacket::continue_authentication("X".to_string(), Some(b"bob".to_vec())).unwrap(),
        );
        sleep(100).await;
        let reply = frames(&client.received());
        assert_eq!(reply[0].0, 0xF0, "reply: {reply:?}");
    }

    #[wasm_bindgen_test]
    async fn packet_identifiers_stay_unique_for_a_client_that_never_acknowledges() {
        let broker = broker();
        let client = connected(&broker, connect_options("no-ack", true, 0)).await;
        client.send_raw(&subscribe_bytes_at("na/x", 1));
        assert_eq!(client.wait_for(1).await.first(), Some(&0x90));
        client.clear();
        for index in 0u32..65_600 {
            route(&broker, "na/x", b"m", QoS::AtLeastOnce).await;
            if index % 50 == 0 {
                sleep(0).await;
            }
        }
        sleep(500).await;
        let all = publish_frames(&client.received());
        let mut ids: Vec<Vec<u8>> = all.iter().map(|(_, b)| publish_packet_id(b)).collect();
        let total = ids.len();
        ids.sort();
        ids.dedup();
        let stored = broker
            .storage
            .get_inflight_messages("no-ack")
            .await
            .unwrap()
            .len();
        assert_eq!((total, ids.len(), stored), (65_535, 65_535, 65_535));
    }

    #[wasm_bindgen_test]
    async fn receive_maximum_is_honoured() {
        let broker = broker();
        let client = connected(
            &broker,
            connect_options("recv-max", true, 0).with_receive_maximum(2),
        )
        .await;
        client.send_raw(&subscribe_bytes_at("rm/x", 1));
        assert_eq!(client.wait_for(1).await.first(), Some(&0x90));
        client.clear();
        for payload in [b"a", b"b", b"c", b"d", b"e"] {
            route(&broker, "rm/x", payload, QoS::AtLeastOnce).await;
        }
        sleep(200).await;
        assert_eq!(publish_frames(&client.received()).len(), 2);
    }

    #[wasm_bindgen_test]
    async fn maximum_packet_size_is_honoured() {
        let broker = broker();
        let mut options = connect_options("max-size", true, 0);
        options.properties.maximum_packet_size = Some(20);
        let client = connected(&broker, options).await;
        client.send_raw(&subscribe_bytes_at("ms/x", 0));
        assert_eq!(client.wait_for(1).await.first(), Some(&0x90));
        client.clear();
        route(&broker, "ms/x", &[b'z'; 60], QoS::AtMostOnce).await;
        sleep(200).await;
        assert_eq!(publish_frames(&client.received()).len(), 0);
    }

    #[wasm_bindgen_test]
    async fn reauthentication_with_another_method_is_refused() {
        let broker = broker();
        let client = authenticated_as_alice(&broker, "reauth-m").await;
        let mut auth = AuthPacket::new(ReasonCode::ReAuthenticate);
        auth.properties.set_authentication_method("Y".to_string());
        auth.properties
            .set_authentication_data(b"alice".to_vec().into());
        client.send(&auth);
        sleep(100).await;
        let reply = frames(&client.received());
        assert_eq!(reply[0].0, 0xE0, "reply: {reply:?}");
    }

    #[wasm_bindgen_test]
    async fn auth_on_a_connection_without_a_method_is_refused() {
        let broker = broker();
        let client = open_with(&broker, Arc::new(Reauth));
        client.send(&ConnectPacket::new(connect_options("reauth-n", true, 60)));
        assert_eq!(client.wait_for(4).await.first(), Some(&0x20));
        client.clear();
        let mut auth = AuthPacket::new(ReasonCode::ReAuthenticate);
        auth.properties.set_authentication_method("X".to_string());
        auth.properties
            .set_authentication_data(b"alice".to_vec().into());
        client.send(&auth);
        sleep(100).await;
        let reply = frames(&client.received());
        assert_eq!(reply[0].0, 0xE0, "reply: {reply:?}");
    }

    #[wasm_bindgen_test]
    async fn pubrec_with_an_error_gets_no_pubrel() {
        let broker = broker();
        let client = connected(&broker, connect_options("pubrec-err", true, 60)).await;
        client.send_raw(&subscribe_bytes_at("pe/x", 2));
        assert_eq!(client.wait_for(1).await.first(), Some(&0x90));
        client.clear();
        route(&broker, "pe/x", b"m", QoS::ExactlyOnce).await;
        sleep(100).await;
        let sent = publish_frames(&client.received());
        let id = publish_packet_id(&sent[0].1);
        client.clear();
        client.send_raw(&[0x50, 0x03, id[0], id[1], 0x80]);
        sleep(100).await;
        assert!(client.received().is_empty(), "got {:?}", client.received());
    }

    #[wasm_bindgen_test]
    async fn retain_handling_1_after_resume_counts_as_existing() {
        let broker = broker();
        route_retained(&broker, "kept/rs", QoS::AtMostOnce).await;
        let first = connected(&broker, connect_options("rh-resume", true, 3600)).await;
        first.send_raw(&subscribe_bytes_at("kept/rs", 0x10));
        sleep(100).await;
        assert_eq!(publish_frames(&first.received()).len(), 1);
        let second = start_connect(&broker, connect_options("rh-resume", false, 3600));
        assert_eq!(second.wait_for(4).await.first(), Some(&0x20));
        second.clear();
        second.send_raw(&subscribe_bytes_at("kept/rs", 0x10));
        sleep(100).await;
        assert_eq!(publish_frames(&second.received()).len(), 0);
    }

    fn v4_connect(client_id: &str) -> Vec<u8> {
        let id = client_id.as_bytes();
        let mut bytes = vec![0x10, u8::try_from(12 + id.len()).unwrap()];
        bytes.extend_from_slice(&[0, 4, b'M', b'Q', b'T', b'T', 4, 0x00, 0, 0]);
        bytes.extend_from_slice(&[0, u8::try_from(id.len()).unwrap()]);
        bytes.extend_from_slice(id);
        bytes
    }

    #[wasm_bindgen_test]
    async fn v311_takeover_stops_without_disconnect_and_resends() {
        let broker = broker();
        let watcher = connected(&broker, connect_options("v4-watch", true, 0)).await;
        subscribed(&watcher, "v4/w").await;
        let first = Client::open(broker.create_client_port().unwrap());
        first.send_raw(&v4_connect("v4-take"));
        assert_eq!(first.wait_for(4).await.first(), Some(&0x20));
        first.clear();
        first.send_raw(&[0x82, 0x09, 0, 1, 0, 4, b'v', b'4', b'/', b'x', 1]);
        assert_eq!(first.wait_for(1).await.first(), Some(&0x90));
        first.clear();
        route(&broker, "v4/x", b"m", QoS::AtLeastOnce).await;
        sleep(100).await;
        assert_eq!(publish_frames(&first.received()).len(), 1);
        first.clear();
        let second = Client::open(broker.create_client_port().unwrap());
        second.send_raw(&v4_connect("v4-take"));
        sleep(200).await;
        assert!(
            first.received().is_empty(),
            "v3.1.1 got {:?}",
            first.received()
        );
        let resent = publish_frames(&second.received());
        assert_eq!(resent.len(), 1);
        assert_eq!(resent[0].0, 0x3A);
        first.send_raw(&[0x32, 0x08, 0, 4, b'v', b'4', b'/', b'w', 0, 9, b'l']);
        sleep(100).await;
        assert!(first.received().is_empty());
        assert!(watcher.received().is_empty());
    }

    #[wasm_bindgen_test]
    async fn client_publish_to_another_subscriber_no_chunk() {
        let broker = broker();
        let sub = connected(&broker, connect_options("ct20-sub", true, 0)).await;
        subscribed(&sub, "ct20/x").await;
        let publisher = connected(&broker, connect_options("ct20-pub", true, 0)).await;
        let publish = mqtt5_protocol::packet::publish::PublishPacket::new(
            "ct20/x",
            b"m".to_vec(),
            QoS::AtLeastOnce,
        )
        .with_packet_id(1);
        publisher.send(&publish);
        sleep(100).await;
        assert_eq!(publish_frames(&sub.received()).len(), 1);
    }

    #[wasm_bindgen_test]
    async fn client_publish_to_itself_no_chunk() {
        let broker = broker();
        let sub = connected(&broker, connect_options("ct21", true, 0)).await;
        sub.send_raw(&subscribe_bytes_at("ct21/x", 1));
        assert_eq!(sub.wait_for(1).await.first(), Some(&0x90));
        sub.clear();
        let publish = mqtt5_protocol::packet::publish::PublishPacket::new(
            "ct21/x",
            b"m".to_vec(),
            QoS::AtLeastOnce,
        )
        .with_packet_id(1);
        sub.send(&publish);
        sleep(100).await;
        assert_eq!(publish_frames(&sub.received()).len(), 1);
    }

    #[wasm_bindgen_test]
    async fn takeover_keeps_order_with_messages_routed_during_the_handoff() {
        let broker = broker();
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let first = open_with(&broker, Arc::new(Gated(Arc::clone(&gate))));
        first.send(&ConnectPacket::new(connect_options("handoff", true, 3600)));
        assert_eq!(first.wait_for(4).await.first(), Some(&0x20));
        first.clear();
        first.send_raw(&subscribe_bytes_at("handoff/x", 1));
        assert_eq!(first.wait_for(1).await.first(), Some(&0x90));
        for payload in [b"a", b"b", b"c"] {
            route(&broker, "handoff/x", payload, QoS::AtLeastOnce).await;
        }
        sleep(50).await;
        let second = connected(&broker, connect_options("handoff", false, 3600)).await;
        route(&broker, "handoff/x", b"d", QoS::AtLeastOnce).await;
        sleep(100).await;
        gate.add_permits(1000);
        sleep(300).await;
        assert_eq!(last_bytes(&publish_frames(&second.received())), "abcd");
    }

    async fn unacknowledged_then_resumed_with(
        client_id: &str,
        payload: &[u8],
        count: usize,
        options: ConnectOptions,
    ) -> Vec<(u8, Vec<u8>)> {
        let broker = broker();
        let first = connected(&broker, connect_options(client_id, true, 3600)).await;
        first.send_raw(&subscribe_bytes_at("rs/x", 1));
        assert_eq!(first.wait_for(1).await.first(), Some(&0x90));
        first.clear();
        for _ in 0..count {
            route(&broker, "rs/x", payload, QoS::AtLeastOnce).await;
        }
        sleep(200).await;
        assert_eq!(publish_frames(&first.received()).len(), count);
        first.port.close();
        sleep(200).await;
        let second = start_connect(&broker, options);
        sleep(300).await;
        publish_frames(&second.received())
    }

    #[wasm_bindgen_test]
    async fn resume_resend_honours_receive_maximum() {
        let resent = unacknowledged_then_resumed_with(
            "rs-rm",
            b"m",
            5,
            connect_options("rs-rm", false, 3600).with_receive_maximum(2),
        )
        .await;
        assert!(
            resent.len() <= 2,
            "resent {} with Receive Maximum 2",
            resent.len()
        );
    }

    #[wasm_bindgen_test]
    async fn resume_resend_honours_maximum_packet_size() {
        let mut options = connect_options("rs-mps", false, 3600);
        options.properties.maximum_packet_size = Some(20);
        let resent = unacknowledged_then_resumed_with("rs-mps", &[b'z'; 60], 1, options).await;
        let oversize: Vec<usize> = resent.iter().map(|(_, body)| body.len() + 2).collect();
        assert!(
            oversize.is_empty(),
            "sent packets of {oversize:?} bytes over a 20 byte limit"
        );
    }

    #[wasm_bindgen_test]
    async fn receive_maximum_zero_is_refused_or_delivers() {
        let broker = broker();
        let client = start_connect(
            &broker,
            connect_options("rm-zero", true, 0).with_receive_maximum(0),
        );
        let connack = client.wait_for(4).await;
        let accepted = connack.first() == Some(&0x20) && connack[3] == 0x00;
        client.clear();
        let mut delivered = 0;
        if accepted {
            client.send_raw(&subscribe_bytes_at("rz/x", 1));
            assert_eq!(client.wait_for(1).await.first(), Some(&0x90));
            client.clear();
            route(&broker, "rz/x", b"m", QoS::AtLeastOnce).await;
            sleep(200).await;
            delivered = publish_frames(&client.received()).len();
        }
        assert!(
            !accepted || delivered == 1,
            "accepted Receive Maximum 0 and delivered {delivered} of 1"
        );
    }

    #[wasm_bindgen_test]
    async fn an_unexpected_auth_before_connack_gets_a_connack() {
        let broker = broker();
        let client = open_with(&broker, Arc::new(Reauth));
        client.send(&ConnectPacket::new(
            ConnectOptions::new("pre-connack").with_authentication_method("X"),
        ));
        assert_eq!(client.wait_for(1).await.first(), Some(&0xF0));
        client.clear();
        let mut auth = AuthPacket::new(ReasonCode::ReAuthenticate);
        auth.properties.set_authentication_method("X".to_string());
        auth.properties
            .set_authentication_data(b"alice".to_vec().into());
        client.send(&auth);
        sleep(100).await;
        let reply = frames(&client.received());
        let connack = reply
            .first()
            .map(|(first_byte, body)| (*first_byte, body.get(..2).map(<[u8]>::to_vec)));
        assert_eq!(connack, Some((0x20, Some(vec![0x00, 0x82]))), "{reply:?}");
    }

    #[wasm_bindgen_test]
    async fn another_auth_method_before_connack_gets_a_connack() {
        let broker = broker();
        let client = open_with(&broker, Arc::new(Reauth));
        client.send(&ConnectPacket::new(
            ConnectOptions::new("pre-connack-m").with_authentication_method("X"),
        ));
        assert_eq!(client.wait_for(1).await.first(), Some(&0xF0));
        client.clear();
        client.send(
            &AuthPacket::continue_authentication("Y".to_string(), Some(b"alice".to_vec())).unwrap(),
        );
        sleep(100).await;
        let reply = frames(&client.received());
        let connack = reply
            .first()
            .map(|(first_byte, body)| (*first_byte, body.get(..2).map(<[u8]>::to_vec)));
        assert_eq!(connack, Some((0x20, Some(vec![0x00, 0x8C]))), "{reply:?}");
    }

    #[wasm_bindgen_test]
    async fn resend_keeps_send_order_across_packet_identifier_wrap() {
        use mqtt5::broker::storage::{InflightDirection, InflightMessage, InflightPhase};
        while js_sys::Date::now() % 1000.0 > 100.0 {
            sleep(5).await;
        }
        let broker = broker();
        let first = connected(&broker, connect_options("wrap", true, 3600)).await;
        first.send_raw(&subscribe_bytes_at("wrap/x", 1));
        assert_eq!(first.wait_for(1).await.first(), Some(&0x90));
        first.port.close();
        sleep(20).await;
        let earlier = mqtt5_protocol::packet::publish::PublishPacket::new(
            "wrap/x",
            b"a".to_vec(),
            QoS::AtLeastOnce,
        )
        .with_packet_id(u16::MAX);
        broker
            .storage
            .store_inflight_message(InflightMessage::from_publish(
                &earlier,
                "wrap".to_string(),
                InflightDirection::Outbound,
                InflightPhase::AwaitingPubrec,
            ))
            .await
            .unwrap();
        let second = start_connect(&broker, connect_options("wrap", false, 3600));
        sleep(30).await;
        route(&broker, "wrap/x", b"b", QoS::AtLeastOnce).await;
        sleep(30).await;
        let on_second = last_bytes(&publish_frames(&second.received()));
        second.port.close();
        sleep(20).await;
        let third = start_connect(&broker, connect_options("wrap", false, 3600));
        sleep(30).await;
        let elapsed_ms = js_sys::Date::now() % 1000.0;
        let resent = last_bytes(&publish_frames(&third.received()));
        assert_eq!(
            (on_second.as_str(), resent.as_str()),
            ("ab", "ab"),
            "ms into the second at the end: {elapsed_ms}"
        );
    }

    #[wasm_bindgen_test]
    async fn window_reopening_delivers_a_long_burst_in_order() {
        let broker = broker();
        let client = connected(
            &broker,
            connect_options("rm-burst", true, 0).with_receive_maximum(1),
        )
        .await;
        client.send_raw(&subscribe_bytes_at("rb/x", 1));
        assert_eq!(client.wait_for(1).await.first(), Some(&0x90));
        client.clear();
        for index in 0u32..300 {
            route(&broker, "rb/x", &index.to_be_bytes(), QoS::AtLeastOnce).await;
        }
        let mut received = Vec::new();
        for _ in 0..600 {
            let batch = publish_frames(&client.received());
            client.clear();
            for (_, body) in &batch {
                received.push(u32::from_be_bytes(
                    body[body.len() - 4..].try_into().unwrap(),
                ));
                client.send_raw(&puback(&publish_packet_id(body)));
            }
            if received.len() >= 300 {
                break;
            }
            sleep(2).await;
        }
        assert_eq!(received, (0u32..300).collect::<Vec<u32>>());
    }

    #[wasm_bindgen_test]
    async fn qos_0_flows_while_the_window_is_closed() {
        let broker = broker();
        let client = connected(
            &broker,
            connect_options("rm-q0", true, 0).with_receive_maximum(1),
        )
        .await;
        client.send_raw(&subscribe_bytes_at("rq/#", 1));
        assert_eq!(client.wait_for(1).await.first(), Some(&0x90));
        client.clear();
        route(&broker, "rq/one", b"1", QoS::AtLeastOnce).await;
        route(&broker, "rq/one", b"2", QoS::AtLeastOnce).await;
        route(&broker, "rq/zero", b"z", QoS::AtMostOnce).await;
        sleep(200).await;
        let first_bytes: Vec<u8> = publish_frames(&client.received())
            .iter()
            .map(|(first_byte, _)| *first_byte)
            .collect();
        assert_eq!(first_bytes, [0x32, 0x30]);
    }

    async fn subscribed_at(client: &Client, topic: &str, qos: u8) {
        client.send_raw(&subscribe_bytes_at(topic, qos));
        assert_eq!(
            client.wait_for(1).await.first(),
            Some(&0x90),
            "expected SUBACK"
        );
        client.clear();
    }

    async fn gated_connection(
        broker: &WasmBroker,
        client_id: &str,
        gate: &Arc<tokio::sync::Semaphore>,
    ) -> Client {
        let client = open_with(broker, Arc::new(Gated(Arc::clone(gate))));
        client.send(&ConnectPacket::new(connect_options(client_id, true, 3600)));
        assert_eq!(client.wait_for(4).await.first(), Some(&0x20));
        client.clear();
        client
    }

    async fn stalled_after_two_of_three(
        broker: &WasmBroker,
        client_id: &str,
        topic: &str,
        qos: QoS,
        gate: &Arc<tokio::sync::Semaphore>,
    ) -> Client {
        let first = gated_connection(broker, client_id, gate).await;
        subscribed_at(&first, topic, qos as u8).await;
        for payload in [b"a", b"b", b"c"] {
            route(broker, topic, payload, qos).await;
        }
        sleep(100).await;
        assert_eq!(last_bytes(&publish_frames(&first.received())), "ab");
        first
    }

    #[wasm_bindgen_test]
    async fn a_handoff_that_times_out_delivers_each_message_once() {
        let broker = broker();
        let gate = Arc::new(tokio::sync::Semaphore::new(2));
        let first =
            stalled_after_two_of_three(&broker, "waive", "waive/x", QoS::AtLeastOnce, &gate).await;
        let second = connected(&broker, connect_options("waive", false, 3600)).await;
        sleep(31_000).await;
        route(&broker, "waive/x", b"d", QoS::AtLeastOnce).await;
        sleep(100).await;
        gate.add_permits(100);
        sleep(300).await;
        let delivered = deliveries(&second.received());
        drop(first);
        assert_eq!(
            delivered,
            vec![
                ('d', false, vec![0, 3]),
                ('a', true, vec![0, 1]),
                ('b', true, vec![0, 2]),
                ('c', false, vec![0, 4])
            ]
        );
    }

    #[wasm_bindgen_test]
    async fn a_handoff_that_times_out_completes_exactly_once_delivery_once() {
        let broker = broker();
        let gate = Arc::new(tokio::sync::Semaphore::new(1));
        let first = gated_connection(&broker, "waive2", &gate).await;
        subscribed_at(&first, "waive2/x", 2).await;
        route(&broker, "waive2/x", b"a", QoS::ExactlyOnce).await;
        route(&broker, "waive2/x", b"b", QoS::ExactlyOnce).await;
        sleep(100).await;
        assert_eq!(last_bytes(&publish_frames(&first.received())), "a");
        let second = connected(&broker, connect_options("waive2", false, 3600)).await;
        sleep(31_000).await;
        let before_release = deliveries(&second.received());
        gate.add_permits(100);
        sleep(300).await;
        let after_release = deliveries(&second.received());
        drop(first);
        assert_eq!(
            (before_release, after_release),
            (
                Vec::new(),
                vec![('a', true, vec![0, 1]), ('b', false, vec![0, 2])]
            )
        );
    }

    #[wasm_bindgen_test]
    async fn a_clean_start_takeover_of_a_stalled_connection_gets_none_of_its_messages() {
        let broker = broker();
        let gate = Arc::new(tokio::sync::Semaphore::new(2));
        let first =
            stalled_after_two_of_three(&broker, "clean", "clean/x", QoS::AtLeastOnce, &gate).await;
        let second = connected(&broker, connect_options("clean", true, 3600)).await;
        subscribed_at(&second, "clean/x", 1).await;
        sleep(100).await;
        gate.add_permits(100);
        sleep(300).await;
        route(&broker, "clean/x", b"d", QoS::AtLeastOnce).await;
        sleep(200).await;
        let on_second = last_bytes(&publish_frames(&second.received()));
        second.port.close();
        sleep(200).await;
        let third = start_connect(&broker, connect_options("clean", false, 3600));
        sleep(300).await;
        let on_third = last_bytes(&publish_frames(&third.received()));
        drop(first);
        assert_eq!((on_second.as_str(), on_third.as_str()), ("d", "d"));
    }

    #[wasm_bindgen_test]
    async fn a_takeover_during_bind_hands_the_session_on_in_order() {
        let broker = broker();
        let gate = Arc::new(tokio::sync::Semaphore::new(2));
        let first =
            stalled_after_two_of_three(&broker, "chain", "chain/x", QoS::AtLeastOnce, &gate).await;
        let second = connected(&broker, connect_options("chain", false, 3600)).await;
        sleep(100).await;
        let third = connected(&broker, connect_options("chain", false, 3600)).await;
        sleep(100).await;
        let told = frames(&second.received());
        route(&broker, "chain/x", b"d", QoS::AtLeastOnce).await;
        sleep(100).await;
        gate.add_permits(100);
        sleep(300).await;
        let on_third = last_bytes(&publish_frames(&third.received()));
        drop(first);
        assert_eq!(
            (
                told.first()
                    .map(|(first_byte, body)| (*first_byte, body[0])),
                on_third.as_str()
            ),
            (Some((0xE0, 0x8E)), "abcd")
        );
    }

    #[wasm_bindgen_test]
    async fn exactly_once_state_survives_a_resume() {
        let broker = broker();
        let first = connected(&broker, connect_options("qos2-resume", true, 3600)).await;
        subscribed_at(&first, "q2/x", 2).await;
        route(&broker, "q2/x", b"r", QoS::ExactlyOnce).await;
        route(&broker, "q2/x", b"u", QoS::ExactlyOnce).await;
        sleep(100).await;
        let sent = publish_frames(&first.received());
        assert_eq!(last_bytes(&sent), "ru");
        let released = publish_packet_id(&sent[0].1);
        let unreceived = publish_packet_id(&sent[1].1);
        first.send_raw(&[0x50, 0x02, released[0], released[1]]);
        sleep(100).await;
        first.port.close();
        sleep(200).await;
        let second = start_connect(&broker, connect_options("qos2-resume", false, 3600));
        sleep(300).await;
        let pubrels: Vec<Vec<u8>> = frames(&second.received())
            .into_iter()
            .filter(|(first_byte, _)| *first_byte == 0x62)
            .map(|(_, body)| body)
            .collect();
        let resent: Vec<(u8, Vec<u8>)> = publish_frames(&second.received())
            .iter()
            .map(|(first_byte, body)| (*first_byte, publish_packet_id(body)))
            .collect();
        assert_eq!(
            (pubrels, resent),
            (vec![released], vec![(0x3C, unreceived)])
        );
    }

    #[wasm_bindgen_test]
    async fn a_keep_alive_timeout_hands_back_unacknowledged_messages() {
        let broker = broker();
        let first = connected(
            &broker,
            connect_options("ka", true, 3600).with_keep_alive(std::time::Duration::from_secs(1)),
        )
        .await;
        subscribed_at(&first, "ka/x", 1).await;
        route(&broker, "ka/x", b"k", QoS::AtLeastOnce).await;
        sleep(2_500).await;
        let second = start_connect(&broker, connect_options("ka", false, 3600));
        sleep(300).await;
        let resent = publish_frames(&second.received());
        assert_eq!(
            (
                resent
                    .iter()
                    .map(|(first_byte, _)| *first_byte)
                    .collect::<Vec<u8>>(),
                last_bytes(&resent)
            ),
            (vec![0x3A], "k".to_string())
        );
    }

    #[wasm_bindgen_test]
    async fn receive_maximum_counts_a_restored_pubrel() {
        let broker = broker();
        let first = connected(&broker, connect_options("rm-rel", true, 3600)).await;
        subscribed_at(&first, "rr/x", 2).await;
        route(&broker, "rr/x", b"r", QoS::ExactlyOnce).await;
        route(&broker, "rr/x", b"q", QoS::AtLeastOnce).await;
        sleep(100).await;
        let sent = publish_frames(&first.received());
        let released = publish_packet_id(&sent[0].1);
        first.send_raw(&[0x50, 0x02, released[0], released[1]]);
        sleep(100).await;
        first.port.close();
        sleep(200).await;
        let second = start_connect(
            &broker,
            connect_options("rm-rel", false, 3600).with_receive_maximum(1),
        );
        sleep(300).await;
        let before = publish_frames(&second.received()).len();
        second.send_raw(&[0x70, 0x02, released[0], released[1]]);
        sleep(200).await;
        let after = last_bytes(&publish_frames(&second.received()));
        assert_eq!((before, after.as_str()), (0, "q"));
    }

    #[wasm_bindgen_test]
    async fn a_second_connect_is_a_protocol_error() {
        let broker = broker();
        let client = connected(&broker, connect_options("twice", true, 0)).await;
        client.send(&ConnectPacket::new(connect_options("twice", true, 0)));
        sleep(200).await;
        let reply = frames(&client.received());
        assert_eq!(
            reply
                .first()
                .map(|(first_byte, body)| (*first_byte, body.first().copied())),
            Some((0xE0, Some(0x82))),
            "{reply:?}"
        );
    }

    fn deliveries(bytes: &[u8]) -> Vec<(char, bool, Vec<u8>)> {
        publish_frames(bytes)
            .iter()
            .map(|(first_byte, body)| {
                (
                    char::from(body[body.len() - 1]),
                    first_byte & 0x08 != 0,
                    publish_packet_id(body),
                )
            })
            .collect()
    }

    async fn a_successor_that_timed_out_then_taken_over_resends_in_order(qos: QoS) {
        let broker = broker();
        let gate = Arc::new(tokio::sync::Semaphore::new(2));
        let first = stalled_after_two_of_three(&broker, "chain-late", "late/x", qos, &gate).await;
        let second = connected(&broker, connect_options("chain-late", false, 3600)).await;
        sleep(31_000).await;
        let on_second = deliveries(&second.received());
        let third = connected(&broker, connect_options("chain-late", false, 3600)).await;
        sleep(200).await;
        gate.add_permits(100);
        sleep(400).await;
        let on_third = deliveries(&third.received());
        drop(first);
        assert_eq!(
            (on_second, on_third),
            (
                Vec::new(),
                vec![
                    ('a', true, vec![0, 1]),
                    ('b', true, vec![0, 2]),
                    ('c', false, vec![0, 3])
                ]
            )
        );
    }

    #[wasm_bindgen_test]
    async fn a_successor_that_timed_out_then_taken_over_resends_qos_1_in_order() {
        a_successor_that_timed_out_then_taken_over_resends_in_order(QoS::AtLeastOnce).await;
    }

    #[wasm_bindgen_test]
    async fn a_successor_that_timed_out_then_taken_over_resends_qos_2_in_order() {
        a_successor_that_timed_out_then_taken_over_resends_in_order(QoS::ExactlyOnce).await;
    }

    #[wasm_bindgen_test]
    async fn a_successor_that_ended_during_the_handoff_leaves_the_session_in_order() {
        let broker = broker();
        let gate = Arc::new(tokio::sync::Semaphore::new(2));
        let first =
            stalled_after_two_of_three(&broker, "ended", "ended/x", QoS::ExactlyOnce, &gate).await;
        let first_ids: Vec<Vec<u8>> = publish_frames(&first.received())
            .iter()
            .map(|(_, body)| publish_packet_id(body))
            .collect();
        let second = connected(&broker, connect_options("ended", false, 3600)).await;
        sleep(100).await;
        second.port.close();
        sleep(300).await;
        gate.add_permits(100);
        sleep(300).await;
        let mut stored: Vec<u16> = broker
            .storage
            .get_inflight_messages("ended")
            .await
            .unwrap()
            .iter()
            .map(|message| message.packet_id)
            .collect();
        stored.sort_unstable();
        let third = start_connect(&broker, connect_options("ended", false, 3600));
        sleep(300).await;
        let on_third = deliveries(&third.received());
        drop(first);
        assert_eq!(
            (stored, on_third),
            (
                vec![1, 2],
                vec![
                    ('a', true, first_ids[0].clone()),
                    ('b', true, first_ids[1].clone()),
                    ('c', false, vec![0, 3])
                ]
            )
        );
    }

    async fn a_late_hand_back_ahead_of_a_resend_leaves_it_its_packet_id(qos: QoS) {
        let broker = broker();
        let gate = Arc::new(tokio::sync::Semaphore::new(2));
        let first = stalled_after_two_of_three(&broker, "ahead", "ahead/x", qos, &gate).await;
        let second = connected(&broker, connect_options("ahead", false, 3600)).await;
        sleep(31_000).await;
        route(&broker, "ahead/x", b"d", qos).await;
        sleep(200).await;
        let on_second = deliveries(&second.received());
        second.port.close();
        sleep(300).await;
        gate.add_permits(100);
        sleep(300).await;
        let third = start_connect(&broker, connect_options("ahead", false, 3600));
        sleep(400).await;
        let on_third = deliveries(&third.received());
        drop(first);
        assert_eq!(
            (on_second, on_third),
            (
                vec![('d', false, vec![0, 3])],
                vec![
                    ('a', true, vec![0, 1]),
                    ('b', true, vec![0, 2]),
                    ('c', false, vec![0, 4]),
                    ('d', true, vec![0, 3])
                ]
            )
        );
    }

    #[wasm_bindgen_test]
    async fn a_late_hand_back_ahead_of_a_qos_1_resend_leaves_it_its_packet_id() {
        a_late_hand_back_ahead_of_a_resend_leaves_it_its_packet_id(QoS::AtLeastOnce).await;
    }

    #[wasm_bindgen_test]
    async fn a_late_hand_back_ahead_of_a_qos_2_resend_leaves_it_its_packet_id() {
        a_late_hand_back_ahead_of_a_resend_leaves_it_its_packet_id(QoS::ExactlyOnce).await;
    }

    #[wasm_bindgen_test]
    async fn reserved_packet_ids_are_not_given_to_new_deliveries_when_ids_run_out() {
        let broker = broker();
        let gate = Arc::new(tokio::sync::Semaphore::new(2));
        let first =
            stalled_after_two_of_three(&broker, "spent", "spent/x", QoS::AtLeastOnce, &gate).await;
        let second = connected(&broker, connect_options("spent", false, 3600)).await;
        sleep(31_000).await;
        for index in 0u32..65_535 {
            route(&broker, "spent/x", b"m", QoS::AtLeastOnce).await;
            if index % 50 == 0 {
                sleep(0).await;
            }
        }
        sleep(500).await;
        let before_hand_back = deliveries(&second.received());
        let fresh_on_reserved = before_hand_back
            .iter()
            .filter(|(_, dup, pid)| !dup && (*pid == vec![0, 1] || *pid == vec![0, 2]))
            .count();
        gate.add_permits(100);
        sleep(300).await;
        second.send_raw(&puback(&[0, 3]));
        second.send_raw(&puback(&[0, 4]));
        sleep(500).await;
        let resent: Vec<(char, Vec<u8>)> = deliveries(&second.received())
            .into_iter()
            .filter(|(payload, dup, _)| *dup && (*payload == 'a' || *payload == 'b'))
            .map(|(payload, _, pid)| (payload, pid))
            .collect();
        drop(first);
        assert_eq!(
            (before_hand_back.len(), fresh_on_reserved, resent),
            (65_533, 0, vec![('a', vec![0, 1]), ('b', vec![0, 2])])
        );
    }

    async fn a_resend_held_by_a_stalled_connection_keeps_its_packet_id(qos: QoS) {
        let broker = broker();
        let gate_first = Arc::new(tokio::sync::Semaphore::new(2));
        let first = stalled_after_two_of_three(&broker, "held", "held/x", qos, &gate_first).await;
        let second = connected(&broker, connect_options("held", false, 3600)).await;
        sleep(31_000).await;
        route(&broker, "held/x", b"d", qos).await;
        sleep(200).await;
        let on_second = deliveries(&second.received());
        second.port.close();
        sleep(300).await;
        gate_first.add_permits(100);
        sleep(300).await;
        let gate_third = Arc::new(tokio::sync::Semaphore::new(0));
        let third = open_with(&broker, Arc::new(Gated(Arc::clone(&gate_third))));
        third.send(&ConnectPacket::new(connect_options("held", false, 3600)));
        sleep(300).await;
        let on_third = deliveries(&third.received());
        let fourth = connected(&broker, connect_options("held", false, 3600)).await;
        sleep(31_000).await;
        route(&broker, "held/x", b"e", qos).await;
        sleep(200).await;
        gate_third.add_permits(100);
        sleep(400).await;
        let on_fourth = deliveries(&fourth.received());
        drop(first);
        drop(third);
        assert_eq!(
            (on_second, on_third, on_fourth),
            (
                vec![('d', false, vec![0, 3])],
                vec![('a', true, vec![0, 1]), ('b', true, vec![0, 2])],
                vec![
                    ('e', false, vec![0, 4]),
                    ('a', true, vec![0, 1]),
                    ('b', true, vec![0, 2]),
                    ('c', false, vec![0, 5]),
                    ('d', true, vec![0, 3])
                ]
            )
        );
    }

    #[wasm_bindgen_test]
    async fn a_qos_1_resend_held_by_a_stalled_connection_keeps_its_packet_id() {
        a_resend_held_by_a_stalled_connection_keeps_its_packet_id(QoS::AtLeastOnce).await;
    }

    #[wasm_bindgen_test]
    async fn a_qos_2_resend_held_by_a_stalled_connection_keeps_its_packet_id() {
        a_resend_held_by_a_stalled_connection_keeps_its_packet_id(QoS::ExactlyOnce).await;
    }

    async fn resumed_after_message_expiry(
        client_id: &str,
        qos: QoS,
        expiry: u32,
    ) -> Vec<(char, bool, Vec<u8>)> {
        let broker = broker();
        let first = connected(&broker, connect_options(client_id, true, 3600)).await;
        subscribed_at(&first, "expiry/x", qos as u8).await;
        let mut publish =
            mqtt5_protocol::packet::publish::PublishPacket::new("expiry/x", b"x".to_vec(), qos);
        publish.properties.set_message_expiry_interval(expiry);
        broker.router.route_message(&publish, None).await;
        sleep(100).await;
        assert_eq!(deliveries(&first.received()).len(), 1);
        first.port.close();
        sleep(1_600).await;
        let second = start_connect(&broker, connect_options(client_id, false, 3600));
        sleep(300).await;
        route(&broker, "expiry/x", b"n", qos).await;
        sleep(200).await;
        deliveries(&second.received())
    }

    #[wasm_bindgen_test]
    async fn an_unacknowledged_qos_1_delivery_is_resent_after_its_message_expiry() {
        assert_eq!(
            resumed_after_message_expiry("expiry1", QoS::AtLeastOnce, 1).await,
            vec![('x', true, vec![0, 1]), ('n', false, vec![0, 2])]
        );
    }

    #[wasm_bindgen_test]
    async fn an_unacknowledged_qos_2_delivery_is_resent_after_its_message_expiry() {
        assert_eq!(
            resumed_after_message_expiry("expiry2", QoS::ExactlyOnce, 1).await,
            vec![('x', true, vec![0, 1]), ('n', false, vec![0, 2])]
        );
    }

    #[wasm_bindgen_test]
    async fn control_an_unacknowledged_delivery_before_its_message_expiry_is_resent() {
        assert_eq!(
            resumed_after_message_expiry("expiry3", QoS::AtLeastOnce, 60).await,
            vec![('x', true, vec![0, 1]), ('n', false, vec![0, 2])]
        );
    }
}
