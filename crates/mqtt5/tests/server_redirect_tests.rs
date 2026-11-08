#![cfg(feature = "broker")]
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

/// A load balancer listening over TLS that redirects every client to
/// `backend`, as the reference it is given says
async fn tls_balancer(backend: String) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    use mqtt5::broker::config::TlsConfig as BrokerTlsConfig;
    use mqtt5::broker::MqttBroker;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certs = common::find_workspace_root().join("test_certs");
    let config = lb_config(vec![backend]).with_tls(
        BrokerTlsConfig::new(certs.join("server.pem"), certs.join("server.key"))
            .with_require_client_cert(false)
            .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap()),
    );
    let mut balancer = MqttBroker::with_config(config).await.unwrap();
    let address = balancer.tls_local_addr().expect("TLS listener bound");
    let handle = tokio::spawn(async move {
        let _ = balancer.run().await;
    });
    (address, handle)
}

#[tokio::test]
async fn a_redirect_without_a_scheme_stays_on_tcp() {
    let backend = TestBroker::start_with_config(backend_config()).await;
    let reference = backend.address().strip_prefix("mqtt://").unwrap();
    let lb = TestBroker::start_with_config(lb_config(vec![reference.to_string()])).await;
    let client = MqttClient::new("redirect-stays-on-tcp");
    client
        .connect(lb.address())
        .await
        .expect("redirected over TCP to the backend");
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn a_redirect_without_a_scheme_keeps_tls() {
    let backend = TestBroker::start_with_tls().await;
    let reference = backend.address().strip_prefix("mqtts://").unwrap();
    let (balancer, handle) = tls_balancer(reference.to_string()).await;
    let client = MqttClient::new("redirect-keeps-tls");
    client.set_insecure_tls(true).await;
    client
        .connect(&format!("mqtts://{balancer}"))
        .await
        .expect("redirected over TLS to the backend's TLS port");
    client.disconnect().await.unwrap();
    handle.abort();
}

#[tokio::test]
async fn a_redirect_without_a_scheme_keeps_tls_after_connect_with_tls() {
    let backend = TestBroker::start_with_tls().await;
    let reference = backend.address().strip_prefix("mqtts://").unwrap();
    let (balancer, handle) = tls_balancer(reference.to_string()).await;
    let client = MqttClient::new("redirect-keeps-tls-from-config");
    client.set_insecure_tls(true).await;
    client
        .connect_with_tls(
            mqtt5::transport::tls::TlsConfig::new(balancer, "localhost")
                .with_verify_server_cert(false),
        )
        .await
        .expect("redirected over TLS to the backend's TLS port");
    client.disconnect().await.unwrap();
    handle.abort();
}

#[tokio::test]
async fn a_redirect_from_tls_to_plain_tcp_is_refused() {
    let backend = TestBroker::start_with_config(backend_config()).await;
    let (balancer, handle) = tls_balancer(backend.address().to_string()).await;
    let client = MqttClient::new("redirect-tls-to-tcp");
    client.set_insecure_tls(true).await;
    let error = client
        .connect(&format!("mqtts://{balancer}"))
        .await
        .expect_err("a connection over TLS is not redirected to one without");
    assert!(
        error.to_string().contains("not over TLS"),
        "refused for that reason: {error}"
    );
    assert!(!client.is_connected().await);
    handle.abort();
}

#[tokio::test]
async fn connect_with_tls_follows_a_redirect_with_its_configuration() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let backend = TestBroker::start_with_tls().await;
    let (backend_proxy, cut) = common::cutting_proxy(
        backend
            .address()
            .strip_prefix("mqtts://")
            .unwrap()
            .parse()
            .unwrap(),
    )
    .await;
    let (balancer, handle) = tls_balancer(backend_proxy.to_string()).await;
    let client = MqttClient::with_options(
        mqtt5::ConnectOptions::new("redirect-with-its-configuration")
            .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(200)),
    );
    let connections = Arc::new(AtomicUsize::new(0));
    let counted = connections.clone();
    client
        .on_connection_event(move |event| {
            if matches!(event, mqtt5::ConnectionEvent::Connected { .. }) {
                counted.fetch_add(1, Ordering::SeqCst);
            }
        })
        .await
        .unwrap();
    let mut config =
        mqtt5::transport::tls::TlsConfig::new(balancer, "localhost").with_system_roots(false);
    config.load_ca_cert_pem_bytes(&common::test_ca()).unwrap();
    client
        .connect_with_tls(config)
        .await
        .expect("redirected to the backend, trusting the CA given");
    // A TLS configuration stored since is for connections made from a URL:
    // the reconnection, redirected again, still trusts the CA it was given.
    client
        .set_tls_config(None, None, Some(common::other_ca()))
        .await;
    cut.notify_waiters();
    tokio::time::timeout(Duration::from_secs(10), async {
        while connections.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("reconnected through the balancer to the backend");
    client.disconnect().await.unwrap();
    handle.abort();
}

#[cfg(feature = "transport-quic")]
#[tokio::test]
async fn connect_with_tls_is_redirected_only_over_tls() {
    let (balancer, handle) = tls_balancer("quics://127.0.0.1:14567".to_string()).await;
    let client = MqttClient::new("redirect-with-tls-to-quic");
    let error = client
        .connect_with_tls(
            mqtt5::transport::tls::TlsConfig::new(balancer, "localhost")
                .with_verify_server_cert(false),
        )
        .await
        .expect_err("its configuration is for TLS over TCP");
    assert!(
        error.to_string().contains("only redirected to mqtts://"),
        "refused for that reason: {error}"
    );
    handle.abort();
}

/// A broker taking MQTT over WebSocket over TLS, at `wss://localhost:<port>/mqtt`,
/// that redirects every client to `backend` if there is one
#[cfg(feature = "transport-websocket")]
async fn wss_broker(backend: Option<String>) -> (u16, tokio::task::JoinHandle<()>) {
    use mqtt5::broker::config::{TlsConfig as BrokerTlsConfig, WebSocketConfig};
    use mqtt5::broker::MqttBroker;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certs = common::find_workspace_root().join("test_certs");
    let port = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = match backend {
        Some(backend) => lb_config(vec![backend]),
        None => backend_config(),
    }
    .with_tls(
        BrokerTlsConfig::new(certs.join("server.pem"), certs.join("server.key"))
            .with_require_client_cert(false)
            .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap()),
    )
    .with_websocket_tls(
        WebSocketConfig::new()
            .with_bind_addresses(vec![format!("127.0.0.1:{port}").parse().unwrap()])
            .with_path("/mqtt".to_string()),
    );
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    (port, handle)
}

/// Options whose WebSocket configuration trusts the test CA alone
#[cfg(feature = "transport-websocket")]
fn trusting_the_test_ca(client_id: &str, url: &str) -> mqtt5::ConnectOptions {
    let mut tls =
        mqtt5::transport::tls::TlsConfig::new("127.0.0.1:0".parse().unwrap(), "localhost")
            .with_system_roots(false);
    tls.load_ca_cert_pem_bytes(&common::test_ca()).unwrap();
    mqtt5::ConnectOptions::new(client_id).with_websocket_config(
        mqtt5::transport::websocket::WebSocketConfig::new(url)
            .unwrap()
            .with_tls_config(tls),
    )
}

#[cfg(feature = "transport-websocket")]
#[tokio::test]
async fn a_wss_redirect_keeps_the_websocket_configurations_tls() {
    let (backend, backend_handle) = wss_broker(None).await;
    let (balancer, handle) = wss_broker(Some(format!("wss://localhost:{backend}/mqtt"))).await;
    let direct = format!("wss://localhost:{backend}/mqtt");
    let client = MqttClient::with_options(trusting_the_test_ca("wss-redirect-keeps-tls", &direct));
    client.connect(&direct).await.expect("trusting the test CA");
    client.disconnect().await.unwrap();
    let url = format!("wss://localhost:{balancer}/mqtt");
    client
        .connect(&url)
        .await
        .expect("redirected to the backend, trusting the test CA");
    client.disconnect().await.unwrap();
    handle.abort();
    backend_handle.abort();
}

#[cfg(feature = "transport-websocket")]
#[tokio::test]
async fn a_wss_redirect_with_its_own_tls_is_refused_to_another_transport() {
    let backend = TestBroker::start_with_tls().await;
    let (balancer, handle) = wss_broker(Some(backend.address().to_string())).await;
    let url = format!("wss://localhost:{balancer}/mqtt");
    let client = MqttClient::with_options(trusting_the_test_ca("wss-redirect-to-mqtts", &url));
    let error = client
        .connect(&url)
        .await
        .expect_err("the WebSocket configuration's TLS is for wss:// alone");
    assert!(
        error.to_string().contains("WebSocket configuration"),
        "refused for that reason: {error}"
    );
    handle.abort();
}
