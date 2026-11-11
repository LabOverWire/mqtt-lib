#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection, BridgeManager};
use mqtt5::broker::router::MessageRouter;
use mqtt5::time::Duration;
use mqtt5::QoS;
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

fn bridge_config(name: &str, addr: SocketAddr, capacity: usize, qos: QoS) -> BridgeConfig {
    let mut config =
        BridgeConfig::new(name, addr.to_string()).add_topic("out/#", BridgeDirection::Out, qos);
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(50);
    config.initial_reconnect_delay = Duration::from_millis(100);
    config.max_reconnect_delay = Duration::from_millis(200);
    config.outbound_queue_capacity = capacity;
    config
}

fn launch(bridge: &Arc<BridgeConnection>) -> JoinHandle<()> {
    bridge.start();
    let running = Arc::clone(bridge);
    tokio::spawn(async move {
        let _ = running.run().await;
    })
}

async fn forward(bridge: &BridgeConnection, payload: &str) {
    let packet = PublishPacket::new("out/x", payload.as_bytes().to_vec(), QoS::AtLeastOnce);
    bridge.forward_message(&packet);
    tokio::task::yield_now().await;
}

async fn delivered_after_disconnected_burst(
    name: &str,
    capacity: usize,
    burst: usize,
) -> Vec<String> {
    let (remote, addr) = FakeRemote::start(100, false).await;
    remote.ack_from.store(0, Ordering::SeqCst);
    let bridge = Arc::new(
        BridgeConnection::new(
            bridge_config(name, addr, capacity, QoS::AtLeastOnce),
            Arc::new(MessageRouter::new()),
        )
        .unwrap(),
    );
    let task = launch(&bridge);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!bridge.get_stats().await.connected);
    for i in 0..burst {
        forward(&bridge, &format!("p{i}")).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    remote.accept.store(true, Ordering::SeqCst);
    let last = format!("p{}", burst - 1);
    wait_until(Duration::from_secs(15), || {
        remote.payloads(0).contains(&last)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let delivered = remote.payloads(0);
    let _ = tokio::time::timeout(Duration::from_secs(5), bridge.stop()).await;
    task.abort();
    delivered
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disconnected_overflow_drops_oldest() {
    let delivered = delivered_after_disconnected_burst("outq-life-overflow", 1000, 1500).await;
    let has = |p: &str| delivered.iter().any(|d| d == p);
    let mut numbers: Vec<usize> = delivered.iter().map(|d| d[1..].parse().unwrap()).collect();
    numbers.sort_unstable();
    let gaps: Vec<(usize, usize)> = numbers
        .windows(2)
        .filter(|w| w[1] != w[0] + 1)
        .map(|w| (w[0], w[1]))
        .collect();
    assert!(
        has("p1499") && !has("p0"),
        "1500 messages forwarded while disconnected with capacity 1000: {} delivered, lowest {:?}, highest {:?}, gaps {gaps:?}; expected the oldest dropped and the newest kept",
        delivered.len(),
        numbers.first(),
        numbers.last()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disconnected_burst_within_capacity_is_kept() {
    let delivered = delivered_after_disconnected_burst("outq-life-capacity", 5000, 1500).await;
    let unique: std::collections::HashSet<&String> = delivered.iter().collect();
    assert_eq!(
        unique.len(),
        1500,
        "1500 messages forwarded while disconnected with outbound_queue_capacity 5000: {} distinct delivered, last {:?}",
        unique.len(),
        delivered.last()
    );
}

fn alive_tasks() -> usize {
    tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks()
}

async fn tasks_left_after_remove(name: &str, forward_one: bool) -> (usize, usize) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let manager = BridgeManager::new(Arc::new(MessageRouter::new()));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = alive_tasks();
    manager
        .add_bridge(bridge_config(name, addr, 1000, QoS::AtLeastOnce))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    if forward_one {
        let packet = PublishPacket::new("out/x", b"m0".to_vec(), QoS::AtLeastOnce);
        manager.handle_outgoing(&packet).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    manager.remove_bridge(name).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    (before, alive_tasks())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removed_bridge_leaves_no_task() {
    let control = tasks_left_after_remove("outq-life-leak-control", false).await;
    let forwarded = tasks_left_after_remove("outq-life-leak", true).await;
    assert_eq!(
        control.0, control.1,
        "control: add_bridge + remove_bridge without forwarding left tasks"
    );
    assert_eq!(
        forwarded.0, forwarded.1,
        "alive tasks before add_bridge {} and after remove_bridge {}",
        forwarded.0, forwarded.1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_capacity_does_not_panic_forwarding() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let config = bridge_config("outq-life-huge", addr, usize::MAX, QoS::AtLeastOnce);
    let Ok(bridge) = BridgeConnection::new(config, Arc::new(MessageRouter::new())) else {
        return;
    };
    bridge.start();
    let packet = PublishPacket::new("out/x", b"m0".to_vec(), QoS::AtLeastOnce);
    let forwarded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        bridge.forward_message(&packet);
    }));
    assert!(
        forwarded.is_ok(),
        "BridgeConnection::new accepted outbound_queue_capacity = usize::MAX but forward_message panicked"
    );
}

async fn connected(manager: &BridgeManager, name: &str) -> bool {
    manager
        .get_bridge_stats(name)
        .await
        .is_some_and(|stats| stats.connected)
}

async fn forward_through(manager: &BridgeManager, payload: &str, qos: QoS) {
    let packet = PublishPacket::new("out/x", payload.as_bytes().to_vec(), qos);
    manager.handle_outgoing(&packet).await;
    tokio::task::yield_now().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qos0_forwarded_after_an_idle_link_drops_waits_for_reconnect() {
    let (remote, addr) = FakeRemote::start(100, true).await;
    remote.ack_from.store(0, Ordering::SeqCst);
    let name = "outq-life-idle-qos0";
    let manager = BridgeManager::new(Arc::new(MessageRouter::new()));
    manager
        .add_bridge(bridge_config(name, addr, 1000, QoS::AtMostOnce))
        .unwrap();
    let mut up = false;
    for _ in 0..100 {
        if connected(&manager, name).await {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(up, "setup: bridge never connected");
    forward_through(&manager, "before", QoS::AtMostOnce).await;
    assert!(
        wait_until(Duration::from_secs(5), || remote
            .payloads(0)
            .contains(&"before".to_string()))
        .await,
        "setup: first message not delivered"
    );

    remote.accept.store(false, Ordering::SeqCst);
    remote.close.store(0, Ordering::SeqCst);
    let mut down = false;
    for _ in 0..200 {
        if !connected(&manager, name).await {
            down = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(down, "setup: bridge never noticed the disconnect");
    tokio::time::sleep(Duration::from_millis(300)).await;

    forward_through(&manager, "while-down", QoS::AtMostOnce).await;
    forward_through(&manager, "while-down-2", QoS::AtMostOnce).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    remote.close.store(NEVER, Ordering::SeqCst);
    remote.accept.store(true, Ordering::SeqCst);

    let delivered = wait_until(Duration::from_secs(10), || {
        let second = remote.payloads(1);
        second.contains(&"while-down".to_string()) && second.contains(&"while-down-2".to_string())
    })
    .await;
    let reconnected = connected(&manager, name).await;
    let _ = manager.remove_bridge(name).await;
    assert!(reconnected, "setup: bridge did not reconnect");
    assert!(
        delivered,
        "QoS 0 message forwarded while the bridge was known to be disconnected was not published after it reconnected; second connection received {:?}",
        remote.payloads(1)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qos0_forwarded_after_stop_and_start_waits_for_reconnect() {
    let (remote, addr) = FakeRemote::start(100, true).await;
    remote.ack_from.store(0, Ordering::SeqCst);
    let bridge = Arc::new(
        BridgeConnection::new(
            bridge_config("outq-life-restart-qos0", addr, 1000, QoS::AtMostOnce),
            Arc::new(MessageRouter::new()),
        )
        .unwrap(),
    );
    bridge.start();
    let running = Arc::clone(&bridge);
    let first = tokio::spawn(async move {
        let _ = running.run().await;
    });
    let mut up = false;
    for _ in 0..100 {
        if bridge.get_stats().await.connected {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(up, "setup: bridge never connected");
    let packet = PublishPacket::new("out/x", b"before".to_vec(), QoS::AtMostOnce);
    bridge.forward_message(&packet);
    assert!(
        wait_until(Duration::from_secs(5), || remote
            .payloads(0)
            .contains(&"before".to_string()))
        .await,
        "setup: first message not delivered"
    );

    bridge.stop().await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), first).await;
    bridge.start();
    for payload in ["after-restart", "after-restart-2"] {
        let packet = PublishPacket::new("out/x", payload.as_bytes().to_vec(), QoS::AtMostOnce);
        bridge.forward_message(&packet);
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let running = Arc::clone(&bridge);
    let second = tokio::spawn(async move {
        let _ = running.run().await;
    });
    let delivered = wait_until(Duration::from_secs(10), || {
        let next = remote.payloads(1);
        next.contains(&"after-restart".to_string()) && next.contains(&"after-restart-2".to_string())
    })
    .await;
    let _ = tokio::time::timeout(Duration::from_secs(5), bridge.stop()).await;
    second.abort();
    assert!(
        delivered,
        "QoS 0 messages forwarded after stop() and start(), before run() reconnected, were not all published; the new connection received {:?}",
        remote.payloads(1)
    );
}
