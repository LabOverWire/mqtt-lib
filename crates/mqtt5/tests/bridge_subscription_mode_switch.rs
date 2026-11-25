#![cfg(feature = "broker")]
use mqtt5::broker::bridge::{BridgeConfig, BridgeDirection};
use mqtt5::broker::{BrokerConfig, BrokerShutdownHandle, MqttBroker, StorageConfig};
use mqtt5::client::MqttClient;
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

fn broker_config(bind: SocketAddr) -> BrokerConfig {
    BrokerConfig::default()
        .with_bind_address(bind)
        .with_max_clients(100)
        .with_storage(StorageConfig::new().with_persistence(false))
}

async fn wait_for_ready(mut rx: tokio::sync::watch::Receiver<bool>) {
    while !*rx.borrow_and_update() {
        rx.changed().await.expect("broker ready channel closed");
    }
}

struct RunningBroker {
    addr: SocketAddr,
    task: JoinHandle<()>,
    shutdown: BrokerShutdownHandle,
}

impl RunningBroker {
    async fn stop(self) {
        self.shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .expect("broker stops on shutdown")
            .expect("broker task joins");
    }
}

async fn start_broker(config: BrokerConfig) -> RunningBroker {
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let addr = broker.local_addr().unwrap();
    let ready = broker.ready_receiver();
    let shutdown = broker.shutdown_handle();
    let task = tokio::spawn(async move {
        if let Err(e) = broker.run().await {
            eprintln!("broker run() error: {e}");
        }
    });
    wait_for_ready(ready).await;
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

async fn wait_until(deadline: Duration, cond: impl Fn() -> bool) -> bool {
    let started = tokio::time::Instant::now();
    while started.elapsed() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond()
}

async fn subscribe_all(client: &MqttClient, filter: &str) -> Arc<Mutex<Vec<(String, u8)>>> {
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    client
        .subscribe_with_options(
            filter,
            mqtt5::SubscribeOptions {
                qos: QoS::ExactlyOnce,
                ..Default::default()
            },
            move |msg| {
                sink.lock()
                    .unwrap()
                    .push((msg.topic.clone(), msg.qos as u8));
            },
        )
        .await
        .unwrap();
    received
}

fn fast(mut config: BridgeConfig) -> BridgeConfig {
    config.initial_reconnect_delay = Duration::from_millis(200);
    config.first_retry_delay = Duration::from_millis(200);
    config.max_reconnect_delay = Duration::from_millis(500);
    config
}

fn remote_config(bind: SocketAddr, ids: bool) -> BrokerConfig {
    let mut config = broker_config(bind);
    config.subscription_identifier_available = ids;
    config
}

fn count(received: &Arc<Mutex<Vec<(String, u8)>>>, topic: &str) -> usize {
    received
        .lock()
        .unwrap()
        .iter()
        .filter(|(t, _)| t == topic)
        .count()
}

async fn retained_copies_after_change(first_ids: bool, second_ids: bool) -> usize {
    let bind = free_port();
    let remote = start_broker(remote_config(bind, first_ids)).await;
    let mut local_config = broker_config(free_port());
    let mut bridge = BridgeConfig::new("retained", bind.to_string()).add_topic(
        "sensors/#",
        BridgeDirection::In,
        QoS::AtLeastOnce,
    );
    bridge.first_retry_delay = Duration::from_millis(1000);
    bridge.initial_reconnect_delay = Duration::from_millis(1000);
    bridge.max_reconnect_delay = Duration::from_millis(1000);
    bridge.retry_jitter = false;
    local_config.bridges = vec![bridge];
    let local = start_broker(local_config).await;
    let local_client = MqttClient::new(format!("local-ret-{first_ids}-{second_ids}"));
    local_client
        .connect(&format!("mqtt://{}", local.addr))
        .await
        .unwrap();
    let received = subscribe_all(&local_client, "#").await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    remote.stop().await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let remote = start_broker(remote_config(bind, second_ids)).await;
    let publisher = MqttClient::new(format!("pub-ret-{first_ids}-{second_ids}"));
    publisher.connect(&format!("mqtt://{bind}")).await.unwrap();
    publisher
        .publish_retain("sensors/retained", b"r".to_vec())
        .await
        .unwrap();
    publisher.disconnect().await.unwrap();
    wait_until(Duration::from_secs(5), || {
        count(&received, "sensors/retained") > 0
    })
    .await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let copies = count(&received, "sensors/retained");
    local_client.disconnect().await.unwrap();
    local.stop().await;
    remote.stop().await;
    copies
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_once_after_lost_session_same_mode_control() {
    assert_eq!(retained_copies_after_change(false, false).await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_once_after_lost_session_gaining_identifiers() {
    assert_eq!(
        retained_copies_after_change(false, true).await,
        1,
        "a retained message reaches the local broker once after reconnecting to a remote that gained identifiers"
    );
}

async fn read_packet(stream: &mut tokio::net::TcpStream) -> Option<(u8, Vec<u8>)> {
    use tokio::io::AsyncReadExt;
    let first = stream.read_u8().await.ok()?;
    let mut length = 0usize;
    let mut shift = 0;
    loop {
        let byte = stream.read_u8().await.ok()?;
        length |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).await.ok()?;
    Some((first, body))
}

fn skip_properties(body: &[u8], mut at: usize) -> usize {
    let mut length = 0usize;
    let mut shift = 0;
    loop {
        let byte = body[at];
        at += 1;
        length |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    at + length
}

fn filter_count(body: &[u8], with_options: bool) -> usize {
    let mut at = skip_properties(body, 2);
    let mut filters = 0;
    while at < body.len() {
        let len = usize::from(u16::from_be_bytes([body[at], body[at + 1]]));
        at += 2 + len + usize::from(with_options);
        filters += 1;
    }
    filters
}

fn publish_qos1(topic: &str, packet_id: u16) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&u16::try_from(topic.len()).unwrap().to_be_bytes());
    body.extend_from_slice(topic.as_bytes());
    body.extend_from_slice(&packet_id.to_be_bytes());
    body.push(0);
    body.extend_from_slice(b"payload");
    let mut packet = vec![0x32, u8::try_from(body.len()).unwrap()];
    packet.extend_from_slice(&body);
    packet
}

async fn fake_remote(
    listener: tokio::net::TcpListener,
    pubacks: Arc<Mutex<Vec<u16>>>,
    log: Arc<Mutex<Vec<String>>>,
) {
    use tokio::io::AsyncWriteExt;
    let mut connection = 0u32;
    while let Ok((mut stream, _)) = listener.accept().await {
        connection += 1;
        let Some((0x10, _)) = read_packet(&mut stream).await else {
            continue;
        };
        if connection == 1 {
            stream.write_all(&[0x20, 3, 0, 0, 0]).await.unwrap();
        } else {
            stream
                .write_all(&[0x20, 5, 1, 0, 2, 0x29, 0])
                .await
                .unwrap();
        }
        let mut injected = false;
        while let Some((first, body)) = read_packet(&mut stream).await {
            let kind = first >> 4;
            log.lock()
                .unwrap()
                .push(format!("conn{connection} type{kind}"));
            if connection >= 2 && !injected && (kind == 8 || kind == 10) {
                injected = true;
                stream
                    .write_all(&publish_qos1("sensors/inflight", 7))
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            match kind {
                8 => {
                    let n = filter_count(&body, true);
                    let mut reply = vec![0x90, u8::try_from(3 + n).unwrap(), body[0], body[1], 0];
                    reply.extend(std::iter::repeat_n(1u8, n));
                    stream.write_all(&reply).await.unwrap();
                }
                10 => {
                    let n = filter_count(&body, false);
                    let mut reply = vec![0xB0, u8::try_from(3 + n).unwrap(), body[0], body[1], 0];
                    reply.extend(std::iter::repeat_n(0u8, n));
                    stream.write_all(&reply).await.unwrap();
                }
                4 => {
                    pubacks
                        .lock()
                        .unwrap()
                        .push(u16::from_be_bytes([body[0], body[1]]));
                }
                12 => stream.write_all(&[0xD0, 0]).await.unwrap(),
                14 => break,
                _ => {}
            }
            if connection == 1 && kind == 8 {
                tokio::time::sleep(Duration::from_millis(300)).await;
                break;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publish_during_mode_switch_with_session_present_is_routed() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote = listener.local_addr().unwrap();
    let pubacks = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let server = tokio::spawn(fake_remote(
        listener,
        Arc::clone(&pubacks),
        Arc::clone(&log),
    ));

    let mut local_config = broker_config(free_port());
    local_config.bridges = vec![fast(
        BridgeConfig::new("switch", remote.to_string()).add_topic(
            "sensors/#",
            BridgeDirection::In,
            QoS::AtLeastOnce,
        ),
    )];
    let local = start_broker(local_config).await;
    let local_client = MqttClient::new("local-switch");
    local_client
        .connect(&format!("mqtt://{}", local.addr))
        .await
        .unwrap();
    let received = subscribe_all(&local_client, "#").await;

    let acked = wait_until(Duration::from_secs(10), || {
        pubacks.lock().unwrap().contains(&7)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let routed = count(&received, "sensors/inflight");
    let seen = log.lock().unwrap().clone();
    local_client.disconnect().await.unwrap();
    local.stop().await;
    server.abort();
    assert!(
        acked,
        "the bridge acknowledges the in-flight message; packets: {seen:?}"
    );
    assert_eq!(
        routed, 1,
        "a message the remote acknowledged as delivered must reach the local broker; packets: {seen:?}"
    );
}

async fn shared_group_case(ids: bool) -> (usize, usize, usize) {
    let remote = start_broker(remote_config(free_port(), ids)).await;
    let other = MqttClient::new(format!("other-{ids}"));
    other
        .connect(&format!("mqtt://{}", remote.addr))
        .await
        .unwrap();
    let other_received = subscribe_all(&other, "$share/g/sensors/#").await;

    let mut config = fast(
        BridgeConfig::new("group", remote.addr.to_string())
            .add_topic("$share/g/sensors/#", BridgeDirection::In, QoS::AtLeastOnce)
            .add_topic("sensors/#", BridgeDirection::In, QoS::AtLeastOnce),
    );
    config.topics[0].local_prefix = Some("group/".to_string());
    config.topics[1].local_prefix = Some("all/".to_string());
    let mut local_config = broker_config(free_port());
    local_config.bridges = vec![config];
    let local = start_broker(local_config).await;
    let local_client = MqttClient::new(format!("local-group-{ids}"));
    local_client
        .connect(&format!("mqtt://{}", local.addr))
        .await
        .unwrap();
    let received = subscribe_all(&local_client, "#").await;
    tokio::time::sleep(Duration::from_millis(800)).await;

    let publisher = MqttClient::new(format!("pub-group-{ids}"));
    publisher
        .connect(&format!("mqtt://{}", remote.addr))
        .await
        .unwrap();
    for _ in 0..20 {
        publisher
            .publish_qos1("sensors/x", b"x".to_vec())
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let group = count(&received, "group/sensors/x");
    let all = count(&received, "all/sensors/x");
    let other_count = other_received.lock().unwrap().len();
    publisher.disconnect().await.unwrap();
    other.disconnect().await.unwrap();
    local_client.disconnect().await.unwrap();
    local.stop().await;
    remote.stop().await;
    (group, other_count, all)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_group_with_identifiers_control() {
    let (group, other, all) = shared_group_case(true).await;
    assert_eq!(group + other, 20, "group={group} other={other} all={all}");
}

async fn dropping_remote(
    listener: tokio::net::TcpListener,
    connections: Arc<Mutex<u32>>,
    drop_on: u32,
) {
    use tokio::io::AsyncWriteExt;
    let mut connection = 0u32;
    while let Ok((mut stream, _)) = listener.accept().await {
        connection += 1;
        *connections.lock().unwrap() = connection;
        let Some((0x10, _)) = read_packet(&mut stream).await else {
            continue;
        };
        if connection == 1 {
            stream.write_all(&[0x20, 3, 0, 0, 0]).await.unwrap();
        } else {
            stream
                .write_all(&[0x20, 5, 1, 0, 2, 0x29, 0])
                .await
                .unwrap();
        }
        while let Some((first, body)) = read_packet(&mut stream).await {
            let kind = first >> 4;
            if connection == drop_on && (kind == 8 || kind == 10) {
                break;
            }
            match kind {
                8 => {
                    let n = filter_count(&body, true);
                    let mut reply = vec![0x90, u8::try_from(3 + n).unwrap(), body[0], body[1], 0];
                    reply.extend(std::iter::repeat_n(1u8, n));
                    stream.write_all(&reply).await.unwrap();
                }
                10 => {
                    let n = filter_count(&body, false);
                    let mut reply = vec![0xB0, u8::try_from(3 + n).unwrap(), body[0], body[1], 0];
                    reply.extend(std::iter::repeat_n(0u8, n));
                    stream.write_all(&reply).await.unwrap();
                }
                12 => stream.write_all(&[0xD0, 0]).await.unwrap(),
                14 => break,
                _ => {}
            }
            if connection == 1 && kind == 8 {
                tokio::time::sleep(Duration::from_millis(300)).await;
                break;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_lost_during_mode_switch_reconnects() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote = listener.local_addr().unwrap();
    let connections = Arc::new(Mutex::new(0u32));
    let server = tokio::spawn(dropping_remote(listener, Arc::clone(&connections), 2));
    let mut local_config = broker_config(free_port());
    local_config.bridges = vec![fast(
        BridgeConfig::new("drop", remote.to_string()).add_topic(
            "sensors/#",
            BridgeDirection::In,
            QoS::AtLeastOnce,
        ),
    )];
    let local = start_broker(local_config).await;
    let reconnected = wait_until(Duration::from_secs(10), || {
        *connections.lock().unwrap() >= 3
    })
    .await;
    let seen = *connections.lock().unwrap();
    local.stop().await;
    server.abort();
    assert!(
        reconnected,
        "the bridge reconnects after the connection drops while it switches modes; connections={seen}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_lost_during_first_subscribe_reconnects() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote = listener.local_addr().unwrap();
    let connections = Arc::new(Mutex::new(0u32));
    let server = tokio::spawn(dropping_remote(listener, Arc::clone(&connections), 1));
    let mut local_config = broker_config(free_port());
    local_config.bridges = vec![fast(
        BridgeConfig::new("drop-first", remote.to_string()).add_topic(
            "sensors/#",
            BridgeDirection::In,
            QoS::AtLeastOnce,
        ),
    )];
    let local = start_broker(local_config).await;
    let reconnected = wait_until(Duration::from_secs(10), || {
        *connections.lock().unwrap() >= 2
    })
    .await;
    let seen = *connections.lock().unwrap();
    local.stop().await;
    server.abort();
    assert!(
        reconnected,
        "the bridge reconnects after the connection drops during its first SUBSCRIBE; connections={seen}"
    );
}
