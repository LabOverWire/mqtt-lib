#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection, BridgeManager};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::{BrokerConfig, MqttBroker, StorageConfig};
use mqtt5::time::Duration;
use mqtt5::{MqttClient, PublishOptions, QoS, SubscribeOptions};
use mqtt5_protocol::packet::publish::PublishPacket;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

const NEVER: usize = usize::MAX;

struct FakeRemote {
    accept: AtomicBool,
    ack_from: AtomicUsize,
    close: AtomicUsize,
    receive_maximum: u16,
    connections: AtomicUsize,
    log: Mutex<Vec<(usize, String)>>,
}

fn split_packet(buf: &[u8]) -> Option<(u8, Vec<u8>, usize)> {
    let header = *buf.first()?;
    let (len, start) = varint(buf, 1)?;
    let end = start + len;
    (buf.len() >= end).then(|| (header, buf[start..end].to_vec(), end))
}

fn varint(buf: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut value = 0usize;
    let mut shift = 0;
    let mut pos = from;
    loop {
        let byte = *buf.get(pos)?;
        value |= usize::from(byte & 0x7f) << shift;
        pos += 1;
        if byte & 0x80 == 0 {
            return Some((value, pos));
        }
        shift += 7;
    }
}

impl FakeRemote {
    async fn start(receive_maximum: u16, accept: bool) -> (Arc<Self>, SocketAddr) {
        let remote = Arc::new(Self {
            accept: AtomicBool::new(accept),
            ack_from: AtomicUsize::new(NEVER),
            close: AtomicUsize::new(NEVER),
            receive_maximum,
            connections: AtomicUsize::new(0),
            log: Mutex::new(Vec::new()),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let serving = Arc::clone(&remote);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                if !serving.accept.load(Ordering::SeqCst) {
                    drop(stream);
                    continue;
                }
                let index = serving.connections.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(Arc::clone(&serving).serve(stream, index));
            }
        });
        (remote, addr)
    }

    fn payloads(&self, connection: usize) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|(c, _)| *c == connection)
            .map(|(_, p)| p.clone())
            .collect()
    }

    async fn serve(self: Arc<Self>, mut stream: TcpStream, index: usize) {
        let mut buf = Vec::new();
        let mut held: Vec<(u8, u16)> = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            if self.close.load(Ordering::SeqCst) == index {
                return;
            }
            if self.ack_from.load(Ordering::SeqCst) <= index {
                for (kind, id) in held.drain(..) {
                    let [hi, lo] = id.to_be_bytes();
                    if stream.write_all(&[kind, 2, hi, lo]).await.is_err() {
                        return;
                    }
                }
            }
            match tokio::time::timeout(Duration::from_millis(10), stream.read(&mut chunk)).await {
                Err(_) => continue,
                Ok(Ok(0) | Err(_)) => return,
                Ok(Ok(n)) => buf.extend_from_slice(&chunk[..n]),
            }
            while let Some((header, body, used)) = split_packet(&buf) {
                buf.drain(..used);
                let reply: Option<Vec<u8>> = match header >> 4 {
                    1 => {
                        let [hi, lo] = self.receive_maximum.to_be_bytes();
                        Some(vec![0x20, 6, 0, 0, 3, 0x21, hi, lo])
                    }
                    3 => {
                        let qos = (header >> 1) & 3;
                        let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
                        let mut pos = 2 + topic_len;
                        let id = (qos > 0).then(|| u16::from_be_bytes([body[pos], body[pos + 1]]));
                        if qos > 0 {
                            pos += 2;
                        }
                        let (props, after) = varint(&body, pos).unwrap();
                        let payload = String::from_utf8_lossy(&body[after + props..]).to_string();
                        self.log.lock().unwrap().push((index, payload));
                        if let Some(id) = id {
                            held.push((if qos == 1 { 0x40 } else { 0x50 }, id));
                        }
                        None
                    }
                    6 => Some(vec![0x70, 2, body[0], body[1]]),
                    12 => Some(vec![0xD0, 0]),
                    14 => return,
                    _ => None,
                };
                if let Some(reply) = reply {
                    if stream.write_all(&reply).await.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

async fn wait_until(limit: Duration, check: impl Fn() -> bool) -> bool {
    let started = tokio::time::Instant::now();
    while started.elapsed() < limit {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    check()
}

fn bridge_config(name: &str, addr: SocketAddr, qos: QoS) -> BridgeConfig {
    let mut config =
        BridgeConfig::new(name, addr.to_string()).add_topic("out/#", BridgeDirection::Out, qos);
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(50);
    config.initial_reconnect_delay = Duration::from_millis(100);
    config.max_reconnect_delay = Duration::from_millis(200);
    config
}

fn launch(bridge: &Arc<BridgeConnection>) -> JoinHandle<()> {
    bridge.start();
    let running = Arc::clone(bridge);
    tokio::spawn(async move {
        let _ = running.run().await;
    })
}

async fn wait_connected(bridge: &BridgeConnection) {
    for _ in 0..250 {
        if bridge.get_stats().await.connected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("bridge never connected");
}

async fn forward(bridge: &BridgeConnection, payload: &str, qos: QoS) {
    let packet = PublishPacket::new("out/x", payload.as_bytes().to_vec(), qos);
    bridge.forward_message(&packet);
    tokio::task::yield_now().await;
}

async fn shut_down(bridge: &BridgeConnection, task: JoinHandle<()>) {
    let _ = tokio::time::timeout(Duration::from_secs(5), bridge.stop()).await;
    task.abort();
}

async fn bridge_flushing_into_full_queue(
    name: &str,
) -> (Arc<FakeRemote>, Arc<BridgeConnection>, JoinHandle<()>) {
    let (remote, addr) = FakeRemote::start(1, false).await;
    let mut config = bridge_config(name, addr, QoS::AtLeastOnce);
    config.outbound_queue_capacity = 1000;
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    let task = launch(&bridge);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!bridge.get_stats().await.connected);
    for i in 0..5 {
        forward(&bridge, &format!("p{i}"), QoS::AtLeastOnce).await;
    }
    remote.accept.store(true, Ordering::SeqCst);
    assert!(
        wait_until(Duration::from_secs(10), || !remote.payloads(0).is_empty()).await,
        "the first pending message reached the remote"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    (remote, bridge, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stats_stay_readable_while_pending_messages_wait_for_queue_room() {
    let (remote, bridge, task) = bridge_flushing_into_full_queue("outq-stats").await;
    let stats = tokio::time::timeout(Duration::from_secs(2), bridge.get_stats()).await;
    remote.ack_from.store(0, Ordering::SeqCst);
    shut_down(&bridge, task).await;
    assert!(
        stats.is_ok(),
        "get_stats blocked while the reconnect flush waits for outbound queue room"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pending_messages_stay_ahead_of_later_messages() {
    let (remote, bridge, task) = bridge_flushing_into_full_queue("outq-order").await;
    let late = Arc::clone(&bridge);
    let late_task = tokio::spawn(async move { forward(&late, "late", QoS::AtLeastOnce).await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    remote.ack_from.store(0, Ordering::SeqCst);
    wait_until(Duration::from_secs(10), || remote.payloads(0).len() >= 6).await;
    let _ = late_task.await;
    shut_down(&bridge, task).await;
    assert_eq!(
        remote.payloads(0),
        vec!["p0", "p1", "p2", "p3", "p4", "late"],
        "a message routed after the pending ones overtook them"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn message_waiting_for_quota_survives_stop_and_restart() {
    let (remote, addr) = FakeRemote::start(1, true).await;
    let config = bridge_config("outq-stop", addr, QoS::AtLeastOnce);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    let task = launch(&bridge);
    wait_connected(&bridge).await;
    forward(&bridge, "m0", QoS::AtLeastOnce).await;
    forward(&bridge, "m1", QoS::AtLeastOnce).await;
    assert!(wait_until(Duration::from_secs(5), || remote.payloads(0).len() == 1).await);
    tokio::time::sleep(Duration::from_millis(300)).await;

    tokio::time::timeout(Duration::from_secs(5), bridge.stop())
        .await
        .unwrap()
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    remote.ack_from.store(1, Ordering::SeqCst);
    let task = launch(&bridge);
    let both = wait_until(Duration::from_secs(10), || {
        let sent = remote.payloads(1);
        sent.contains(&"m0".to_string()) && sent.contains(&"m1".to_string())
    })
    .await;
    let sent = remote.payloads(1);
    shut_down(&bridge, task).await;
    assert!(
        both,
        "after restart the remote received {sent:?}; one of m0/m1 was waiting for send quota at stop()"
    );
}

async fn mesh_broker() -> (SocketAddr, Arc<MessageRouter>) {
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_storage(StorageConfig::new().with_persistence(false))
        .with_server_receive_maximum(2);
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let addr = broker.local_addr().unwrap();
    let router = broker.router();
    let mut ready = broker.ready_receiver();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&is_ready| is_ready).await.unwrap();
    (addr, router)
}

async fn mesh_bridge(
    router: &Arc<MessageRouter>,
    name: &str,
    remote: SocketAddr,
) -> Arc<BridgeManager> {
    let manager = Arc::new(BridgeManager::new(Arc::clone(router)));
    router.set_bridge_manager(Arc::clone(&manager)).await;
    let mut config = BridgeConfig::new(name, remote.to_string()).add_topic(
        "t/#",
        BridgeDirection::Out,
        QoS::AtLeastOnce,
    );
    config.retry_jitter = false;
    manager.add_bridge(config).unwrap();
    for _ in 0..250 {
        if manager
            .get_bridge_stats(name)
            .await
            .is_some_and(|stats| stats.connected)
        {
            return manager;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("bridge {name} never connected");
}

async fn counting_subscriber(
    addr: SocketAddr,
    id: &str,
    filter: &str,
) -> (MqttClient, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let client = MqttClient::new(id);
    client.connect(&format!("mqtt://{addr}")).await.unwrap();
    let seen = Arc::clone(&count);
    client
        .subscribe_with_options(
            filter,
            SubscribeOptions {
                qos: QoS::AtLeastOnce,
                ..SubscribeOptions::default()
            },
            move |_| {
                seen.fetch_add(1, Ordering::SeqCst);
            },
        )
        .await
        .unwrap();
    (client, count)
}

fn publish_burst(
    addr: SocketAddr,
    id: String,
    prefix: String,
    count: usize,
) -> JoinHandle<MqttClient> {
    tokio::spawn(async move {
        let client = MqttClient::new(id);
        client.connect(&format!("mqtt://{addr}")).await.unwrap();
        let options = PublishOptions {
            qos: QoS::AtLeastOnce,
            ..PublishOptions::default()
        };
        for i in 0..count {
            let _ = client
                .begin_publish(
                    format!("{prefix}/{i}"),
                    format!("{prefix}-{i}"),
                    options.clone(),
                )
                .await;
        }
        client
    })
}

async fn mesh_delivery(publishers: usize, each: usize) -> Option<String> {
    let (a, router_a) = mesh_broker().await;
    let (b, router_b) = mesh_broker().await;
    let manager_a = mesh_bridge(&router_a, "outq-a-to-b", b).await;
    let manager_b = mesh_bridge(&router_b, "outq-b-to-a", a).await;
    let (sub_b, from_a_on_b) = counting_subscriber(b, "outq-sub-b", "t/a/#").await;
    let (sub_a, from_b_on_a) = counting_subscriber(a, "outq-sub-a", "t/b/#").await;

    let bursts: Vec<_> = (0..publishers)
        .flat_map(|p| {
            [
                publish_burst(a, format!("outq-pub-a{p}"), format!("t/a/{p}"), each),
                publish_burst(b, format!("outq-pub-b{p}"), format!("t/b/{p}"), each),
            ]
        })
        .collect();
    let total = publishers * each;
    let done = wait_until(Duration::from_secs(45), || {
        from_a_on_b.load(Ordering::SeqCst) == total && from_b_on_a.load(Ordering::SeqCst) == total
    })
    .await;
    let a_to_b = from_a_on_b.load(Ordering::SeqCst);
    let b_to_a = from_b_on_a.load(Ordering::SeqCst);
    let _ = tokio::time::timeout(Duration::from_secs(5), manager_a.stop_all()).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), manager_b.stop_all()).await;
    for burst in bursts {
        burst.abort();
    }
    drop((sub_a, sub_b));
    (!done).then(|| {
        format!(
            "after 45 s B received {a_to_b}/{total} messages published on A and A received {b_to_a}/{total} published on B"
        )
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mutually_bridged_brokers_keep_forwarding() {
    let stalled = mesh_delivery(4, 50).await;
    assert!(stalled.is_none(), "{stalled:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mutually_bridged_brokers_keep_forwarding_after_an_overflow() {
    let (a, router_a) = mesh_broker().await;
    let (b, router_b) = mesh_broker().await;
    let manager_a = mesh_bridge(&router_a, "outq-a-to-b", b).await;
    let manager_b = mesh_bridge(&router_b, "outq-b-to-a", a).await;
    let bursts: Vec<_> = (0..32)
        .flat_map(|p| {
            [
                publish_burst(a, format!("outq-pub-a{p}"), format!("t/a/{p}"), 300),
                publish_burst(b, format!("outq-pub-b{p}"), format!("t/b/{p}"), 300),
            ]
        })
        .collect();
    let mut publishers = Vec::new();
    for burst in bursts {
        publishers.push(burst.await.unwrap());
    }
    let (sub_b, after_a_on_b) = counting_subscriber(b, "outq-after-b", "t/a/after/#").await;
    let (sub_a, after_b_on_a) = counting_subscriber(a, "outq-after-a", "t/b/after/#").await;
    let late = [
        publish_burst(a, "outq-late-a".to_string(), "t/a/after".to_string(), 50),
        publish_burst(b, "outq-late-b".to_string(), "t/b/after".to_string(), 50),
    ];
    let arrived = wait_until(Duration::from_secs(90), || {
        after_a_on_b.load(Ordering::SeqCst) == 50 && after_b_on_a.load(Ordering::SeqCst) == 50
    })
    .await;
    let counts = (
        after_a_on_b.load(Ordering::SeqCst),
        after_b_on_a.load(Ordering::SeqCst),
    );
    let _ = tokio::time::timeout(Duration::from_secs(5), manager_a.stop_all()).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), manager_b.stop_all()).await;
    for burst in late {
        burst.abort();
    }
    drop((sub_a, sub_b, publishers));
    assert!(
        arrived,
        "messages published after the burst arrived {counts:?} of (50, 50)"
    );
}
