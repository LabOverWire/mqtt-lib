use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketConfig {
    pub bind_addresses: Vec<SocketAddr>,
    pub path: String,
    pub subprotocol: String,
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
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    #[must_use]
    pub fn with_tls(mut self, use_tls: bool) -> Self {
        self.use_tls = use_tls;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ClusterTransport {
    #[default]
    Tcp,
    Quic,
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cluster_listener_config_new_creates_tcp_transport() {
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let config = ClusterListenerConfig::new(vec![addr]);

        assert_eq!(config.bind_addresses, vec![addr]);
        assert_eq!(config.transport, ClusterTransport::Tcp);
        assert!(config.cert_file.is_none());
        assert!(config.key_file.is_none());
        assert!(!config.require_client_cert);
    }

    #[test]
    fn cluster_listener_config_quic_creates_quic_transport() {
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let config = ClusterListenerConfig::quic(
            vec![addr],
            PathBuf::from("/path/to/cert.pem"),
            PathBuf::from("/path/to/key.pem"),
        );

        assert_eq!(config.transport, ClusterTransport::Quic);
        assert!(config.uses_tls());
        assert!(config.is_quic());
    }

    #[test]
    fn cluster_listener_config_with_tls_builder() {
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let config = ClusterListenerConfig::new(vec![addr])
            .with_tls(
                PathBuf::from("/path/to/cert.pem"),
                PathBuf::from("/path/to/key.pem"),
            )
            .with_ca_file(PathBuf::from("/path/to/ca.pem"))
            .with_require_client_cert(true);

        assert!(config.uses_tls());
        assert!(!config.is_quic());
        assert!(config.require_client_cert);
        assert!(config.ca_file.is_some());
    }

    #[test]
    fn cluster_listener_config_validate_empty_addresses() {
        let config = ClusterListenerConfig::new(vec![]);
        let result = config.validate();

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("at least one bind address"));
    }

    #[test]
    fn cluster_listener_config_validate_quic_without_tls() {
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let config = ClusterListenerConfig::new(vec![addr]).with_transport(ClusterTransport::Quic);
        let result = config.validate();

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cert_file and key_file"));
    }

    #[test]
    fn cluster_listener_config_validate_partial_tls() {
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let mut config = ClusterListenerConfig::new(vec![addr]);
        config.cert_file = Some(PathBuf::from("/path/to/cert.pem"));
        let result = config.validate();

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("both cert_file and key_file"));
    }

    #[test]
    fn cluster_listener_config_validate_valid_tcp() {
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let config = ClusterListenerConfig::new(vec![addr]);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn cluster_listener_config_validate_valid_tls() {
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let config = ClusterListenerConfig::new(vec![addr]).with_tls(
            PathBuf::from("/path/to/cert.pem"),
            PathBuf::from("/path/to/key.pem"),
        );
        assert!(config.validate().is_ok());
    }

    #[test]
    fn cluster_listener_config_validate_valid_quic() {
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let config = ClusterListenerConfig::quic(
            vec![addr],
            PathBuf::from("/path/to/cert.pem"),
            PathBuf::from("/path/to/key.pem"),
        );
        assert!(config.validate().is_ok());
    }

    #[test]
    fn cluster_listener_config_add_bind_address() {
        let addr1: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let addr2: SocketAddr = "127.0.0.1:9998".parse().unwrap();
        let config = ClusterListenerConfig::new(vec![addr1]).add_bind_address(addr2);

        assert_eq!(config.bind_addresses.len(), 2);
        assert!(config.bind_addresses.contains(&addr1));
        assert!(config.bind_addresses.contains(&addr2));
    }

    #[test]
    fn cluster_listener_config_with_bind_address_replaces() {
        let addr1: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let addr2: SocketAddr = "127.0.0.1:9998".parse().unwrap();
        let config = ClusterListenerConfig::new(vec![addr1]).with_bind_address(addr2);

        assert_eq!(config.bind_addresses.len(), 1);
        assert_eq!(config.bind_addresses[0], addr2);
    }
}
