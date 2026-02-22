//! Test harness for running conformance tests against a real in-process broker.
//!
//! Provides [`ConformanceBroker`] (memory-backed broker on loopback TCP),
//! helper functions for creating and connecting MQTT clients, and
//! [`MessageCollector`] for capturing published messages in tests.

#![allow(clippy::missing_panics_doc)]

use mqtt5::broker::auth::AuthProvider;
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig, WebSocketConfig};
use mqtt5::broker::server::MqttBroker;
use mqtt5::types::Message;
use mqtt5::{ConnectOptions, MessageProperties, MqttClient, QoS};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Duration;
use ulid::Ulid;

/// An in-process MQTT v5.0 broker for conformance testing.
///
/// Runs a real [`MqttBroker`] with memory-backed storage on a random loopback
/// port. Each test should create its own `ConformanceBroker` to ensure isolation.
pub struct ConformanceBroker {
    address: String,
    port: u16,
    ws_port: Option<u16>,
    config: BrokerConfig,
    handle: Option<JoinHandle<()>>,
}

impl ConformanceBroker {
    /// Starts a broker with default configuration (memory storage, random port).
    pub async fn start() -> Self {
        let storage_config = StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        };

        let config = BrokerConfig::default()
            .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
            .with_storage(storage_config);

        Self::start_with_config(config).await
    }

    /// Starts a broker with WebSocket enabled on a random port.
    pub async fn start_with_websocket() -> Self {
        let storage_config = StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        };

        let ws_config =
            WebSocketConfig::new().with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap());

        let config = BrokerConfig::default()
            .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
            .with_websocket(ws_config)
            .with_storage(storage_config);

        Self::start_with_config(config).await
    }

    /// Starts a broker with the given configuration.
    pub async fn start_with_config(config: BrokerConfig) -> Self {
        let mut broker = MqttBroker::with_config(config.clone())
            .await
            .expect("broker creation failed");

        let addr = broker.local_addr().expect("no local addr");
        let port = addr.port();
        let address = format!("mqtt://{addr}");
        let ws_port = broker.ws_local_addr().map(|a| a.port());

        let handle = tokio::spawn(async move {
            let _ = broker.run().await;
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        Self {
            address,
            port,
            ws_port,
            config,
            handle: Some(handle),
        }
    }

    /// Starts a broker with a custom auth provider.
    pub async fn start_with_auth_provider(provider: Arc<dyn AuthProvider>) -> Self {
        let storage_config = StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        };

        let config = BrokerConfig::default()
            .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
            .with_storage(storage_config);

        let mut broker = MqttBroker::with_config(config.clone())
            .await
            .expect("broker creation failed");

        broker = broker.with_auth_provider(provider);

        let addr = broker.local_addr().expect("no local addr");
        let port = addr.port();
        let address = format!("mqtt://{addr}");
        let ws_port = broker.ws_local_addr().map(|a| a.port());

        let handle = tokio::spawn(async move {
            let _ = broker.run().await;
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        Self {
            address,
            port,
            ws_port,
            config,
            handle: Some(handle),
        }
    }

    /// Returns the MQTT connection URL (e.g. `mqtt://127.0.0.1:12345`).
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Returns the TCP port the broker is listening on.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Returns the WebSocket port the broker is listening on, if enabled.
    #[must_use]
    pub fn ws_port(&self) -> Option<u16> {
        self.ws_port
    }

    /// Returns the broker's socket address for raw TCP connections.
    #[must_use]
    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], self.port))
    }

    /// Returns the broker's WebSocket socket address.
    ///
    /// # Panics
    /// Panics if WebSocket is not enabled on this broker.
    #[must_use]
    pub fn ws_socket_addr(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], self.ws_port.expect("WebSocket not enabled")))
    }

    /// Stops the broker by aborting its task.
    pub async fn stop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    /// Stops and restarts the broker on a new random port.
    ///
    /// Preserves the original configuration but binds to a fresh port.
    pub async fn restart(&mut self) {
        self.stop().await;

        let config = self
            .config
            .clone()
            .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap());

        let mut broker = MqttBroker::with_config(config)
            .await
            .expect("broker restart failed");

        let addr = broker.local_addr().expect("no local addr");
        self.port = addr.port();
        self.address = format!("mqtt://{addr}");
        self.ws_port = broker.ws_local_addr().map(|a| a.port());

        let handle = tokio::spawn(async move {
            let _ = broker.run().await;
        });

        self.handle = Some(handle);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

impl Drop for ConformanceBroker {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

/// Generates a unique client ID with the given prefix using a ULID suffix.
#[must_use]
pub fn unique_client_id(prefix: &str) -> String {
    format!("conf-{prefix}-{}", Ulid::new())
}

/// Creates an unconnected [`MqttClient`] with a unique client ID.
#[must_use]
pub fn new_client(prefix: &str) -> MqttClient {
    MqttClient::new(unique_client_id(prefix))
}

/// Creates and connects an [`MqttClient`] with default options.
pub async fn connected_client(prefix: &str, broker: &ConformanceBroker) -> MqttClient {
    let client = new_client(prefix);
    client
        .connect(broker.address())
        .await
        .expect("connect failed");
    client
}

/// Creates and connects an [`MqttClient`] with the given [`ConnectOptions`].
pub async fn connected_client_with_options(
    broker: &ConformanceBroker,
    options: ConnectOptions,
) -> MqttClient {
    let client = MqttClient::with_options(options.clone());
    Box::pin(client.connect_with_options(broker.address(), options))
        .await
        .expect("connect failed");
    client
}

/// A message received by a subscriber during a conformance test.
#[derive(Debug, Clone)]
pub struct ReceivedMessage {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: QoS,
    pub retain: bool,
    pub properties: MessageProperties,
}

/// Collects messages delivered to a subscriber for later assertion.
///
/// Thread-safe and clone-friendly. Use [`callback`](Self::callback) to create
/// a closure suitable for passing to `MqttClient::subscribe`.
#[derive(Clone)]
pub struct MessageCollector {
    messages: Arc<Mutex<Vec<ReceivedMessage>>>,
}

impl MessageCollector {
    #[must_use]
    pub fn new() -> Self {
        Self {
            messages: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Returns a callback closure that stores received messages.
    pub fn callback(&self) -> impl Fn(Message) + Send + Sync + 'static {
        let messages = self.messages.clone();
        move |msg| {
            let received = ReceivedMessage {
                topic: msg.topic.clone(),
                payload: msg.payload.clone(),
                qos: msg.qos,
                retain: msg.retain,
                properties: msg.properties.clone(),
            };
            messages.lock().unwrap().push(received);
        }
    }

    /// Polls until at least `count` messages are received, or `timeout` elapses.
    pub async fn wait_for_messages(&self, count: usize, timeout: Duration) -> bool {
        let start = tokio::time::Instant::now();
        while start.elapsed() < timeout {
            if self.messages.lock().unwrap().len() >= count {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// Returns a snapshot of all received messages.
    #[must_use]
    pub fn get_messages(&self) -> Vec<ReceivedMessage> {
        self.messages.lock().unwrap().clone()
    }

    /// Returns the number of messages received so far.
    #[must_use]
    pub fn count(&self) -> usize {
        self.messages.lock().unwrap().len()
    }

    /// Clears all collected messages.
    pub fn clear(&self) {
        self.messages.lock().unwrap().clear();
    }
}

impl Default for MessageCollector {
    fn default() -> Self {
        Self::new()
    }
}
