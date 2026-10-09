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
use crate::transport::packet_io::{decode_buffered_packet, PacketReader, PacketWriter};
use crate::transport::tls::{SystemRoots, TlsConfig};
use crate::Transport;
use bytes::{Buf, Bytes, BytesMut};
use futures_util::{stream::SplitSink, stream::SplitStream, StreamExt};
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    tungstenite::{
        client::IntoClientRequest,
        http::{
            header::{SEC_WEBSOCKET_PROTOCOL, USER_AGENT},
            HeaderName, HeaderValue, Request,
        },
        protocol::Message,
    },
    Connector, MaybeTlsStream, WebSocketStream,
};
use tracing::{debug, error, info, instrument};
use url::Url;

/// Subprotocol always offered. MQTT-6.0.0-3 requires the client to include
/// "mqtt" in the subprotocols it offers, so it is appended when the configured
/// list does not already contain it.
const DEFAULT_SUBPROTOCOL: &str = "mqtt";

/// Headers a custom header may not use. The first five are set by the
/// transport, and a second copy would duplicate or override the handshake.
/// `sec-websocket-extensions` is included because the transport negotiates no
/// extensions and cannot speak one a server accepts, `sec-websocket-accept`
/// because it belongs in the server's response, and `content-length` and
/// `transfer-encoding` because the upgrade request has no body and a server
/// told otherwise would wait for one. The rest are hop-by-hop or
/// expectation headers (`te`, `trailer`, `keep-alive`, `proxy-connection`,
/// `expect`) that describe the connection or a body rather than the upgrade.
const RESERVED_HEADERS: [&str; 14] = [
    "host",
    "connection",
    "upgrade",
    "sec-websocket-version",
    "sec-websocket-key",
    "sec-websocket-extensions",
    "sec-websocket-accept",
    "content-length",
    "transfer-encoding",
    "te",
    "trailer",
    "keep-alive",
    "proxy-connection",
    "expect",
];

/// WebSocket transport configuration
#[derive(Clone)]
pub struct WebSocketConfig {
    /// WebSocket URL (ws:// or wss://)
    pub url: Url,
    /// Connection timeout
    pub timeout: Duration,
    /// Subprotocols to offer, in order of preference (e.g., "mqtt", "mqttv5.0").
    /// "mqtt" is appended when it is not in the list.
    pub subprotocols: Vec<String>,
    /// Custom HTTP headers for the WebSocket handshake
    pub headers: HashMap<String, String>,
    /// User agent string, sent as the `User-Agent` header when set
    pub user_agent: Option<String>,
    /// TLS configuration for secure WebSocket connections (wss://)
    pub tls_config: Option<TlsConfig>,
    /// Adjusts the upgrade request before each connection attempt; see
    /// [`WebSocketConfig::with_request_modifier`]
    pub request_modifier: Option<RequestModifier>,
}

/// Error a [`RequestModifier`] fails with
pub type RequestModifierError = Box<dyn std::error::Error + Send + Sync>;

type ModifiedRequest =
    Pin<Box<dyn Future<Output = std::result::Result<Request<()>, RequestModifierError>> + Send>>;

/// A callback that adjusts the WebSocket upgrade request before each
/// connection attempt, set with [`WebSocketConfig::with_request_modifier`]
#[derive(Clone)]
pub struct RequestModifier(Arc<dyn Fn(Request<()>) -> ModifiedRequest + Send + Sync>);

impl RequestModifier {
    async fn apply(&self, request: Request<()>) -> Result<Request<()>> {
        (self.0)(request).await.map_err(|e| {
            MqttError::ConnectionError(format!("WebSocket request modifier failed: {e}"))
        })
    }
}

impl std::fmt::Debug for RequestModifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RequestModifier(..)")
    }
}

impl std::fmt::Debug for WebSocketConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Header values often carry credentials, so only the names are shown.
        let header_names: Vec<&str> = self.headers.keys().map(String::as_str).collect();
        f.debug_struct("WebSocketConfig")
            .field("url", &self.url.as_str())
            .field("timeout", &self.timeout)
            .field("subprotocols", &self.subprotocols)
            .field("headers", &header_names)
            .field("user_agent", &self.user_agent)
            .field("tls_config", &self.tls_config)
            .field("request_modifier", &self.request_modifier)
            .finish()
    }
}

impl WebSocketConfig {
    /// Creates a new WebSocket configuration
    ///
    /// # Errors
    ///
    /// Returns an error if the URL is invalid or uses an unsupported scheme
    pub fn new(url: &str) -> Result<Self> {
        Ok(Self {
            url: parse_websocket_url(url)?,
            timeout: Duration::from_secs(30),
            subprotocols: vec![DEFAULT_SUBPROTOCOL.to_string()],
            headers: HashMap::new(),
            user_agent: Some(concat!("mqtt5/", env!("CARGO_PKG_VERSION")).to_string()),
            tls_config: None,
            request_modifier: None,
        })
    }

    /// Returns this configuration with its URL replaced
    ///
    /// Used by `MqttClient`, which connects to the address it is given with
    /// the rest of the configuration from
    /// [`ConnectOptions::with_websocket_config`](crate::ConnectOptions::with_websocket_config).
    pub(crate) fn with_url(mut self, url: &str) -> Result<Self> {
        self.url = parse_websocket_url(url)?;
        Ok(self)
    }

    /// Sets the connection timeout
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the WebSocket subprotocols to offer, in order of preference
    ///
    /// "mqtt" is appended to the offer when it is not in the list, as
    /// MQTT-6.0.0-3 requires; the server selects one value, so a broker that
    /// wants another listed subprotocol can still choose it. Each subprotocol
    /// must be an HTTP token and appear only once (RFC 6455 §4.1), or
    /// [`connect`](WebSocketTransport::connect) fails.
    #[must_use]
    pub fn with_subprotocols(mut self, subprotocols: &[&str]) -> Self {
        self.subprotocols = subprotocols
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        self
    }

    /// Sets a single WebSocket subprotocol to offer, in preference to "mqtt"
    ///
    /// "mqtt" is still offered after it, as MQTT-6.0.0-3 requires; see
    /// [`with_subprotocols`](Self::with_subprotocols).
    #[must_use]
    pub fn with_subprotocol(mut self, subprotocol: &str) -> Self {
        self.subprotocols = vec![subprotocol.to_string()];
        self
    }

    /// Adds a custom HTTP header to the WebSocket handshake request
    ///
    /// The name and value are validated when the request is built, so an
    /// invalid header makes [`connect`](WebSocketTransport::connect) fail
    /// rather than this call. Names are matched case-insensitively and must not
    /// repeat. Headers that belong to the handshake itself (`Host`,
    /// `Connection`, `Upgrade`, `Sec-WebSocket-Version`, `Sec-WebSocket-Key`,
    /// `Sec-WebSocket-Extensions`, `Sec-WebSocket-Accept`), would describe a
    /// request body (`Content-Length`, `Transfer-Encoding`, `Trailer`), or are
    /// hop-by-hop or expectation headers (`TE`, `Keep-Alive`,
    /// `Proxy-Connection`, `Expect`) are rejected, as are
    /// `Sec-WebSocket-Protocol` and `User-Agent`, which are set with
    /// [`with_subprotocols`](Self::with_subprotocols) and
    /// [`with_user_agent`](Self::with_user_agent). Custom headers are sent in
    /// name order.
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

    /// Sets a callback that adjusts the upgrade request before each connection
    /// attempt
    ///
    /// The callback receives the validated request that
    /// [`build_handshake_request`](Self::build_handshake_request) produces and
    /// returns the request to send. It can add values computed at connect
    /// time, such as a short-lived signed token, or replace the URI, such as
    /// with a presigned URL. Through
    /// [`ConnectOptions::with_websocket_config`](crate::ConnectOptions::with_websocket_config)
    /// it runs before every connection attempt an `MqttClient` makes,
    /// including automatic reconnects, so those values never go stale.
    ///
    /// The returned request is sent as is: the checks
    /// [`with_header`](Self::with_header) applies do not cover headers the
    /// callback adds, and tungstenite only requires its own handshake headers
    /// to be present once. The connection is made to the returned request's
    /// URI; a callback that points it at another host must update `Host` too.
    /// The callback runs within the [`timeout`](Self::with_timeout), and an
    /// error fails the attempt with `MqttError::ConnectionError`.
    ///
    /// ```rust,no_run
    /// # use mqtt5::transport::websocket::WebSocketConfig;
    /// # async fn sign() -> Result<String, std::io::Error> { Ok(String::new()) }
    /// # fn example() -> mqtt5::Result<()> {
    /// let config = WebSocketConfig::new("wss://broker.example.com/mqtt")?
    ///     .with_header("x-amz-customauthorizer-name", "my-authorizer")
    ///     .with_request_modifier(|mut request| async move {
    ///         let signature = sign().await?;
    ///         request
    ///             .headers_mut()
    ///             .insert("x-amz-customauthorizer-signature", signature.parse()?);
    ///         Ok::<_, Box<dyn std::error::Error + Send + Sync>>(request)
    ///     });
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn with_request_modifier<F, Fut, E>(mut self, modifier: F) -> Self
    where
        F: Fn(Request<()>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = std::result::Result<Request<()>, E>> + Send + 'static,
        E: Into<RequestModifierError>,
    {
        self.request_modifier = Some(RequestModifier(Arc::new(move |request| {
            let pending = modifier(request);
            Box::pin(async move { pending.await.map_err(Into::into) })
        })));
        self
    }

    /// Sets a custom TLS configuration for wss:// connections
    ///
    /// Its root certificates, system-roots setting, server-certificate
    /// verification, client certificate and ALPN protocols are used for the
    /// TLS handshake. With `use_system_roots`, the system roots are the
    /// platform's native root certificates, as for a wss:// connection with no
    /// TLS configuration, rather than the bundled `webpki-roots` that
    /// [`TlsTransport`](crate::transport::tls::TlsTransport) uses.
    /// Its `addr`, `hostname` and `connect_timeout` are not used: the server
    /// and the name verified against its certificate come from the WebSocket
    /// URL, and the timeout from [`with_timeout`](Self::with_timeout).
    ///
    /// ALPN protocols are offered as configured. A WebSocket server negotiates
    /// HTTP, so one that advertises only `http/1.1`, as this crate's broker
    /// does, rejects a handshake offering only an MQTT protocol such as
    /// `mqtt` with `NoApplicationProtocol`.
    ///
    /// Without a TLS configuration, a wss:// connection verifies the server
    /// against the platform's native root certificates.
    #[must_use]
    pub fn with_tls_config(mut self, tls_config: TlsConfig) -> Self {
        self.tls_config = Some(tls_config);
        self
    }

    /// Creates a TLS configuration automatically from the WebSocket URL
    ///
    /// This is a convenience method that creates a TLS config with the same
    /// host and port as the WebSocket URL. The host may be a name or an IP
    /// address. A wss:// connection does not use the config's `addr`, so for a
    /// host name it is the unspecified address `0.0.0.0` with the URL's port,
    /// rather than a resolved one. Set a real address before using the config
    /// with [`TlsTransport`](crate::transport::tls::TlsTransport), which would
    /// otherwise dial the local host.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The URL is not a secure WebSocket (wss://)
    /// - The URL does not have a valid host
    pub fn with_tls_auto(mut self) -> Result<Self> {
        if !self.is_secure() {
            return Err(MqttError::ProtocolError(
                "TLS configuration only applies to wss:// URLs".to_string(),
            ));
        }

        let host = self.host().ok_or_else(|| {
            MqttError::ProtocolError("WebSocket URL must have a host".to_string())
        })?;

        let port = self.port();
        let addr = format!("{host}:{port}").parse().unwrap_or_else(|_| {
            SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), port)
        });

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

        if self.tls_config.is_none() {
            self = self.with_tls_auto()?;
        }

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

        if self.tls_config.is_none() {
            self = self.with_tls_auto()?;
        }

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

        if self.tls_config.is_none() {
            self = self.with_tls_auto()?;
        }

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

        if self.tls_config.is_none() {
            self = self.with_tls_auto()?;
        }

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

    /// Builds the WebSocket handshake request this configuration produces
    ///
    /// The request carries the configured subprotocols followed by "mqtt" when
    /// they do not include it, the user agent when one is set, and every custom
    /// header. Each call generates a fresh `Sec-WebSocket-Key`.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - A subprotocol is not a valid HTTP token or is listed more than once
    /// - The user agent is not a valid header value
    /// - A custom header has an invalid name or value, names a header the
    ///   transport sets itself, or repeats another custom header's name
    pub fn build_handshake_request(&self) -> Result<Request<()>> {
        let mut request = self.url.as_str().into_client_request().map_err(|e| {
            MqttError::ConnectionError(format!("Failed to build WebSocket request: {e}"))
        })?;
        let headers = request.headers_mut();

        let mut offered: Vec<&str> = Vec::with_capacity(self.subprotocols.len() + 1);
        for subprotocol in &self.subprotocols {
            if !is_http_token(subprotocol) {
                return Err(MqttError::Configuration(format!(
                    "Invalid WebSocket subprotocol {subprotocol:?}: must be a non-empty HTTP token"
                )));
            }
            if offered.contains(&subprotocol.as_str()) {
                return Err(MqttError::Configuration(format!(
                    "WebSocket subprotocol {subprotocol:?} is listed more than once"
                )));
            }
            offered.push(subprotocol);
        }
        if !offered.contains(&DEFAULT_SUBPROTOCOL) {
            offered.push(DEFAULT_SUBPROTOCOL);
        }
        let subprotocols = offered.join(", ");
        headers.insert(
            SEC_WEBSOCKET_PROTOCOL,
            header_value("Sec-WebSocket-Protocol", &subprotocols)?,
        );

        if let Some(user_agent) = &self.user_agent {
            headers.insert(USER_AGENT, header_value("User-Agent", user_agent)?);
        }

        // Sorted so the request, and which error a bad configuration reports,
        // do not depend on HashMap iteration order.
        let mut custom: Vec<(&String, &String)> = self.headers.iter().collect();
        custom.sort_unstable();
        for (name, value) in custom {
            let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
                MqttError::Configuration(format!("Invalid WebSocket header name {name:?}: {e}"))
            })?;
            if RESERVED_HEADERS.contains(&header_name.as_str()) {
                return Err(MqttError::Configuration(format!(
                    "WebSocket header {name:?} is reserved for the handshake and cannot be set"
                )));
            }
            if header_name == SEC_WEBSOCKET_PROTOCOL {
                return Err(MqttError::Configuration(format!(
                    "WebSocket header {name:?} cannot be set directly; use with_subprotocols"
                )));
            }
            if header_name == USER_AGENT {
                return Err(MqttError::Configuration(format!(
                    "WebSocket header {name:?} cannot be set directly; use with_user_agent"
                )));
            }
            if headers.contains_key(&header_name) {
                return Err(MqttError::Configuration(format!(
                    "WebSocket header {name:?} is configured more than once \
                     (names are case-insensitive)"
                )));
            }
            headers.insert(header_name, header_value(name, value)?);
        }

        Ok(request)
    }
}

/// Parses a handshake header value, naming the header in the error.
fn header_value(name: &str, value: &str) -> Result<HeaderValue> {
    HeaderValue::from_str(value).map_err(|e| {
        MqttError::Configuration(format!("Invalid value for WebSocket header {name:?}: {e}"))
    })
}

/// Whether `s` is an HTTP token (RFC 9110 §5.6.2), as RFC 6455 §4.1 requires
/// of each offered subprotocol.
/// Parses a WebSocket URL, accepting only the ws and wss schemes
fn parse_websocket_url(url: &str) -> Result<Url> {
    let parsed_url = Url::parse(url)
        .map_err(|e| MqttError::ProtocolError(format!("Invalid WebSocket URL: {e}")))?;

    match parsed_url.scheme() {
        "ws" | "wss" => Ok(parsed_url),
        scheme => Err(MqttError::ProtocolError(format!(
            "Unsupported WebSocket scheme: {scheme}. Use 'ws' or 'wss'"
        ))),
    }
}

fn is_http_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
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

    /// Gets the most preferred configured subprotocol (if any)
    ///
    /// This is the first subprotocol offered, not the one the server selected.
    #[must_use]
    pub fn subprotocol(&self) -> Option<&str> {
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

        let read_handle = WebSocketReadHandle {
            reader: read,
            buffer: BytesMut::from(&self.read_buffer[..]),
        };
        let write_handle = WebSocketWriteHandle { writer: write };

        Ok((read_handle, write_handle))
    }
}

/// WebSocket read handle for split operations
pub struct WebSocketReadHandle {
    reader: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    buffer: BytesMut,
}

/// WebSocket write handle for split operations
pub struct WebSocketWriteHandle {
    writer: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
}

impl WebSocketReadHandle {
    async fn next_binary(&mut self) -> Result<Bytes> {
        loop {
            match self.reader.next().await {
                Some(Ok(Message::Binary(data))) => return Ok(data),
                Some(Ok(Message::Text(_))) => {
                    return Err(MqttError::ProtocolError(
                        "WebSocket text frame received [MQTT-6.0.0-1]".to_string(),
                    ))
                }
                Some(Ok(Message::Close(_))) | None => return Err(MqttError::ClientClosed),
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
                Some(Err(e)) => return Err(MqttError::Io(e.to_string())),
            }
        }
    }

    /// Reads data from the WebSocket.
    ///
    /// # Errors
    /// Returns an error if the connection is closed, a text frame arrives, or a read error occurs.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if self.buffer.is_empty() {
            let data = self.next_binary().await?;
            self.buffer.extend_from_slice(&data);
        }
        let len = self.buffer.len().min(buf.len());
        buf[..len].copy_from_slice(&self.buffer[..len]);
        self.buffer.advance(len);
        Ok(len)
    }

    /// Reads one MQTT packet, reassembling it from the binary frame byte stream
    /// regardless of frame boundaries [MQTT-6.0.0-2].
    ///
    /// # Errors
    /// Returns an error if the connection fails or closes, a text frame arrives,
    /// or the packet is oversized or malformed.
    pub async fn read_packet_limited(
        &mut self,
        protocol_version: u8,
        max_packet_size: usize,
    ) -> Result<Packet> {
        loop {
            if let Some(packet) =
                decode_buffered_packet(&mut self.buffer, protocol_version, max_packet_size)?
            {
                return Ok(packet);
            }
            let data = self.next_binary().await?;
            self.buffer.extend_from_slice(&data);
        }
    }
}

impl WebSocketWriteHandle {
    /// Sends a WebSocket Close frame and closes the sink.
    ///
    /// # Errors
    /// Returns an error if the close handshake cannot be written.
    pub async fn close(&mut self) -> Result<()> {
        use futures_util::SinkExt;
        self.writer
            .close()
            .await
            .map_err(|e| MqttError::Io(e.to_string()))
    }

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
        self.read_packet_limited(protocol_version, usize::MAX).await
    }
}

impl PacketWriter for WebSocketWriteHandle {
    async fn write_packet(&mut self, packet: Packet) -> Result<()> {
        use bytes::BytesMut;
        use futures_util::SinkExt;

        let mut buf = BytesMut::with_capacity(1024);
        crate::transport::packet_io::encode_packet_to_buffer(&packet, &mut buf)?;

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

        let request = self.config.build_handshake_request()?;

        // With no TLS configuration, tokio-tungstenite builds its own client
        // config from the platform's native root certificates.
        let connector = match self.config.tls_config.as_ref() {
            Some(tls_config) if self.config.is_secure() => Some(Connector::Rustls(Arc::new(
                tls_config.client_config_with(SystemRoots::Native)?,
            ))),
            _ => None,
        };

        let handshake = async {
            let request = match &self.config.request_modifier {
                Some(modifier) => modifier.apply(request).await?,
                None => request,
            };
            tokio_tungstenite::connect_async_tls_with_config(request, None, false, connector)
                .await
                .map_err(|e| {
                    error!(error = %e, "WebSocket connection failed");
                    MqttError::ConnectionError(e.to_string())
                })
        };
        // Boxed so the connect futures that await this one stay small.
        let ws_result = tokio::time::timeout(self.config.timeout, Box::pin(handshake)).await;

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
            Ok(Err(e)) => Err(e),
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
                Some(Ok(Message::Text(_))) => {
                    self.connected = false;
                    return Err(MqttError::ProtocolError(
                        "WebSocket text frame received [MQTT-6.0.0-1]".to_string(),
                    ));
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
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
        assert_eq!(config.port(), 443);
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

    fn request_header<'a>(request: &'a Request<()>, name: &str) -> Option<&'a str> {
        request
            .headers()
            .get(name)
            .map(|v| v.to_str().expect("ascii header"))
    }

    fn handshake_error(config: &WebSocketConfig) -> String {
        config
            .build_handshake_request()
            .expect_err("handshake request should be rejected")
            .to_string()
    }

    #[test]
    fn test_handshake_request_defaults() {
        let config = WebSocketConfig::new("ws://localhost:8080/mqtt").unwrap();
        let request = config.build_handshake_request().unwrap();

        assert_eq!(request.uri(), "ws://localhost:8080/mqtt");
        assert_eq!(request_header(&request, "Host"), Some("localhost:8080"));
        assert_eq!(request_header(&request, "Connection"), Some("Upgrade"));
        assert_eq!(request_header(&request, "Upgrade"), Some("websocket"));
        assert_eq!(
            request_header(&request, "Sec-WebSocket-Version"),
            Some("13")
        );
        assert!(request_header(&request, "Sec-WebSocket-Key").is_some());
        assert_eq!(
            request_header(&request, "Sec-WebSocket-Protocol"),
            Some("mqtt")
        );
        assert_eq!(
            request_header(&request, "User-Agent"),
            Some(concat!("mqtt5/", env!("CARGO_PKG_VERSION")))
        );
    }

    #[test]
    fn test_handshake_request_applies_configuration() {
        let config = WebSocketConfig::new("wss://broker.example.com/mqtt")
            .unwrap()
            .with_subprotocols(&["mqttv5.0", "mqtt"])
            .with_header("Authorization", "Bearer token123")
            .with_header("x-amz-customauthorizer-name", "authorizer")
            .with_user_agent("custom-client/1.0");
        let request = config.build_handshake_request().unwrap();

        assert_eq!(
            request_header(&request, "Sec-WebSocket-Protocol"),
            Some("mqttv5.0, mqtt")
        );
        assert_eq!(
            request_header(&request, "Authorization"),
            Some("Bearer token123")
        );
        assert_eq!(
            request_header(&request, "x-amz-customauthorizer-name"),
            Some("authorizer")
        );
        assert_eq!(
            request_header(&request, "User-Agent"),
            Some("custom-client/1.0")
        );
    }

    #[test]
    fn test_handshake_request_offers_mqtt_when_no_subprotocols() {
        let mut config = WebSocketConfig::new("ws://localhost/mqtt").unwrap();
        config.subprotocols.clear();
        let request = config.build_handshake_request().unwrap();

        assert_eq!(
            request_header(&request, "Sec-WebSocket-Protocol"),
            Some("mqtt")
        );
    }

    #[test]
    fn test_handshake_request_omits_unset_user_agent() {
        let mut config = WebSocketConfig::new("ws://localhost/mqtt").unwrap();
        config.user_agent = None;
        let request = config.build_handshake_request().unwrap();

        assert_eq!(request_header(&request, "User-Agent"), None);
    }

    #[test]
    fn test_handshake_request_host_header() {
        let default_port = WebSocketConfig::new("wss://broker.example.com/mqtt").unwrap();
        let request = default_port.build_handshake_request().unwrap();
        assert_eq!(request_header(&request, "Host"), Some("broker.example.com"));

        let with_userinfo = WebSocketConfig::new("ws://user:pass@localhost:8080/mqtt").unwrap();
        let request = with_userinfo.build_handshake_request().unwrap();
        assert_eq!(request_header(&request, "Host"), Some("localhost:8080"));

        let ipv6 = WebSocketConfig::new("ws://[::1]:8080/mqtt").unwrap();
        let request = ipv6.build_handshake_request().unwrap();
        assert_eq!(request_header(&request, "Host"), Some("[::1]:8080"));
    }

    #[test]
    fn test_handshake_request_rejects_reserved_headers() {
        for name in [
            "Host",
            "connection",
            "UPGRADE",
            "Sec-WebSocket-Version",
            "Sec-WebSocket-Key",
            "Sec-WebSocket-Extensions",
            "Sec-WebSocket-Accept",
            "Content-Length",
            "transfer-encoding",
            "TE",
            "Trailer",
            "Keep-Alive",
            "proxy-connection",
            "Expect",
        ] {
            let config = WebSocketConfig::new("ws://localhost/mqtt")
                .unwrap()
                .with_header(name, "value");
            assert!(
                handshake_error(&config).contains("reserved for the handshake"),
                "{name} should be rejected"
            );
        }
    }

    #[test]
    fn test_handshake_request_header_order_is_deterministic() {
        // Each config gets a freshly seeded HashMap, so any dependence on
        // iteration order would show up across iterations.
        for _ in 0..32 {
            let config = WebSocketConfig::new("ws://localhost/mqtt")
                .unwrap()
                .with_header("X-Charlie", "3")
                .with_header("X-Alpha", "1")
                .with_header("X-Bravo", "2");
            let request = config.build_handshake_request().unwrap();
            let custom: Vec<&str> = request
                .headers()
                .keys()
                .map(HeaderName::as_str)
                .filter(|name| name.starts_with("x-"))
                .collect();
            assert_eq!(custom, ["x-alpha", "x-bravo", "x-charlie"]);

            let invalid = WebSocketConfig::new("ws://localhost/mqtt")
                .unwrap()
                .with_header("B-Header", "bad\r\nvalue")
                .with_header("A Header", "value");
            assert!(handshake_error(&invalid).contains("Invalid WebSocket header name"));
        }
    }

    #[test]
    fn test_handshake_request_rejects_headers_with_dedicated_setters() {
        let config = WebSocketConfig::new("ws://localhost/mqtt")
            .unwrap()
            .with_header("sec-websocket-protocol", "mqttv5.0");
        assert!(handshake_error(&config).contains("use with_subprotocols"));

        let config = WebSocketConfig::new("ws://localhost/mqtt")
            .unwrap()
            .with_header("User-Agent", "other/1.0");
        assert!(handshake_error(&config).contains("use with_user_agent"));
    }

    #[test]
    fn test_handshake_request_rejects_case_insensitive_duplicates() {
        let config = WebSocketConfig::new("ws://localhost/mqtt")
            .unwrap()
            .with_header("Authorization", "Bearer a")
            .with_header("authorization", "Bearer b");
        assert!(handshake_error(&config).contains("more than once"));
    }

    #[test]
    fn test_handshake_request_rejects_invalid_headers() {
        let config = WebSocketConfig::new("ws://localhost/mqtt")
            .unwrap()
            .with_header("Bad Name", "value");
        assert!(handshake_error(&config).contains("Invalid WebSocket header name"));

        let config = WebSocketConfig::new("ws://localhost/mqtt")
            .unwrap()
            .with_header("X-Injected", "value\r\nX-Other: smuggled");
        assert!(handshake_error(&config).contains("Invalid value"));

        let config = WebSocketConfig::new("ws://localhost/mqtt")
            .unwrap()
            .with_user_agent("agent\r\n");
        assert!(handshake_error(&config).contains("User-Agent"));
    }

    #[test]
    fn test_handshake_request_rejects_invalid_subprotocols() {
        for subprotocol in ["", "mqtt, mqttv5.0", "mqtt v5", "mqtt\r\n"] {
            let config = WebSocketConfig::new("ws://localhost/mqtt")
                .unwrap()
                .with_subprotocol(subprotocol);
            assert!(
                handshake_error(&config).contains("Invalid WebSocket subprotocol"),
                "{subprotocol:?} should be rejected"
            );
        }
    }

    #[test]
    fn test_handshake_request_appends_mqtt_to_other_subprotocols() {
        let config = WebSocketConfig::new("ws://localhost/mqtt")
            .unwrap()
            .with_subprotocol("mqttv5.0");
        let request = config.build_handshake_request().unwrap();
        assert_eq!(
            request_header(&request, "Sec-WebSocket-Protocol"),
            Some("mqttv5.0, mqtt")
        );

        let config = WebSocketConfig::new("ws://localhost/mqtt")
            .unwrap()
            .with_subprotocols(&["mqtt", "mqttv5.0"]);
        let request = config.build_handshake_request().unwrap();
        assert_eq!(
            request_header(&request, "Sec-WebSocket-Protocol"),
            Some("mqtt, mqttv5.0")
        );
    }

    #[test]
    fn test_handshake_request_rejects_duplicate_subprotocols() {
        for list in [&["mqtt", "mqtt"][..], &["mqttv5.0", "mqtt", "mqttv5.0"][..]] {
            let config = WebSocketConfig::new("ws://localhost/mqtt")
                .unwrap()
                .with_subprotocols(list);
            assert!(
                handshake_error(&config).contains("listed more than once"),
                "{list:?} should be rejected"
            );
        }
    }

    #[test]
    fn test_websocket_config_debug_redacts_header_values() {
        let config = WebSocketConfig::new("ws://localhost/mqtt")
            .unwrap()
            .with_header("Authorization", "Bearer secret-token");
        let debug = format!("{config:?}");

        assert!(debug.contains("Authorization"));
        assert!(!debug.contains("secret-token"));
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

        let result = transport.connect().await;
        assert!(result.is_err());
        assert!(!transport.is_connected());

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

        assert!(transport.close().await.is_ok());
    }

    #[tokio::test]
    async fn test_websocket_transport_close() {
        let config = WebSocketConfig::new("ws://localhost:8080/mqtt").unwrap();
        let mut transport = WebSocketTransport::new(config);

        let _result = transport.connect().await;

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
        let config = WebSocketConfig::new("wss://127.0.0.1:8443/mqtt")
            .unwrap()
            .with_tls_auto()
            .unwrap();

        assert!(config.tls_config().is_some());
        let tls_config = config.tls_config().unwrap();
        assert_eq!(tls_config.addr.port(), 8443);
        assert_eq!(tls_config.hostname, "127.0.0.1");

        let result = WebSocketConfig::new("ws://127.0.0.1:8080/mqtt")
            .unwrap()
            .with_tls_auto();
        assert!(result.is_err());

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
        assert!(config.tls_config().is_none());

        let tls_config = tls_config.unwrap();
        assert_eq!(tls_config.hostname, "127.0.0.1");
    }
}
