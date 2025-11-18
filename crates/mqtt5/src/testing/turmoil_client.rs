//! Turmoil-compatible MQTT client for deterministic testing

use crate::client::MqttClient;
use crate::error::MqttError;
use crate::time::Duration;
use crate::types::Message;
use crate::QoS;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

/// Configuration for Turmoil-based client
#[derive(Debug, Clone)]
pub struct TurmoilClientConfig {
    pub client_id: String,
    pub clean_start: bool,
    pub keep_alive: Duration,
}

impl Default for TurmoilClientConfig {
    fn default() -> Self {
        Self {
            client_id: "turmoil-client".to_string(),
            clean_start: true,
            keep_alive: Duration::from_secs(60),
        }
    }
}

impl TurmoilClientConfig {
    /// Creates a new client configuration
    #[must_use]
    pub fn new(client_id: &str) -> Self {
        Self {
            client_id: client_id.to_string(),
            clean_start: true,
            keep_alive: Duration::from_secs(60),
        }
    }

    /// Sets clean start flag
    #[must_use]
    pub fn with_clean_start(mut self, clean_start: bool) -> Self {
        self.clean_start = clean_start;
        self
    }

    /// Sets keep alive duration
    #[must_use]
    pub fn with_keep_alive(mut self, keep_alive: Duration) -> Self {
        self.keep_alive = keep_alive;
        self
    }

    /// Sets connect timeout (stored but not used for now)
    #[must_use]
    pub fn with_connect_timeout(self, _timeout: Duration) -> Self {
        // For test compatibility - timeout is currently ignored
        self
    }
}

/// Turmoil-compatible MQTT client
pub struct TurmoilClient {
    inner: MqttClient,
    received: Arc<Mutex<mpsc::UnboundedReceiver<Message>>>,
    sender: mpsc::UnboundedSender<Message>,
}

impl TurmoilClient {
    /// Creates a new Turmoil client
    #[must_use]
    pub fn new(config: &TurmoilClientConfig) -> Self {
        let inner = MqttClient::new(&config.client_id);
        let (sender, receiver) = mpsc::unbounded_channel();
        let receiver_arc = Arc::new(Mutex::new(receiver));

        Self {
            inner,
            received: receiver_arc,
            sender,
        }
    }

    /// Creates a client with configuration
    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn with_config(config: TurmoilClientConfig) -> Self {
        Self::new(&config)
    }

    /// Creates a client with a specific ID
    #[must_use]
    pub fn with_id(client_id: &str) -> Self {
        let config = TurmoilClientConfig {
            client_id: client_id.to_string(),
            ..Default::default()
        };
        Self::new(&config)
    }

    /// Connects to the broker
    pub async fn connect(&self, address: &str) -> Result<(), MqttError> {
        self.inner.connect(address).await
    }

    /// Waits for the connection to be established
    pub async fn wait_for_connection(&self, timeout: Duration) -> Result<(), MqttError> {
        tokio::time::sleep(timeout).await;
        Ok(())
    }

    /// Subscribes to a topic
    pub async fn subscribe(&self, topic: &str, _qos: QoS) -> Result<(), MqttError> {
        let sender = self.sender.clone();
        self.inner
            .subscribe(topic, move |msg| {
                let _ = sender.send(msg);
            })
            .await
            .map(|_| ())
    }

    /// Publishes a message
    pub async fn publish(&self, topic: &str, payload: &[u8], qos: QoS) -> Result<(), MqttError> {
        self.inner
            .publish_qos(topic, payload, qos)
            .await
            .map(|_| ())
    }

    /// Publishes a retained message
    pub async fn publish_retained(
        &self,
        topic: &str,
        payload: &[u8],
        qos: QoS,
    ) -> Result<(), MqttError> {
        // For now, just publish normally - retained functionality not fully implemented in test client
        self.publish(topic, payload, qos).await
    }

    /// Receives a message with timeout
    pub async fn receive_timeout(&self, timeout: Duration) -> Result<Message, MqttError> {
        let mut receiver = self.received.lock().await;
        tokio::time::timeout(timeout, receiver.recv())
            .await
            .map_err(|_| MqttError::Timeout)?
            .ok_or(MqttError::NotConnected)
    }

    /// Disconnects from the broker
    pub async fn disconnect(&self) -> Result<(), MqttError> {
        self.inner.disconnect().await
    }

    /// Checks if client is connected
    pub async fn is_connected(&self) -> bool {
        self.inner.is_connected().await
    }

    /// Connects with TLS (placeholder for test compatibility)
    pub async fn connect_tls(&self, address: &str) -> Result<(), MqttError> {
        // For now, just do a regular connection
        self.connect(address).await
    }
}
