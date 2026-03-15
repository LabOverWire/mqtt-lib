mod common;

use common::{MessageCollector, TestBroker};
use mqtt5::broker::config::{BrokerConfig, LoadBalancerConfig, StorageBackend, StorageConfig};
use mqtt5::MqttClient;
use mqtt5_protocol::packet::connack::ConnAckPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn memory_storage() -> StorageConfig {
    StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    }
}

fn lb_config(backends: Vec<String>) -> BrokerConfig {
    BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(memory_storage())
        .with_load_balancer(LoadBalancerConfig::new(backends))
}

fn backend_config() -> BrokerConfig {
    BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(memory_storage())
}

#[tokio::test]
async fn test_basic_redirect() {
    let backend1 = TestBroker::start_with_config(backend_config()).await;
    let backend2 = TestBroker::start_with_config(backend_config()).await;

    let lb = TestBroker::start_with_config(lb_config(vec![
        backend1.address().to_string(),
        backend2.address().to_string(),
    ]))
    .await;

    let client = MqttClient::new("redirect-test-client");
    client.connect(lb.address()).await.unwrap();
    assert!(client.is_connected().await);

    let collector = MessageCollector::new();
    client
        .subscribe("test/redirect", collector.callback())
        .await
        .unwrap();

    client
        .publish("test/redirect", b"hello from redirect")
        .await
        .unwrap();

    assert!(collector.wait_for_messages(1, Duration::from_secs(3)).await);
    let msgs = collector.get_messages().await;
    assert_eq!(msgs[0].payload, b"hello from redirect");

    client.disconnect().await.unwrap();

    drop(lb);
    drop(backend1);
    drop(backend2);
}

#[tokio::test]
async fn test_redirect_to_dead_backend() {
    let lb = TestBroker::start_with_config(lb_config(vec!["mqtt://127.0.0.1:1".to_string()])).await;

    let client = MqttClient::new("redirect-dead-backend");
    let result = client.connect(lb.address()).await;
    assert!(result.is_err());

    drop(lb);
}

#[tokio::test]
async fn test_redirect_loop_capped() {
    let listener_a = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let listener_b = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr_a = listener_a.local_addr().unwrap();
    let addr_b = listener_b.local_addr().unwrap();

    let target_b = format!("mqtt://{addr_b}");
    let target_a = format!("mqtt://{addr_a}");

    let spawn_redirector = |listener: TcpListener, target: String, count: usize| {
        tokio::spawn(async move {
            for _ in 0..count {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = [0u8; 4096];
                if stream.read(&mut buf).await.is_err() {
                    break;
                }
                let connack = ConnAckPacket::new(false, ReasonCode::UseAnotherServer)
                    .with_server_reference(target.clone());
                let mut encoded = Vec::new();
                connack.encode(&mut encoded).unwrap();
                let _ = stream.write_all(&encoded).await;
                let _ = stream.flush().await;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
    };

    let _handle_a = spawn_redirector(listener_a, target_b, 4);
    let _handle_b = spawn_redirector(listener_b, target_a, 4);

    let client = MqttClient::new("redirect-loop-client");
    let result = client.connect(&format!("mqtt://{addr_a}")).await;
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("too many redirects"),
        "expected 'too many redirects' error, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_multiple_clients_distribute() {
    let backend1 = TestBroker::start_with_config(backend_config()).await;
    let backend2 = TestBroker::start_with_config(backend_config()).await;

    let lb = TestBroker::start_with_config(lb_config(vec![
        backend1.address().to_string(),
        backend2.address().to_string(),
    ]))
    .await;

    let mut connected = 0;
    for i in 0..10 {
        let client = MqttClient::new(format!("dist-client-{i}"));
        if client.connect(lb.address()).await.is_ok() {
            connected += 1;
            client.disconnect().await.ok();
        }
    }

    assert_eq!(connected, 10);

    drop(lb);
    drop(backend1);
    drop(backend2);
}

async fn mock_redirect_server(target: String, reason_code: ReasonCode) -> SocketAddr {
    mock_redirect_server_multi(target, reason_code, 1).await
}

async fn mock_redirect_server_multi(
    target: String,
    reason_code: ReasonCode,
    accept_count: usize,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        for _ in 0..accept_count {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let mut buf = [0u8; 4096];
            if stream.read(&mut buf).await.is_err() {
                break;
            }

            let connack =
                ConnAckPacket::new(false, reason_code).with_server_reference(target.clone());
            let mut encoded = Vec::new();
            connack.encode(&mut encoded).unwrap();
            let _ = stream.write_all(&encoded).await;
            let _ = stream.flush().await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });

    addr
}

#[tokio::test]
async fn test_server_moved_redirect() {
    let backend = TestBroker::start_with_config(backend_config()).await;
    let mock_addr =
        mock_redirect_server(backend.address().to_string(), ReasonCode::ServerMoved).await;

    let client = MqttClient::new("server-moved-client");
    client
        .connect(&format!("mqtt://{mock_addr}"))
        .await
        .unwrap();
    assert!(client.is_connected().await);

    let collector = MessageCollector::new();
    client
        .subscribe("test/moved", collector.callback())
        .await
        .unwrap();

    client
        .publish("test/moved", b"hello from moved")
        .await
        .unwrap();

    assert!(collector.wait_for_messages(1, Duration::from_secs(3)).await);
    let msgs = collector.get_messages().await;
    assert_eq!(msgs[0].payload, b"hello from moved");

    client.disconnect().await.unwrap();
    drop(backend);
}

#[tokio::test]
async fn test_empty_backends_acts_as_normal_broker() {
    let broker = TestBroker::start_with_config(lb_config(vec![])).await;

    let client = MqttClient::new("empty-backends-client");
    client.connect(broker.address()).await.unwrap();
    assert!(client.is_connected().await);

    let collector = MessageCollector::new();
    client
        .subscribe("test/empty-lb", collector.callback())
        .await
        .unwrap();

    client
        .publish("test/empty-lb", b"no redirect")
        .await
        .unwrap();

    assert!(collector.wait_for_messages(1, Duration::from_secs(3)).await);
    let msgs = collector.get_messages().await;
    assert_eq!(msgs[0].payload, b"no redirect");

    client.disconnect().await.unwrap();
    drop(broker);
}
