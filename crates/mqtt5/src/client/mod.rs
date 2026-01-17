//! MQTT v5.0 Client - Direct Async Implementation
//!
//! This client uses direct async/await patterns throughout.

use crate::callback::PublishCallback;
use crate::error::{MqttError, Result};
use crate::packet::publish::PublishPacket;
use crate::packet::subscribe::{SubscribePacket, SubscriptionOptions, TopicFilter};
use crate::packet::unsubscribe::UnsubscribePacket;
use crate::protocol::v5::properties::Properties;
#[cfg(not(target_arch = "wasm32"))]
use crate::transport::tls::TlsConfig;
use crate::types::{
    ConnectOptions, ConnectResult, PublishOptions, PublishResult, SubscribeOptions,
};
use crate::QoS;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::instrument;

mod auth_handler;
pub mod auth_handlers;
mod builders;
mod connection;
#[cfg(not(target_arch = "wasm32"))]
mod direct;
mod error_recovery;
mod inner;
pub mod mock;
mod retry;
mod state;
pub mod r#trait;

pub use auth_handler::{AuthHandler, AuthResponse};
pub use auth_handlers::{JwtAuthHandler, PlainAuthHandler, ScramSha256AuthHandler};

pub use self::connection::{ConnectionEvent, DisconnectReason, ReconnectConfig};
pub use self::error_recovery::{
    is_recoverable, retry_delay, ErrorCallback, ErrorRecoveryConfig, RecoverableError, RetryState,
};
pub use self::mock::{MockCall, MockMqttClient};
pub use self::r#trait::MqttClientTrait;

pub use builders::ConnectionEventCallback;

#[cfg(not(target_arch = "wasm32"))]
use self::direct::DirectClientInner;

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
///     let client = MqttClient::new("my-client-id");
///
///     client.connect("mqtt://localhost:1883").await?;
///
///     client.subscribe("temperature/room1", |msg| {
///         println!("Received: {} on topic {}",
///                  String::from_utf8_lossy(&msg.payload),
///                  msg.topic);
///     }).await?;
///
///     client.publish("temperature/room1", b"22.5").await?;
///
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
///     let options = ConnectOptions::new("my-client")
///         .with_clean_start(true)
///         .with_keep_alive(Duration::from_secs(30))
///         .with_credentials("user", b"pass");
///
///     let client = MqttClient::with_options(options);
///
///     client.connect("mqtts://broker.example.com:8883").await?;
///
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
    pub(crate) inner: Arc<RwLock<DirectClientInner>>,
    pub(crate) connection_event_callbacks: Arc<RwLock<Vec<ConnectionEventCallback>>>,
    pub(crate) error_callbacks: Arc<RwLock<Vec<error_recovery::ErrorCallback>>>,
    pub(crate) error_recovery_config: Arc<RwLock<error_recovery::ErrorRecoveryConfig>>,
    pub(crate) connection_mutex: Arc<tokio::sync::Mutex<()>>,
    pub(crate) tls_config: Arc<RwLock<Option<TlsConfig>>>,
    pub(crate) transport_config: Arc<RwLock<crate::transport::ClientTransportConfig>>,
}

impl MqttClient {
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
    ///         ConnectionEvent::Connecting => {
    ///             println!("Connecting...");
    ///         }
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
    ///             println!("Reconnection failed: {error}");
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
            let connection_guard = self.connection_mutex.lock().await;
            let options = self.inner.read().await.options.clone();
            let result = self.connect_with_options_internal(address, options).await;
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
        let connection_guard = self.connection_mutex.lock().await;
        let result = self.connect_with_options_internal(address, options).await;
        drop(connection_guard);
        result
    }

    /// Connects to the MQTT broker using a custom TLS configuration
    ///
    /// # Errors
    ///
    /// Returns an error if connection fails or TLS configuration is invalid
    pub async fn connect_with_tls(
        &self,
        tls_config: crate::transport::tls::TlsConfig,
    ) -> Result<()> {
        let connection_guard = self.connection_mutex.lock().await;
        let options = self.inner.read().await.options.clone();
        let result = self
            .connect_with_tls_and_options_internal(tls_config, options)
            .await;
        drop(connection_guard);
        result.map(|_| ())
    }

    /// Connects to the MQTT broker using custom TLS configuration and connect options
    ///
    /// Returns `session_present` flag from CONNACK
    ///
    /// # Errors
    ///
    /// Returns an error if connection fails or configuration is invalid
    pub async fn connect_with_tls_and_options(
        &self,
        tls_config: crate::transport::tls::TlsConfig,
        options: ConnectOptions,
    ) -> Result<ConnectResult> {
        let connection_guard = self.connection_mutex.lock().await;
        let result = self
            .connect_with_tls_and_options_internal(tls_config, options)
            .await;
        drop(connection_guard);
        result
    }

    /// Disconnects from the MQTT broker
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

    /// Subscribes to a topic with custom options and a callback
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

        let inner = self.inner.read().await;
        let callback: PublishCallback = Arc::new(callback);
        let callback_id = inner
            .callback_manager
            .register_with_id(&topic_filter, callback)?;

        let filter = TopicFilter {
            filter: topic_filter.clone(),
            options: SubscriptionOptions {
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
            packet_id: 0,
            filters: vec![filter],
            properties: Properties::default(),
            protocol_version: inner.options.protocol_version.as_u8(),
        };

        if let Some(sub_id) = options.subscription_identifier {
            packet = packet.with_subscription_identifier(sub_id);
        }

        match inner.subscribe_with_callback(packet, callback_id).await {
            Ok(results) => {
                if let Some(&(packet_id, qos)) = results.first() {
                    Ok((packet_id, qos))
                } else {
                    inner.callback_manager.unregister(&topic_filter)?;
                    Err(MqttError::ProtocolError(
                        "No results returned for subscription".to_string(),
                    ))
                }
            }
            Err(e) => {
                inner.callback_manager.unregister(&topic_filter)?;
                Err(e)
            }
        }
    }

    /// Unsubscribes from a topic
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

        let inner = self.inner.read().await;
        inner.callback_manager.unregister(&topic_filter)?;

        let packet = UnsubscribePacket {
            packet_id: 0,
            filters: vec![topic_filter.clone()],
            properties: Properties::default(),
            protocol_version: inner.options.protocol_version.as_u8(),
        };

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
    pub async fn error_recovery_config(&self) -> error_recovery::ErrorRecoveryConfig {
        self.error_recovery_config.read().await.clone()
    }

    /// Set error recovery configuration
    pub async fn set_error_recovery_config(&self, config: error_recovery::ErrorRecoveryConfig) {
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

    /// Sets an authentication handler for enhanced authentication
    pub async fn set_auth_handler(&self, handler: impl AuthHandler + 'static) {
        self.inner.write().await.set_auth_handler(handler);
    }

    /// Initiates re-authentication with the broker
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Client is not connected
    /// - No auth handler is configured
    /// - No authentication method was used during initial connection
    pub async fn reauthenticate(&self) -> Result<()> {
        self.inner.read().await.reauthenticate().await
    }

    /// Simulate abnormal disconnection (for testing will messages)
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn disconnect_abnormally(&self) -> Result<()> {
        let mut inner = self.inner.write().await;
        inner.disconnect_with_packet(false).await
    }
}

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
        async move { Box::pin(self.connect_with_options(address, options)).await }
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
        let (transport, host, port) = MqttClient::parse_address("mqtt://localhost:1883").unwrap();
        assert!(matches!(transport, state::ClientTransportType::Tcp));
        assert_eq!(host, "localhost");
        assert_eq!(port, 1883);

        let (transport, host, port) = MqttClient::parse_address("mqtt://localhost").unwrap();
        assert!(matches!(transport, state::ClientTransportType::Tcp));
        assert_eq!(host, "localhost");
        assert_eq!(port, 1883);

        let (transport, host, port) =
            MqttClient::parse_address("mqtts://broker.example.com").unwrap();
        assert!(matches!(transport, state::ClientTransportType::Tls));
        assert_eq!(host, "broker.example.com");
        assert_eq!(port, 8883);

        let (transport, host, port) =
            MqttClient::parse_address("mqtts://secure.broker:9999").unwrap();
        assert!(matches!(transport, state::ClientTransportType::Tls));
        assert_eq!(host, "secure.broker");
        assert_eq!(port, 9999);

        let (transport, host, port) =
            MqttClient::parse_address("tcp://192.168.1.100:1234").unwrap();
        assert!(matches!(transport, state::ClientTransportType::Tcp));
        assert_eq!(host, "192.168.1.100");
        assert_eq!(port, 1234);

        let (transport, host, port) =
            MqttClient::parse_address("ssl://secure.broker.com:8883").unwrap();
        assert!(matches!(transport, state::ClientTransportType::Tls));
        assert_eq!(host, "secure.broker.com");
        assert_eq!(port, 8883);

        let (transport, host, port) = MqttClient::parse_address("localhost").unwrap();
        assert!(matches!(transport, state::ClientTransportType::Tcp));
        assert_eq!(host, "localhost");
        assert_eq!(port, 1883);

        let (transport, host, port) = MqttClient::parse_address("broker.local:9999").unwrap();
        assert!(matches!(transport, state::ClientTransportType::Tcp));
        assert_eq!(host, "broker.local");
        assert_eq!(port, 9999);

        let (transport, host, port) = MqttClient::parse_address("ws://localhost").unwrap();
        assert!(matches!(
            transport,
            state::ClientTransportType::WebSocket(_)
        ));
        assert_eq!(host, "localhost");
        assert_eq!(port, 80);

        let (transport, host, port) = MqttClient::parse_address("ws://localhost:8080").unwrap();
        assert!(matches!(
            transport,
            state::ClientTransportType::WebSocket(_)
        ));
        assert_eq!(host, "localhost");
        assert_eq!(port, 8080);

        let (transport, host, port) = MqttClient::parse_address("wss://secure.broker").unwrap();
        assert!(matches!(
            transport,
            state::ClientTransportType::WebSocketSecure(_)
        ));
        assert_eq!(host, "secure.broker");
        assert_eq!(port, 443);

        let (transport, host, port) =
            MqttClient::parse_address("wss://secure.broker:8443").unwrap();
        assert!(matches!(
            transport,
            state::ClientTransportType::WebSocketSecure(_)
        ));
        assert_eq!(host, "secure.broker");
        assert_eq!(port, 8443);

        let (transport, host, port) =
            MqttClient::parse_address("ws://broker.emqx.io:8083/mqtt").unwrap();
        if let state::ClientTransportType::WebSocket(url) = transport {
            assert_eq!(url, "ws://broker.emqx.io:8083/mqtt");
        } else {
            panic!("Expected WebSocket transport type");
        }
        assert_eq!(host, "broker.emqx.io");
        assert_eq!(port, 8083);

        let (transport, host, port) =
            MqttClient::parse_address("wss://broker.hivemq.com:8884/mqtt").unwrap();
        if let state::ClientTransportType::WebSocketSecure(url) = transport {
            assert_eq!(url, "wss://broker.hivemq.com:8884/mqtt");
        } else {
            panic!("Expected WebSocketSecure transport type");
        }
        assert_eq!(host, "broker.hivemq.com");
        assert_eq!(port, 8884);

        let (transport, host, port) = MqttClient::parse_address("[::1]:1883").unwrap();
        assert!(matches!(transport, state::ClientTransportType::Tcp));
        assert_eq!(host, "[::1]");
        assert_eq!(port, 1883);
    }
}
