#![cfg(feature = "broker")]
use mqtt5::broker::bridge::{BridgeConfig, BridgeDirection};
use mqtt5::broker::{BrokerConfig, BrokerShutdownHandle, MqttBroker, StorageConfig};
use mqtt5::client::MqttClient;
use mqtt5::time::Duration;
use mqtt5::{PublishOptions, QoS};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

fn broker_config(bind: SocketAddr) -> BrokerConfig {
    BrokerConfig::default()
        .with_bind_address(bind)
        .with_max_clients(100)
        .with_storage(StorageConfig::new().with_persistence(false))
}

struct RunningBroker {
    addr: SocketAddr,
    task: JoinHandle<()>,
    shutdown: BrokerShutdownHandle,
}

impl RunningBroker {
    async fn stop(self) {
        self.shutdown.shutdown();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.task).await;
    }
}

async fn start_broker(config: BrokerConfig) -> RunningBroker {
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let addr = broker.local_addr().unwrap();
    let mut ready = broker.ready_receiver();
    let shutdown = broker.shutdown_handle();
    let task = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    while !*ready.borrow_and_update() {
        ready.changed().await.unwrap();
    }
    RunningBroker {
        addr,
        task,
        shutdown,
    }
}

fn free_port() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

async fn read_raw_packet(stream: &mut tokio::net::tcp::OwnedReadHalf) -> Option<Vec<u8>> {
    let first = stream.read_u8().await.ok()?;
    let mut raw = vec![first];
    let mut length = 0usize;
    let mut shift = 0;
    loop {
        let byte = stream.read_u8().await.ok()?;
        raw.push(byte);
        length |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).await.ok()?;
    raw.extend_from_slice(&body);
    Some(raw)
}

async fn suback_dropping_proxy(
    listener: TcpListener,
    upstream: SocketAddr,
    drops_left: Arc<AtomicUsize>,
    log: Arc<Mutex<Vec<String>>>,
) {
    while let Ok((client, _)) = listener.accept().await {
        let Ok(server) = TcpStream::connect(upstream).await else {
            continue;
        };
        let (mut client_read, mut client_write) = client.into_split();
        let (mut server_read, mut server_write) = server.into_split();
        let drops_left = Arc::clone(&drops_left);
        let log = Arc::clone(&log);
        let up_log = Arc::clone(&log);
        tokio::spawn(async move {
            let upward = tokio::spawn(async move {
                while let Some(packet) = read_raw_packet(&mut client_read).await {
                    let kind = packet[0] >> 4;
                    if kind == 1 || kind == 8 || kind == 10 {
                        up_log.lock().unwrap().push(format!(
                            "up type {kind} last byte {:#x}",
                            packet[packet.len() - 1]
                        ));
                    }
                    if server_write.write_all(&packet).await.is_err() {
                        break;
                    }
                }
            });
            while let Some(packet) = read_raw_packet(&mut server_read).await {
                if packet[0] >> 4 == 9
                    && drops_left
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                        .is_ok()
                {
                    log.lock().unwrap().push("dropped SUBACK".to_string());
                    break;
                }
                let kind = packet[0] >> 4;
                if kind == 2 || kind == 3 || kind == 9 {
                    log.lock().unwrap().push(format!(
                        "down type {kind} byte2 {:#x}",
                        packet.get(2).copied().unwrap_or(0)
                    ));
                }
                if client_write.write_all(&packet).await.is_err() {
                    break;
                }
            }
            upward.abort();
        });
    }
}

async fn remote_with_retained_state(message_qos: QoS) -> (RunningBroker, MqttClient) {
    let remote = start_broker(
        BrokerConfig::default()
            .with_bind_address(free_port())
            .with_max_clients(100)
            .with_storage(StorageConfig {
                backend: mqtt5::broker::config::StorageBackend::Memory,
                enable_persistence: true,
                ..Default::default()
            }),
    )
    .await;
    let publisher = MqttClient::new("dropped-suback-pub");
    publisher
        .connect(&format!("mqtt://{}", remote.addr))
        .await
        .unwrap();
    publisher
        .publish_with_options(
            "sensors/state",
            b"on".to_vec(),
            PublishOptions {
                qos: message_qos,
                retain: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    (remote, publisher)
}

async fn watch_all(local: SocketAddr) -> (MqttClient, Arc<Mutex<Vec<String>>>) {
    let received = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&received);
    let watcher = MqttClient::new("dropped-suback-watch");
    watcher.connect(&format!("mqtt://{local}")).await.unwrap();
    watcher
        .subscribe_with_options(
            "#",
            mqtt5::SubscribeOptions {
                qos: QoS::AtLeastOnce,
                ..Default::default()
            },
            move |msg| sink.lock().unwrap().push(msg.topic.clone()),
        )
        .await
        .unwrap();
    (watcher, received)
}

async fn retained_copies_after_dropped_suback(
    message_qos: QoS,
    qos: QoS,
) -> (usize, usize, Vec<String>) {
    let (remote, publisher) = remote_with_retained_state(message_qos).await;

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let proxy = tokio::spawn(suback_dropping_proxy(
        proxy_listener,
        remote.addr,
        Arc::new(AtomicUsize::new(1)),
        Arc::clone(&log),
    ));

    let mut bridge = BridgeConfig::new("dropped-suback", proxy_addr.to_string()).add_topic(
        "sensors/#",
        BridgeDirection::In,
        qos,
    );
    bridge.first_retry_delay = Duration::from_millis(100);
    bridge.initial_reconnect_delay = Duration::from_millis(200);
    bridge.max_reconnect_delay = Duration::from_millis(300);
    bridge.retry_jitter = false;
    let mut local_config = broker_config(free_port());
    local_config.bridges = vec![bridge];
    let local = start_broker(local_config).await;

    let (watcher, received) = watch_all(local.addr).await;

    let started = tokio::time::Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        let probe_seen = received
            .lock()
            .unwrap()
            .iter()
            .any(|t| t == "sensors/probe");
        if probe_seen {
            break;
        }
        publisher
            .publish_qos1("sensors/probe", b"x".to_vec())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let state = received
        .lock()
        .unwrap()
        .iter()
        .filter(|t| *t == "sensors/state")
        .count();
    let probes = received
        .lock()
        .unwrap()
        .iter()
        .filter(|t| *t == "sensors/probe")
        .count();
    let proxy_log = log.lock().unwrap().clone();
    let _ = watcher.disconnect().await;
    let _ = publisher.disconnect().await;
    local.stop().await;
    proxy.abort();
    remote.stop().await;
    (state, probes, proxy_log)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qos0_retained_once_when_connection_drops_before_suback() {
    let (state, probes, proxy_log) =
        retained_copies_after_dropped_suback(QoS::AtMostOnce, QoS::AtMostOnce).await;
    assert!(probes > 0, "bridge never resumed; proxy log {proxy_log:?}");
    assert_eq!(
        state, 1,
        "retained sensors/state reached the local broker {state} times; proxy log {proxy_log:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qos0_retained_on_qos1_mapping_once_when_connection_drops_before_suback() {
    let (state, probes, proxy_log) =
        retained_copies_after_dropped_suback(QoS::AtMostOnce, QoS::AtLeastOnce).await;
    assert!(probes > 0, "bridge never resumed; proxy log {proxy_log:?}");
    assert_eq!(
        state, 1,
        "retained sensors/state reached the local broker {state} times; proxy log {proxy_log:?}"
    );
}

async fn overlapping_retained_after_failover(
    primary_ids: bool,
    backup_ids: bool,
) -> (usize, usize) {
    let primary_bind = free_port();
    let mut primary_config = broker_config(primary_bind);
    primary_config.subscription_identifier_available = primary_ids;
    let primary = start_broker(primary_config).await;
    let mut backup_config = broker_config(free_port());
    backup_config.subscription_identifier_available = backup_ids;
    let backup = start_broker(backup_config).await;
    let publisher = MqttClient::new(format!("pub-{primary_ids}-{backup_ids}"));
    publisher
        .connect(&format!("mqtt://{}", backup.addr))
        .await
        .unwrap();
    publisher
        .publish_retain("sensors/x/temp", b"r".to_vec())
        .await
        .unwrap();
    publisher.disconnect().await.unwrap();

    let mut bridge = BridgeConfig::new("overlap", primary_bind.to_string())
        .add_topic("sensors/#", BridgeDirection::In, QoS::AtLeastOnce)
        .add_topic("sensors/+/temp", BridgeDirection::In, QoS::AtLeastOnce)
        .add_backup_broker(backup.addr.to_string());
    bridge.topics[0].local_prefix = Some("l1/".to_string());
    bridge.topics[1].local_prefix = Some("l2/".to_string());
    bridge.enable_failback = false;
    bridge.first_retry_delay = Duration::from_millis(200);
    bridge.initial_reconnect_delay = Duration::from_millis(200);
    bridge.max_reconnect_delay = Duration::from_millis(500);
    bridge.retry_jitter = false;
    let mut local_config = broker_config(free_port());
    local_config.bridges = vec![bridge];
    let local = start_broker(local_config).await;
    let received = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&received);
    let watcher = MqttClient::new(format!("watch-{primary_ids}-{backup_ids}"));
    watcher
        .connect(&format!("mqtt://{}", local.addr))
        .await
        .unwrap();
    watcher
        .subscribe_with_options(
            "#",
            mqtt5::SubscribeOptions {
                qos: QoS::AtLeastOnce,
                ..Default::default()
            },
            move |msg| sink.lock().unwrap().push(msg.topic.clone()),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    primary.stop().await;
    let started = tokio::time::Instant::now();
    while started.elapsed() < Duration::from_secs(5)
        && !received
            .lock()
            .unwrap()
            .iter()
            .any(|t| t == "l1/sensors/x/temp")
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let count = |topic: &str| {
        received
            .lock()
            .unwrap()
            .iter()
            .filter(|t| *t == topic)
            .count()
    };
    let copies = (count("l1/sensors/x/temp"), count("l2/sensors/x/temp"));
    let _ = watcher.disconnect().await;
    local.stop().await;
    backup.stop().await;
    copies
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlapping_retained_once_after_failover_gaining_identifiers() {
    assert_eq!(
        overlapping_retained_after_failover(false, true).await,
        (1, 1)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlapping_retained_once_after_failover_both_identifiers_control() {
    assert_eq!(
        overlapping_retained_after_failover(true, true).await,
        (1, 1)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlapping_retained_once_after_failover_losing_identifiers_control() {
    assert_eq!(
        overlapping_retained_after_failover(true, false).await,
        (1, 1)
    );
}
