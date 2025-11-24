//! TLS acceptor for server-side TLS connections
//!
//! This module provides server-side TLS support for the MQTT broker,
//! allowing secure connections on a dedicated TLS port (typically 8883).

use crate::error::{MqttError, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::{server::TlsStream, TlsAcceptor};
use tracing::{debug, error};

/// Server-side TLS configuration
#[derive(Debug)]
pub struct TlsAcceptorConfig {
    /// Server certificate chain
    pub cert_chain: Vec<CertificateDer<'static>>,
    /// Server private key
    pub private_key: PrivateKeyDer<'static>,
    /// Client CA certificates for mutual TLS (optional)
    pub client_ca_certs: Option<Vec<CertificateDer<'static>>>,
    /// Whether to require client certificates
    pub require_client_cert: bool,
    /// ALPN protocols to advertise
    pub alpn_protocols: Vec<Vec<u8>>,
}

impl TlsAcceptorConfig {
    /// Creates a new TLS acceptor configuration
    pub fn new(
        cert_chain: Vec<CertificateDer<'static>>,
        private_key: PrivateKeyDer<'static>,
    ) -> Self {
        Self {
            cert_chain,
            private_key,
            client_ca_certs: None,
            require_client_cert: false,
            alpn_protocols: vec![b"mqtt".to_vec()],
        }
    }

    /// Loads server certificate from PEM file
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed
    pub async fn load_cert_chain_from_file(
        path: impl AsRef<Path>,
    ) -> Result<Vec<CertificateDer<'static>>> {
        let cert_pem = tokio::fs::read(path.as_ref()).await?;
        let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &cert_pem[..])
            .filter_map(std::result::Result::ok)
            .collect();

        if certs.is_empty() {
            return Err(MqttError::Configuration(
                "No certificates found in file".to_string(),
            ));
        }

        Ok(certs)
    }

    /// Loads server private key from PEM file
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed
    pub async fn load_private_key_from_file(
        path: impl AsRef<Path>,
    ) -> Result<PrivateKeyDer<'static>> {
        let key_pem = tokio::fs::read(path.as_ref()).await?;

        // Try PKCS#8 format first
        let mut keys: Vec<PrivateKeyDer<'static>> =
            rustls_pemfile::pkcs8_private_keys(&mut &key_pem[..])
                .filter_map(std::result::Result::ok)
                .map(PrivateKeyDer::from)
                .collect();

        // Try RSA format if PKCS#8 didn't work
        if keys.is_empty() {
            keys = rustls_pemfile::rsa_private_keys(&mut &key_pem[..])
                .filter_map(std::result::Result::ok)
                .map(PrivateKeyDer::from)
                .collect();
        }

        // Try EC format if RSA didn't work
        if keys.is_empty() {
            keys = rustls_pemfile::ec_private_keys(&mut &key_pem[..])
                .filter_map(std::result::Result::ok)
                .map(PrivateKeyDer::from)
                .collect();
        }

        keys.into_iter()
            .next()
            .ok_or_else(|| MqttError::Configuration("No private keys found in file".to_string()))
    }

    /// Sets client CA certificates for mutual TLS
    #[must_use]
    pub fn with_client_ca_certs(mut self, certs: Vec<CertificateDer<'static>>) -> Self {
        self.client_ca_certs = Some(certs);
        self
    }

    /// Sets whether to require client certificates
    #[must_use]
    pub fn with_require_client_cert(mut self, require: bool) -> Self {
        self.require_client_cert = require;
        self
    }

    /// Sets ALPN protocols
    #[must_use]
    pub fn with_alpn_protocols(mut self, protocols: Vec<Vec<u8>>) -> Self {
        self.alpn_protocols = protocols;
        self
    }

    /// Builds a rustls ServerConfig from this configuration
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration is invalid
    pub fn build_server_config(&self) -> Result<ServerConfig> {
        let mut config = if let Some(ref client_ca_certs) = self.client_ca_certs {
            // Set up client certificate verification
            let mut root_store = RootCertStore::empty();
            for cert in client_ca_certs {
                root_store.add(cert.clone()).map_err(|e| {
                    MqttError::Configuration(format!("Failed to add client CA cert: {e}"))
                })?;
            }

            let client_verifier = WebPkiClientVerifier::builder(Arc::new(root_store))
                .build()
                .map_err(|e| {
                    MqttError::Configuration(format!("Failed to build client verifier: {e}"))
                })?;

            ServerConfig::builder()
                .with_client_cert_verifier(client_verifier)
                .with_single_cert(self.cert_chain.clone(), self.private_key.clone_key())
                .map_err(|e| {
                    MqttError::Configuration(format!("Failed to configure server cert: {e}"))
                })?
        } else {
            // No client certificate verification
            ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(self.cert_chain.clone(), self.private_key.clone_key())
                .map_err(|e| {
                    MqttError::Configuration(format!("Failed to configure server cert: {e}"))
                })?
        };

        // Set ALPN protocols
        config.alpn_protocols.clone_from(&self.alpn_protocols);

        Ok(config)
    }

    /// Creates a TLS acceptor from this configuration
    ///
    /// # Errors
    ///
    /// Returns an error if the acceptor cannot be created
    pub fn build_acceptor(&self) -> Result<TlsAcceptor> {
        let server_config = self.build_server_config()?;
        Ok(TlsAcceptor::from(Arc::new(server_config)))
    }
}

/// Wrapper for TLS-enabled TCP streams
pub struct TlsStreamWrapper {
    inner: TlsStream<TcpStream>,
}

impl TlsStreamWrapper {
    /// Creates a new TLS stream wrapper
    pub fn new(stream: TlsStream<TcpStream>) -> Self {
        Self { inner: stream }
    }

    /// Gets the peer address
    ///
    /// # Errors
    ///
    /// Returns an error if the peer address cannot be retrieved
    pub fn peer_addr(&self) -> Result<std::net::SocketAddr> {
        self.inner
            .get_ref()
            .0
            .peer_addr()
            .map_err(|e| MqttError::Io(format!("Failed to get peer address: {e}")))
    }

    /// Gets the negotiated ALPN protocol
    pub fn alpn_protocol(&self) -> Option<Vec<u8>> {
        self.inner.get_ref().1.alpn_protocol().map(<[u8]>::to_vec)
    }

    /// Checks if a client certificate was provided
    pub fn has_client_cert(&self) -> bool {
        self.inner.get_ref().1.peer_certificates().is_some()
    }

    /// Gets the client certificate chain if provided
    pub fn client_cert_chain(&self) -> Option<Vec<CertificateDer<'static>>> {
        self.inner
            .get_ref()
            .1
            .peer_certificates()
            .map(<[rustls::pki_types::CertificateDer<'_>]>::to_vec)
    }
}

impl AsyncRead for TlsStreamWrapper {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for TlsStreamWrapper {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Accepts a TLS connection from a TCP stream
///
/// # Errors
///
/// Returns an error if the TLS handshake fails
pub async fn accept_tls_connection(
    acceptor: &TlsAcceptor,
    tcp_stream: TcpStream,
    peer_addr: std::net::SocketAddr,
) -> Result<TlsStreamWrapper> {
    debug!("Starting TLS handshake with {}", peer_addr);

    match acceptor.accept(tcp_stream).await {
        Ok(tls_stream) => {
            let wrapper = TlsStreamWrapper::new(tls_stream);

            if let Some(alpn) = wrapper.alpn_protocol() {
                debug!(
                    "TLS handshake completed with {} (ALPN: {})",
                    peer_addr,
                    String::from_utf8_lossy(&alpn)
                );
            } else {
                debug!("TLS handshake completed with {} (no ALPN)", peer_addr);
            }

            if wrapper.has_client_cert() {
                debug!("Client {} provided certificate", peer_addr);
            }

            Ok(wrapper)
        }
        Err(e) => {
            error!("TLS handshake failed with {}: {}", peer_addr, e);
            Err(MqttError::ConnectionError(format!(
                "TLS handshake failed: {e}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_tls_acceptor_config() {
        // Create dummy certificate and key for testing
        let cert = CertificateDer::from(vec![0x30, 0x82, 0x01, 0x00]);
        let key = PrivateKeyDer::from(rustls::pki_types::PrivatePkcs8KeyDer::from(vec![
            0x30, 0x48, 0x02, 0x01,
        ]));

        let config = TlsAcceptorConfig::new(vec![cert.clone()], key.clone_key())
            .with_require_client_cert(true)
            .with_alpn_protocols(vec![b"mqtt".to_vec(), b"mqttv5.0".to_vec()]);

        assert!(config.require_client_cert);
        assert_eq!(config.alpn_protocols.len(), 2);
        assert_eq!(config.cert_chain.len(), 1);
    }

    #[test]
    fn test_build_server_config_without_client_auth() {
        // Initialize the crypto provider for tests
        let _ = rustls::crypto::ring::default_provider().install_default();

        let cert = CertificateDer::from(vec![0x30, 0x82, 0x01, 0x00]);
        let key = PrivateKeyDer::from(rustls::pki_types::PrivatePkcs8KeyDer::from(vec![
            0x30, 0x48, 0x02, 0x01,
        ]));

        let config = TlsAcceptorConfig::new(vec![cert], key.clone_key());

        // This will fail with invalid cert/key in tests, but tests the code path
        let result = config.build_server_config();
        assert!(result.is_err()); // Expected to fail with dummy cert/key
    }
}
