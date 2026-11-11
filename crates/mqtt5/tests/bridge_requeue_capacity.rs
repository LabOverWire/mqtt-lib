#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
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
fn bridge_config(name: &str, addr: SocketAddr) -> BridgeConfig {
    let mut config = BridgeConfig::new(name, addr.to_string()).add_topic(
        "out/#",
        BridgeDirection::Out,
        QoS::ExactlyOnce,
    );
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(50);
    config.initial_reconnect_delay = Duration::from_millis(100);
    config.max_reconnect_delay = Duration::from_millis(200);
    config.outbound_queue_capacity = 3;
    config
}

fn forward(bridge: &BridgeConnection, payload: &str) {
    let packet = PublishPacket::new("out/x", payload.as_bytes().to_vec(), QoS::ExactlyOnce);
    bridge.forward_message(&packet);
}

async fn connected_bridge(name: &str, addr: SocketAddr) -> (Arc<BridgeConnection>, JoinHandle<()>) {
    let bridge = Arc::new(
        BridgeConnection::new(bridge_config(name, addr), Arc::new(MessageRouter::new())).unwrap(),
    );
    bridge.start();
    let running = Arc::clone(&bridge);
    let task: JoinHandle<()> = tokio::spawn(async move {
        let _ = running.run().await;
    });
    for _ in 0..100 {
        if bridge.get_stats().await.connected {
            return (bridge, task);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("setup: bridge never connected");
}

async fn wait_disconnected(bridge: &BridgeConnection) {
    for _ in 0..200 {
        if !bridge.get_stats().await.connected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("setup: bridge never noticed the drop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn requeue_into_full_queue_drops_the_oldest_messages() {
    let (remote, addr) = FakeRemote::start(u16::MAX, true).await;
    let (bridge, task) = connected_bridge("requeue-full", addr).await;
    for i in 0..3 {
        forward(&bridge, &format!("a{i}"));
    }
    assert!(
        wait_until(Duration::from_secs(5), || remote.payloads(0).len() == 3).await,
        "setup: first connection got {:?}",
        remote.payloads(0)
    );
    remote.accept.store(false, Ordering::SeqCst);
    remote.close.store(0, Ordering::SeqCst);
    wait_disconnected(&bridge).await;
    for i in 0..3 {
        forward(&bridge, &format!("n{i}"));
    }
    remote.ack_from.store(1, Ordering::SeqCst);
    remote.accept.store(true, Ordering::SeqCst);
    wait_until(Duration::from_secs(10), || remote.payloads(1).len() >= 3).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let second = remote.payloads(1);
    let _ = tokio::time::timeout(Duration::from_secs(5), bridge.stop()).await;
    task.abort();
    let newer_dropped: Vec<&str> = ["n0", "n1", "n2"]
        .into_iter()
        .filter(|n| !second.iter().any(|p| p == n))
        .collect();
    assert!(
        newer_dropped.is_empty(),
        "queue overflow on requeue dropped newer messages {newer_dropped:?} and kept older ones; second connection received {second:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lost_qos2_after_stop_and_restart_is_republished_once_in_order() {
    let (remote, addr) = FakeRemote::start(u16::MAX, true).await;
    let (bridge, task) = connected_bridge("stop-restart", addr).await;
    for i in 0..3 {
        forward(&bridge, &format!("a{i}"));
    }
    assert!(
        wait_until(Duration::from_secs(5), || remote.payloads(0).len() == 3).await,
        "setup: first connection got {:?}",
        remote.payloads(0)
    );
    bridge.stop().await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
    remote.ack_from.store(1, Ordering::SeqCst);
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move {
        let _ = running.run().await;
    });
    wait_until(Duration::from_secs(10), || remote.payloads(1).len() >= 3).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let second = remote.payloads(1);
    let sent = bridge.get_stats().await.messages_sent;
    let _ = tokio::time::timeout(Duration::from_secs(5), bridge.stop()).await;
    task.abort();
    assert_eq!(second, vec!["a0", "a1", "a2"], "messages_sent {sent}");
    assert_eq!(sent, 3);
}
