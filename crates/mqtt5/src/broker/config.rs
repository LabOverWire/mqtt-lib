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
use std::sync::Arc;

use super::events::BrokerEventHandler;

/// Broker configuration
#[derive(Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct BrokerConfig {
    /// TCP listener addresses (supports multiple addresses for dual-stack IPv4/IPv6)
    pub bind_addresses: Vec<SocketAddr>,

    /// Maximum number of concurrent clients
    pub max_clients: usize,

    /// Session expiry interval for disconnected clients
    #[cfg_attr(not(target_arch = "wasm32"), serde(with = "humantime_serde"))]
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

    /// Maximum subscriptions per client (0 = unlimited)
    #[serde(default)]
    pub max_subscriptions_per_client: usize,

    /// Maximum retained messages globally (0 = unlimited)
    #[serde(default)]
    pub max_retained_messages: usize,

    /// Maximum retained message payload size in bytes (0 = unlimited)
    #[serde(default)]
    pub max_retained_message_size: usize,

    /// Client message channel capacity (default: 10000)
    /// This is the buffer size for messages waiting to be sent to each client.
    /// Increase this value for high-throughput scenarios.
    #[serde(default = "default_client_channel_capacity")]
    pub client_channel_capacity: usize,

    /// Server keep alive time
    #[cfg_attr(not(target_arch = "wasm32"), serde(with = "humantime_serde"))]
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

    /// Cluster listener configuration for inter-node communication
    pub cluster_listener_config: Option<ClusterListenerConfig>,

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

    /// Event handler for broker events (connect, subscribe, publish, disconnect)
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
            .field("storage_config", &self.storage_config);
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
            session_expiry_interval: Duration::from_secs(3600), // 1 hour
            max_packet_size: 268_435_456,                       // 256 MB
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
            #[cfg(not(target_arch = "wasm32"))]
            bridges: vec![],
            #[cfg(feature = "opentelemetry")]
            opentelemetry_config: None,
            event_handler: None,
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

    /// Sets the maximum subscriptions per client (0 = unlimited)
    #[must_use]
    pub fn with_max_subscriptions_per_client(mut self, max: usize) -> Self {
        self.max_subscriptions_per_client = max;
        self
    }

    /// Sets the maximum retained messages globally (0 = unlimited)
    #[must_use]
    pub fn with_max_retained_messages(mut self, max: usize) -> Self {
        self.max_retained_messages = max;
        self
    }

    /// Sets the maximum retained message payload size in bytes (0 = unlimited)
    #[must_use]
    pub fn with_max_retained_message_size(mut self, max: usize) -> Self {
        self.max_retained_message_size = max;
        self
    }

    /// Sets the client message channel capacity
    #[must_use]
    pub fn with_client_channel_capacity(mut self, capacity: usize) -> Self {
        self.client_channel_capacity = capacity;
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

    /// Sets the cluster listener configuration
    #[must_use]
    pub fn with_cluster_listener(mut self, cluster: ClusterListenerConfig) -> Self {
        self.cluster_listener_config = Some(cluster);
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

    /// Sets the event handler for broker events
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

/// Authentication configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub allow_anonymous: bool,
    pub password_file: Option<PathBuf>,
    pub acl_file: Option<PathBuf>,
    pub auth_method: AuthMethod,
    pub auth_data: Option<Vec<u8>>,
    pub scram_file: Option<PathBuf>,
    pub jwt_config: Option<JwtConfig>,
    pub federated_jwt_config: Option<FederatedJwtConfig>,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    pub enabled: bool,
    pub max_attempts: u32,
    pub window_secs: u64,
    pub lockout_secs: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_attempts: 5,
            window_secs: 60,
            lockout_secs: 300,
        }
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            allow_anonymous: true,
            password_file: None,
            acl_file: None,
            auth_method: AuthMethod::None,
            auth_data: None,
            scram_file: None,
            jwt_config: None,
            federated_jwt_config: None,
            rate_limit: RateLimitConfig::default(),
        }
    }
}

impl AuthConfig {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_password_file(mut self, path: PathBuf) -> Self {
        self.password_file = Some(path);
        self.auth_method = AuthMethod::Password;
        self
    }

    #[must_use]
    pub fn with_scram_file(mut self, path: PathBuf) -> Self {
        self.scram_file = Some(path);
        self.auth_method = AuthMethod::ScramSha256;
        self
    }

    #[must_use]
    pub fn with_jwt(mut self, config: JwtConfig) -> Self {
        self.jwt_config = Some(config);
        self.auth_method = AuthMethod::Jwt;
        self
    }

    #[must_use]
    pub fn with_federated_jwt(mut self, config: FederatedJwtConfig) -> Self {
        self.federated_jwt_config = Some(config);
        self.auth_method = AuthMethod::JwtFederated;
        self
    }

    #[must_use]
    pub fn with_acl_file(mut self, path: PathBuf) -> Self {
        self.acl_file = Some(path);
        self
    }

    #[must_use]
    pub fn with_allow_anonymous(mut self, allow: bool) -> Self {
        self.allow_anonymous = allow;
        self
    }
}

/// Authentication method
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMethod {
    None,
    Password,
    ScramSha256,
    Jwt,
    JwtFederated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JwtAlgorithm {
    HS256,
    RS256,
    ES256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtConfig {
    pub algorithm: JwtAlgorithm,
    pub secret_or_key_file: PathBuf,
    pub issuer: Option<String>,
    pub audience: Option<String>,
    pub clock_skew_secs: u64,
}

impl Default for JwtConfig {
    fn default() -> Self {
        Self {
            algorithm: JwtAlgorithm::HS256,
            secret_or_key_file: PathBuf::new(),
            issuer: None,
            audience: None,
            clock_skew_secs: 60,
        }
    }
}

impl JwtConfig {
    #[must_use]
    pub fn new(algorithm: JwtAlgorithm, secret_or_key_file: PathBuf) -> Self {
        Self {
            algorithm,
            secret_or_key_file,
            issuer: None,
            audience: None,
            clock_skew_secs: 60,
        }
    }

    #[must_use]
    pub fn with_issuer(mut self, issuer: impl Into<String>) -> Self {
        self.issuer = Some(issuer.into());
        self
    }

    #[must_use]
    pub fn with_audience(mut self, audience: impl Into<String>) -> Self {
        self.audience = Some(audience.into());
        self
    }

    #[must_use]
    pub fn with_clock_skew(mut self, seconds: u64) -> Self {
        self.clock_skew_secs = seconds;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JwtKeySource {
    StaticFile {
        algorithm: JwtAlgorithm,
        path: PathBuf,
    },
    Jwks {
        uri: String,
        fallback_key_file: PathBuf,
        #[serde(default = "default_refresh_interval")]
        refresh_interval_secs: u64,
        #[serde(default = "default_cache_ttl")]
        cache_ttl_secs: u64,
    },
}

fn default_client_channel_capacity() -> usize {
    10000
}

fn default_refresh_interval() -> u64 {
    3600
}

fn default_cache_ttl() -> u64 {
    86400
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtIssuerConfig {
    pub name: String,
    pub issuer: String,
    pub key_source: JwtKeySource,
    pub audience: Option<String>,
    #[serde(default = "default_clock_skew")]
    pub clock_skew_secs: u64,
    #[serde(default)]
    pub auth_mode: FederatedAuthMode,
    #[serde(default)]
    pub role_mappings: Vec<JwtRoleMapping>,
    #[serde(default)]
    pub default_roles: Vec<String>,
    #[serde(default)]
    pub trusted_role_claims: Vec<String>,
    #[serde(default = "default_session_scoped_roles")]
    pub session_scoped_roles: bool,
    #[serde(default)]
    pub issuer_prefix: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    #[deprecated(note = "Use auth_mode instead")]
    pub role_merge_mode: RoleMergeMode,
}

fn default_clock_skew() -> u64 {
    60
}

fn default_session_scoped_roles() -> bool {
    true
}

fn default_enabled() -> bool {
    true
}

impl JwtIssuerConfig {
    #[must_use]
    #[allow(deprecated)]
    pub fn new(
        name: impl Into<String>,
        issuer: impl Into<String>,
        key_source: JwtKeySource,
    ) -> Self {
        Self {
            name: name.into(),
            issuer: issuer.into(),
            key_source,
            audience: None,
            clock_skew_secs: 60,
            auth_mode: FederatedAuthMode::default(),
            role_mappings: Vec::new(),
            default_roles: Vec::new(),
            trusted_role_claims: Vec::new(),
            session_scoped_roles: true,
            issuer_prefix: None,
            enabled: true,
            role_merge_mode: RoleMergeMode::default(),
        }
    }

    #[must_use]
    pub fn with_audience(mut self, audience: impl Into<String>) -> Self {
        self.audience = Some(audience.into());
        self
    }

    #[must_use]
    pub fn with_auth_mode(mut self, mode: FederatedAuthMode) -> Self {
        self.auth_mode = mode;
        self
    }

    #[must_use]
    pub fn with_role_mapping(mut self, mapping: JwtRoleMapping) -> Self {
        self.role_mappings.push(mapping);
        self
    }

    #[must_use]
    pub fn with_default_roles(mut self, roles: Vec<String>) -> Self {
        self.default_roles = roles;
        self
    }

    #[must_use]
    pub fn with_trusted_role_claims(mut self, claims: Vec<String>) -> Self {
        self.trusted_role_claims = claims;
        self
    }

    #[must_use]
    pub fn with_session_scoped_roles(mut self, session_scoped: bool) -> Self {
        self.session_scoped_roles = session_scoped;
        self
    }

    #[must_use]
    pub fn with_issuer_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.issuer_prefix = Some(prefix.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RoleMergeMode {
    #[default]
    Merge,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FederatedAuthMode {
    #[default]
    IdentityOnly,
    ClaimBinding,
    TrustedRoles,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtRoleMapping {
    pub claim_path: String,
    pub pattern: ClaimPattern,
    pub assign_roles: Vec<String>,
}

impl JwtRoleMapping {
    #[must_use]
    pub fn new(claim_path: impl Into<String>, pattern: ClaimPattern, roles: Vec<String>) -> Self {
        Self {
            claim_path: claim_path.into(),
            pattern,
            assign_roles: roles,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClaimPattern {
    Equals(String),
    Contains(String),
    EndsWith(String),
    StartsWith(String),
    Regex(String),
    Any,
}

impl ClaimPattern {
    #[must_use]
    pub fn matches(&self, value: &str) -> bool {
        match self {
            Self::Equals(s) => value == s,
            Self::Contains(s) => value.contains(s),
            Self::EndsWith(s) => value.ends_with(s),
            Self::StartsWith(s) => value.starts_with(s),
            Self::Regex(pattern) => regex::Regex::new(pattern)
                .map(|re| re.is_match(value))
                .unwrap_or(false),
            Self::Any => true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FederatedJwtConfig {
    pub issuers: Vec<JwtIssuerConfig>,
    #[serde(default = "default_clock_skew")]
    pub clock_skew_secs: u64,
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
    /// Creates a new TLS configuration with default bind addresses.
    ///
    /// # Panics
    /// Panics if the default socket addresses fail to parse (should never happen).
    #[must_use]
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
    /// Creates a new QUIC configuration with default bind addresses.
    ///
    /// # Panics
    /// Panics if the default socket addresses fail to parse (should never happen).
    #[must_use]
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

/// Cluster listener transport type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ClusterTransport {
    #[default]
    Tcp,
    Quic,
}

/// Cluster listener configuration for inter-node communication
///
/// Connections on cluster listeners automatically have bridge forwarding disabled
/// to prevent message loops in distributed broker deployments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterListenerConfig {
    pub bind_addresses: Vec<SocketAddr>,
    pub transport: ClusterTransport,
    pub cert_file: Option<PathBuf>,
    pub key_file: Option<PathBuf>,
    pub ca_file: Option<PathBuf>,
    pub require_client_cert: bool,
}

impl ClusterListenerConfig {
    #[must_use]
    pub fn new(bind_addresses: Vec<SocketAddr>) -> Self {
        Self {
            bind_addresses,
            transport: ClusterTransport::Tcp,
            cert_file: None,
            key_file: None,
            ca_file: None,
            require_client_cert: false,
        }
    }

    #[must_use]
    pub fn quic(bind_addresses: Vec<SocketAddr>, cert_file: PathBuf, key_file: PathBuf) -> Self {
        Self {
            bind_addresses,
            transport: ClusterTransport::Quic,
            cert_file: Some(cert_file),
            key_file: Some(key_file),
            ca_file: None,
            require_client_cert: false,
        }
    }

    #[must_use]
    pub fn with_bind_address(mut self, addr: impl Into<SocketAddr>) -> Self {
        self.bind_addresses = vec![addr.into()];
        self
    }

    #[must_use]
    pub fn add_bind_address(mut self, addr: impl Into<SocketAddr>) -> Self {
        self.bind_addresses.push(addr.into());
        self
    }

    #[must_use]
    pub fn with_transport(mut self, transport: ClusterTransport) -> Self {
        self.transport = transport;
        self
    }

    #[must_use]
    pub fn with_tls(mut self, cert_file: PathBuf, key_file: PathBuf) -> Self {
        self.cert_file = Some(cert_file);
        self.key_file = Some(key_file);
        self
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
    pub fn uses_tls(&self) -> bool {
        self.cert_file.is_some() && self.key_file.is_some()
    }

    #[must_use]
    pub fn is_quic(&self) -> bool {
        self.transport == ClusterTransport::Quic
    }

    /// Validates the configuration, returning an error description if invalid.
    ///
    /// # Errors
    ///
    /// Returns an error string if:
    /// - No bind addresses are configured
    /// - QUIC transport is selected but `cert_file` or `key_file` is missing
    /// - TLS is partially configured (only cert or only key)
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.bind_addresses.is_empty() {
            return Err("cluster listener requires at least one bind address".to_string());
        }

        if self.is_quic() && !self.uses_tls() {
            return Err("QUIC cluster listener requires cert_file and key_file".to_string());
        }

        let has_cert = self.cert_file.is_some();
        let has_key = self.key_file.is_some();
        if has_cert != has_key {
            return Err(
                "TLS configuration incomplete: both cert_file and key_file are required"
                    .to_string(),
            );
        }

        Ok(())
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
    #[cfg_attr(not(target_arch = "wasm32"), serde(with = "humantime_serde"))]
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
