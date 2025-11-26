//! Broker configuration
//!
//! Configuration options for the MQTT v5.0 broker, following the same
//! direct async patterns as the client.

#[cfg(not(target_arch = "wasm32"))]
use crate::broker::bridge::BridgeConfig;
use crate::error::Result;
#[cfg(feature = "opentelemetry")]
use crate::telemetry::TelemetryConfig;
use crate::time::Duration;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;

/// Broker configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct BrokerConfig {
    /// TCP listener addresses (supports multiple addresses for dual-stack IPv4/IPv6)
    pub bind_addresses: Vec<SocketAddr>,

    /// Maximum number of concurrent clients
    pub max_clients: usize,

    /// Session expiry interval for disconnected clients
    pub session_expiry_interval: Duration,

    /// Maximum packet size in bytes
    pub max_packet_size: usize,

    /// Maximum topic alias
    pub topic_alias_maximum: u16,

    /// Whether to retain messages
    pub retain_available: bool,

    /// Maximum `QoS` supported
    pub maximum_qos: u8,

    /// Wildcard subscription available
    pub wildcard_subscription_available: bool,

    /// Subscription identifiers available
    pub subscription_identifier_available: bool,

    /// Shared subscription available
    pub shared_subscription_available: bool,

    /// Server keep alive time
    pub server_keep_alive: Option<Duration>,

    /// Response information
    pub response_information: Option<String>,

    /// Authentication configuration
    pub auth_config: AuthConfig,

    /// TLS configuration
    pub tls_config: Option<TlsConfig>,

    /// WebSocket configuration
    pub websocket_config: Option<WebSocketConfig>,

    /// WebSocket TLS configuration
    pub websocket_tls_config: Option<WebSocketConfig>,

    /// QUIC configuration
    pub quic_config: Option<QuicConfig>,

    /// Storage configuration
    pub storage_config: StorageConfig,

    /// Bridge configurations
    #[cfg(not(target_arch = "wasm32"))]
    #[serde(default)]
    pub bridges: Vec<BridgeConfig>,

    /// OpenTelemetry configuration
    #[cfg(feature = "opentelemetry")]
    #[serde(skip)]
    pub opentelemetry_config: Option<TelemetryConfig>,
}

impl Default for BrokerConfig {
    fn default() -> Self {
        Self {
            bind_addresses: vec![
                "0.0.0.0:1883".parse().unwrap(),
                "[::]:1883".parse().unwrap(),
            ],
            max_clients: 10000,
            session_expiry_interval: Duration::from_secs(3600), // 1 hour
            max_packet_size: 268_435_456,                       // 256 MB
            topic_alias_maximum: 65535,
            retain_available: true,
            maximum_qos: 2,
            wildcard_subscription_available: true,
            subscription_identifier_available: true,
            shared_subscription_available: true,
            server_keep_alive: None,
            response_information: None,
            auth_config: AuthConfig::default(),
            tls_config: None,
            websocket_config: None,
            websocket_tls_config: None,
            quic_config: None,
            storage_config: StorageConfig::default(),
            #[cfg(not(target_arch = "wasm32"))]
            bridges: vec![],
            #[cfg(feature = "opentelemetry")]
            opentelemetry_config: None,
        }
    }
}

impl BrokerConfig {
    /// Creates a new broker configuration with default values
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the bind addresses (replaces all existing addresses)
    #[must_use]
    pub fn with_bind_addresses(mut self, addrs: Vec<SocketAddr>) -> Self {
        self.bind_addresses = addrs;
        self
    }

    /// Adds a bind address to the list
    #[must_use]
    pub fn add_bind_address(mut self, addr: impl Into<SocketAddr>) -> Self {
        self.bind_addresses.push(addr.into());
        self
    }

    /// Sets a single bind address (replaces all existing addresses)
    #[must_use]
    pub fn with_bind_address(mut self, addr: impl Into<SocketAddr>) -> Self {
        self.bind_addresses = vec![addr.into()];
        self
    }

    /// Sets the maximum number of concurrent clients
    #[must_use]
    pub fn with_max_clients(mut self, max: usize) -> Self {
        self.max_clients = max;
        self
    }

    /// Sets the session expiry interval
    #[must_use]
    pub fn with_session_expiry(mut self, interval: Duration) -> Self {
        self.session_expiry_interval = interval;
        self
    }

    /// Sets the maximum packet size
    #[must_use]
    pub fn with_max_packet_size(mut self, size: usize) -> Self {
        self.max_packet_size = size;
        self
    }

    /// Sets the maximum `QoS` level
    #[must_use]
    pub fn with_maximum_qos(mut self, qos: u8) -> Self {
        self.maximum_qos = qos.min(2);
        self
    }

    /// Enables or disables retained messages
    #[must_use]
    pub fn with_retain_available(mut self, available: bool) -> Self {
        self.retain_available = available;
        self
    }

    /// Sets the authentication configuration
    #[must_use]
    pub fn with_auth(mut self, auth: AuthConfig) -> Self {
        self.auth_config = auth;
        self
    }

    /// Sets the TLS configuration
    #[must_use]
    pub fn with_tls(mut self, tls: TlsConfig) -> Self {
        self.tls_config = Some(tls);
        self
    }

    /// Sets the WebSocket configuration
    #[must_use]
    pub fn with_websocket(mut self, ws: WebSocketConfig) -> Self {
        self.websocket_config = Some(ws);
        self
    }

    /// Sets the WebSocket TLS configuration
    #[must_use]
    pub fn with_websocket_tls(mut self, ws_tls: WebSocketConfig) -> Self {
        self.websocket_tls_config = Some(ws_tls);
        self
    }

    /// Sets the QUIC configuration
    #[must_use]
    pub fn with_quic(mut self, quic: QuicConfig) -> Self {
        self.quic_config = Some(quic);
        self
    }

    /// Sets the storage configuration
    #[must_use]
    pub fn with_storage(mut self, storage: StorageConfig) -> Self {
        self.storage_config = storage;
        self
    }

    /// Sets the OpenTelemetry configuration
    #[must_use]
    #[cfg(feature = "opentelemetry")]
    pub fn with_opentelemetry(mut self, config: TelemetryConfig) -> Self {
        self.opentelemetry_config = Some(config);
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

/// Authentication configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// Whether anonymous connections are allowed
    pub allow_anonymous: bool,

    /// Path to password file (username:password format)
    pub password_file: Option<PathBuf>,

    /// Path to ACL file (user `<username>` topic `<pattern>` permission `<type>` format)
    pub acl_file: Option<PathBuf>,

    /// Authentication method
    pub auth_method: AuthMethod,

    /// Authentication data
    pub auth_data: Option<Vec<u8>>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            allow_anonymous: true,
            password_file: None,
            acl_file: None,
            auth_method: AuthMethod::None,
            auth_data: None,
        }
    }
}

/// Authentication method
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMethod {
    /// No authentication
    None,
    /// Simple username/password
    Password,
    /// SCRAM-SHA-256
    ScramSha256,
}

/// TLS configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    /// Path to certificate file
    pub cert_file: PathBuf,

    /// Path to private key file
    pub key_file: PathBuf,

    /// Path to CA certificate file
    pub ca_file: Option<PathBuf>,

    /// Whether to require client certificates
    pub require_client_cert: bool,

    /// TLS listener addresses (supports multiple addresses for dual-stack)
    pub bind_addresses: Vec<SocketAddr>,
}

impl TlsConfig {
    /// Creates a new TLS configuration
    #[must_use]
    #[allow(clippy::missing_panics_doc)]
    pub fn new(cert_file: PathBuf, key_file: PathBuf) -> Self {
        Self {
            cert_file,
            key_file,
            ca_file: None,
            require_client_cert: false,
            bind_addresses: vec![
                "0.0.0.0:8883".parse().expect("valid IPv4 address"),
                "[::]:8883".parse().expect("valid IPv6 address"),
            ],
        }
    }

    /// Sets the CA certificate file
    #[must_use]
    pub fn with_ca_file(mut self, ca_file: PathBuf) -> Self {
        self.ca_file = Some(ca_file);
        self
    }

    /// Sets whether to require client certificates
    #[must_use]
    pub fn with_require_client_cert(mut self, require: bool) -> Self {
        self.require_client_cert = require;
        self
    }

    /// Sets the TLS bind addresses (replaces all existing)
    #[must_use]
    pub fn with_bind_addresses(mut self, addrs: Vec<SocketAddr>) -> Self {
        self.bind_addresses = addrs;
        self
    }

    /// Adds a TLS bind address
    #[must_use]
    pub fn add_bind_address(mut self, addr: impl Into<SocketAddr>) -> Self {
        self.bind_addresses.push(addr.into());
        self
    }

    /// Sets a single bind address (replaces all existing)
    #[must_use]
    pub fn with_bind_address(mut self, addr: impl Into<SocketAddr>) -> Self {
        self.bind_addresses = vec![addr.into()];
        self
    }
}

/// QUIC configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuicConfig {
    pub cert_file: PathBuf,
    pub key_file: PathBuf,
    pub ca_file: Option<PathBuf>,
    pub require_client_cert: bool,
    pub bind_addresses: Vec<SocketAddr>,
}

impl QuicConfig {
    #[must_use]
    #[allow(clippy::missing_panics_doc)]
    pub fn new(cert_file: PathBuf, key_file: PathBuf) -> Self {
        Self {
            cert_file,
            key_file,
            ca_file: None,
            require_client_cert: false,
            bind_addresses: vec![
                "0.0.0.0:14567".parse().expect("valid IPv4 address"),
                "[::]:14567".parse().expect("valid IPv6 address"),
            ],
        }
    }

    #[must_use]
    pub fn with_ca_file(mut self, ca_file: PathBuf) -> Self {
        self.ca_file = Some(ca_file);
        self
    }

    #[must_use]
    pub fn with_require_client_cert(mut self, require: bool) -> Self {
        self.require_client_cert = require;
        self
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
}

/// WebSocket configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketConfig {
    /// WebSocket listener addresses (supports multiple addresses for dual-stack)
    pub bind_addresses: Vec<SocketAddr>,

    /// Path for WebSocket connections
    pub path: String,

    /// Subprotocol name
    pub subprotocol: String,

    /// Whether to enable WebSocket over TLS
    pub use_tls: bool,
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self {
            bind_addresses: vec![
                "0.0.0.0:8080".parse().unwrap(),
                "[::]:8080".parse().unwrap(),
            ],
            path: "/mqtt".to_string(),
            subprotocol: "mqtt".to_string(),
            use_tls: false,
        }
    }
}

impl WebSocketConfig {
    /// Creates a new WebSocket configuration
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the bind addresses (replaces all existing)
    #[must_use]
    pub fn with_bind_addresses(mut self, addrs: Vec<SocketAddr>) -> Self {
        self.bind_addresses = addrs;
        self
    }

    /// Adds a bind address
    #[must_use]
    pub fn add_bind_address(mut self, addr: impl Into<SocketAddr>) -> Self {
        self.bind_addresses.push(addr.into());
        self
    }

    /// Sets a single bind address (replaces all existing)
    #[must_use]
    pub fn with_bind_address(mut self, addr: impl Into<SocketAddr>) -> Self {
        self.bind_addresses = vec![addr.into()];
        self
    }

    /// Sets the WebSocket path
    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    /// Enables WebSocket over TLS
    #[must_use]
    pub fn with_tls(mut self, use_tls: bool) -> Self {
        self.use_tls = use_tls;
        self
    }
}

/// Storage configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Storage backend type
    pub backend: StorageBackend,

    /// Base directory for file storage
    pub base_dir: PathBuf,

    /// Cleanup interval for expired entries
    pub cleanup_interval: Duration,

    /// Enable storage persistence
    pub enable_persistence: bool,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            backend: StorageBackend::File,
            base_dir: PathBuf::from("./mqtt_storage"),
            cleanup_interval: Duration::from_secs(3600), // 1 hour
            enable_persistence: true,
        }
    }
}

impl StorageConfig {
    /// Creates a new storage configuration
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the storage backend
    #[must_use]
    pub fn with_backend(mut self, backend: StorageBackend) -> Self {
        self.backend = backend;
        self
    }

    /// Sets the base directory for file storage
    #[must_use]
    pub fn with_base_dir(mut self, dir: PathBuf) -> Self {
        self.base_dir = dir;
        self
    }

    /// Sets the cleanup interval
    #[must_use]
    pub fn with_cleanup_interval(mut self, interval: Duration) -> Self {
        self.cleanup_interval = interval;
        self
    }

    /// Enables or disables persistence
    #[must_use]
    pub fn with_persistence(mut self, enabled: bool) -> Self {
        self.enable_persistence = enabled;
        self
    }
}

/// Storage backend type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageBackend {
    /// File-based storage
    File,
    /// In-memory storage (for testing)
    Memory,
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
