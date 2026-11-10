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

struct FakeRemote {
    withheld: Vec<String>,
    received: AtomicUsize,
    close_first: AtomicBool,
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
    async fn start(withheld: &str) -> (Arc<Self>, SocketAddr) {
        let remote = Arc::new(Self {
            withheld: withheld.split(',').map(str::to_string).collect(),
            received: AtomicUsize::new(0),
            close_first: AtomicBool::new(false),
            connections: AtomicUsize::new(0),
            log: Mutex::new(Vec::new()),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let serving = Arc::clone(&remote);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
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
        let mut chunk = [0u8; 16384];
        loop {
            if index == 0 && self.close_first.load(Ordering::SeqCst) {
                return;
            }
            let n = match tokio::time::timeout(Duration::from_millis(10), stream.read(&mut chunk))
                .await
            {
                Err(_) => continue,
                Ok(Ok(0) | Err(_)) => return,
                Ok(Ok(n)) => n,
            };
            buf.extend_from_slice(&chunk[..n]);
            let mut out = Vec::new();
            while let Some((header, body, used)) = split_packet(&buf) {
                buf.drain(..used);
                match header >> 4 {
                    1 => out.extend_from_slice(&[0x20, 6, 0, 0, 3, 0x21, 0xFF, 0xFF]),
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
                        let withhold = self.withheld.contains(&payload);
                        self.log.lock().unwrap().push((index, payload));
                        self.received.fetch_add(1, Ordering::SeqCst);
                        if let (Some(id), false) = (id, withhold) {
                            let [hi, lo] = id.to_be_bytes();
                            let kind = if qos == 1 { 0x40 } else { 0x50 };
                            out.extend_from_slice(&[kind, 2, hi, lo]);
                        }
                    }
                    6 => out.extend_from_slice(&[0x70, 2, body[0], body[1]]),
                    12 => out.extend_from_slice(&[0xD0, 0]),
                    14 => return,
                    _ => {}
                }
            }
            if !out.is_empty() && stream.write_all(&out).await.is_err() {
                return;
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

fn bridge_config(name: &str, addr: SocketAddr, capacity: usize) -> BridgeConfig {
    let mut config = BridgeConfig::new(name, addr.to_string()).add_topic(
        "out/#",
        BridgeDirection::Out,
        QoS::AtLeastOnce,
    );
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(50);
    config.initial_reconnect_delay = Duration::from_millis(100);
    config.max_reconnect_delay = Duration::from_millis(200);
    config.outbound_queue_capacity = capacity;
    config
}

async fn connected_bridge(
    name: &str,
    addr: SocketAddr,
    capacity: usize,
) -> (Arc<BridgeConnection>, JoinHandle<()>) {
    let bridge = Arc::new(
        BridgeConnection::new(
            bridge_config(name, addr, capacity),
            Arc::new(MessageRouter::new()),
        )
        .unwrap(),
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

fn forward(bridge: &BridgeConnection, payload: &str) {
    let packet = PublishPacket::new("out/x", payload.as_bytes().to_vec(), QoS::AtLeastOnce);
    bridge.forward_message(&packet);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_unacknowledged_message_does_not_stall_the_bridge() {
    let (remote, addr) = FakeRemote::start("m0").await;
    let count = 70_000;
    let (bridge, task) = connected_bridge("stall", addr, count + 1).await;
    for i in 0..count {
        forward(&bridge, &format!("m{i}"));
        if (i + 1) % 1000 == 0 {
            wait_until(Duration::from_secs(5), || {
                remote.received.load(Ordering::SeqCst) > i
            })
            .await;
        }
    }
    let all = wait_until(Duration::from_secs(30), || {
        remote.received.load(Ordering::SeqCst) >= count
    })
    .await;
    let received = remote.received.load(Ordering::SeqCst);
    let stats = bridge.get_stats().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), bridge.stop()).await;
    task.abort();
    assert!(
        all,
        "remote received {received} of {count} messages; messages_sent {}",
        stats.messages_sent
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn messages_sent_counts_acknowledged_messages_behind_an_unacknowledged_one() {
    let (remote, addr) = FakeRemote::start("m0").await;
    let (bridge, task) = connected_bridge("stats", addr, 1000).await;
    for i in 0..10 {
        forward(&bridge, &format!("m{i}"));
    }
    assert!(
        wait_until(Duration::from_secs(5), || remote
            .received
            .load(Ordering::SeqCst)
            >= 10)
        .await,
        "setup: remote did not receive all messages"
    );
    let mut sent = 0;
    for _ in 0..50 {
        sent = bridge.get_stats().await.messages_sent;
        if sent >= 9 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(5), bridge.stop()).await;
    task.abort();
    assert_eq!(
        sent, 9,
        "messages_sent after 9 of 10 messages were acknowledged"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qos2_lost_with_the_session_is_resent_while_an_earlier_qos1_waits_for_its_ack() {
    let (remote, addr) = FakeRemote::start("a0,b0").await;
    let mut config = BridgeConfig::new("mixed", addr.to_string())
        .add_topic("out/a/#", BridgeDirection::Out, QoS::AtLeastOnce)
        .add_topic("out/b/#", BridgeDirection::Out, QoS::ExactlyOnce);
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(50);
    config.initial_reconnect_delay = Duration::from_millis(100);
    config.max_reconnect_delay = Duration::from_millis(200);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let task: JoinHandle<()> = tokio::spawn(async move {
        let _ = running.run().await;
    });
    assert!(
        wait_until(Duration::from_secs(2), || remote
            .connections
            .load(Ordering::SeqCst)
            >= 1)
        .await,
        "setup: bridge never connected"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    let a0 = PublishPacket::new("out/a/x", b"a0".to_vec(), QoS::AtLeastOnce);
    let b0 = PublishPacket::new("out/b/x", b"b0".to_vec(), QoS::ExactlyOnce);
    bridge.forward_message(&a0);
    bridge.forward_message(&b0);
    assert!(
        wait_until(Duration::from_secs(2), || remote.payloads(0).len() >= 2).await,
        "setup: messages not sent on the first connection"
    );
    remote.close_first.store(true, Ordering::SeqCst);
    let resent = wait_until(Duration::from_secs(8), || {
        remote.payloads(1).contains(&"b0".to_string())
    })
    .await;
    let second = remote.payloads(1);
    let _ = tokio::time::timeout(Duration::from_secs(5), bridge.stop()).await;
    task.abort();
    assert!(
        resent,
        "QoS 2 b0 lost with the session was not published again on the new connection; it received {second:?}"
    );
}
