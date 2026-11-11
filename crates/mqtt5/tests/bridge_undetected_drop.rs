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
    async fn start() -> (Arc<Self>, SocketAddr) {
        let remote = Arc::new(Self {
            accept: AtomicBool::new(true),
            ack_from: AtomicUsize::new(0),
            close: AtomicUsize::new(NEVER),
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
        let mut chunk = [0u8; 4096];
        loop {
            if self.close.load(Ordering::SeqCst) == index {
                return;
            }
            match tokio::time::timeout(Duration::from_millis(10), stream.read(&mut chunk)).await {
                Err(_) => continue,
                Ok(Ok(0) | Err(_)) => return,
                Ok(Ok(n)) => buf.extend_from_slice(&chunk[..n]),
            }
            while let Some((header, body, used)) = split_packet(&buf) {
                buf.drain(..used);
                let reply: Option<Vec<u8>> = match header >> 4 {
                    1 => Some(vec![0x20, 6, 0, 0, 3, 0x21, 0, 100]),
                    3 => {
                        let qos = (header >> 1) & 3;
                        let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
                        let mut pos = 2 + topic_len;
                        let id = (qos > 0).then(|| [body[pos], body[pos + 1]]);
                        if qos > 0 {
                            pos += 2;
                        }
                        let (props, after) = varint(&body, pos).unwrap();
                        let payload = String::from_utf8_lossy(&body[after + props..]).to_string();
                        self.log.lock().unwrap().push((index, payload));
                        id.filter(|_| self.ack_from.load(Ordering::SeqCst) <= index)
                            .map(|[hi, lo]| vec![if qos == 1 { 0x40 } else { 0x50 }, 2, hi, lo])
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

async fn forward(bridge: &BridgeConnection, payload: &str, qos: QoS) {
    let packet = PublishPacket::new("out/x", payload.as_bytes().to_vec(), qos);
    bridge.forward_message(&packet);
    tokio::task::yield_now().await;
}

async fn delivered_after_undetected_drop(name: &str, qos: QoS) -> Vec<String> {
    let (remote, bridge, task) = connected_bridge(name, qos).await;
    forward(&bridge, "before", qos).await;
    assert!(
        wait_until(Duration::from_secs(2), || remote
            .payloads(0)
            .contains(&"before".to_string()))
        .await,
        "setup: first message not delivered"
    );

    remote.close.store(0, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        bridge.get_stats().await.connected,
        "setup: the bridge already noticed the drop, so the window was missed"
    );
    forward(&bridge, "in-window", qos).await;
    assert!(
        wait_until(Duration::from_secs(5), || remote
            .connections
            .load(Ordering::SeqCst)
            >= 2)
        .await,
        "setup: the bridge never reconnected"
    );
    forward(&bridge, "after-detect", qos).await;

    wait_until(Duration::from_secs(10), || remote.payloads(1).len() >= 2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let delivered = remote.payloads(1);
    let _ = tokio::time::timeout(Duration::from_secs(5), bridge.stop()).await;
    task.abort();
    delivered
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qos0_forwarded_before_the_bridge_notices_a_drop_is_published_after_reconnect() {
    let delivered = delivered_after_undetected_drop("drop-window-qos0", QoS::AtMostOnce).await;
    assert_eq!(
        delivered,
        vec!["in-window".to_string(), "after-detect".to_string()],
        "QoS 0 message forwarded after the remote connection dropped, before the bridge's poll noticed it, was not published in order after reconnect"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qos1_forwarded_before_the_bridge_notices_a_drop_is_published_after_reconnect() {
    let delivered = delivered_after_undetected_drop("drop-window-qos1", QoS::AtLeastOnce).await;
    assert_eq!(
        delivered,
        vec!["in-window".to_string(), "after-detect".to_string()],
        "QoS 1 message forwarded after the remote connection dropped, before the bridge's poll noticed it, was not published in order after reconnect"
    );
}

async fn connected_bridge(
    name: &str,
    qos: QoS,
) -> (Arc<FakeRemote>, Arc<BridgeConnection>, JoinHandle<()>) {
    let (remote, addr) = FakeRemote::start().await;
    let bridge = Arc::new(
        BridgeConnection::new(
            bridge_config(name, addr, qos),
            Arc::new(MessageRouter::new()),
        )
        .unwrap(),
    );
    let task = launch(&bridge);
    let mut up = false;
    for _ in 0..500 {
        if bridge.get_stats().await.connected {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(up, "setup: bridge never connected");
    (remote, bridge, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unacknowledged_qos1_is_resent_ahead_of_newer_messages_after_a_drop() {
    let (remote, bridge, task) = connected_bridge("drop-inflight-qos1", QoS::AtLeastOnce).await;
    remote.ack_from.store(1, Ordering::SeqCst);
    forward(&bridge, "m0", QoS::AtLeastOnce).await;
    forward(&bridge, "m1", QoS::AtLeastOnce).await;
    assert!(
        wait_until(Duration::from_secs(2), || remote.payloads(0).len() >= 2).await,
        "setup: messages not sent on the first connection"
    );
    remote.close.store(0, Ordering::SeqCst);
    assert!(
        wait_until(Duration::from_secs(5), || remote
            .connections
            .load(Ordering::SeqCst)
            >= 2)
        .await,
        "setup: the bridge never reconnected"
    );
    forward(&bridge, "m2", QoS::AtLeastOnce).await;
    wait_until(Duration::from_secs(5), || remote.payloads(1).len() >= 3).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let delivered = remote.payloads(1);
    let stats = bridge.get_stats().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), bridge.stop()).await;
    task.abort();
    assert_eq!(delivered, vec!["m0", "m1", "m2"]);
    assert_eq!(stats.messages_sent, 3);
}

fn alive_tasks() -> usize {
    tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_a_bridge_with_an_unacknowledged_message_leaves_no_task() {
    let (remote, addr) = FakeRemote::start().await;
    remote.ack_from.store(NEVER, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = alive_tasks();
    let manager = BridgeManager::new(Arc::new(MessageRouter::new()));
    let name = "drop-leak-inflight";
    manager
        .add_bridge(bridge_config(name, addr, QoS::AtLeastOnce))
        .unwrap();
    let mut up = false;
    for _ in 0..200 {
        if manager
            .get_bridge_stats(name)
            .await
            .is_some_and(|s| s.connected)
        {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(up, "setup: bridge never connected");
    let packet = PublishPacket::new("out/x", b"held".to_vec(), QoS::AtLeastOnce);
    manager.handle_outgoing(&packet).await;
    assert!(
        wait_until(Duration::from_secs(2), || !remote.payloads(0).is_empty()).await,
        "setup: message not sent"
    );
    manager.remove_bridge(name).await.unwrap();
    drop(manager);
    tokio::time::sleep(Duration::from_secs(2)).await;
    let after = alive_tasks();
    assert_eq!(
        before, after,
        "alive tasks before add_bridge {before}, after remove_bridge {after}"
    );
}
