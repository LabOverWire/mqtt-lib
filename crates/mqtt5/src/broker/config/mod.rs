mod auth;
mod delta;
mod storage;
mod tls;
mod transport;

pub use auth::{
    AuthConfig, AuthMethod, ClaimPattern, FederatedAuthMode, FederatedJwtConfig, JwtAlgorithm,
    JwtConfig, JwtIssuerConfig, JwtKeySource, JwtRoleMapping, RateLimitConfig, RoleMergeMode,
};
pub use delta::DeltaSubscriptionConfig;
pub use storage::{StorageBackend, StorageConfig};
pub use tls::TlsConfig;
pub use transport::{ClusterListenerConfig, ClusterTransport, QuicConfig, WebSocketConfig};

#[cfg(not(target_arch = "wasm32"))]
use crate::broker::bridge::BridgeConfig;
use crate::error::Result;
#[cfg(feature = "opentelemetry")]
use crate::telemetry::TelemetryConfig;
use crate::time::Duration;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;

use super::events::BrokerEventHandler;

fn default_client_channel_capacity() -> usize {
    10000
}

#[derive(Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct BrokerConfig {
    pub bind_addresses: Vec<SocketAddr>,
    pub max_clients: usize,
    #[cfg_attr(not(target_arch = "wasm32"), serde(with = "humantime_serde"))]
    pub session_expiry_interval: Duration,
    pub max_packet_size: usize,
    pub topic_alias_maximum: u16,
    pub retain_available: bool,
    pub maximum_qos: u8,
    pub wildcard_subscription_available: bool,
    pub subscription_identifier_available: bool,
    pub shared_subscription_available: bool,
    #[serde(default)]
    pub max_subscriptions_per_client: usize,
    #[serde(default)]
    pub max_retained_messages: usize,
    #[serde(default)]
    pub max_retained_message_size: usize,
    #[serde(default = "default_client_channel_capacity")]
    pub client_channel_capacity: usize,
    #[cfg_attr(not(target_arch = "wasm32"), serde(with = "humantime_serde"))]
    pub server_keep_alive: Option<Duration>,
    pub response_information: Option<String>,
    pub auth_config: AuthConfig,
    pub tls_config: Option<TlsConfig>,
    pub websocket_config: Option<WebSocketConfig>,
    pub websocket_tls_config: Option<WebSocketConfig>,
    pub quic_config: Option<QuicConfig>,
    pub cluster_listener_config: Option<ClusterListenerConfig>,
    pub storage_config: StorageConfig,
    #[serde(default)]
    pub delta_subscription_config: DeltaSubscriptionConfig,
    #[cfg(not(target_arch = "wasm32"))]
    #[serde(default)]
    pub bridges: Vec<BridgeConfig>,
    #[cfg(feature = "opentelemetry")]
    #[serde(skip)]
    pub opentelemetry_config: Option<TelemetryConfig>,
    #[serde(skip)]
    pub event_handler: Option<Arc<dyn BrokerEventHandler>>,
}

impl std::fmt::Debug for BrokerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("BrokerConfig");
        d.field("bind_addresses", &self.bind_addresses)
            .field("max_clients", &self.max_clients)
            .field("session_expiry_interval", &self.session_expiry_interval)
            .field("max_packet_size", &self.max_packet_size)
            .field("topic_alias_maximum", &self.topic_alias_maximum)
            .field("retain_available", &self.retain_available)
            .field("maximum_qos", &self.maximum_qos)
            .field(
                "wildcard_subscription_available",
                &self.wildcard_subscription_available,
            )
            .field(
                "subscription_identifier_available",
                &self.subscription_identifier_available,
            )
            .field(
                "shared_subscription_available",
                &self.shared_subscription_available,
            )
            .field(
                "max_subscriptions_per_client",
                &self.max_subscriptions_per_client,
            )
            .field("max_retained_messages", &self.max_retained_messages)
            .field("max_retained_message_size", &self.max_retained_message_size)
            .field("client_channel_capacity", &self.client_channel_capacity)
            .field("server_keep_alive", &self.server_keep_alive)
            .field("response_information", &self.response_information)
            .field("auth_config", &self.auth_config)
            .field("tls_config", &self.tls_config)
            .field("websocket_config", &self.websocket_config)
            .field("websocket_tls_config", &self.websocket_tls_config)
            .field("quic_config", &self.quic_config)
            .field("cluster_listener_config", &self.cluster_listener_config)
            .field("storage_config", &self.storage_config)
            .field("delta_subscription_config", &self.delta_subscription_config);
        #[cfg(not(target_arch = "wasm32"))]
        d.field("bridges", &self.bridges);
        #[cfg(feature = "opentelemetry")]
        d.field("opentelemetry_config", &self.opentelemetry_config);
        d.field("event_handler", &self.event_handler.as_ref().map(|_| "..."))
            .finish()
    }
}

impl Default for BrokerConfig {
    fn default() -> Self {
        Self {
            bind_addresses: vec![
                "0.0.0.0:1883".parse().unwrap(),
                "[::]:1883".parse().unwrap(),
            ],
            max_clients: 10000,
            session_expiry_interval: Duration::from_secs(3600),
            max_packet_size: 268_435_456,
            topic_alias_maximum: 65535,
            retain_available: true,
            maximum_qos: 2,
            wildcard_subscription_available: true,
            subscription_identifier_available: true,
            shared_subscription_available: true,
            max_subscriptions_per_client: 0,
            max_retained_messages: 0,
            max_retained_message_size: 0,
            client_channel_capacity: default_client_channel_capacity(),
            server_keep_alive: None,
            response_information: None,
            auth_config: AuthConfig::default(),
            tls_config: None,
            websocket_config: None,
            websocket_tls_config: None,
            quic_config: None,
            cluster_listener_config: None,
            storage_config: StorageConfig::default(),
            delta_subscription_config: DeltaSubscriptionConfig::default(),
            #[cfg(not(target_arch = "wasm32"))]
            bridges: vec![],
            #[cfg(feature = "opentelemetry")]
            opentelemetry_config: None,
            event_handler: None,
        }
    }
}

impl BrokerConfig {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_bind_addresses(mut self, addrs: Vec<SocketAddr>) -> Self {
        self.bind_addresses = addrs;
        self
    }

    #[must_use]
    pub fn add_bind_address(mut self, addr: impl Into<SocketAddr>) -> Self {
        self.bind_addresses.push(addr.into());
        self
    }

    #[must_use]
    pub fn with_bind_address(mut self, addr: impl Into<SocketAddr>) -> Self {
        self.bind_addresses = vec![addr.into()];
        self
    }

    #[must_use]
    pub fn with_max_clients(mut self, max: usize) -> Self {
        self.max_clients = max;
        self
    }

    #[must_use]
    pub fn with_session_expiry(mut self, interval: Duration) -> Self {
        self.session_expiry_interval = interval;
        self
    }

    #[must_use]
    pub fn with_max_packet_size(mut self, size: usize) -> Self {
        self.max_packet_size = size;
        self
    }

    #[must_use]
    pub fn with_maximum_qos(mut self, qos: u8) -> Self {
        self.maximum_qos = qos.min(2);
        self
    }

    #[must_use]
    pub fn with_retain_available(mut self, available: bool) -> Self {
        self.retain_available = available;
        self
    }

    #[must_use]
    pub fn with_max_subscriptions_per_client(mut self, max: usize) -> Self {
        self.max_subscriptions_per_client = max;
        self
    }

    #[must_use]
    pub fn with_max_retained_messages(mut self, max: usize) -> Self {
        self.max_retained_messages = max;
        self
    }

    #[must_use]
    pub fn with_max_retained_message_size(mut self, max: usize) -> Self {
        self.max_retained_message_size = max;
        self
    }

    #[must_use]
    pub fn with_client_channel_capacity(mut self, capacity: usize) -> Self {
        self.client_channel_capacity = capacity;
        self
    }

    #[must_use]
    pub fn with_auth(mut self, auth: AuthConfig) -> Self {
        self.auth_config = auth;
        self
    }

    #[must_use]
    pub fn with_tls(mut self, tls: TlsConfig) -> Self {
        self.tls_config = Some(tls);
        self
    }

    #[must_use]
    pub fn with_websocket(mut self, ws: WebSocketConfig) -> Self {
        self.websocket_config = Some(ws);
        self
    }

    #[must_use]
    pub fn with_websocket_tls(mut self, ws_tls: WebSocketConfig) -> Self {
        self.websocket_tls_config = Some(ws_tls);
        self
    }

    #[must_use]
    pub fn with_quic(mut self, quic: QuicConfig) -> Self {
        self.quic_config = Some(quic);
        self
    }

    #[must_use]
    pub fn with_cluster_listener(mut self, cluster: ClusterListenerConfig) -> Self {
        self.cluster_listener_config = Some(cluster);
        self
    }

    #[must_use]
    pub fn with_storage(mut self, storage: StorageConfig) -> Self {
        self.storage_config = storage;
        self
    }

    #[must_use]
    pub fn with_delta_subscription(mut self, delta: DeltaSubscriptionConfig) -> Self {
        self.delta_subscription_config = delta;
        self
    }

    #[must_use]
    #[cfg(feature = "opentelemetry")]
    pub fn with_opentelemetry(mut self, config: TelemetryConfig) -> Self {
        self.opentelemetry_config = Some(config);
        self
    }

    #[must_use]
    pub fn with_event_handler(mut self, handler: Arc<dyn BrokerEventHandler>) -> Self {
        self.event_handler = Some(handler);
        self
    }

    /// Validates the configuration
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration is invalid
    pub fn validate(&self) -> Result<&Self> {
        if self.max_clients == 0 {
            return Err(crate::error::MqttError::Configuration(
                "max_clients must be greater than 0".to_string(),
            ));
        }

        if self.max_packet_size < 1024 {
            return Err(crate::error::MqttError::Configuration(
                "max_packet_size must be at least 1024 bytes".to_string(),
            ));
        }

        if self.maximum_qos > 2 {
            return Err(crate::error::MqttError::Configuration(
                "maximum_qos must be 0, 1, or 2".to_string(),
            ));
        }

        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = BrokerConfig::default();
        assert_eq!(config.bind_addresses.len(), 2);
        assert_eq!(config.bind_addresses[0].to_string(), "0.0.0.0:1883");
        assert_eq!(config.bind_addresses[1].to_string(), "[::]:1883");
        assert_eq!(config.max_clients, 10000);
        assert_eq!(config.maximum_qos, 2);
        assert!(config.retain_available);
    }

    #[test]
    fn test_config_builder() {
        let config = BrokerConfig::new()
            .with_bind_address("127.0.0.1:1884".parse::<SocketAddr>().unwrap())
            .with_max_clients(5000)
            .with_maximum_qos(1)
            .with_retain_available(false);

        assert_eq!(config.bind_addresses.len(), 1);
        assert_eq!(config.bind_addresses[0].to_string(), "127.0.0.1:1884");
        assert_eq!(config.max_clients, 5000);
        assert_eq!(config.maximum_qos, 1);
        assert!(!config.retain_available);
    }

    #[test]
    fn test_config_validation() {
        let mut config = BrokerConfig::default();
        assert!(config.validate().is_ok());

        config.max_clients = 0;
        assert!(config.validate().is_err());

        config.max_clients = 1000;
        config.max_packet_size = 512;
        assert!(config.validate().is_err());

        config.max_packet_size = 1024;
        config.maximum_qos = 3;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_tls_config() {
        let tls = TlsConfig::new("cert.pem".into(), "key.pem".into())
            .with_ca_file("ca.pem".into())
            .with_require_client_cert(true);

        assert_eq!(tls.cert_file.to_str().unwrap(), "cert.pem");
        assert_eq!(tls.key_file.to_str().unwrap(), "key.pem");
        assert_eq!(tls.ca_file.unwrap().to_str().unwrap(), "ca.pem");
        assert!(tls.require_client_cert);
    }

    #[test]
    fn test_quic_config() {
        let quic = QuicConfig::new("cert.pem".into(), "key.pem".into())
            .with_ca_file("ca.pem".into())
            .with_require_client_cert(true)
            .with_bind_address("0.0.0.0:14567".parse::<SocketAddr>().unwrap());

        assert_eq!(quic.cert_file.to_str().unwrap(), "cert.pem");
        assert_eq!(quic.key_file.to_str().unwrap(), "key.pem");
        assert_eq!(quic.ca_file.unwrap().to_str().unwrap(), "ca.pem");
        assert!(quic.require_client_cert);
        assert_eq!(quic.bind_addresses.len(), 1);
        assert_eq!(quic.bind_addresses[0].to_string(), "0.0.0.0:14567");
    }

    #[test]
    fn test_websocket_config() {
        let ws = WebSocketConfig::new()
            .with_bind_address("0.0.0.0:8443".parse::<SocketAddr>().unwrap())
            .with_path("/ws")
            .with_tls(true);

        assert_eq!(ws.bind_addresses.len(), 1);
        assert_eq!(ws.bind_addresses[0].to_string(), "0.0.0.0:8443");
        assert_eq!(ws.path, "/ws");
        assert!(ws.use_tls);
    }

    #[test]
    fn test_storage_config() {
        let storage = StorageConfig::new()
            .with_backend(StorageBackend::Memory)
            .with_base_dir("/tmp/mqtt".into())
            .with_cleanup_interval(Duration::from_secs(1800))
            .with_persistence(false);

        assert_eq!(storage.backend, StorageBackend::Memory);
        assert_eq!(storage.base_dir.to_str().unwrap(), "/tmp/mqtt");
        assert_eq!(storage.cleanup_interval, Duration::from_secs(1800));
        assert!(!storage.enable_persistence);
    }
}
