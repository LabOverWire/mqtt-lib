#![cfg(feature = "broker")]
use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection, ConnectedBroker};
use mqtt5::broker::config::TlsConfig as BrokerTlsConfig;
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::{BrokerConfig, BrokerShutdownHandle, MqttBroker, StorageConfig};
use mqtt5::client::MqttClient;
use mqtt5::packet::publish::PublishPacket;
use mqtt5::time::Duration;
use mqtt5::{ConnectOptions, QoS};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

fn certs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test_certs")
}

struct Remote {
    plain: SocketAddr,
    task: JoinHandle<()>,
    shutdown: BrokerShutdownHandle,
}

impl Remote {
    async fn stop(self) {
        self.shutdown.shutdown();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.task).await;
    }
}

async fn start_remote(port: u16, tls: bool) -> Remote {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let tested: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let storage = StorageConfig::new().with_persistence(false);
    let config = if tls {
        BrokerConfig::default()
            .with_bind_address(([127, 0, 0, 1], 0))
            .with_storage(storage)
            .with_tls(
                BrokerTlsConfig::new(certs().join("server.pem"), certs().join("server.key"))
                    .with_bind_address(tested),
            )
    } else {
        BrokerConfig::default()
            .with_bind_addresses(vec![tested, "127.0.0.1:0".parse().unwrap()])
            .with_storage(storage)
    };
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let plain = if tls {
        broker.local_addr().unwrap()
    } else {
        tested
    };
    let mut ready = broker.ready_receiver();
    let shutdown = broker.shutdown_handle();
    let task = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    while !*ready.borrow_and_update() {
        ready.changed().await.unwrap();
    }
    Remote {
        plain,
        task,
        shutdown,
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn bridge_config(name: &str, port: u16, tls: bool) -> BridgeConfig {
    let mut config = BridgeConfig::new(name, format!("127.0.0.1:{port}"))
        .add_topic("in/#", BridgeDirection::In, QoS::AtLeastOnce)
        .add_topic("out/#", BridgeDirection::Out, QoS::AtLeastOnce);
    config.retry_jitter = false;
    if tls {
        config.use_tls = true;
        config.tls_server_name = Some("localhost".to_string());
        config.ca_file = Some(certs().join("ca.pem").to_string_lossy().into_owned());
    }
    config
}

fn run_bridge(config: BridgeConfig) -> (Arc<BridgeConnection>, JoinHandle<()>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let bridge =
        Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).expect("config"));
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move {
        let result = running.run().await;
        eprintln!("bridge run() returned {result:?}");
    });
    (bridge, task)
}

async fn plain_client(addr: SocketAddr, id: &str) -> MqttClient {
    let client = MqttClient::new(id);
    client
        .connect_with_options(
            &format!("mqtt://{addr}"),
            ConnectOptions::new(id).with_automatic_reconnect(false),
        )
        .await
        .expect("test client connects");
    client
}

async fn collect(client: &MqttClient, filter: &str) -> Arc<Mutex<Vec<String>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    client
        .subscribe(filter, move |message| {
            sink.lock().unwrap().push(message.topic.clone());
        })
        .await
        .expect("subscribe");
    seen
}

async fn report(label: &str, bridge: &BridgeConnection) -> mqtt5::broker::bridge::BridgeStats {
    let stats = bridge.get_stats().await;
    let broker = *bridge.current_broker().read().await;
    eprintln!(
        "{label}: connected={} current_broker_stat={:?} recorded={broker:?} received={} sent={} attempts={} last_error={:?}",
        stats.connected,
        stats.current_broker,
        stats.messages_received,
        stats.messages_sent,
        stats.connection_attempts,
        stats.last_error
    );
    stats
}

async fn starts_while_remote_is_down(name: &str, tls: bool) {
    let port = free_port();
    let (bridge, task) = run_bridge(bridge_config(name, port, tls));
    tokio::time::sleep(Duration::from_millis(5500)).await;
    let remote = start_remote(port, tls).await;
    let publisher = plain_client(remote.plain, &format!("{name}-pub")).await;

    tokio::time::sleep(Duration::from_secs(18)).await;
    publisher
        .publish_qos("in/a", b"hello".to_vec(), QoS::AtLeastOnce)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;

    let stats = report(name, &bridge).await;
    let recorded = *bridge.current_broker().read().await;
    let _ = bridge.stop().await;
    task.abort();
    remote.stop().await;
    assert!(stats.connected, "the bridge records that it connected");
    assert_eq!(recorded, Some(ConnectedBroker::Primary));
    assert_eq!(stats.messages_received, 1, "the bridge subscribed");
}

#[tokio::test]
async fn start_with_remote_down_tls() {
    starts_while_remote_is_down("s1-tls", true).await;
}

#[tokio::test]
async fn start_with_remote_down_plain() {
    starts_while_remote_is_down("s1-plain", false).await;
}

async fn run_out_of_attempts(name: &str, tls: bool) {
    let port = free_port();
    let mut config = bridge_config(name, port, tls);
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(200);
    config.max_reconnect_attempts = Some(4);
    let (bridge, task) = run_bridge(config);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let remote = start_remote(port, tls).await;
    let _ = tokio::time::timeout(Duration::from_secs(25), async {
        while !task.is_finished() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    let finished = task.is_finished();
    let stats = report(name, &bridge).await;
    let _ = bridge.stop().await;
    task.abort();
    remote.stop().await;
    assert!(!finished, "the bridge's run ended though the remote is up");
    assert!(stats.connected);
}

#[tokio::test]
async fn max_attempts_tls() {
    run_out_of_attempts("s1b-tls", true).await;
}

async fn outage_with_a_message_pending(name: &str, tls: bool) {
    let port = free_port();
    let remote = start_remote(port, tls).await;
    let (bridge, task) = run_bridge(bridge_config(name, port, tls));
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(bridge.get_stats().await.connected, "first connect");

    remote.stop().await;
    tokio::time::sleep(Duration::from_millis(5500)).await;
    let packet = PublishPacket::new("out/pending".to_string(), &b"x"[..], QoS::AtLeastOnce);
    bridge.forward_message(&packet);

    let remote = start_remote(port, tls).await;
    let subscriber = plain_client(remote.plain, &format!("{name}-sub")).await;
    let seen = collect(&subscriber, "out/#").await;
    let publisher = plain_client(remote.plain, &format!("{name}-pub")).await;

    tokio::time::sleep(Duration::from_secs(18)).await;
    publisher
        .publish_qos("in/a", b"hello".to_vec(), QoS::AtLeastOnce)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;

    let stats = report(name, &bridge).await;
    let recorded = *bridge.current_broker().read().await;
    let delivered = seen.lock().unwrap().clone();
    eprintln!("{name}: delivered to remote {delivered:?}");
    let _ = bridge.stop().await;
    task.abort();
    remote.stop().await;
    assert_eq!(stats.messages_received, 1, "subscriptions came back");
    assert_eq!(
        delivered,
        vec!["out/pending".to_string()],
        "pending flushed"
    );
    assert!(stats.connected);
    assert_eq!(recorded, Some(ConnectedBroker::Primary));
}

#[tokio::test]
async fn outage_pending_tls() {
    outage_with_a_message_pending("s2-tls", true).await;
}

async fn failover_to_backup(name: &str, tls: bool) {
    let primary_port = free_port();
    let backup_port = free_port();
    let primary = start_remote(primary_port, tls).await;
    let backup = start_remote(backup_port, tls).await;
    let mut config = bridge_config(name, primary_port, tls);
    config.enable_failback = false;
    config.backup_brokers = vec![format!("127.0.0.1:{backup_port}")];
    let (bridge, task) = run_bridge(config);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        *bridge.current_broker().read().await,
        Some(ConnectedBroker::Primary)
    );
    let publisher = plain_client(backup.plain, &format!("{name}-pub")).await;

    primary.stop().await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    publisher
        .publish_qos("in/a", b"hello".to_vec(), QoS::AtLeastOnce)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;

    let stats = report(name, &bridge).await;
    let recorded = *bridge.current_broker().read().await;
    let _ = bridge.stop().await;
    task.abort();
    backup.stop().await;
    assert_eq!(recorded, Some(ConnectedBroker::Backup(0)));
    assert!(stats.connected);
    assert_eq!(stats.messages_received, 1, "subscribed on the backup");
}

#[tokio::test]
async fn failover_tls() {
    failover_to_backup("s3-tls", true).await;
}

async fn counting_proxy(to: u16) -> (u16, Arc<AtomicUsize>, Arc<tokio::sync::Notify>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let count = Arc::new(AtomicUsize::new(0));
    let cut = Arc::new(tokio::sync::Notify::new());
    let counted = Arc::clone(&count);
    let cuts = Arc::clone(&cut);
    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            let cuts = Arc::clone(&cuts);
            tokio::spawn(async move {
                let mut server = tokio::net::TcpStream::connect(("127.0.0.1", to))
                    .await
                    .unwrap();
                tokio::select! {
                    _ = tokio::io::copy_bidirectional(&mut client, &mut server) => {}
                    () = cuts.notified() => {}
                }
            });
        }
    });
    (port, count, cut)
}

async fn short_cut(name: &str, tls: bool) {
    let port = free_port();
    let remote = start_remote(port, tls).await;
    let (proxy, count, cut) = counting_proxy(port).await;
    let publisher = plain_client(remote.plain, &format!("{name}-pub")).await;
    let (bridge, task) = run_bridge(bridge_config(name, proxy, tls));
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(count.load(Ordering::SeqCst), 1);

    cut.notify_one();
    tokio::time::sleep(Duration::from_secs(6)).await;
    publisher
        .publish_qos("in/a", b"hello".to_vec(), QoS::AtLeastOnce)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;

    let stats = report(name, &bridge).await;
    let recorded = *bridge.current_broker().read().await;
    let connections = count.load(Ordering::SeqCst);
    eprintln!("{name}: connections through proxy {connections}");
    let _ = bridge.stop().await;
    task.abort();
    remote.stop().await;
    assert_eq!(connections, 2, "one reconnection");
    assert!(stats.connected);
    assert_eq!(recorded, Some(ConnectedBroker::Primary));
    assert_eq!(stats.messages_received, 1);
}

#[tokio::test]
async fn short_cut_tls() {
    short_cut("s4-tls", true).await;
}

async fn stop_while_remote_is_down(name: &str, tls: bool) {
    let port = free_port();
    let (bridge, task) = run_bridge(bridge_config(name, port, tls));
    tokio::time::sleep(Duration::from_millis(5500)).await;
    bridge.stop().await.unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    let accepted = tokio::time::timeout(Duration::from_secs(8), listener.accept()).await;
    task.abort();
    assert!(accepted.is_err(), "the stopped bridge's client came back");
}

#[tokio::test]
async fn stop_while_down_tls() {
    stop_while_remote_is_down("s5-tls", true).await;
}

async fn both_down_then_both_up(name: &str, tls: bool) {
    let primary_port = free_port();
    let backup_port = free_port();
    let mut config = bridge_config(name, primary_port, tls);
    config.backup_brokers = vec![format!("127.0.0.1:{backup_port}")];
    let (bridge, task) = run_bridge(config);
    tokio::time::sleep(Duration::from_millis(5500)).await;
    let primary = start_remote(primary_port, tls).await;
    let backup = start_remote(backup_port, tls).await;
    let on_primary = plain_client(primary.plain, &format!("{name}-pub-primary")).await;

    tokio::time::sleep(Duration::from_secs(18)).await;
    on_primary
        .publish_qos("in/a", b"hello".to_vec(), QoS::AtLeastOnce)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;

    let stats = report(name, &bridge).await;
    let recorded = *bridge.current_broker().read().await;
    let _ = bridge.stop().await;
    task.abort();
    primary.stop().await;
    backup.stop().await;
    assert_eq!(recorded, Some(ConnectedBroker::Primary));
    assert_eq!(stats.messages_received, 1, "subscribed on the primary");
}

#[tokio::test]
async fn both_down_then_both_up_tls() {
    both_down_then_both_up("s6-tls", true).await;
}

async fn silent_server(port: u16, tls: bool) -> Arc<AtomicUsize> {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    let acceptor = tls.then(|| {
        let chain: Vec<CertificateDer<'static>> =
            CertificateDer::pem_file_iter(certs().join("server.pem"))
                .unwrap()
                .map(Result::unwrap)
                .collect();
        let key = PrivateKeyDer::from_pem_file(certs().join("server.key")).unwrap();
        tokio_rustls::TlsAcceptor::from(Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(chain, key)
                .unwrap(),
        ))
    });
    let connects = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&connects);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let counted = Arc::clone(&counted);
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                if let Some(acceptor) = acceptor {
                    if let Ok(mut stream) = acceptor.accept(stream).await {
                        let _ = stream.read(&mut buffer).await;
                        counted.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_secs(300)).await;
                    }
                } else {
                    let mut stream = stream;
                    let _ = stream.read(&mut buffer).await;
                    counted.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_secs(300)).await;
                }
            });
        }
    });
    connects
}

async fn stop_while_a_connect_waits_for_connack(name: &str, tls: bool) {
    let port = free_port();
    let (bridge, task) = run_bridge(bridge_config(name, port, tls));
    tokio::time::sleep(Duration::from_millis(5500)).await;
    let connects = silent_server(port, tls).await;
    let waited = std::time::Instant::now();
    while connects.load(Ordering::SeqCst) == 0 && waited.elapsed() < Duration::from_secs(12) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let reached_after = waited.elapsed();
    let started = std::time::Instant::now();
    bridge.stop().await.unwrap();
    let took = started.elapsed();
    eprintln!(
        "{name}: CONNECT reached the server after {reached_after:?} ({} times), stop took {took:?}",
        connects.load(Ordering::SeqCst)
    );
    task.abort();
    assert!(took < Duration::from_secs(5), "stop took {took:?}");
}

#[tokio::test]
async fn stop_while_waiting_for_connack_tls() {
    stop_while_a_connect_waits_for_connack("s7-tls", true).await;
}
