//! Common test utilities and scenarios

pub mod cli_helpers;

use mqtt5::time::Duration;
use mqtt5::{MessageProperties, MqttClient, QoS};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use ulid::Ulid;

#[allow(dead_code)]
pub fn find_workspace_root() -> PathBuf {
    let mut current = std::env::current_dir().expect("Failed to get current directory");

    loop {
        let cargo_toml = current.join("Cargo.toml");
        if cargo_toml.exists() {
            if let Ok(contents) = std::fs::read_to_string(&cargo_toml) {
                if contents.contains("[workspace]") {
                    return current;
                }
            }
        }

        assert!(current.pop(), "Could not find workspace root");
    }
}

#[allow(dead_code)]
pub fn get_cli_binary_path() -> PathBuf {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_mqttv5") {
        return PathBuf::from(path);
    }

    let workspace_root = find_workspace_root();
    workspace_root.join("target").join("release").join("mqttv5")
}

#[allow(dead_code)]
pub const TEST_BROKER: &str = "mqtt://127.0.0.1:1883";

use mqtt5::broker::config::BrokerConfig;
use mqtt5::broker::server::MqttBroker;
use std::net::SocketAddr;
use tokio::task::JoinHandle;

#[derive(Debug)]
#[allow(dead_code)]
pub struct TestBroker {
    address: String,
    port: u16,
    config: BrokerConfig,
    handle: Option<JoinHandle<()>>,
}

impl TestBroker {
    #[allow(dead_code)]
    pub async fn start() -> Self {
        use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};

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

    #[allow(dead_code)]
    pub async fn stop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    #[allow(dead_code)]
    pub async fn restart(&mut self) {
        self.stop().await;

        let mut config = self.config.clone();
        config = config.with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap());

        let mut broker = MqttBroker::with_config(config)
            .await
            .expect("Failed to restart test broker");

        let addr = broker.local_addr().expect("Failed to get broker address");
        self.port = addr.port();
        self.address = format!("mqtt://{addr}");

        let handle = tokio::spawn(async move {
            let _ = broker.run().await;
        });

        self.handle = Some(handle);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    /// Start a test broker with TLS support on a random port
    #[allow(dead_code)]
    pub async fn start_with_tls() -> Self {
        use mqtt5::broker::config::{
            BrokerConfig, StorageBackend, StorageConfig, TlsConfig as BrokerTlsConfig,
        };

        let _ = rustls::crypto::ring::default_provider().install_default();

        let storage_config = StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        };

        let workspace_root = find_workspace_root();
        let tls_config = BrokerTlsConfig::new(
            workspace_root.join("test_certs/server.pem"),
            workspace_root.join("test_certs/server.key"),
        )
        .with_require_client_cert(false)
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap());

        let config = BrokerConfig::default()
            .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
            .with_storage(storage_config)
            .with_tls(tls_config);

        let mut broker = MqttBroker::with_config(config.clone())
            .await
            .expect("Failed to create test broker with TLS");

        // Its listener is bound as it is made, on a port of its own.
        let tls_port = broker
            .tls_local_addr()
            .expect("TLS listener must be bound")
            .port();
        let address = format!("mqtts://127.0.0.1:{tls_port}");

        let handle = tokio::spawn(async move {
            let _ = broker.run().await;
        });

        Self {
            address,
            port: tls_port,
            config,
            handle: Some(handle),
        }
    }

    #[cfg(feature = "transport-websocket")]
    #[allow(dead_code)]
    pub async fn start_with_websocket() -> Self {
        use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig, WebSocketConfig};

        let storage_config = StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        };

        let ws_config = WebSocketConfig::new()
            .with_bind_addresses(vec!["127.0.0.1:0".parse().unwrap()])
            .with_path("/mqtt".to_string());

        let config = BrokerConfig::default()
            .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
            .with_storage(storage_config)
            .with_websocket(ws_config);

        let mut broker = MqttBroker::with_config(config.clone())
            .await
            .expect("Failed to create test broker with WebSocket");

        // Its listener is bound as it is made, on a port of its own.
        let ws_port = broker
            .ws_local_addr()
            .expect("WebSocket listener must be bound")
            .port();
        let address = format!("ws://127.0.0.1:{ws_port}/mqtt");

        let handle = tokio::spawn(async move {
            let _ = broker.run().await;
        });

        Self {
            address,
            port: ws_port,
            config,
            handle: Some(handle),
        }
    }

    #[allow(dead_code)]
    pub async fn start_with_authentication() -> Self {
        use mqtt5::broker::config::{
            AuthConfig, AuthMethod, BrokerConfig, RateLimitConfig, StorageBackend, StorageConfig,
        };
        use std::process::Command;

        let temp_dir = std::env::temp_dir();
        let password_file = temp_dir.join(format!("test_passwords_{}.txt", std::process::id()));

        let _ = std::fs::remove_file(&password_file);

        let cli_binary = get_cli_binary_path();
        let status = Command::new(&cli_binary)
            .args([
                "passwd",
                "-c",
                "-b",
                "testpass",
                "testuser",
                password_file.to_str().unwrap(),
            ])
            .status()
            .expect("Failed to create password file");

        assert!(status.success(), "Failed to create password file");

        for _ in 0..10 {
            if password_file.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let storage_config = StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        };

        let auth_config = AuthConfig {
            allow_anonymous: false,
            password_file: Some(password_file.clone()),
            acl_file: None,
            auth_method: AuthMethod::Password,
            auth_data: Some(std::fs::read(&password_file).expect("Failed to read password file")),
            scram_file: None,
            jwt_config: None,
            federated_jwt_config: None,
            rate_limit: RateLimitConfig::default(),
        };

        let config = BrokerConfig::default()
            .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
            .with_storage(storage_config)
            .with_auth(auth_config);

        let mut broker = MqttBroker::with_config(config.clone())
            .await
            .expect("Failed to create test broker with authentication");

        let addr = broker.local_addr().expect("Failed to get broker address");
        let port = addr.port();
        let address = format!("mqtt://{addr}");

        let handle = tokio::spawn(async move {
            let _ = broker.run().await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        Self {
            address,
            port,
            config,
            handle: Some(handle),
        }
    }

    #[allow(dead_code)]
    pub async fn start_with_config(config: BrokerConfig) -> Self {
        let mut broker = MqttBroker::with_config(config.clone())
            .await
            .expect("Failed to create test broker");

        let addr = broker.local_addr().expect("Failed to get broker address");
        let port = addr.port();
        let address = format!("mqtt://{addr}");

        let handle = tokio::spawn(async move {
            let _ = broker.run().await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        Self {
            address,
            port,
            config,
            handle: Some(handle),
        }
    }

    /// Get the broker address
    #[allow(dead_code)]
    pub fn address(&self) -> &str {
        &self.address
    }
}

impl Drop for TestBroker {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

/// Forwards each connection to `to`, and drops every one open when `cut` is
/// notified, while still taking new ones
#[allow(dead_code)]
pub async fn cutting_proxy(to: SocketAddr) -> (SocketAddr, std::sync::Arc<tokio::sync::Notify>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cut = std::sync::Arc::new(tokio::sync::Notify::new());
    let cuts = cut.clone();
    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            let Ok(mut server) = tokio::net::TcpStream::connect(to).await else {
                return;
            };
            let cuts = cuts.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = tokio::io::copy_bidirectional(&mut client, &mut server) => {}
                    () = cuts.notified() => {}
                }
            });
        }
    });
    (addr, cut)
}

/// The test CA, which signs the test brokers' certificates
#[allow(dead_code)]
pub fn test_ca() -> Vec<u8> {
    std::fs::read(find_workspace_root().join("test_certs/ca.pem")).unwrap()
}

/// A CA that signs no test broker's certificate
#[allow(dead_code)]
pub fn other_ca() -> Vec<u8> {
    rcgen::generate_simple_self_signed(vec!["other-ca".to_string()])
        .unwrap()
        .cert
        .pem()
        .into_bytes()
}

/// Default timeout for test operations
#[allow(dead_code)]
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

#[allow(dead_code)]
pub fn test_client_id(test_name: &str) -> String {
    format!("test-{test_name}-{}", Ulid::new())
}

#[allow(dead_code)]
pub async fn create_test_client(name: &str) -> MqttClient {
    create_test_client_with_broker(name, TEST_BROKER).await
}

#[allow(dead_code)]
pub async fn create_test_client_with_broker(name: &str, broker_addr: &str) -> MqttClient {
    let client = MqttClient::new(test_client_id(name));
    client
        .connect(broker_addr)
        .await
        .expect("Failed to connect");
    client
}

/// Message collector for testing subscriptions
#[derive(Clone)]
pub struct MessageCollector {
    messages: Arc<Mutex<Vec<ReceivedMessage>>>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ReceivedMessage {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: QoS,
    pub retain: bool,
    pub properties: MessageProperties,
}

#[allow(dead_code)]
impl MessageCollector {
    pub fn new() -> Self {
        Self {
            messages: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Get a callback function for subscriptions
    pub fn callback(&self) -> impl Fn(mqtt5::types::Message) + Send + Sync + 'static {
        let messages = self.messages.clone();
        move |msg| {
            messages.lock().unwrap().push(ReceivedMessage {
                topic: msg.topic,
                payload: msg.payload,
                qos: msg.qos,
                retain: msg.retain,
                properties: msg.properties,
            });
        }
    }

    /// Wait for a specific number of messages
    pub async fn wait_for_messages(&self, count: usize, timeout: Duration) -> bool {
        let start = tokio::time::Instant::now();
        while start.elapsed() < timeout {
            if self.count() >= count {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// Get all received messages
    pub fn get_messages(&self) -> Vec<ReceivedMessage> {
        self.messages.lock().unwrap().clone()
    }

    /// Clear all messages
    pub fn clear(&self) {
        self.messages.lock().unwrap().clear();
    }

    /// Get message count
    pub fn count(&self) -> usize {
        self.messages.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_collector_holds_each_message_in_callback_order_once_the_callback_returns() {
        let collector = super::MessageCollector::new();
        let callback = collector.callback();
        let topics: Vec<String> = (0..2000).map(|index| format!("test/{index}")).collect();
        for topic in &topics {
            callback(mqtt5::types::Message {
                topic: topic.clone(),
                payload: b"test".to_vec(),
                qos: mqtt5::QoS::AtMostOnce,
                retain: false,
                properties: mqtt5::MessageProperties::default(),
                stream_id: None,
            });
        }
        let collected: Vec<String> = collector
            .get_messages()
            .into_iter()
            .map(|message| message.topic)
            .collect();
        assert_eq!(collected, topics);
    }
}
