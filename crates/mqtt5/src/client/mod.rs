//! MQTT v5.0 Client - Direct Async Implementation
//!
//! This client uses direct async/await patterns throughout.

use crate::callback::{CallbackId, PublishCallback};
use crate::error::{MqttError, Result};
use crate::packet::publish::PublishPacket;
use crate::packet::subscribe::{SubscribePacket, SubscriptionOptions, TopicFilter};
use crate::packet::unsubscribe::UnsubscribePacket;
use crate::protocol::v5::properties::Properties;
#[cfg(not(target_arch = "wasm32"))]
use crate::transport::tcp::TcpConfig;
#[cfg(not(target_arch = "wasm32"))]
use crate::transport::tls::TlsConfig;
#[cfg(not(target_arch = "wasm32"))]
use crate::transport::websocket::{WebSocketConfig, WebSocketTransport};
#[cfg(not(target_arch = "wasm32"))]
use crate::transport::{TcpTransport, TlsTransport, TransportType};
use crate::types::{
    ConnectOptions, ConnectResult, PublishOptions, PublishResult, SubscribeOptions,
};
use crate::QoS;
#[cfg(not(target_arch = "wasm32"))]
use crate::Transport;
use std::future::Future;
use std::net::ToSocketAddrs;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::Duration;
use tracing::instrument;

mod connection;
#[cfg(not(target_arch = "wasm32"))]
mod direct;
mod error_recovery;
pub mod mock;
mod retry;
pub mod r#trait;

pub use self::connection::{ConnectionEvent, DisconnectReason, ReconnectConfig};
pub use self::error_recovery::{ErrorCallback, ErrorRecoveryConfig, RecoverableError, RetryState};
pub use self::mock::{MockCall, MockMqttClient};
pub use self::r#trait::MqttClientTrait;

#[cfg(not(target_arch = "wasm32"))]
use self::direct::DirectClientInner;

/// Type alias for connection event callback
pub type ConnectionEventCallback = Arc<dyn Fn(ConnectionEvent) + Send + Sync>;

/// Thread-safe MQTT v5.0 client
///
/// # Examples
///
/// ## Basic usage
///
/// ```rust,no_run
/// use mqtt5::MqttClient;
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     // Create a client with a unique ID
///     let client = MqttClient::new("my-client-id");
///     
///     // Connect to the broker
///     client.connect("mqtt://localhost:1883").await?;
///     
///     // Subscribe to a topic
///     client.subscribe("temperature/room1", |msg| {
///         println!("Received: {} on topic {}",
///                  String::from_utf8_lossy(&msg.payload),
///                  msg.topic);
///     }).await?;
///     
///     // Publish a message
///     client.publish("temperature/room1", b"22.5").await?;
///     
///     // Disconnect when done
///     client.disconnect().await?;
///     Ok(())
/// }
/// ```
///
/// ## Advanced usage with options
///
/// ```rust,no_run
/// use mqtt5::{MqttClient, ConnectOptions, PublishOptions, QoS};
/// use std::time::Duration;
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     // Create client with custom options
///     let options = ConnectOptions::new("my-client")
///         .with_clean_start(true)
///         .with_keep_alive(Duration::from_secs(30))
///         .with_credentials("user", b"pass");
///     
///     let client = MqttClient::with_options(options);
///     
///     // Connect with TLS
///     client.connect("mqtts://broker.example.com:8883").await?;
///     
///     // Publish with QoS 2 and retain flag
///     let mut pub_options = PublishOptions::default();
///     pub_options.qos = QoS::ExactlyOnce;
///     pub_options.retain = true;
///     
///     client.publish_with_options(
///         "status/device1",
///         b"online",
///         pub_options
///     ).await?;
///     
///     Ok(())
/// }
/// ```
#[derive(Clone)]
pub struct MqttClient {
    /// Shared inner state - uses direct async implementation
    inner: Arc<RwLock<DirectClientInner>>,
    /// Connection event callbacks
    connection_event_callbacks: Arc<RwLock<Vec<ConnectionEventCallback>>>,
    /// Error callbacks
    error_callbacks: Arc<RwLock<Vec<ErrorCallback>>>,
    /// Error recovery configuration
    error_recovery_config: Arc<RwLock<ErrorRecoveryConfig>>,
    /// Connection attempt synchronization - prevents multiple concurrent connection attempts
    connection_mutex: Arc<tokio::sync::Mutex<()>>,
    /// Optional TLS configuration
    tls_config: Arc<RwLock<Option<TlsConfig>>>,
    /// Skip TLS certificate verification (insecure, for testing only)
    insecure_tls: Arc<RwLock<bool>>,
}

impl MqttClient {
    /// Creates a new MQTT client with default options
    ///
    /// # Examples
    ///
    /// ```
    /// use mqtt5::MqttClient;
    ///
    /// let client = MqttClient::new("my-device-001");
    /// ```
    pub fn new(client_id: impl Into<String>) -> Self {
        let client_id_str = client_id.into();
        tracing::trace!(client_id = %client_id_str, "MQTT CLIENT - new() method called");
        let options = ConnectOptions::new(client_id_str); // Use default clean_start=true
        Self::with_options(options)
    }

    /// Creates a new MQTT client with custom options
    ///
    /// # Examples
    ///
    /// ```
    /// use mqtt5::{MqttClient, ConnectOptions};
    /// use std::time::Duration;
    ///
    /// let options = ConnectOptions::new("client-001")
    ///     .with_clean_start(true)
    ///     .with_keep_alive(Duration::from_secs(60))
    ///     .with_credentials("mqtt_user", b"secret");
    ///
    /// let client = MqttClient::with_options(options);
    /// ```
    #[must_use]
    pub fn with_options(options: ConnectOptions) -> Self {
        tracing::trace!(client_id = %options.client_id, "MQTT CLIENT - with_options() method called");
        let inner = DirectClientInner::new(options);

        Self {
            inner: Arc::new(RwLock::new(inner)),
            connection_event_callbacks: Arc::new(RwLock::new(Vec::new())),
            error_callbacks: Arc::new(RwLock::new(Vec::new())),
            error_recovery_config: Arc::new(RwLock::new(ErrorRecoveryConfig::default())),
            connection_mutex: Arc::new(tokio::sync::Mutex::new(())),
            tls_config: Arc::new(RwLock::new(None)),
            insecure_tls: Arc::new(RwLock::new(false)),
        }
    }

    /// Checks if the client is connected
    pub async fn is_connected(&self) -> bool {
        self.inner.read().await.is_connected()
    }

    /// Gets the client ID
    pub async fn client_id(&self) -> String {
        self.inner
            .read()
            .await
            .session
            .read()
            .await
            .client_id()
            .to_string()
    }

    /// Sets a callback for connection events
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use mqtt5::{MqttClient, ConnectionEvent, DisconnectReason};
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = MqttClient::new("my-client");
    ///
    /// client.on_connection_event(|event| {
    ///     match event {
    ///         ConnectionEvent::Connected { session_present } => {
    ///             println!("Connected! Session present: {}", session_present);
    ///         }
    ///         ConnectionEvent::Disconnected { reason } => {
    ///             println!("Disconnected: {:?}", reason);
    ///         }
    ///         ConnectionEvent::Reconnecting { attempt } => {
    ///             println!("Reconnecting attempt {}", attempt);
    ///         }
    ///         ConnectionEvent::ReconnectFailed { error } => {
    ///             println!("Reconnection failed: {}", error);
    ///         }
    ///     }
    /// }).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the callback storage is inaccessible
    pub async fn on_connection_event<F>(&self, callback: F) -> Result<()>
    where
        F: Fn(ConnectionEvent) + Send + Sync + 'static,
    {
        let mut callbacks = self.connection_event_callbacks.write().await;
        callbacks.push(Arc::new(callback));
        Ok(())
    }

    /// Triggers a connection event to all registered callbacks
    async fn trigger_connection_event(&self, event: ConnectionEvent) {
        let callbacks = self.connection_event_callbacks.read().await.clone();
        for callback in callbacks {
            callback(event.clone());
        }
    }

    /// Sets an error callback
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn on_error<F>(&self, callback: F) -> Result<()>
    where
        F: Fn(&MqttError) + Send + Sync + 'static,
    {
        let mut callbacks = self.error_callbacks.write().await;
        callbacks.push(Box::new(callback));
        Ok(())
    }

    /// Set whether to skip TLS certificate verification
    ///
    /// # Safety
    ///
    /// This disables certificate verification and should only be used for testing
    /// with self-signed certificates. Never use in production.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use mqtt5::MqttClient;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = MqttClient::new("my-client");
    ///
    /// // Enable insecure mode for testing
    /// client.set_insecure_tls(true).await;
    ///
    /// // Connect will now skip certificate verification
    /// client.connect("mqtts://test-broker:8883").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn set_insecure_tls(&self, insecure: bool) {
        *self.insecure_tls.write().await = insecure;
    }

    pub async fn set_tls_config(
        &self,
        cert_pem: Option<Vec<u8>>,
        key_pem: Option<Vec<u8>>,
        ca_cert_pem: Option<Vec<u8>>,
    ) {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        tracing::debug!(
            "set_tls_config called - cert: {}, key: {}, ca: {}",
            cert_pem.is_some(),
            key_pem.is_some(),
            ca_cert_pem.is_some()
        );

        let mut config_lock = self.tls_config.write().await;
        let placeholder_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);

        if cert_pem.is_some() || key_pem.is_some() || ca_cert_pem.is_some() {
            let mut config = TlsConfig::new(placeholder_addr, "placeholder");

            if let (Some(cert), Some(key)) = (cert_pem, key_pem) {
                if let Err(e) = config.load_client_cert_pem_bytes(&cert) {
                    tracing::error!("Failed to load client certificate: {}", e);
                    return;
                }
                if let Err(e) = config.load_client_key_pem_bytes(&key) {
                    tracing::error!("Failed to load client key: {}", e);
                    return;
                }
                tracing::debug!("Loaded client cert and key");
            }

            if let Some(ca_cert) = ca_cert_pem {
                if let Err(e) = config.load_ca_cert_pem_bytes(&ca_cert) {
                    tracing::error!("Failed to load CA certificate: {}", e);
                    return;
                }
                config.use_system_roots = false;
                tracing::debug!(
                    "Loaded CA cert, use_system_roots=false, has {} certs",
                    config.root_certs.as_ref().map_or(0, bebytes::Vec::len)
                );
            }

            *config_lock = Some(config);
            tracing::debug!("TLS config stored");
        }
    }

    /// Connects to the MQTT broker with default options
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use mqtt5::MqttClient;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = MqttClient::new("my-client");
    ///
    /// // Connect via TCP
    /// client.connect("mqtt://broker.example.com:1883").await?;
    ///
    /// // Or connect via TLS
    /// // client.connect("mqtts://secure.broker.com:8883").await?;
    ///
    /// // Or use just host:port (defaults to TCP)
    /// // client.connect("localhost:1883").await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if connection fails, address is invalid, or transport cannot be established
    #[instrument(skip(self))]
    pub async fn connect(&self, address: &str) -> Result<()> {
        let client_id = self.client_id().await;
        tracing::trace!(client_id = %client_id, address = %address, "MQTT CLIENT - connect() method called");
        tracing::info!(client_id = %client_id, address = %address, "Initiating MQTT connection");

        let result = {
            // Acquire connection mutex to prevent concurrent connection attempts
            let connection_guard = self.connection_mutex.lock().await;

            let options = self.inner.read().await.options.clone();
            let result = self.connect_with_options_internal(address, options).await;

            // Explicitly drop guard to show we're done with the critical section
            drop(connection_guard);
            result
        };

        match result {
            Ok(connect_result) => {
                tracing::info!(client_id = %client_id, session_present = %connect_result.session_present, "Successfully connected to MQTT broker");
                Ok(())
            }
            Err(e) => {
                tracing::error!(client_id = %client_id, error = %e, "Failed to connect to MQTT broker");
                Err(e)
            }
        }
    }

    /// Connects to the MQTT broker with custom options
    ///
    /// Returns `session_present` flag from CONNACK
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    #[instrument(skip(self, options), fields(client_id = %options.client_id, clean_start = %options.clean_start), level = "debug")]
    pub async fn connect_with_options(
        &self,
        address: &str,
        options: ConnectOptions,
    ) -> Result<ConnectResult> {
        // Acquire connection mutex to prevent concurrent connection attempts
        let connection_guard = self.connection_mutex.lock().await;
        let result = self.connect_with_options_internal(address, options).await;

        // Explicitly drop guard to show we're done with the critical section
        drop(connection_guard);
        result
    }

    /// Internal connection method with custom options (no mutex guard)
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    #[instrument(skip(self, options), fields(client_id = %options.client_id, clean_start = %options.clean_start), level = "debug")]
    async fn connect_with_options_internal(
        &self,
        address: &str,
        options: ConnectOptions,
    ) -> Result<ConnectResult> {
        // Check if already connected
        if self.is_connected().await {
            return Err(MqttError::AlreadyConnected);
        }

        // Update the inner client with new options
        {
            let mut inner = self.inner.write().await;
            inner.options = options.clone();
            // Always store address for potential reconnection
            inner.last_address = Some(address.to_string());
        }

        // Try to connect
        let result = self.connect_internal(address).await;

        // Handle connection result
        if let Err(ref error) = result {
            // For initial connection failures, don't trigger disconnect events
            // Only connections that were previously established should trigger disconnect events

            // Check if this error should trigger automatic reconnection
            let error_recovery_config =
                crate::client::error_recovery::ErrorRecoveryConfig::default();
            if let Some(_recoverable_error) =
                crate::client::error_recovery::RecoverableError::is_recoverable(
                    error,
                    &error_recovery_config,
                )
            {
                // This is a recoverable error and automatic reconnection is enabled
                if options.reconnect_config.enabled {
                    tracing::debug!(error = %error, "🔄 SPAWN MONITOR - Initial connection failed with recoverable error, starting background reconnection");
                    let client = self.clone();
                    tokio::spawn(async move {
                        client.monitor_connection().await;
                    });
                } else {
                    tracing::debug!(error = %error, "Initial connection failed with recoverable error, but automatic reconnection is disabled");
                }
            } else {
                tracing::debug!(error = %error, "Initial connection failed with non-recoverable error, not starting background reconnection");
            }
        } else if result.is_ok() && options.reconnect_config.enabled {
            // Start monitoring for future disconnections only after successful connection
            tracing::debug!("🔄 SPAWN MONITOR - Successful connection, starting monitor task for future disconnections");
            let client = self.clone();
            tokio::spawn(async move {
                client.monitor_connection().await;
            });
        }

        result
    }

    /// Internal connection method that does the actual connection work
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    /// Try to connect to a specific address
    async fn try_connect_address(
        &self,
        addr: std::net::SocketAddr,
        client_transport_type: ClientTransportType,
        host: &str,
    ) -> Result<TransportType> {
        match client_transport_type {
            ClientTransportType::Tcp => {
                let config = TcpConfig::new(addr);
                let mut tcp_transport = TcpTransport::new(config);
                tcp_transport
                    .connect()
                    .await
                    .map_err(|e| MqttError::ConnectionError(format!("TCP connect failed: {e}")))?;
                Ok(TransportType::Tcp(tcp_transport))
            }
            ClientTransportType::Tls => {
                let insecure = *self.insecure_tls.read().await;
                let tls_config_lock = self.tls_config.read().await;
                let config = if let Some(existing_config) = &*tls_config_lock {
                    tracing::debug!(
                        "Using stored TLS config - use_system_roots: {}, has_ca: {}, has_cert: {}",
                        existing_config.use_system_roots,
                        existing_config.root_certs.is_some(),
                        existing_config.client_cert.is_some()
                    );
                    let mut cfg = existing_config.clone();
                    cfg.addr = addr;
                    cfg.hostname = host.to_string();
                    cfg.verify_server_cert = !insecure;
                    cfg
                } else {
                    tracing::debug!("No stored TLS config, using default");
                    TlsConfig::new(addr, host).with_verify_server_cert(!insecure)
                };
                drop(tls_config_lock);

                let mut tls_transport = TlsTransport::new(config);
                tls_transport
                    .connect()
                    .await
                    .map_err(|e| MqttError::ConnectionError(format!("TLS connect failed: {e}")))?;
                Ok(TransportType::Tls(Box::new(tls_transport)))
            }
            ClientTransportType::WebSocket(url) => {
                let config = WebSocketConfig::new(&url).map_err(|e| {
                    MqttError::ConnectionError(format!("Invalid WebSocket URL: {e}"))
                })?;
                let mut ws_transport = WebSocketTransport::new(config);
                ws_transport.connect().await.map_err(|e| {
                    MqttError::ConnectionError(format!("WebSocket connect failed: {e}"))
                })?;
                Ok(TransportType::WebSocket(Box::new(ws_transport)))
            }
            ClientTransportType::WebSocketSecure(url) => {
                let insecure = *self.insecure_tls.read().await;
                let mut config = WebSocketConfig::new(&url).map_err(|e| {
                    MqttError::ConnectionError(format!("Invalid WebSocket URL: {e}"))
                })?;

                if insecure {
                    let tls_config = TlsConfig::new(addr, host).with_verify_server_cert(false);
                    config = config.with_tls_config(tls_config);
                }

                let mut ws_transport = WebSocketTransport::new(config);
                ws_transport.connect().await.map_err(|e| {
                    MqttError::ConnectionError(format!("WebSocket connect failed: {e}"))
                })?;
                Ok(TransportType::WebSocket(Box::new(ws_transport)))
            }
        }
    }

    async fn connect_internal(&self, address: &str) -> Result<ConnectResult> {
        let client_id = self.inner.read().await.options.client_id.clone();
        tracing::debug!(
            address = %address,
            client_id = %client_id,
            "🔄 CONNECTION ATTEMPT - Tracking source of connection attempt"
        );

        let (client_transport_type, host, port) = Self::parse_address(address)?;
        let addrs = self.resolve_addresses(host, port)?;
        let addresses_to_try = self.select_addresses_for_connection(&addrs, host);

        self.try_connect_to_addresses(addresses_to_try, client_transport_type, host)
            .await
    }

    fn resolve_addresses(&self, host: &str, port: u16) -> Result<Vec<std::net::SocketAddr>> {
        let addr_str = format!("{host}:{port}");
        tracing::debug!(addr_str = %addr_str, "🌐 DNS RESOLUTION - Starting address resolution");

        let addrs: Vec<_> = addr_str
            .to_socket_addrs()
            .map_err(|e| {
                tracing::error!(addr_str = %addr_str, error = %e, "🌐 DNS RESOLUTION - Failed to resolve address");
                MqttError::ConnectionError(format!("Failed to resolve address: {e}"))
            })?
            .collect();

        tracing::debug!(addr_str = %addr_str, resolved_count = addrs.len(), "🌐 DNS RESOLUTION - Address resolved successfully");

        if addrs.is_empty() {
            return Err(MqttError::ConnectionError(
                "No valid address found".to_string(),
            ));
        }

        Ok(addrs)
    }

    fn select_addresses_for_connection<'a>(
        &self,
        addrs: &'a [std::net::SocketAddr],
        host: &str,
    ) -> &'a [std::net::SocketAddr] {
        let is_aws_iot = Self::is_aws_iot_endpoint(host);

        if is_aws_iot {
            tracing::debug!("AWS IoT endpoint detected, limiting to first resolved address");
            &addrs[0..1]
        } else {
            addrs
        }
    }

    async fn try_connect_to_addresses(
        &self,
        addresses: &[std::net::SocketAddr],
        transport_type: ClientTransportType,
        host: &str,
    ) -> Result<ConnectResult> {
        let mut last_error = None;

        for addr in addresses {
            tracing::debug!("Trying to connect to address: {}", addr);

            let transport = match self
                .try_connect_address(*addr, transport_type.clone(), host)
                .await
            {
                Ok(t) => t,
                Err(e) => {
                    tracing::debug!("Failed to connect to {}: {}", addr, e);
                    last_error = Some(e);
                    continue;
                }
            };

            self.reset_reconnect_counter().await;

            let mut inner = self.inner.write().await;
            match inner.connect(transport).await {
                Ok(result) => {
                    let stored_subs = inner.stored_subscriptions.read().await.clone();
                    let session_present = result.session_present;
                    drop(inner);

                    self.trigger_connection_event(ConnectionEvent::Connected { session_present })
                        .await;
                    self.restore_subscriptions_after_connect(stored_subs, session_present)
                        .await;

                    return Ok(result);
                }
                Err(e) => {
                    drop(inner);
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            MqttError::ConnectionError("Failed to connect to any address".to_string())
        }))
    }

    async fn reset_reconnect_counter(&self) {
        let mut inner = self.inner.write().await;
        inner.reconnect_attempt = 0;
    }

    async fn restore_subscriptions_after_connect(
        &self,
        stored_subs: Vec<(String, SubscriptionOptions, CallbackId)>,
        session_present: bool,
    ) {
        if stored_subs.is_empty() {
            return;
        }

        if session_present {
            tracing::info!("Session resumed, restoring {} callbacks", stored_subs.len());
            let inner = self.inner.read().await;
            for (topic, _, callback_id) in stored_subs {
                if let Err(e) = inner.callback_manager.restore_callback(callback_id).await {
                    tracing::warn!("Failed to restore callback for {}: {}", topic, e);
                }
            }
        } else {
            tracing::info!(
                "Session not resumed, restoring {} subscriptions",
                stored_subs.len()
            );
            for (topic, options, callback_id) in stored_subs {
                if let Err(e) = self
                    .resubscribe_internal(&topic, options, callback_id)
                    .await
                {
                    tracing::warn!("Failed to restore subscription to {}: {}", topic, e);
                }
            }
        }
    }

    /// Internal connection method using custom TLS configuration
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    async fn connect_internal_with_tls(
        &self,
        tls_config: crate::transport::tls::TlsConfig,
    ) -> Result<ConnectResult> {
        // Create TLS transport directly from config
        let mut tls_transport = crate::transport::tls::TlsTransport::new(tls_config);
        tls_transport
            .connect()
            .await
            .map_err(|e| MqttError::ConnectionError(format!("TLS connect failed: {e}")))?;

        let transport = TransportType::Tls(Box::new(tls_transport));

        // Reset reconnect attempt counter
        {
            let mut inner = self.inner.write().await;
            inner.reconnect_attempt = 0;
        }

        // Try to connect using direct async method
        let mut inner = self.inner.write().await;
        match inner.connect(transport).await {
            Ok(result) => {
                // Get stored subscriptions before releasing the lock
                let stored_subs = inner.stored_subscriptions.read().await.clone();
                let session_present = result.session_present;
                drop(inner); // Release lock before potentially resubscribing

                // Trigger connected event
                self.trigger_connection_event(ConnectionEvent::Connected { session_present })
                    .await;

                // Restore callbacks and subscriptions
                if !stored_subs.is_empty() {
                    if session_present {
                        // Session was resumed - only restore callbacks
                        tracing::info!(
                            "Session resumed, restoring {} callbacks",
                            stored_subs.len()
                        );
                        let inner = self.inner.read().await;
                        for (topic, _, callback_id) in stored_subs {
                            if let Err(e) =
                                inner.callback_manager.restore_callback(callback_id).await
                            {
                                tracing::warn!("Failed to restore callback for {}: {}", topic, e);
                            }
                        }
                    } else {
                        // Session was not resumed - need to resubscribe and restore callbacks
                        tracing::info!(
                            "Session not resumed, restoring {} subscriptions",
                            stored_subs.len()
                        );
                        for (topic, options, callback_id) in stored_subs {
                            if let Err(e) = self
                                .resubscribe_internal(&topic, options, callback_id)
                                .await
                            {
                                tracing::warn!(
                                    "Failed to restore subscription to {}: {}",
                                    topic,
                                    e
                                );
                            }
                        }
                    }
                }

                Ok(result)
            }
            Err(e) => {
                drop(inner);
                Err(e)
            }
        }
    }

    /// Connects to the MQTT broker using a custom TLS configuration
    ///
    /// This method allows direct configuration of TLS settings including certificates,
    /// ALPN protocols, and other TLS-specific options.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use mqtt5::{MqttClient, transport::tls::TlsConfig};
    /// # use std::net::SocketAddr;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = MqttClient::new("my-client");
    ///
    /// let mut tls_config = TlsConfig::new(
    ///     "broker.example.com:8883".parse()?,
    ///     "broker.example.com"
    /// );
    /// tls_config.load_client_cert_pem("client-cert.pem")?;
    /// tls_config.load_client_key_pem("client-key.pem")?;
    /// tls_config.load_ca_cert_pem("ca-cert.pem")?;
    /// tls_config = tls_config.with_alpn_protocols(&["x-amzn-mqtt-ca"]);
    ///
    /// client.connect_with_tls(tls_config).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if connection fails or TLS configuration is invalid
    pub async fn connect_with_tls(
        &self,
        tls_config: crate::transport::tls::TlsConfig,
    ) -> Result<()> {
        // Acquire connection mutex to prevent concurrent connection attempts
        let connection_guard = self.connection_mutex.lock().await;

        let options = self.inner.read().await.options.clone();
        let result = self
            .connect_with_tls_and_options_internal(tls_config, options)
            .await;

        // Explicitly drop guard to show we're done with the critical section
        drop(connection_guard);
        result.map(|_| ())
    }

    /// Connects to the MQTT broker using custom TLS configuration and connect options
    ///
    /// This method combines custom TLS settings with MQTT connection options.
    /// Returns `session_present` flag from CONNACK
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use mqtt5::{MqttClient, ConnectOptions, transport::tls::TlsConfig};
    /// # use std::net::SocketAddr;
    /// # use std::time::Duration;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut options = ConnectOptions::new("aws-iot-client")
    ///     .with_clean_start(false)
    ///     .with_keep_alive(Duration::from_secs(30));
    /// options.properties.maximum_packet_size = Some(131072); // 128KB for AWS IoT
    ///
    /// let mut tls_config = TlsConfig::new(
    ///     "your-endpoint.iot.us-east-1.amazonaws.com:443".parse()?,
    ///     "your-endpoint.iot.us-east-1.amazonaws.com"
    /// );
    /// tls_config.load_client_cert_pem("device-cert.pem")?;
    /// tls_config.load_client_key_pem("device-key.pem")?;
    /// tls_config.load_ca_cert_pem("AmazonRootCA1.pem")?;
    /// tls_config = tls_config.with_alpn_protocols(&["x-amzn-mqtt-ca"]);
    ///
    /// let client = MqttClient::with_options(options.clone());
    /// let result = client.connect_with_tls_and_options(tls_config, options).await?;
    /// println!("Connected! Session present: {}", result.session_present);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if connection fails or configuration is invalid
    pub async fn connect_with_tls_and_options(
        &self,
        tls_config: crate::transport::tls::TlsConfig,
        options: ConnectOptions,
    ) -> Result<ConnectResult> {
        // Acquire connection mutex to prevent concurrent connection attempts
        let connection_guard = self.connection_mutex.lock().await;
        let result = self
            .connect_with_tls_and_options_internal(tls_config, options)
            .await;

        // Explicitly drop guard to show we're done with the critical section
        drop(connection_guard);
        result
    }

    /// Internal TLS connection method (no mutex guard)
    ///
    /// # Errors
    ///
    /// Returns an error if connection fails or configuration is invalid
    async fn connect_with_tls_and_options_internal(
        &self,
        tls_config: crate::transport::tls::TlsConfig,
        options: ConnectOptions,
    ) -> Result<ConnectResult> {
        // Check if already connected
        if self.is_connected().await {
            return Err(MqttError::AlreadyConnected);
        }

        // Update the inner client with new options
        {
            let mut inner = self.inner.write().await;
            inner.options = options.clone();
            // Store address for potential reconnection
            inner.last_address = Some(format!(
                "{}:{}",
                tls_config.hostname,
                tls_config.addr.port()
            ));
        }

        // Try to connect with TLS config
        let result = self.connect_internal_with_tls(tls_config).await;

        // Handle reconnection if enabled and initial connection fails
        if let Err(ref error) = result {
            if options.reconnect_config.enabled {
                // Trigger initial disconnection event
                self.trigger_connection_event(ConnectionEvent::Disconnected {
                    reason: DisconnectReason::NetworkError(error.to_string()),
                })
                .await;

                // Start reconnection attempts in background
                // Note: For TLS config reconnection, we'd need to store the TLS config
                // This is a limitation that could be addressed in future versions
                tracing::warn!(
                    "Automatic reconnection with custom TLS config is not yet supported"
                );
            }
        }

        result
    }

    /// Disconnects from the MQTT broker
    ///
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    #[instrument(skip(self))]
    pub async fn disconnect(&self) -> Result<()> {
        let client_id = self.client_id().await;
        tracing::info!(client_id = %client_id, "Initiating MQTT disconnect");

        let mut inner = self.inner.write().await;
        match inner.disconnect().await {
            Ok(()) => {
                tracing::info!(client_id = %client_id, "Successfully disconnected from MQTT broker");

                // Trigger disconnected event
                self.trigger_connection_event(ConnectionEvent::Disconnected {
                    reason: DisconnectReason::ClientInitiated,
                })
                .await;

                Ok(())
            }
            Err(e) => {
                tracing::error!(client_id = %client_id, error = %e, "Failed to disconnect from MQTT broker");
                Err(e)
            }
        }
    }

    /// Publishes a message to a topic
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use mqtt5::MqttClient;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = MqttClient::new("my-client");
    /// client.connect("mqtt://localhost:1883").await?;
    ///
    /// // Publish a simple string message
    /// client.publish("sensors/temperature", "23.5°C").await?;
    ///
    /// // Publish binary data
    /// let data = vec![0x01, 0x02, 0x03, 0x04];
    /// client.publish("sensors/binary", data).await?;
    ///
    /// // Publish JSON
    /// let json = r#"{"temperature": 23.5, "humidity": 45}"#;
    /// client.publish("sensors/json", json).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    #[instrument(skip(self, topic, payload))]
    pub async fn publish(
        &self,
        topic: impl Into<String>,
        payload: impl Into<Vec<u8>>,
    ) -> Result<PublishResult> {
        let options = PublishOptions::default();
        self.publish_with_options(topic, payload, options).await
    }

    /// Publishes a message to a topic with specific `QoS` (compatibility method)
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn publish_qos(
        &self,
        topic: impl Into<String>,
        payload: impl Into<Vec<u8>>,
        qos: QoS,
    ) -> Result<PublishResult> {
        let options = PublishOptions {
            qos,
            ..Default::default()
        };
        self.publish_with_options(topic, payload, options).await
    }

    /// Publishes a message with custom options
    ///
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    #[instrument(skip(self, topic, payload, options), fields(qos = ?options.qos, retain = %options.retain))]
    pub async fn publish_with_options(
        &self,
        topic: impl Into<String>,
        payload: impl Into<Vec<u8>>,
        options: PublishOptions,
    ) -> Result<PublishResult> {
        let topic_str = topic.into();
        let payload_vec = payload.into();
        let client_id = self.client_id().await;

        tracing::debug!(
            client_id = %client_id,
            topic = %topic_str,
            payload_len = payload_vec.len(),
            qos = ?options.qos,
            retain = %options.retain,
            "Publishing MQTT message"
        );

        // Direct publish - no command channels!
        let inner = self.inner.read().await;
        match inner.publish(topic_str.clone(), payload_vec, options).await {
            Ok(result) => {
                match &result {
                    PublishResult::QoS0 => {
                        tracing::debug!(client_id = %client_id, topic = %topic_str, "Published QoS0 message");
                    }
                    PublishResult::QoS1Or2 { packet_id } => {
                        tracing::debug!(client_id = %client_id, topic = %topic_str, packet_id = %packet_id, "Published QoS1/2 message");
                    }
                }
                Ok(result)
            }
            Err(e) => {
                tracing::error!(client_id = %client_id, topic = %topic_str, error = %e, "Failed to publish MQTT message");
                Err(e)
            }
        }
    }

    /// Subscribes to a topic with a callback
    ///
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use mqtt5::MqttClient;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = MqttClient::new("my-client");
    /// client.connect("mqtt://localhost:1883").await?;
    ///
    /// // Subscribe to a specific topic
    /// client.subscribe("sensors/temperature", |msg| {
    ///     println!("Temperature: {}", String::from_utf8_lossy(&msg.payload));
    /// }).await?;
    ///
    /// // Subscribe with wildcards
    /// client.subscribe("sensors/+/status", |msg| {
    ///     println!("Status update on {}: {}", msg.topic,
    ///              String::from_utf8_lossy(&msg.payload));
    /// }).await?;
    ///
    /// // Subscribe to all topics under sensors/
    /// client.subscribe("sensors/#", |msg| {
    ///     println!("Sensor data: {} = {}", msg.topic,
    ///              String::from_utf8_lossy(&msg.payload));
    /// }).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if subscription fails, topic filter is invalid, or client is not connected
    #[instrument(skip(self, topic_filter, callback))]
    pub async fn subscribe<F>(
        &self,
        topic_filter: impl Into<String>,
        callback: F,
    ) -> Result<(u16, QoS)>
    where
        F: Fn(crate::types::Message) + Send + Sync + 'static,
    {
        let options = SubscribeOptions::default();
        self.subscribe_with_options(topic_filter, options, callback)
            .await
    }

    /// Subscribes to a topic with custom options and a callback (using Message type)
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    #[instrument(skip(self, topic_filter, options, callback), fields(qos = ?options.qos), level = "debug")]
    pub async fn subscribe_with_options<F>(
        &self,
        topic_filter: impl Into<String>,
        options: SubscribeOptions,
        callback: F,
    ) -> Result<(u16, QoS)>
    where
        F: Fn(crate::types::Message) + Send + Sync + 'static,
    {
        let topic_filter_str = topic_filter.into();
        let client_id = self.client_id().await;

        tracing::debug!(
            client_id = %client_id,
            topic_filter = %topic_filter_str,
            qos = ?options.qos,
            "Subscribing to MQTT topic"
        );

        // Wrap the callback to convert PublishPacket to Message
        let wrapped_callback = move |packet: PublishPacket| {
            let msg = crate::types::Message::from(packet);
            callback(msg);
        };

        match self
            .subscribe_with_options_raw(topic_filter_str.clone(), options, wrapped_callback)
            .await
        {
            Ok((packet_id, granted_qos)) => {
                tracing::debug!(
                    client_id = %client_id,
                    topic_filter = %topic_filter_str,
                    packet_id = %packet_id,
                    granted_qos = ?granted_qos,
                    "Successfully subscribed to MQTT topic"
                );
                Ok((packet_id, granted_qos))
            }
            Err(e) => {
                tracing::error!(
                    client_id = %client_id,
                    topic_filter = %topic_filter_str,
                    error = %e,
                    "Failed to subscribe to MQTT topic"
                );
                Err(e)
            }
        }
    }

    /// Internal method that accepts `PublishPacket` callbacks
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    async fn subscribe_with_options_raw<F>(
        &self,
        topic_filter: impl Into<String>,
        options: SubscribeOptions,
        callback: F,
    ) -> Result<(u16, QoS)>
    where
        F: Fn(PublishPacket) + Send + Sync + 'static,
    {
        let topic_filter = topic_filter.into();

        // Register callback first and get callback ID
        let inner = self.inner.read().await;
        let callback: PublishCallback = Arc::new(callback);
        let callback_id = inner
            .callback_manager
            .register_with_id(topic_filter.clone(), callback)
            .await?;

        // Create subscribe packet
        let filter = TopicFilter {
            filter: topic_filter.clone(),
            options: crate::packet::subscribe::SubscriptionOptions {
                qos: options.qos,
                no_local: options.no_local,
                retain_as_published: options.retain_as_published,
                retain_handling: match options.retain_handling {
                    crate::types::RetainHandling::SendAtSubscribe => {
                        crate::packet::subscribe::RetainHandling::SendAtSubscribe
                    }
                    crate::types::RetainHandling::SendIfNew => {
                        crate::packet::subscribe::RetainHandling::SendAtSubscribeIfNew
                    }
                    crate::types::RetainHandling::DontSend => {
                        crate::packet::subscribe::RetainHandling::DoNotSend
                    }
                },
            },
        };

        let mut packet = SubscribePacket {
            packet_id: 0, // Will be assigned in subscribe method
            filters: vec![filter],
            properties: Properties::default(),
        };

        // Add subscription identifier if provided
        if let Some(sub_id) = options.subscription_identifier {
            packet = packet.with_subscription_identifier(sub_id);
        }

        // Direct subscribe with callback ID - no command channels!
        match inner.subscribe_with_callback(packet, callback_id).await {
            Ok(results) => {
                if let Some(&(packet_id, qos)) = results.first() {
                    Ok((packet_id, qos))
                } else {
                    // Unregister callback on failure
                    inner.callback_manager.unregister(&topic_filter).await?;
                    Err(MqttError::ProtocolError(
                        "No results returned for subscription".to_string(),
                    ))
                }
            }
            Err(e) => {
                // Unregister callback on failure
                inner.callback_manager.unregister(&topic_filter).await?;
                Err(e)
            }
        }
    }

    /// Unsubscribes from a topic
    ///
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    #[instrument(skip(self, topic_filter))]
    pub async fn unsubscribe(&self, topic_filter: impl Into<String>) -> Result<()> {
        let topic_filter = topic_filter.into();
        let client_id = self.client_id().await;

        tracing::info!(
            client_id = %client_id,
            topic_filter = %topic_filter,
            "Unsubscribing from MQTT topic"
        );

        // Unregister callback first
        let inner = self.inner.read().await;
        inner.callback_manager.unregister(&topic_filter).await?;

        // Create unsubscribe packet
        let packet = UnsubscribePacket {
            packet_id: 0, // Will be assigned in unsubscribe method
            filters: vec![topic_filter.clone()],
            properties: Properties::default(),
        };

        // Direct unsubscribe - no command channels!
        match inner.unsubscribe(packet).await {
            Ok(()) => {
                tracing::info!(
                    client_id = %client_id,
                    topic_filter = %topic_filter,
                    "Successfully unsubscribed from MQTT topic"
                );
                Ok(())
            }
            Err(e) => {
                tracing::error!(
                    client_id = %client_id,
                    topic_filter = %topic_filter,
                    error = %e,
                    "Failed to unsubscribe from MQTT topic"
                );
                Err(e)
            }
        }
    }

    /// Subscribe to multiple topics at once
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn subscribe_many<F>(
        &self,
        topics: Vec<(&str, QoS)>,
        callback: F,
    ) -> Result<Vec<(u16, QoS)>>
    where
        F: Fn(crate::types::Message) + Send + Sync + 'static + Clone,
    {
        let mut results = Vec::new();
        for (topic, qos) in topics {
            let opts = SubscribeOptions {
                qos,
                ..Default::default()
            };
            let result = self
                .subscribe_with_options(topic, opts, callback.clone())
                .await?;
            results.push(result);
        }
        Ok(results)
    }

    /// Unsubscribe from multiple topics at once
    ///
    /// Returns a vector of results, one for each topic. Each result contains the topic
    /// and whether the unsubscribe operation succeeded for that topic.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn unsubscribe_many(&self, topics: Vec<&str>) -> Result<Vec<(String, Result<()>)>> {
        let mut results = Vec::with_capacity(topics.len());

        for topic in topics {
            let topic_string = topic.to_string();
            let result = self.unsubscribe(topic).await;
            results.push((topic_string, result));
        }

        Ok(results)
    }

    /// Publish a retained message
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn publish_retain(
        &self,
        topic: impl Into<String>,
        payload: impl Into<Vec<u8>>,
    ) -> Result<PublishResult> {
        let opts = PublishOptions {
            retain: true,
            ..Default::default()
        };
        self.publish_with_options(topic, payload, opts).await
    }

    /// Publish with `QoS` 0 (convenience method)
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn publish_qos0(
        &self,
        topic: impl Into<String>,
        payload: impl Into<Vec<u8>>,
    ) -> Result<PublishResult> {
        self.publish_qos(topic, payload, QoS::AtMostOnce).await
    }

    /// Publish with `QoS` 1 (convenience method)
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn publish_qos1(
        &self,
        topic: impl Into<String>,
        payload: impl Into<Vec<u8>>,
    ) -> Result<PublishResult> {
        self.publish_qos(topic, payload, QoS::AtLeastOnce).await
    }

    /// Publish with `QoS` 2 (convenience method)
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn publish_qos2(
        &self,
        topic: impl Into<String>,
        payload: impl Into<Vec<u8>>,
    ) -> Result<PublishResult> {
        self.publish_qos(topic, payload, QoS::ExactlyOnce).await
    }

    /// Check if message queuing is enabled
    pub async fn is_queue_on_disconnect(&self) -> bool {
        let inner = self.inner.read().await;
        inner.is_queue_on_disconnect()
    }

    /// Set whether to queue messages when disconnected
    pub async fn set_queue_on_disconnect(&self, enabled: bool) {
        let mut inner = self.inner.write().await;
        inner.set_queue_on_disconnect(enabled);
    }

    /// Get error recovery configuration
    pub async fn error_recovery_config(&self) -> ErrorRecoveryConfig {
        self.error_recovery_config.read().await.clone()
    }

    /// Set error recovery configuration
    pub async fn set_error_recovery_config(&self, config: ErrorRecoveryConfig) {
        *self.error_recovery_config.write().await = config;
    }

    /// Clear all error callbacks
    pub async fn clear_error_callbacks(&self) {
        self.error_callbacks.write().await.clear();
    }

    /// Clear all connection event callbacks
    pub async fn clear_connection_event_callbacks(&self) {
        self.connection_event_callbacks.write().await.clear();
    }

    /// Get direct access to session state (for testing)
    #[cfg(test)]
    pub async fn session_state(&self) -> Arc<RwLock<crate::session::SessionState>> {
        Arc::clone(&self.inner.read().await.session)
    }
}

/// Implementation of `MqttClientTrait` for `MqttClient`
#[allow(clippy::manual_async_fn)]
impl MqttClientTrait for MqttClient {
    fn is_connected(&self) -> impl Future<Output = bool> + Send + '_ {
        async move { self.is_connected().await }
    }

    fn client_id(&self) -> impl Future<Output = String> + Send + '_ {
        async move { self.client_id().await }
    }

    fn connect<'a>(&'a self, address: &'a str) -> impl Future<Output = Result<()>> + Send + 'a {
        async move { self.connect(address).await }
    }

    fn connect_with_options<'a>(
        &'a self,
        address: &'a str,
        options: ConnectOptions,
    ) -> impl Future<Output = Result<ConnectResult>> + Send + 'a {
        async move { self.connect_with_options(address, options).await }
    }

    fn disconnect(&self) -> impl Future<Output = Result<()>> + Send + '_ {
        async move { self.disconnect().await }
    }

    fn publish<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a {
        async move { self.publish(topic, payload).await }
    }

    fn publish_qos<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
        qos: QoS,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a {
        async move { self.publish_qos(topic, payload, qos).await }
    }

    fn publish_with_options<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
        options: PublishOptions,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a {
        async move { self.publish_with_options(topic, payload, options).await }
    }

    fn subscribe<'a, F>(
        &'a self,
        topic_filter: impl Into<String> + Send + 'a,
        callback: F,
    ) -> impl Future<Output = Result<(u16, QoS)>> + Send + 'a
    where
        F: Fn(crate::types::Message) + Send + Sync + 'static,
    {
        async move { self.subscribe(topic_filter, callback).await }
    }

    fn subscribe_with_options<'a, F>(
        &'a self,
        topic_filter: impl Into<String> + Send + 'a,
        options: SubscribeOptions,
        callback: F,
    ) -> impl Future<Output = Result<(u16, QoS)>> + Send + 'a
    where
        F: Fn(crate::types::Message) + Send + Sync + 'static,
    {
        async move {
            self.subscribe_with_options(topic_filter, options, callback)
                .await
        }
    }

    fn unsubscribe<'a>(
        &'a self,
        topic_filter: impl Into<String> + Send + 'a,
    ) -> impl Future<Output = Result<()>> + Send + 'a {
        async move { self.unsubscribe(topic_filter).await }
    }

    fn subscribe_many<'a, F>(
        &'a self,
        topics: Vec<(&'a str, QoS)>,
        callback: F,
    ) -> impl Future<Output = Result<Vec<(u16, QoS)>>> + Send + 'a
    where
        F: Fn(crate::types::Message) + Send + Sync + 'static + Clone,
    {
        async move { self.subscribe_many(topics, callback).await }
    }

    fn unsubscribe_many<'a>(
        &'a self,
        topics: Vec<&'a str>,
    ) -> impl Future<Output = Result<Vec<(String, Result<()>)>>> + Send + 'a {
        async move { self.unsubscribe_many(topics).await }
    }

    fn publish_retain<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a {
        async move { self.publish_retain(topic, payload).await }
    }

    fn publish_qos0<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a {
        async move { self.publish_qos0(topic, payload).await }
    }

    fn publish_qos1<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a {
        async move { self.publish_qos1(topic, payload).await }
    }

    fn publish_qos2<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a {
        async move { self.publish_qos2(topic, payload).await }
    }

    fn is_queue_on_disconnect(&self) -> impl Future<Output = bool> + Send + '_ {
        async move { self.is_queue_on_disconnect().await }
    }

    fn set_queue_on_disconnect(&self, enabled: bool) -> impl Future<Output = ()> + Send + '_ {
        async move { self.set_queue_on_disconnect(enabled).await }
    }
}

impl MqttClient {
    /// Simulate abnormal disconnection (for testing will messages)
    /// This method closes the connection without sending a DISCONNECT packet,
    /// which causes the broker to send the will message.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn disconnect_abnormally(&self) -> Result<()> {
        let mut inner = self.inner.write().await;
        inner.disconnect_with_packet(false).await
    }

    /// Detects if the hostname is an AWS IoT endpoint
    fn is_aws_iot_endpoint(hostname: &str) -> bool {
        // AWS IoT Core endpoints follow the pattern: *.iot.*.amazonaws.com
        hostname.contains(".iot.") && hostname.ends_with(".amazonaws.com")
    }

    /// Parses an address string to determine transport type and components
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    fn parse_address(address: &str) -> Result<(ClientTransportType, &str, u16)> {
        if let Some(rest) = address.strip_prefix("mqtt://") {
            let (host, port) = Self::split_host_port(rest, 1883)?;
            Ok((ClientTransportType::Tcp, host, port))
        } else if let Some(rest) = address.strip_prefix("mqtts://") {
            let (host, port) = Self::split_host_port(rest, 8883)?;
            Ok((ClientTransportType::Tls, host, port))
        } else if let Some(rest) = address.strip_prefix("ws://") {
            let (host, port) = Self::split_host_port(rest, 80)?;
            Ok((
                ClientTransportType::WebSocket(address.to_string()),
                host,
                port,
            ))
        } else if let Some(rest) = address.strip_prefix("wss://") {
            let (host, port) = Self::split_host_port(rest, 443)?;
            Ok((
                ClientTransportType::WebSocketSecure(address.to_string()),
                host,
                port,
            ))
        } else if let Some(rest) = address.strip_prefix("tcp://") {
            let (host, port) = Self::split_host_port(rest, 1883)?;
            Ok((ClientTransportType::Tcp, host, port))
        } else if let Some(rest) = address.strip_prefix("ssl://") {
            let (host, port) = Self::split_host_port(rest, 8883)?;
            Ok((ClientTransportType::Tls, host, port))
        } else {
            let (host, port) = Self::split_host_port(address, 1883)?;
            Ok((ClientTransportType::Tcp, host, port))
        }
    }

    /// Splits a host:port string
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    fn split_host_port(address: &str, default_port: u16) -> Result<(&str, u16)> {
        let address_without_path = address.split('/').next().unwrap_or(address);

        if let Some(colon_pos) = address_without_path.rfind(':') {
            let host = &address_without_path[..colon_pos];
            let port_str = &address_without_path[colon_pos + 1..];
            let port = port_str
                .parse::<u16>()
                .map_err(|_| MqttError::ConnectionError(format!("Invalid port: {port_str}")))?;
            Ok((host, port))
        } else {
            Ok((address_without_path, default_port))
        }
    }

    /// Monitor connection and handle automatic reconnection
    async fn monitor_connection(&self) {
        tracing::info!("🔍 MONITOR TASK - Starting connection monitor task");

        loop {
            // Wait for disconnection
            tokio::time::sleep(Duration::from_secs(1)).await;

            let inner = self.inner.read().await;
            if !inner.is_connected() {
                tracing::info!(
                    "🔍 MONITOR TASK - Detected disconnection, triggering reconnection logic"
                );

                // Get reconnection config
                let reconnect_config = inner.options.reconnect_config.clone();
                let last_address = inner.last_address.clone();
                drop(inner); // Release lock before potentially long-running operation

                if !reconnect_config.enabled {
                    tracing::info!("🔍 MONITOR TASK - Reconnection disabled, exiting monitor");
                    break;
                }

                if let Some(address) = last_address {
                    tracing::info!(
                        address = %address,
                        "🔍 MONITOR TASK - Starting reconnection attempt"
                    );

                    // Attempt reconnection with exponential backoff
                    if let Err(e) = self.attempt_reconnection(&address, &reconnect_config).await {
                        tracing::error!("🔍 MONITOR TASK - Reconnection failed: {}", e);
                        break;
                    }
                } else {
                    tracing::info!("🔍 MONITOR TASK - No last address available for reconnection");
                    break;
                }
            }
        }
    }

    /// Attempt reconnection with exponential backoff
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    async fn attempt_reconnection(
        &self,
        address: &str,
        config: &crate::types::ReconnectConfig,
    ) -> Result<()> {
        tracing::info!(
            address = %address,
            max_attempts = config.max_attempts,
            initial_delay = ?config.initial_delay,
            "🔄 RECONNECTION - Starting reconnection loop"
        );

        let mut delay = config.initial_delay;

        loop {
            // Check if already connected (might have been connected manually)
            if self.is_connected().await {
                tracing::info!(
                    "🔄 RECONNECTION - Already connected, stopping reconnection attempts"
                );
                return Ok(());
            }

            // Increment attempt counter
            let attempt = {
                let mut inner = self.inner.write().await;
                inner.reconnect_attempt += 1;
                inner.reconnect_attempt
            };

            tracing::info!(
                attempt = attempt,
                max_attempts = config.max_attempts,
                delay = ?delay,
                "🔄 RECONNECTION - Attempting reconnection #{}", attempt
            );

            // Check max attempts
            if config.max_attempts > 0 && attempt > config.max_attempts {
                tracing::error!(
                    attempt = attempt,
                    max_attempts = config.max_attempts,
                    "🔄 RECONNECTION - Max attempts exceeded"
                );
                return Err(MqttError::ConnectionError(
                    "Max reconnection attempts exceeded".to_string(),
                ));
            }

            // Trigger reconnecting event
            self.trigger_connection_event(ConnectionEvent::Reconnecting { attempt })
                .await;

            // Wait before attempting (release mutex during wait)
            tokio::time::sleep(delay).await;

            // Acquire connection mutex only for the actual connection attempt
            let connection_guard = self.connection_mutex.lock().await;

            // Double-check connection status after acquiring mutex
            if self.is_connected().await {
                tracing::info!("Connected during wait, stopping reconnection attempts");
                return Ok(());
            }

            // Try to reconnect using internal method
            tracing::info!(
                attempt = attempt,
                address = %address,
                "🔄 RECONNECTION - Making connection attempt #{} to {}", attempt, address
            );
            let reconnection_result = self.connect_internal(address).await;

            // Release connection guard before restoration logic
            drop(connection_guard);

            match reconnection_result {
                Ok(_) => {
                    tracing::info!(
                        "🔄 RECONNECTION - Reconnected successfully after {} attempts",
                        attempt
                    );

                    // Restore subscriptions if session was not resumed
                    let inner = self.inner.read().await;
                    let stored_subs = inner.stored_subscriptions.read().await.clone();
                    drop(inner); // Release lock before resubscribing

                    for (topic, options, callback_id) in stored_subs {
                        if let Err(e) = self
                            .resubscribe_internal(&topic, options, callback_id)
                            .await
                        {
                            tracing::warn!("Failed to restore subscription to {}: {}", topic, e);
                        }
                    }

                    // Send queued messages
                    self.send_queued_messages().await;

                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!("Reconnection attempt {} failed: {}", attempt, e);

                    // Calculate next delay with exponential backoff
                    delay = std::cmp::min(
                        Duration::from_secs_f32(delay.as_secs_f32() * config.backoff_multiplier),
                        config.max_delay,
                    );
                }
            }
        }
    }

    /// Send messages that were queued during disconnection
    async fn send_queued_messages(&self) {
        let messages = {
            let inner = self.inner.read().await;
            let mut queued = inner.queued_messages.lock().await;
            std::mem::take(&mut *queued)
        };

        for mut msg in messages {
            // Set DUP flag for replayed messages
            msg.dup = true;

            if let Err(e) = self.publish_packet(msg).await {
                tracing::warn!("Failed to send queued message: {}", e);
            }
        }
    }

    /// Internal method to resubscribe with stored options and callback
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    async fn resubscribe_internal(
        &self,
        topic: &str,
        options: crate::packet::subscribe::SubscriptionOptions,
        callback_id: crate::callback::CallbackId,
    ) -> Result<()> {
        // Restore the callback first
        let inner = self.inner.read().await;
        inner.callback_manager.restore_callback(callback_id).await?;
        drop(inner);

        // Create subscribe packet
        let packet = SubscribePacket {
            packet_id: self.inner.read().await.packet_id_generator.next(),
            filters: vec![crate::packet::subscribe::TopicFilter {
                filter: topic.to_string(),
                options,
            }],
            properties: Properties::new(),
        };

        // Send the subscribe packet with the callback ID
        let inner = self.inner.read().await;
        inner.subscribe_with_callback(packet, callback_id).await?;
        Ok(())
    }

    /// Internal method to publish a packet
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    async fn publish_packet(&self, packet: PublishPacket) -> Result<()> {
        let mut inner = self.inner.write().await;
        inner
            .send_packet(crate::packet::Packet::Publish(packet))
            .await
    }
}

/// Client transport type for parsing addresses
#[derive(Debug, Clone)]
enum ClientTransportType {
    Tcp,
    Tls,
    WebSocket(String),
    WebSocketSecure(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_client_creation() {
        let client = MqttClient::new("test-client");
        assert!(!client.is_connected().await);
        assert_eq!(client.client_id().await, "test-client");
    }

    #[test]
    fn test_parse_address() {
        // Test MQTT scheme with explicit port
        let (transport, host, port) = MqttClient::parse_address("mqtt://localhost:1883").unwrap();
        assert!(matches!(transport, ClientTransportType::Tcp));
        assert_eq!(host, "localhost");
        assert_eq!(port, 1883);

        // Test MQTT scheme with default port
        let (transport, host, port) = MqttClient::parse_address("mqtt://localhost").unwrap();
        assert!(matches!(transport, ClientTransportType::Tcp));
        assert_eq!(host, "localhost");
        assert_eq!(port, 1883);

        // Test MQTTS scheme with default port
        let (transport, host, port) =
            MqttClient::parse_address("mqtts://broker.example.com").unwrap();
        assert!(matches!(transport, ClientTransportType::Tls));
        assert_eq!(host, "broker.example.com");
        assert_eq!(port, 8883);

        // Test MQTTS scheme with custom port
        let (transport, host, port) =
            MqttClient::parse_address("mqtts://secure.broker:9999").unwrap();
        assert!(matches!(transport, ClientTransportType::Tls));
        assert_eq!(host, "secure.broker");
        assert_eq!(port, 9999);

        // Test TCP scheme
        let (transport, host, port) =
            MqttClient::parse_address("tcp://192.168.1.100:1234").unwrap();
        assert!(matches!(transport, ClientTransportType::Tcp));
        assert_eq!(host, "192.168.1.100");
        assert_eq!(port, 1234);

        // Test SSL scheme (alias for TLS)
        let (transport, host, port) =
            MqttClient::parse_address("ssl://secure.broker.com:8883").unwrap();
        assert!(matches!(transport, ClientTransportType::Tls));
        assert_eq!(host, "secure.broker.com");
        assert_eq!(port, 8883);

        // Test no scheme with host only (defaults to TCP)
        let (transport, host, port) = MqttClient::parse_address("localhost").unwrap();
        assert!(matches!(transport, ClientTransportType::Tcp));
        assert_eq!(host, "localhost");
        assert_eq!(port, 1883);

        // Test no scheme with host and port
        let (transport, host, port) = MqttClient::parse_address("broker.local:9999").unwrap();
        assert!(matches!(transport, ClientTransportType::Tcp));
        assert_eq!(host, "broker.local");
        assert_eq!(port, 9999);

        // Test WebSocket scheme with default port
        let (transport, host, port) = MqttClient::parse_address("ws://localhost").unwrap();
        assert!(matches!(transport, ClientTransportType::WebSocket(_)));
        assert_eq!(host, "localhost");
        assert_eq!(port, 80);

        // Test WebSocket scheme with custom port
        let (transport, host, port) = MqttClient::parse_address("ws://localhost:8080").unwrap();
        assert!(matches!(transport, ClientTransportType::WebSocket(_)));
        assert_eq!(host, "localhost");
        assert_eq!(port, 8080);

        // Test WebSocket Secure scheme with default port
        let (transport, host, port) = MqttClient::parse_address("wss://secure.broker").unwrap();
        assert!(matches!(transport, ClientTransportType::WebSocketSecure(_)));
        assert_eq!(host, "secure.broker");
        assert_eq!(port, 443);

        // Test WebSocket Secure scheme with custom port
        let (transport, host, port) =
            MqttClient::parse_address("wss://secure.broker:8443").unwrap();
        assert!(matches!(transport, ClientTransportType::WebSocketSecure(_)));
        assert_eq!(host, "secure.broker");
        assert_eq!(port, 8443);

        // Test WebSocket with path preservation
        let (transport, host, port) =
            MqttClient::parse_address("ws://broker.emqx.io:8083/mqtt").unwrap();
        if let ClientTransportType::WebSocket(url) = transport {
            assert_eq!(url, "ws://broker.emqx.io:8083/mqtt");
        } else {
            panic!("Expected WebSocket transport type");
        }
        assert_eq!(host, "broker.emqx.io");
        assert_eq!(port, 8083);

        // Test WebSocket Secure with path preservation
        let (transport, host, port) =
            MqttClient::parse_address("wss://broker.hivemq.com:8884/mqtt").unwrap();
        if let ClientTransportType::WebSocketSecure(url) = transport {
            assert_eq!(url, "wss://broker.hivemq.com:8884/mqtt");
        } else {
            panic!("Expected WebSocketSecure transport type");
        }
        assert_eq!(host, "broker.hivemq.com");
        assert_eq!(port, 8884);

        // Test IPv6 addresses
        let (transport, host, port) = MqttClient::parse_address("[::1]:1883").unwrap();
        assert!(matches!(transport, ClientTransportType::Tcp));
        assert_eq!(host, "[::1]");
        assert_eq!(port, 1883);
    }
}
