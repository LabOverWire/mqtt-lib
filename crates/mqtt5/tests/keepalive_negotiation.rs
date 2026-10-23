#![cfg(feature = "broker")]
mod common;

use common::TestBroker;

use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::time::Duration;
use mqtt5::{ConnectOptions, ConnectionEvent, MqttClient};
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::protocol::v5::properties::Properties;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

#[tokio::test]
async fn server_keep_alive_overrides_client_request() {
    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };
    let mut config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage_config);
    config.server_keep_alive = Some(Duration::from_secs(10));

    let broker = TestBroker::start_with_config(config).await;

    let mut options = ConnectOptions::new("kanego-1");
    options.keep_alive = Duration::from_secs(60);

    let observed = Arc::new(Mutex::new(None::<Duration>));
    let observed_clone = Arc::clone(&observed);

    let client = MqttClient::with_options(options);
    client
        .on_connection_event(move |event| {
            if let ConnectionEvent::Connected { keep_alive, .. } = event {
                let observed = Arc::clone(&observed_clone);
                tokio::spawn(async move {
                    *observed.lock().await = Some(keep_alive);
                });
            }
        })
        .await
        .unwrap();

    client.connect(broker.address()).await.unwrap();

    assert_eq!(client.keep_alive().await, Duration::from_secs(10));

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        *observed.lock().await,
        Some(Duration::from_secs(10)),
        "ConnectionEvent::Connected must carry the broker-negotiated keep-alive",
    );

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn keep_alive_falls_back_to_client_value_without_server_override() {
    let broker = TestBroker::start().await;

    let mut options = ConnectOptions::new("kanego-2");
    options.keep_alive = Duration::from_secs(30);

    let client = MqttClient::with_options(options);
    client.connect(broker.address()).await.unwrap();

    assert_eq!(client.keep_alive().await, Duration::from_secs(30));

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn keep_alive_before_connect_returns_configured_value() {
    let mut options = ConnectOptions::new("kanego-3");
    options.keep_alive = Duration::from_secs(45);

    let client = MqttClient::with_options(options);
    assert_eq!(client.keep_alive().await, Duration::from_secs(45));
}

#[tokio::test]
async fn keep_alive_renegotiates_against_each_broker_on_reconnect() {
    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };

    let mut config_a = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage_config.clone());
    config_a.server_keep_alive = Some(Duration::from_secs(10));
    let broker_a = TestBroker::start_with_config(config_a).await;

    let mut config_b = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage_config);
    config_b.server_keep_alive = Some(Duration::from_secs(25));
    let broker_b = TestBroker::start_with_config(config_b).await;

    let mut options = ConnectOptions::new("kanego-4");
    options.keep_alive = Duration::from_secs(60);

    let client = MqttClient::with_options(options);

    client.connect(broker_a.address()).await.unwrap();
    assert_eq!(
        client.keep_alive().await,
        Duration::from_secs(10),
        "first CONNECT must adopt broker A's ServerKeepAlive",
    );
    client.disconnect().await.unwrap();

    client.connect(broker_b.address()).await.unwrap();
    assert_eq!(
        client.keep_alive().await,
        Duration::from_secs(25),
        "reconnect must re-negotiate from the original 60s request, not the previously-negotiated 10s",
    );
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn broker_read_timeout_uses_negotiated_keep_alive_not_client_request() {
    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };
    let mut config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage_config);
    config.server_keep_alive = Some(Duration::from_secs(1));

    let broker = TestBroker::start_with_config(config).await;
    let mut stream = connect_raw(&broker, "kanego-broker-timeout", 600).await;

    let elapsed = time_until_close(&mut stream).await;
    assert!(
        elapsed >= Duration::from_millis(1400) && elapsed < Duration::from_millis(1800),
        "broker disconnect took {elapsed:?}; expected ~1.5s (negotiated 1s * 1.5)",
    );
}

#[tokio::test]
async fn broker_closes_idle_connection_after_one_and_a_half_keep_alives() {
    let broker = TestBroker::start().await;
    let mut stream = connect_raw(&broker, "kanego-client-timeout", 2).await;

    let elapsed = time_until_close(&mut stream).await;
    assert!(
        elapsed >= Duration::from_millis(2900) && elapsed < Duration::from_millis(3300),
        "broker disconnect took {elapsed:?}; expected ~3s (client 2s * 1.5)",
    );
}

#[tokio::test]
async fn pingreq_moves_the_broker_keep_alive_deadline() {
    let broker = TestBroker::start().await;
    let mut stream = connect_raw(&broker, "kanego-ping", 2).await;

    for _ in 0..4 {
        tokio::time::sleep(Duration::from_millis(1400)).await;
        stream
            .write_all(&[0xC0, 0x00])
            .await
            .expect("write PINGREQ");
        let mut pingresp = [0u8; 2];
        tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut pingresp))
            .await
            .expect("PINGRESP read timed out")
            .expect("broker closed the connection despite PINGREQs within the keep alive");
        assert_eq!(pingresp, [0xD0, 0x00]);
    }
}

async fn connect_raw(broker: &TestBroker, client_id: &str, keep_alive: u16) -> TcpStream {
    let addr = broker.address().trim_start_matches("mqtt://").to_string();
    let mut stream = TcpStream::connect(&addr).await.expect("connect tcp");

    let connect = ConnectPacket {
        protocol_version: 5,
        clean_start: true,
        keep_alive,
        client_id: client_id.to_string(),
        username: None,
        password: None,
        will: None,
        properties: Properties::new(),
        will_properties: Properties::new(),
    };
    let mut buf = Vec::new();
    connect.encode(&mut buf).expect("encode CONNECT");
    stream.write_all(&buf).await.expect("write CONNECT");
    stream.flush().await.expect("flush");

    let mut connack = [0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut connack))
        .await
        .expect("CONNACK read timed out")
        .expect("read CONNACK");
    assert!(n > 0, "broker closed before sending CONNACK");
    stream
}

async fn time_until_close(stream: &mut TcpStream) -> Duration {
    let start = tokio::time::Instant::now();
    let mut drain = [0u8; 256];
    let bytes = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut drain))
        .await
        .expect("broker did not close the idle connection within 10s")
        .expect("socket read error");
    assert_eq!(
        bytes, 0,
        "expected EOF after broker timeout, got {bytes} bytes"
    );
    start.elapsed()
}

#[tokio::test]
async fn server_keep_alive_zero_disables_keepalive_even_with_nonzero_client_request() {
    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };
    let mut config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage_config);
    config.server_keep_alive = Some(Duration::ZERO);

    let broker = TestBroker::start_with_config(config).await;

    let mut options = ConnectOptions::new("kanego-5");
    options.keep_alive = Duration::from_secs(60);

    let client = MqttClient::with_options(options);
    client.connect(broker.address()).await.unwrap();

    assert_eq!(
        client.keep_alive().await,
        Duration::ZERO,
        "ServerKeepAlive=0 must disable keep-alive even when the client requested a non-zero interval",
    );

    client.disconnect().await.unwrap();
}
