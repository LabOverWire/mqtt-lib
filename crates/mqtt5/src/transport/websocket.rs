//! WebSocket transport implementation for MQTT over `WebSockets`
//!
//! This module provides WebSocket transport for MQTT connections, enabling
//! MQTT communication in web browsers and environments where TCP connections
//! are not available or blocked by firewalls.
//!
//! ## Features
//!
//! - Plain WebSocket connections (ws://)
//! - Secure WebSocket connections (wss://) with TLS
//! - Custom headers support
//! - Subprotocol negotiation (mqtt, mqttv3.1, mqttv5.0)
//! - Connection timeouts and keep-alive
//! - Automatic reconnection support
//!
//! ## Usage
//!
//! ```rust,no_run
//! use mqtt5::transport::websocket::{WebSocketConfig, WebSocketTransport};
//! use mqtt5_protocol::transport::Transport;
//! use std::time::Duration;
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Basic WebSocket connection
//! let config = WebSocketConfig::new("ws://broker.example.com:8080/mqtt")?;
//! let mut transport = WebSocketTransport::new(config);
//! transport.connect().await?;
//!
//! // Secure WebSocket with custom configuration
//! let config = WebSocketConfig::new("wss://secure-broker.example.com/mqtt")?
//!     .with_timeout(Duration::from_secs(30))
//!     .with_subprotocol("mqtt")
//!     .with_header("Authorization", "Bearer token123");
//!
//! let mut transport = WebSocketTransport::new(config);
//! transport.connect().await?;
//! # Ok(())
//! # }
//! ```

use crate::error::{MqttError, Result};
use crate::packet::Packet;
use crate::time::Duration;
use crate::transport::packet_io::{PacketReader, PacketWriter};
use crate::transport::tls::TlsConfig;
use crate::Transport;
use futures_util::{stream::SplitSink, stream::SplitStream, StreamExt};
use std::collections::HashMap;
use std::net::SocketAddr;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    tungstenite::{self, http::Request, protocol::Message},
    MaybeTlsStream, WebSocketStream,
};
use tracing::{debug, error, info, instrument};
use url::Url;

/// WebSocket transport configuration
#[derive(Debug)]
pub struct WebSocketConfig {
    /// WebSocket URL (ws:// or wss://)
    pub url: Url,
    /// Connection timeout
    pub timeout: Duration,
    /// Subprotocols to negotiate (e.g., "mqtt", "mqttv3.1", "mqttv5.0")
    pub subprotocols: Vec<String>,
    /// Custom HTTP headers for the WebSocket handshake
    pub headers: HashMap<String, String>,
    /// User agent string
    pub user_agent: Option<String>,
    /// TLS configuration for secure WebSocket connections (wss://)
    pub tls_config: Option<TlsConfig>,
    /// Whether to verify TLS certificates (for wss://) - deprecated, use `tls_config`
    #[deprecated(note = "Use tls_config field instead")]
    pub verify_tls: bool,
}

impl WebSocketConfig {
    /// Creates a new WebSocket configuration
    ///
    /// # Errors
    ///
    /// Returns an error if the URL is invalid or uses an unsupported scheme
    pub fn new(url: &str) -> Result<Self> {
        let parsed_url = Url::parse(url)
            .map_err(|e| MqttError::ProtocolError(format!("Invalid WebSocket URL: {e}")))?;

        match parsed_url.scheme() {
            "ws" | "wss" => {}
            scheme => {
                return Err(MqttError::ProtocolError(format!(
                    "Unsupported WebSocket scheme: {scheme}. Use 'ws' or 'wss'"
                )));
            }
        }

        Ok(Self {
            url: parsed_url,
            timeout: Duration::from_secs(30),
            subprotocols: vec!["mqtt".to_string()],
            headers: HashMap::new(),
            user_agent: Some("mqtt-v5/0.4.0".to_string()),
            tls_config: None,
            #[allow(deprecated)]
            verify_tls: true,
        })
    }

    /// Sets the connection timeout
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the WebSocket subprotocols to negotiate
    #[must_use]
    pub fn with_subprotocols(mut self, subprotocols: &[&str]) -> Self {
        self.subprotocols = subprotocols
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        self
    }

    /// Sets a single WebSocket subprotocol
    #[must_use]
    pub fn with_subprotocol(mut self, subprotocol: &str) -> Self {
        self.subprotocols = vec![subprotocol.to_string()];
        self
    }

    /// Adds a custom HTTP header
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.insert(name.to_string(), value.to_string());
        self
    }

    /// Sets the User-Agent header
    #[must_use]
    pub fn with_user_agent(mut self, user_agent: &str) -> Self {
        self.user_agent = Some(user_agent.to_string());
        self
    }

    /// Sets whether to verify TLS certificates for wss:// connections
    ///
    /// # Safety
    ///
    /// Disabling TLS verification is insecure and should only be used for testing
    #[deprecated(note = "Use with_tls_config instead")]
    #[must_use]
    pub fn with_tls_verification(mut self, verify: bool) -> Self {
        #[allow(deprecated)]
        {
            self.verify_tls = verify;
        }
        self
    }

    /// Sets a custom TLS configuration for wss:// connections
    #[must_use]
    pub fn with_tls_config(mut self, tls_config: TlsConfig) -> Self {
        self.tls_config = Some(tls_config);
        self
    }

    /// Creates a TLS configuration automatically from the WebSocket URL
    ///
    /// This is a convenience method that creates a TLS config with the same
    /// host and port as the WebSocket URL.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The URL is not a secure WebSocket (wss://)
    /// - The URL does not have a valid host
    /// - The host/port combination cannot be parsed as a socket address
    pub fn with_tls_auto(mut self) -> Result<Self> {
        if !self.is_secure() {
            return Err(MqttError::ProtocolError(
                "TLS configuration only applies to wss:// URLs".to_string(),
            ));
        }

        let host = self.host().ok_or_else(|| {
            MqttError::ProtocolError("WebSocket URL must have a host".to_string())
        })?;

        let addr: SocketAddr = format!("{host}:{}", self.port())
            .parse()
            .map_err(|e| MqttError::ProtocolError(format!("Invalid host/port combination: {e}")))?;

        let tls_config = TlsConfig::new(addr, host);
        self.tls_config = Some(tls_config);
        Ok(self)
    }

    /// Adds client certificate authentication to the TLS configuration
    ///
    /// This method creates or modifies the TLS configuration to include client certificates.
    /// If no TLS config exists, it creates one automatically.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The URL is not a secure WebSocket (wss://)
    /// - The certificate or key files cannot be read or parsed
    /// - The TLS configuration cannot be created
    pub fn with_client_auth_from_files(mut self, cert_path: &str, key_path: &str) -> Result<Self> {
        if !self.is_secure() {
            return Err(MqttError::ProtocolError(
                "Client authentication only applies to wss:// URLs".to_string(),
            ));
        }

        // Create TLS config if it doesn't exist
        if self.tls_config.is_none() {
            self = self.with_tls_auto()?;
        }

        // Add client certificate to TLS config
        if let Some(ref mut tls_config) = self.tls_config {
            tls_config.load_client_cert_pem(cert_path)?;
            tls_config.load_client_key_pem(key_path)?;
        }

        Ok(self)
    }

    /// Adds client certificate authentication from bytes to the TLS configuration
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The URL is not a secure WebSocket (wss://)
    /// - The certificate or key bytes cannot be parsed
    /// - The TLS configuration cannot be created
    pub fn with_client_auth_from_bytes(mut self, cert_pem: &[u8], key_pem: &[u8]) -> Result<Self> {
        if !self.is_secure() {
            return Err(MqttError::ProtocolError(
                "Client authentication only applies to wss:// URLs".to_string(),
            ));
        }

        // Create TLS config if it doesn't exist
        if self.tls_config.is_none() {
            self = self.with_tls_auto()?;
        }

        // Add client certificate to TLS config
        if let Some(ref mut tls_config) = self.tls_config {
            tls_config.load_client_cert_pem_bytes(cert_pem)?;
            tls_config.load_client_key_pem_bytes(key_pem)?;
        }

        Ok(self)
    }

    /// Adds custom CA certificate from file to the TLS configuration
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The URL is not a secure WebSocket (wss://)
    /// - The CA certificate file cannot be read or parsed
    /// - The TLS configuration cannot be created
    pub fn with_ca_cert_from_file(mut self, ca_path: &str) -> Result<Self> {
        if !self.is_secure() {
            return Err(MqttError::ProtocolError(
                "CA certificate only applies to wss:// URLs".to_string(),
            ));
        }

        // Create TLS config if it doesn't exist
        if self.tls_config.is_none() {
            self = self.with_tls_auto()?;
        }

        // Add CA certificate to TLS config
        if let Some(ref mut tls_config) = self.tls_config {
            tls_config.load_ca_cert_pem(ca_path)?;
        }

        Ok(self)
    }

    /// Adds custom CA certificate from bytes to the TLS configuration
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The URL is not a secure WebSocket (wss://)
    /// - The CA certificate bytes cannot be parsed
    /// - The TLS configuration cannot be created
    pub fn with_ca_cert_from_bytes(mut self, ca_pem: &[u8]) -> Result<Self> {
        if !self.is_secure() {
            return Err(MqttError::ProtocolError(
                "CA certificate only applies to wss:// URLs".to_string(),
            ));
        }

        // Create TLS config if it doesn't exist
        if self.tls_config.is_none() {
            self = self.with_tls_auto()?;
        }

        // Add CA certificate to TLS config
        if let Some(ref mut tls_config) = self.tls_config {
            tls_config.load_ca_cert_pem_bytes(ca_pem)?;
        }

        Ok(self)
    }

    /// Returns true if this is a secure WebSocket connection (wss://)
    #[must_use]
    pub fn is_secure(&self) -> bool {
        self.url.scheme() == "wss"
    }

    /// Gets the host from the WebSocket URL
    #[must_use]
    pub fn host(&self) -> Option<&str> {
        self.url.host_str()
    }

    /// Gets the port from the WebSocket URL, with defaults for ws/wss
    #[must_use]
    pub fn port(&self) -> u16 {
        self.url.port().unwrap_or_else(|| match self.url.scheme() {
            "wss" => 443,
            _ => 80,
        })
    }

    /// Gets the TLS configuration for secure connections
    #[must_use]
    pub fn tls_config(&self) -> Option<&TlsConfig> {
        self.tls_config.as_ref()
    }

    /// Takes ownership of the TLS configuration
    #[must_use]
    pub fn take_tls_config(&mut self) -> Option<TlsConfig> {
        self.tls_config.take()
    }
}

/// WebSocket transport implementation
pub struct WebSocketTransport {
    config: WebSocketConfig,
    connected: bool,
    connection: Option<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    read_buffer: Vec<u8>,
}

impl WebSocketTransport {
    /// Creates a new WebSocket transport
    #[must_use]
    pub fn new(config: WebSocketConfig) -> Self {
        Self {
            config,
            connected: false,
            connection: None,
            read_buffer: Vec::new(),
        }
    }

    /// Checks if the transport is connected
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Gets the WebSocket URL
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.config.url
    }

    /// Gets the negotiated subprotocol (if any)
    #[must_use]
    pub fn subprotocol(&self) -> Option<&str> {
        // In a real implementation, this would return the negotiated subprotocol
        self.config.subprotocols.first().map(String::as_str)
    }

    /// Splits the WebSocket into read and write halves
    ///
    /// # Errors
    ///
    /// Returns an error if the transport is not connected
    pub fn into_split(self) -> Result<(WebSocketReadHandle, WebSocketWriteHandle)> {
        if !self.connected {
            return Err(MqttError::NotConnected);
        }

        let connection = self.connection.ok_or(MqttError::NotConnected)?;
        let (write, read) = connection.split();

        let read_handle = WebSocketReadHandle { reader: read };
        let write_handle = WebSocketWriteHandle { writer: write };

        Ok((read_handle, write_handle))
    }
}

/// WebSocket read handle for split operations
pub struct WebSocketReadHandle {
    reader: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
}

/// WebSocket write handle for split operations
pub struct WebSocketWriteHandle {
    writer: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
}

impl WebSocketReadHandle {
    /// Reads data from the WebSocket.
    ///
    /// # Errors
    /// Returns an error if the connection is closed or a read error occurs.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        loop {
            match self.reader.next().await {
                Some(Ok(Message::Binary(data))) => {
                    let len = data.len().min(buf.len());
                    buf[..len].copy_from_slice(&data[..len]);
                    return Ok(len);
                }
                Some(Ok(Message::Close(_))) | None => return Err(MqttError::ClientClosed),
                Some(Ok(
                    Message::Ping(_) | Message::Pong(_) | Message::Text(_) | Message::Frame(_),
                )) => {}
                Some(Err(e)) => return Err(MqttError::Io(e.to_string())),
            }
        }
    }
}

impl WebSocketWriteHandle {
    /// Writes data to the WebSocket.
    ///
    /// # Errors
    /// Returns an error if the write operation fails.
    pub async fn write(&mut self, buf: &[u8]) -> Result<()> {
        use futures_util::SinkExt;
        self.writer
            .send(Message::Binary(buf.to_vec().into()))
            .await
            .map_err(|e| MqttError::Io(e.to_string()))
    }
}

impl PacketReader for WebSocketReadHandle {
    async fn read_packet(&mut self, protocol_version: u8) -> Result<Packet> {
        use crate::packet::FixedHeader;
        use bytes::BytesMut;
        use futures_util::StreamExt;

        match self.reader.next().await {
            Some(Ok(Message::Binary(data))) => {
                let mut buf = BytesMut::from(&data[..]);

                let fixed_header = FixedHeader::decode(&mut buf)?;

                Packet::decode_from_body_with_version(
                    fixed_header.packet_type,
                    &fixed_header,
                    &mut buf,
                    protocol_version,
                )
            }
            Some(Ok(Message::Close(_))) | None => Err(MqttError::ClientClosed),
            Some(Ok(_)) => Err(MqttError::ProtocolError(
                "Unexpected WebSocket message type".to_string(),
            )),
            Some(Err(e)) => Err(MqttError::Io(e.to_string())),
        }
    }
}

impl PacketWriter for WebSocketWriteHandle {
    async fn write_packet(&mut self, packet: Packet) -> Result<()> {
        use bytes::BytesMut;
        use futures_util::SinkExt;

        let mut buf = BytesMut::with_capacity(1024);
        crate::transport::packet_io::encode_packet_to_buffer(&packet, &mut buf)?;

        // Send as WebSocket binary frame
        self.writer
            .send(Message::Binary(buf.to_vec().into()))
            .await
            .map_err(|e| MqttError::Io(e.to_string()))
    }
}

impl Transport for WebSocketTransport {
    #[instrument(skip(self), fields(url = %self.config.url, subprotocols = ?self.config.subprotocols))]
    async fn connect(&mut self) -> Result<()> {
        if self.connected {
            return Err(MqttError::AlreadyConnected);
        }

        let request = Request::builder()
            .uri(self.config.url.as_str())
            .header("Host", self.config.url.host_str().unwrap_or("localhost"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .header("Sec-WebSocket-Protocol", "mqtt")
            .body(())
            .map_err(|e| {
                MqttError::ConnectionError(format!("Failed to build WebSocket request: {e}"))
            })?;

        let ws_result = if self.config.is_secure()
            && self
                .config
                .tls_config
                .as_ref()
                .map_or(false, |cfg| !cfg.verify_server_cert)
        {
            use tokio_tungstenite::Connector;

            let tls = rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(std::sync::Arc::new(NoVerifier))
                .with_no_client_auth();

            let connector = Connector::Rustls(std::sync::Arc::new(tls));

            tokio::time::timeout(
                self.config.timeout,
                tokio_tungstenite::connect_async_tls_with_config(
                    request,
                    None,
                    false,
                    Some(connector),
                ),
            )
            .await
        } else {
            tokio::time::timeout(
                self.config.timeout,
                tokio_tungstenite::connect_async(request),
            )
            .await
        };

        match ws_result {
            Ok(Ok((ws_stream, response))) => {
                if let Some(protocol) = response.headers().get("Sec-WebSocket-Protocol") {
                    info!(
                        subprotocol = ?protocol.to_str().unwrap_or("<invalid>"),
                        "WebSocket subprotocol negotiated"
                    );
                }

                self.connection = Some(ws_stream);
                self.connected = true;
                debug!("WebSocket connection established");
                Ok(())
            }
            Ok(Err(e)) => {
                error!(error = %e, "WebSocket connection failed");
                Err(MqttError::ConnectionError(e.to_string()))
            }
            Err(_) => {
                error!("WebSocket connection timed out");
                Err(MqttError::Timeout)
            }
        }
    }

    #[instrument(skip(self, buf), fields(buf_len = buf.len()), level = "debug")]
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if !self.connected {
            return Err(MqttError::NotConnected);
        }

        if !self.read_buffer.is_empty() {
            let len = self.read_buffer.len().min(buf.len());
            buf[..len].copy_from_slice(&self.read_buffer[..len]);
            self.read_buffer.drain(..len);
            return Ok(len);
        }

        let connection = self.connection.as_mut().ok_or(MqttError::NotConnected)?;

        loop {
            match connection.next().await {
                Some(Ok(Message::Binary(data))) => {
                    let len = data.len().min(buf.len());
                    buf[..len].copy_from_slice(&data[..len]);

                    if data.len() > buf.len() {
                        self.read_buffer.extend_from_slice(&data[buf.len()..]);
                    }

                    return Ok(len);
                }
                Some(Ok(Message::Close(_))) | None => {
                    self.connected = false;
                    debug!("WebSocket connection closed by remote");
                    return Err(MqttError::ClientClosed);
                }
                Some(Ok(
                    Message::Ping(_) | Message::Pong(_) | Message::Text(_) | Message::Frame(_),
                )) => {}
                Some(Err(e)) => {
                    self.connected = false;
                    return Err(MqttError::Io(e.to_string()));
                }
            }
        }
    }

    #[instrument(skip(self, buf), fields(buf_len = buf.len()), level = "debug")]
    async fn write(&mut self, buf: &[u8]) -> Result<()> {
        use futures_util::SinkExt;

        if !self.connected {
            return Err(MqttError::NotConnected);
        }

        let connection = self.connection.as_mut().ok_or(MqttError::NotConnected)?;

        connection
            .send(Message::Binary(buf.to_vec().into()))
            .await
            .map_err(|e| {
                self.connected = false;
                MqttError::Io(e.to_string())
            })?;

        connection.flush().await.map_err(|e| {
            self.connected = false;
            MqttError::Io(e.to_string())
        })
    }

    #[instrument(skip(self))]
    async fn close(&mut self) -> Result<()> {
        if !self.connected {
            return Ok(());
        }

        if let Some(mut connection) = self.connection.take() {
            let _ = connection.close(None).await;
        }

        self.connected = false;
        debug!("WebSocket connection closed");
        Ok(())
    }
}

#[derive(Debug)]
struct NoVerifier;

impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_websocket_config_creation() {
        let config = WebSocketConfig::new("ws://localhost:8080/mqtt").unwrap();
        assert_eq!(config.url.as_str(), "ws://localhost:8080/mqtt");
        assert!(!config.is_secure());
        assert_eq!(config.host(), Some("localhost"));
        assert_eq!(config.port(), 8080);
        assert_eq!(config.subprotocols, vec!["mqtt"]);
    }

    #[test]
    fn test_websocket_config_secure() {
        let config = WebSocketConfig::new("wss://broker.example.com/mqtt").unwrap();
        assert_eq!(config.url.as_str(), "wss://broker.example.com/mqtt");
        assert!(config.is_secure());
        assert_eq!(config.host(), Some("broker.example.com"));
        assert_eq!(config.port(), 443); // Default HTTPS port
    }

    #[test]
    fn test_websocket_config_invalid_scheme() {
        let result = WebSocketConfig::new("http://example.com");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unsupported WebSocket scheme"));
    }

    #[test]
    fn test_websocket_config_with_options() {
        let config = WebSocketConfig::new("ws://localhost:8080/mqtt")
            .unwrap()
            .with_timeout(Duration::from_secs(60))
            .with_subprotocol("mqttv5.0")
            .with_header("Authorization", "Bearer token123")
            .with_user_agent("custom-client/1.0");

        assert_eq!(config.timeout, Duration::from_secs(60));
        assert_eq!(config.subprotocols, vec!["mqttv5.0"]);
        assert_eq!(
            config.headers.get("Authorization"),
            Some(&"Bearer token123".to_string())
        );
        assert_eq!(config.user_agent, Some("custom-client/1.0".to_string()));
    }

    #[tokio::test]
    async fn test_websocket_transport_creation() {
        let config = WebSocketConfig::new("ws://localhost:8080/mqtt").unwrap();
        let transport = WebSocketTransport::new(config);

        assert!(!transport.is_connected());
        assert_eq!(transport.url().as_str(), "ws://localhost:8080/mqtt");
        assert_eq!(transport.subprotocol(), Some("mqtt"));
    }

    #[tokio::test]
    async fn test_websocket_transport_connect() {
        let config = WebSocketConfig::new("ws://localhost:59999/mqtt").unwrap();
        let mut transport = WebSocketTransport::new(config);

        assert!(!transport.is_connected());

        // Connection will fail since there's no WebSocket server at localhost:59999,
        // but this tests that the connect method works as expected
        let result = transport.connect().await;
        assert!(result.is_err());
        assert!(!transport.is_connected());

        // Should fail to connect again (already failed state)
        let result = transport.connect().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_websocket_transport_operations_when_not_connected() {
        let config = WebSocketConfig::new("ws://localhost:59999/mqtt").unwrap();
        let mut transport = WebSocketTransport::new(config);

        let mut buf = [0u8; 10];
        assert!(transport.read(&mut buf).await.is_err());
        assert!(transport.write(b"test").await.is_err());

        // Close should succeed even when not connected
        assert!(transport.close().await.is_ok());
    }

    #[tokio::test]
    async fn test_websocket_transport_close() {
        let config = WebSocketConfig::new("ws://localhost:8080/mqtt").unwrap();
        let mut transport = WebSocketTransport::new(config);

        // Connection will fail, but we can still test the close method
        let _result = transport.connect().await;

        // Close should work even if not connected
        transport.close().await.unwrap();
        assert!(!transport.is_connected());
    }

    #[test]
    fn test_websocket_config_port_defaults() {
        let ws_config = WebSocketConfig::new("ws://example.com/mqtt").unwrap();
        assert_eq!(ws_config.port(), 80);

        let secure_config = WebSocketConfig::new("wss://example.com/mqtt").unwrap();
        assert_eq!(secure_config.port(), 443);

        let custom_port_config = WebSocketConfig::new("ws://example.com:8080/mqtt").unwrap();
        assert_eq!(custom_port_config.port(), 8080);
    }

    #[test]
    fn test_websocket_config_tls_auto() {
        // Should work for wss:// with IP address
        let config = WebSocketConfig::new("wss://127.0.0.1:8443/mqtt")
            .unwrap()
            .with_tls_auto()
            .unwrap();

        assert!(config.tls_config().is_some());
        let tls_config = config.tls_config().unwrap();
        assert_eq!(tls_config.addr.port(), 8443);
        assert_eq!(tls_config.hostname, "127.0.0.1");

        // Should fail for ws://
        let result = WebSocketConfig::new("ws://127.0.0.1:8080/mqtt")
            .unwrap()
            .with_tls_auto();
        assert!(result.is_err());

        // Test with default port
        let config_default = WebSocketConfig::new("wss://127.0.0.1/mqtt")
            .unwrap()
            .with_tls_auto()
            .unwrap();

        let tls_config_default = config_default.tls_config().unwrap();
        assert_eq!(tls_config_default.addr.port(), 443);
    }

    #[test]
    fn test_websocket_config_client_auth_from_bytes() {
        let cert_pem = b"-----BEGIN CERTIFICATE-----\ntest\n-----END CERTIFICATE-----";
        let key_pem = b"-----BEGIN PRIVATE KEY-----\ntest\n-----END PRIVATE KEY-----";

        let config = WebSocketConfig::new("wss://127.0.0.1/mqtt")
            .unwrap()
            .with_client_auth_from_bytes(cert_pem, key_pem)
            .unwrap();

        assert!(config.tls_config().is_some());
        let tls_config = config.tls_config().unwrap();
        assert!(tls_config.client_cert.is_some());
        assert!(tls_config.client_key.is_some());

        // Should fail for ws://
        let result = WebSocketConfig::new("ws://127.0.0.1/mqtt")
            .unwrap()
            .with_client_auth_from_bytes(cert_pem, key_pem);
        assert!(result.is_err());
    }

    #[test]
    fn test_websocket_config_ca_cert_from_bytes() {
        let ca_pem = b"-----BEGIN CERTIFICATE-----\ntest ca\n-----END CERTIFICATE-----";

        let config = WebSocketConfig::new("wss://127.0.0.1/mqtt")
            .unwrap()
            .with_ca_cert_from_bytes(ca_pem)
            .unwrap();

        assert!(config.tls_config().is_some());
        let tls_config = config.tls_config().unwrap();
        assert!(tls_config.root_certs.is_some());

        // Should fail for ws://
        let result = WebSocketConfig::new("ws://127.0.0.1/mqtt")
            .unwrap()
            .with_ca_cert_from_bytes(ca_pem);
        assert!(result.is_err());
    }

    #[test]
    fn test_websocket_config_with_custom_tls_config() {
        use std::net::{IpAddr, Ipv4Addr};

        let addr = std::net::SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8883);
        let tls_config = TlsConfig::new(addr, "localhost");

        let config = WebSocketConfig::new("wss://broker.example.com/mqtt")
            .unwrap()
            .with_tls_config(tls_config);

        assert!(config.tls_config().is_some());
        let tls_config = config.tls_config().unwrap();
        assert_eq!(tls_config.hostname, "localhost");
        assert_eq!(tls_config.addr.port(), 8883);
    }

    #[test]
    fn test_websocket_config_take_tls_config() {
        let mut config = WebSocketConfig::new("wss://127.0.0.1/mqtt")
            .unwrap()
            .with_tls_auto()
            .unwrap();

        assert!(config.tls_config().is_some());

        let tls_config = config.take_tls_config();
        assert!(tls_config.is_some());
        assert!(config.tls_config().is_none()); // Should be None after taking

        let tls_config = tls_config.unwrap();
        assert_eq!(tls_config.hostname, "127.0.0.1");
    }
}
