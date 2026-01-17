use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    pub cert_file: PathBuf,
    pub key_file: PathBuf,
    pub ca_file: Option<PathBuf>,
    pub require_client_cert: bool,
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
