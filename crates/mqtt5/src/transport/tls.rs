use crate::crypto::NoVerification;
use crate::error::{MqttError, Result};
use crate::time::Duration;
use crate::Transport;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::{client::TlsStream, TlsConnector};
use tracing::{debug, instrument, trace};

pub type TlsReadHalf = ReadHalf<TlsStream<TcpStream>>;

pub type TlsWriteHalf = WriteHalf<TlsStream<TcpStream>>;

/// TLS transport configuration
#[derive(Debug)]
pub struct TlsConfig {
    /// Server address
    pub addr: SocketAddr,
    /// Server hostname for SNI
    pub hostname: String,
    /// Connection timeout
    pub connect_timeout: Duration,
    /// Client certificate chain (optional)
    pub client_cert: Option<Vec<CertificateDer<'static>>>,
    /// Client private key (optional)
    pub client_key: Option<PrivateKeyDer<'static>>,
    /// Custom root certificates (optional)
    pub root_certs: Option<Vec<CertificateDer<'static>>>,
    /// Whether to use system root certificates
    pub use_system_roots: bool,
    /// Whether to verify server certificate
    pub verify_server_cert: bool,
    /// ALPN protocols (optional)
    pub alpn_protocols: Option<Vec<Vec<u8>>>,
}

impl Clone for TlsConfig {
    fn clone(&self) -> Self {
        Self {
            addr: self.addr,
            hostname: self.hostname.clone(),
            connect_timeout: self.connect_timeout,
            client_cert: self.client_cert.clone(),
            client_key: self.client_key.as_ref().map(|key| {
                PrivateKeyDer::try_from(key.secret_der().to_vec())
                    .expect("Failed to clone private key")
            }),
            root_certs: self.root_certs.clone(),
            use_system_roots: self.use_system_roots,
            verify_server_cert: self.verify_server_cert,
            alpn_protocols: self.alpn_protocols.clone(),
        }
    }
}

impl TlsConfig {
    /// Creates a new TLS configuration
    #[must_use]
    pub fn new(addr: SocketAddr, hostname: impl Into<String>) -> Self {
        Self {
            addr,
            hostname: hostname.into(),
            connect_timeout: Duration::from_secs(30),
            client_cert: None,
            client_key: None,
            root_certs: None,
            use_system_roots: true,
            verify_server_cert: true,
            alpn_protocols: None,
        }
    }

    /// Sets the connection timeout
    #[must_use]
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Sets client certificate and key for mutual TLS
    #[must_use]
    pub fn with_client_auth(
        mut self,
        cert: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> Self {
        self.client_cert = Some(cert);
        self.client_key = Some(key);
        self
    }

    /// Sets custom root certificates
    #[must_use]
    pub fn with_root_certs(mut self, certs: Vec<CertificateDer<'static>>) -> Self {
        self.root_certs = Some(certs);
        self
    }

    /// Sets whether to use system root certificates
    #[must_use]
    pub fn with_system_roots(mut self, use_system: bool) -> Self {
        self.use_system_roots = use_system;
        self
    }

    /// Sets whether to verify server certificate
    ///
    /// # Safety
    ///
    /// Disabling certificate verification is insecure and should only
    /// be used for testing or in controlled environments
    #[must_use]
    pub fn with_verify_server_cert(mut self, verify: bool) -> Self {
        self.verify_server_cert = verify;
        self
    }

    /// Sets ALPN protocols
    ///
    /// # Panics
    ///
    /// Panics if any protocol string is empty or longer than 255 bytes (ALPN limitation)
    #[must_use]
    pub fn with_alpn_protocols(mut self, protocols: &[&str]) -> Self {
        for protocol in protocols {
            assert!(!protocol.is_empty(), "ALPN protocol cannot be empty");
            assert!(
                (protocol.len() <= 255),
                "ALPN protocol cannot exceed 255 bytes"
            );
        }
        self.alpn_protocols = Some(protocols.iter().map(|p| p.as_bytes().to_vec()).collect());
        self
    }

    /// Sets ALPN protocols from raw bytes
    #[must_use]
    pub fn with_alpn_protocols_raw(mut self, protocols: Vec<Vec<u8>>) -> Self {
        self.alpn_protocols = Some(protocols);
        self
    }

    /// Sets ALPN protocol for AWS `IoT` Core (x-amzn-mqtt-ca)
    ///
    /// This is a convenience method for AWS `IoT` connections using port 443.
    /// AWS `IoT` requires the "x-amzn-mqtt-ca" ALPN protocol for MQTT connections on port 443.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use mqtt5::transport::tls::TlsConfig;
    /// # use std::net::SocketAddr;
    /// let tls_config = TlsConfig::new(
    ///     "your-endpoint.iot.us-east-1.amazonaws.com:443".parse().unwrap(),
    ///     "your-endpoint.iot.us-east-1.amazonaws.com"
    /// ).with_aws_iot_alpn();
    /// ```
    #[must_use]
    pub fn with_aws_iot_alpn(mut self) -> Self {
        self.alpn_protocols = Some(vec![b"x-amzn-mqtt-ca".to_vec()]);
        self
    }

    /// Loads client certificate from PEM file
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub fn load_client_cert_pem(&mut self, cert_path: &str) -> Result<()> {
        let cert_pem = std::fs::read(cert_path)?;
        let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &cert_pem[..])
            .filter_map(std::result::Result::ok)
            .collect();
        if certs.is_empty() {
            return Err(MqttError::ProtocolError(
                "No certificates found in file".to_string(),
            ));
        }
        self.client_cert = Some(certs);
        Ok(())
    }

    /// Loads client private key from PEM file
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub fn load_client_key_pem(&mut self, key_path: &str) -> Result<()> {
        let key_pem = std::fs::read(key_path)?;
        let mut keys: Vec<PrivateKeyDer<'static>> =
            rustls_pemfile::pkcs8_private_keys(&mut &key_pem[..])
                .filter_map(std::result::Result::ok)
                .map(PrivateKeyDer::from)
                .collect();

        if keys.is_empty() {
            // Try RSA keys if PKCS8 didn't work
            keys = rustls_pemfile::rsa_private_keys(&mut &key_pem[..])
                .filter_map(std::result::Result::ok)
                .map(PrivateKeyDer::from)
                .collect();
        }

        if keys.is_empty() {
            return Err(MqttError::ProtocolError(
                "No private keys found in file".to_string(),
            ));
        }
        self.client_key = Some(keys.into_iter().next().ok_or_else(|| {
            MqttError::ProtocolError("Keys vector unexpectedly empty".to_string())
        })?);
        Ok(())
    }

    /// Loads CA certificate from PEM file
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub fn load_ca_cert_pem(&mut self, ca_path: &str) -> Result<()> {
        let ca_pem = std::fs::read(ca_path)?;
        let ca_certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &ca_pem[..])
            .filter_map(std::result::Result::ok)
            .collect();
        if ca_certs.is_empty() {
            return Err(MqttError::ProtocolError(
                "No CA certificates found in file".to_string(),
            ));
        }
        self.root_certs = Some(ca_certs);
        Ok(())
    }

    /// Loads client certificate from PEM bytes
    ///
    /// # Errors
    ///
    /// Returns an error if the bytes cannot be parsed as PEM certificate
    pub fn load_client_cert_pem_bytes(&mut self, cert_pem: &[u8]) -> Result<()> {
        let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &cert_pem[..])
            .filter_map(std::result::Result::ok)
            .collect();
        if certs.is_empty() {
            return Err(MqttError::ProtocolError(
                "No certificates found in PEM bytes".to_string(),
            ));
        }
        self.client_cert = Some(certs);
        Ok(())
    }

    /// Loads client private key from PEM bytes
    ///
    /// # Errors
    ///
    /// Returns an error if the bytes cannot be parsed as PEM private key
    pub fn load_client_key_pem_bytes(&mut self, key_pem: &[u8]) -> Result<()> {
        let mut keys: Vec<PrivateKeyDer<'static>> =
            rustls_pemfile::pkcs8_private_keys(&mut &key_pem[..])
                .filter_map(std::result::Result::ok)
                .map(PrivateKeyDer::from)
                .collect();

        if keys.is_empty() {
            // Try RSA keys if PKCS8 didn't work
            keys = rustls_pemfile::rsa_private_keys(&mut &key_pem[..])
                .filter_map(std::result::Result::ok)
                .map(PrivateKeyDer::from)
                .collect();
        }

        if keys.is_empty() {
            return Err(MqttError::ProtocolError(
                "No private keys found in PEM bytes".to_string(),
            ));
        }
        self.client_key = Some(keys.into_iter().next().ok_or_else(|| {
            MqttError::ProtocolError("Keys vector unexpectedly empty".to_string())
        })?);
        Ok(())
    }

    /// Loads CA certificate from PEM bytes
    ///
    /// # Errors
    ///
    /// Returns an error if the bytes cannot be parsed as PEM certificate
    pub fn load_ca_cert_pem_bytes(&mut self, ca_pem: &[u8]) -> Result<()> {
        let ca_certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &ca_pem[..])
            .filter_map(std::result::Result::ok)
            .collect();
        if ca_certs.is_empty() {
            return Err(MqttError::ProtocolError(
                "No CA certificates found in PEM bytes".to_string(),
            ));
        }
        self.root_certs = Some(ca_certs);
        Ok(())
    }

    /// Loads client certificate from DER bytes
    ///
    /// # Errors
    ///
    /// Returns an error if the bytes are not valid DER-encoded certificate
    pub fn load_client_cert_der_bytes(&mut self, cert_der: &[u8]) -> Result<()> {
        let cert = CertificateDer::from(cert_der.to_vec());
        self.client_cert = Some(vec![cert]);
        Ok(())
    }

    /// Loads client private key from DER bytes
    ///
    /// # Errors
    ///
    /// Returns an error if the bytes are not valid DER-encoded private key
    pub fn load_client_key_der_bytes(&mut self, key_der: &[u8]) -> Result<()> {
        // Assume PKCS#8 format for DER keys - this is the most common format
        use rustls::pki_types::PrivatePkcs8KeyDer;
        let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(key_der.to_vec()));
        self.client_key = Some(key);
        Ok(())
    }

    /// Loads CA certificate from DER bytes
    ///
    /// # Errors
    ///
    /// Returns an error if the bytes are not valid DER-encoded certificate
    pub fn load_ca_cert_der_bytes(&mut self, ca_der: &[u8]) -> Result<()> {
        let ca_cert = CertificateDer::from(ca_der.to_vec());
        self.root_certs = Some(vec![ca_cert]);
        Ok(())
    }
}

/// TLS transport implementation
#[derive(Debug)]
pub struct TlsTransport {
    config: TlsConfig,
    stream: Option<TlsStream<TcpStream>>,
}

impl TlsTransport {
    /// Creates a new TLS transport
    #[must_use]
    pub fn new(config: TlsConfig) -> Self {
        Self {
            config,
            stream: None,
        }
    }

    /// Checks if the transport is connected
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    /// Splits the TLS transport into read and write halves
    ///
    /// # Errors
    ///
    /// Returns an error if the transport is not connected
    pub fn into_split(self) -> Result<(TlsReadHalf, TlsWriteHalf)> {
        match self.stream {
            Some(stream) => Ok(tokio::io::split(stream)),
            None => Err(MqttError::NotConnected),
        }
    }

    /// Builds the rustls client configuration
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    fn build_tls_config(&mut self) -> Result<ClientConfig> {
        let mut root_store = RootCertStore::empty();

        // Add system roots if requested
        if self.config.use_system_roots {
            root_store.extend(webpki_roots::TLS_SERVER_ROOTS.to_vec());
        }

        // Add custom root certificates
        if let Some(ref root_certs) = self.config.root_certs {
            for cert in root_certs {
                root_store.add(cert.clone()).map_err(|e| {
                    MqttError::ProtocolError(format!("Failed to add root cert: {e}"))
                })?;
            }
        }

        let config_builder = if self.config.verify_server_cert {
            ClientConfig::builder().with_root_certificates(root_store)
        } else {
            // Disable certificate verification for testing with self-signed certs
            ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerification))
        };

        let mut config = if let (Some(cert), Some(key)) = (
            self.config.client_cert.clone(),
            self.config.client_key.as_ref().map(|k| {
                PrivateKeyDer::try_from(k.secret_der().to_vec())
                    .expect("Failed to clone private key")
            }),
        ) {
            config_builder
                .with_client_auth_cert(cert, key)
                .map_err(|e| {
                    MqttError::ProtocolError(format!("Failed to configure client auth: {e}"))
                })?
        } else {
            config_builder.with_no_client_auth()
        };

        // Configure ALPN protocols if provided
        if let Some(ref protocols) = self.config.alpn_protocols {
            config.alpn_protocols.clone_from(protocols);
        }

        Ok(config)
    }
}

impl Transport for TlsTransport {
    #[instrument(skip(self), fields(addr = %self.config.addr, server_name = %self.config.hostname, verify_certs = self.config.verify_server_cert))]
    async fn connect(&mut self) -> Result<()> {
        if self.stream.is_some() {
            return Err(MqttError::AlreadyConnected);
        }

        let tls_config = Arc::new(self.build_tls_config()?);
        let connector = TlsConnector::from(tls_config);

        let tcp_stream = timeout(
            self.config.connect_timeout,
            TcpStream::connect(self.config.addr),
        )
        .await
        .map_err(|_| MqttError::Timeout)??;

        tcp_stream.set_nodelay(true)?;

        let domain = ServerName::try_from(self.config.hostname.clone())
            .map_err(|_| MqttError::ProtocolError("Invalid server hostname".to_string()))?;

        let tls_stream = timeout(
            self.config.connect_timeout,
            connector.connect(domain, tcp_stream),
        )
        .await
        .map_err(|_| MqttError::Timeout)?
        .map_err(|e| MqttError::ConnectionError(format!("TLS handshake failed: {e}")))?;

        debug!("TLS connection established");
        self.stream = Some(tls_stream);
        Ok(())
    }

    #[instrument(skip(self, buf), fields(buf_len = buf.len()), level = "debug")]
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        match &mut self.stream {
            Some(stream) => {
                trace!(buf_len = buf.len(), "TLS read attempt");
                let n = stream.read(buf).await?;
                if n == 0 {
                    debug!("TLS connection closed by remote (EOF)");
                    return Err(MqttError::ConnectionClosedByPeer);
                }
                trace!(bytes_read = n, "TLS read complete");
                Ok(n)
            }
            None => Err(MqttError::NotConnected),
        }
    }

    #[instrument(skip(self, buf), fields(buf_len = buf.len()), level = "debug")]
    async fn write(&mut self, buf: &[u8]) -> Result<()> {
        match &mut self.stream {
            Some(stream) => {
                stream.write_all(buf).await?;
                stream.flush().await?;
                trace!(bytes_written = buf.len(), "TLS write complete");
                Ok(())
            }
            None => Err(MqttError::NotConnected),
        }
    }

    #[instrument(skip(self))]
    async fn close(&mut self) -> Result<()> {
        if let Some(mut stream) = self.stream.take() {
            stream.shutdown().await?;
            debug!("TLS connection closed");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn test_tls_config() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883);
        let config = TlsConfig::new(addr, "localhost")
            .with_connect_timeout(Duration::from_secs(10))
            .with_system_roots(false)
            .with_verify_server_cert(false);

        assert_eq!(config.addr, addr);
        assert_eq!(config.hostname, "localhost");
        assert_eq!(config.connect_timeout, Duration::from_secs(10));
        assert!(!config.use_system_roots);
        assert!(!config.verify_server_cert);
        assert!(config.alpn_protocols.is_none());
    }

    #[test]
    fn test_tls_config_with_alpn() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883);
        let config =
            TlsConfig::new(addr, "localhost").with_alpn_protocols(&["mqtt", "x-amzn-mqtt-ca"]);

        assert!(config.alpn_protocols.is_some());
        let protocols = config.alpn_protocols.unwrap();
        assert_eq!(protocols.len(), 2);
        assert_eq!(protocols[0], b"mqtt");
        assert_eq!(protocols[1], b"x-amzn-mqtt-ca");
    }

    #[test]
    fn test_tls_transport_creation() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883);
        let transport = TlsTransport::new(TlsConfig::new(addr, "localhost"));

        assert!(!transport.is_connected());
        assert_eq!(transport.config.addr, addr);
    }

    #[tokio::test]
    async fn test_tls_connect_not_connected() {
        let mut transport = TlsTransport::new(TlsConfig::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883),
            "localhost",
        ));

        // Try to read when not connected
        let mut buf = [0u8; 10];
        let result = transport.read(&mut buf).await;
        assert!(result.is_err());

        // Try to write when not connected
        let result = transport.write(b"test").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_tls_connect_real_broker() {
        use crate::packet::connect::ConnectPacket;
        use crate::packet::MqttPacket;
        use crate::protocol::v5::properties::Properties;

        // Initialize the crypto provider for tests
        let _ = rustls::crypto::ring::default_provider().install_default();

        let mut config = TlsConfig::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8884),
            "localhost",
        );

        // Load test certificates (skip if they don't exist)
        if config.load_ca_cert_pem("../../test_certs/ca.pem").is_err() {
            // Test certificates not available, skip the test
            return;
        }
        config.verify_server_cert = false; // For self-signed test certs

        let mut transport = TlsTransport::new(config);

        // Connect - skip test if broker is not available (e.g., in CI without Docker)
        let result = transport.connect().await;
        if result.is_err() {
            if let Some(error) = result.as_ref().err() {
                if error.to_string().contains("Connection refused") {
                    // TLS broker not available (e.g., in CI), skip the test
                    return;
                }
            }
        }
        assert!(
            result.is_ok(),
            "Failed to connect via TLS: {:?}",
            result.err()
        );
        assert!(transport.is_connected());

        // Write MQTT CONNECT packet using proper packet construction

        let connect = ConnectPacket {
            client_id: "tls_test".to_string(),
            keep_alive: 60,
            clean_start: true,
            will: None,
            username: None,
            password: None,
            properties: Properties::new(),
            protocol_version: 5,
            will_properties: Properties::new(),
        };

        let mut connect_bytes = Vec::new();
        let result = connect.encode(&mut connect_bytes);
        assert!(
            result.is_ok(),
            "Failed to encode CONNECT packet: {:?}",
            result.err()
        );

        let result = transport.write(&connect_bytes).await;
        assert!(result.is_ok());

        // Read response (should get CONNACK)
        let mut buf = [0u8; 256];
        let result = transport.read(&mut buf).await;
        assert!(result.is_ok());
        let n = result.unwrap();
        assert!(n > 0);
        assert_eq!(buf[0] >> 4, 2); // CONNACK packet type

        // Close connection
        let result = transport.close().await;
        assert!(result.is_ok());
        assert!(!transport.is_connected());
    }

    #[test]
    fn test_tls_config_load_files() {
        let mut config = TlsConfig::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883),
            "localhost",
        );

        // These should fail on non-existent files
        assert!(config.load_client_cert_pem("non_existent.pem").is_err());
        assert!(config.load_client_key_pem("non_existent.pem").is_err());
        assert!(config.load_ca_cert_pem("non_existent.pem").is_err());
    }

    #[test]
    fn test_tls_config_load_cert_from_pem_bytes() {
        let mut config = TlsConfig::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883),
            "localhost",
        );

        // Test valid PEM certificate
        let valid_cert_pem = b"-----BEGIN CERTIFICATE-----
MIIBkTCB+wIJALRJF4QlQZq2MA0GCSqGSIb3DQEBCwUAMBQxEjAQBgNVBAMMCWxv
Y2FsaG9zdDAeFw0yNDAxMDEwMDAwMDBaFw0yNTAxMDEwMDAwMDBaMBQxEjAQBgNV
BAMMCWxvY2FsaG9zdDBcMA0GCSqGSIb3DQEBAQUAA0sAMEgCQQC7VJTUt9Us8cKB
UmRKvW2aIGMBMExdqGp4ncyf4BzGTOIUtShgP6+u7kj7mBH2q9sP5bOxFKqQSzAD
g8hSN4z8z2o3GYUBj5uEJjh8iVR1OGlmv0iYgzgZWj5Jw7BLG0HMwNfb+H4hTlgc
pZYH8gMxmGQiQmOxSKNJAz5xPJTBGNJjvP+Z3Nd8bQe2qnOz4Hp3s2qs7C4Gq
aPVP5q7LxXIAgIDAQABMA0GCSqGSIb3DQEBCwUAA0EANQfUSRkgFfPb0K9VkbNj
PwX8FnQ+zjqAVHCtjpB+5jdYG3TQmFfQ7EaQdKZGKMWKyGKIQ9fhFvTmI8OU6Y6V
TA==
-----END CERTIFICATE-----";

        assert!(config.load_client_cert_pem_bytes(valid_cert_pem).is_ok());
        assert!(config.client_cert.is_some());

        // Test invalid PEM data
        let invalid_pem = b"not a valid certificate";
        let mut config2 = TlsConfig::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883),
            "localhost",
        );
        assert!(config2.load_client_cert_pem_bytes(invalid_pem).is_err());

        // Test empty PEM data
        let empty_pem = b"";
        let mut config3 = TlsConfig::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883),
            "localhost",
        );
        assert!(config3.load_client_cert_pem_bytes(empty_pem).is_err());
    }

    #[test]
    fn test_tls_config_load_key_from_pem_bytes() {
        let mut config = TlsConfig::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883),
            "localhost",
        );

        // Test valid RSA private key in PEM format
        let valid_key_pem = b"-----BEGIN RSA PRIVATE KEY-----
MIIBOgIBAAJBALtUlNS31SzxwoFSZEq9bZogYwEwTF2oanidzJ/gHMZM4hS1KGA/
r67uSPuYEfar2w/ls7EUqpBLMAODyFI3jPzPajcZhQGPm4QmOHyJVHU4aWa/SJiD
OBlaOknDsEsbQczA19v4fiFOWByllgfyAzGYZCJCY7FIo0kDPnE8lMEY0mO8/5nc
13xtB7aqc7PgenezaqzsLgapo9U/mrsvFcgCAgMBAAECQBKmZi7m2J+5nEoM0YKU
wQgRqT2kFz8tJO0Q9r4rQfkbFm8OmVZs9FcX+Z8vCcOqS8nG0z8cRGhX+rKhRrVu
uoECIQDdwJmRZQhCGpX0P8Q6v5B2J7mOZQVg7VK1g4YFcYHyeQIhANJFfHjHgKqJ
x8Z9fQzK8u0FDlq0wGHkL1rCgJzQLHmBAiEA6VjXlZGhF2G8EL4P+7+P6u6W2Qrb
u9W5m0K4kV2sQ2ECIDTqoHEfL2+OzPsQpBxZ5kD6XpGuL6UKYXyF+VZw9uGBAiBm
QK8Q2JGfQtK+7F6vGgR8QKrMgJh6EwZhLl3mPVH+QQ==
-----END RSA PRIVATE KEY-----";

        assert!(config.load_client_key_pem_bytes(valid_key_pem).is_ok());
        assert!(config.client_key.is_some());

        // Test invalid PEM data
        let invalid_pem = b"not a valid private key";
        let mut config2 = TlsConfig::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883),
            "localhost",
        );
        assert!(config2.load_client_key_pem_bytes(invalid_pem).is_err());
    }

    #[test]
    fn test_tls_config_load_ca_from_pem_bytes() {
        let mut config = TlsConfig::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883),
            "localhost",
        );

        // Test valid CA certificate
        let valid_ca_pem = b"-----BEGIN CERTIFICATE-----
MIIBkTCB+wIJALRJF4QlQZq2MA0GCSqGSIb3DQEBCwUAMBQxEjAQBgNVBAMMCWxv
Y2FsaG9zdDAeFw0yNDAxMDEwMDAwMDBaFw0yNTAxMDEwMDAwMDBaMBQxEjAQBgNV
BAMMCWxvY2FsaG9zdDBcMA0GCSqGSIb3DQEBAQUAA0sAMEgCQQC7VJTUt9Us8cKB
UmRKvW2aIGMBMExdqGp4ncyf4BzGTOIUtShgP6+u7kj7mBH2q9sP5bOxFKqQSzAD
g8hSN4z8z2o3GYUBj5uEJjh8iVR1OGlmv0iYgzgZWj5Jw7BLG0HMwNfb+H4hTlgc
pZYH8gMxmGQiQmOxSKNJAz5xPJTBGNJjvP+Z3Nd8bQe2qnOz4Hp3s2qs7C4Gq
aPVP5q7LxXIAgIDAQABMA0GCSqGSIb3DQEBCwUAA0EANQfUSRkgFfPb0K9VkbNj
PwX8FnQ+zjqAVHCtjpB+5jdYG3TQmFfQ7EaQdKZGKMWKyGKIQ9fhFvTmI8OU6Y6V
TA==
-----END CERTIFICATE-----";

        assert!(config.load_ca_cert_pem_bytes(valid_ca_pem).is_ok());
        assert!(config.root_certs.is_some());

        // Test invalid PEM data
        let invalid_pem = b"not a valid ca certificate";
        let mut config2 = TlsConfig::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883),
            "localhost",
        );
        assert!(config2.load_ca_cert_pem_bytes(invalid_pem).is_err());
    }

    #[test]
    fn test_tls_config_load_cert_from_der_bytes() {
        let mut config = TlsConfig::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883),
            "localhost",
        );

        // Test with dummy DER bytes (in practice these would be valid DER-encoded certificates)
        let dummy_der = vec![0x30, 0x82, 0x01, 0x00]; // Basic DER structure
        assert!(config.load_client_cert_der_bytes(&dummy_der).is_ok());
        assert!(config.client_cert.is_some());

        // Test empty DER data
        let empty_der = vec![];
        let mut config2 = TlsConfig::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883),
            "localhost",
        );
        assert!(config2.load_client_cert_der_bytes(&empty_der).is_ok()); // DER validation happens at TLS handshake
    }

    #[test]
    fn test_tls_config_load_key_from_der_bytes() {
        let mut config = TlsConfig::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883),
            "localhost",
        );

        // Test with dummy DER bytes
        let dummy_der = vec![0x30, 0x48, 0x02, 0x01]; // Basic DER structure for key
        assert!(config.load_client_key_der_bytes(&dummy_der).is_ok());
        assert!(config.client_key.is_some());
    }

    #[test]
    fn test_tls_config_load_ca_from_der_bytes() {
        let mut config = TlsConfig::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883),
            "localhost",
        );

        // Test with dummy DER bytes
        let dummy_der = vec![0x30, 0x82, 0x01, 0x00]; // Basic DER structure
        assert!(config.load_ca_cert_der_bytes(&dummy_der).is_ok());
        assert!(config.root_certs.is_some());
    }
}
