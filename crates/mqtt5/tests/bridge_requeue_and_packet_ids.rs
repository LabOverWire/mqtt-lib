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
    config.outbound_queue_capacity = 1_000_000;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lost_qos2_requeue_is_not_interleaved_with_new_messages() {
    let (remote, addr) = FakeRemote::start(u16::MAX, true).await;
    let (bridge, task) = connected_bridge("interleave", addr).await;
    let count = 3000;
    for i in 0..count {
        forward(&bridge, &format!("m{i}"));
    }
    assert!(
        wait_until(Duration::from_secs(10), || remote.payloads(0).len()
            == count)
        .await,
        "setup: not all messages reached the first connection: {}",
        remote.payloads(0).len()
    );

    let stop = Arc::new(AtomicBool::new(false));
    let forwarder = {
        let bridge = Arc::clone(&bridge);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut j = 0usize;
            while !stop.load(Ordering::SeqCst) {
                forward(&bridge, &format!("n{j}"));
                j += 1;
                std::thread::sleep(std::time::Duration::from_micros(5));
            }
            j
        })
    };
    remote.ack_from.store(1, Ordering::SeqCst);
    remote.close.store(0, Ordering::SeqCst);
    wait_until(Duration::from_secs(15), || {
        remote
            .payloads(1)
            .iter()
            .filter(|p| p.starts_with('m'))
            .count()
            >= count
    })
    .await;
    stop.store(true, Ordering::SeqCst);
    let sent_new = forwarder.join().unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let republished = remote.payloads(1);
    let _ = tokio::time::timeout(Duration::from_secs(5), bridge.stop()).await;
    task.abort();

    let lost_positions: Vec<usize> = republished
        .iter()
        .enumerate()
        .filter(|(_, p)| p.starts_with('m'))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        lost_positions.len(),
        count,
        "setup: lost messages republished (new sent {sent_new})"
    );
    let first = lost_positions[0];
    let intruders: Vec<&String> = republished[first..first + count]
        .iter()
        .filter(|p| !p.starts_with('m'))
        .collect();
    let m_order: Vec<usize> = republished
        .iter()
        .filter_map(|p| p.strip_prefix('m'))
        .map(|v| v.parse().unwrap())
        .collect();
    let in_order = m_order.windows(2).all(|w| w[0] < w[1]);
    let runs: Vec<(usize, usize)> = {
        let mut runs = Vec::new();
        let mut run_start = first;
        for (k, pos) in lost_positions.iter().enumerate().skip(1) {
            if *pos != lost_positions[k - 1] + 1 {
                runs.push((run_start, lost_positions[k - 1]));
                run_start = *pos;
            }
        }
        runs.push((run_start, *lost_positions.last().unwrap()));
        runs
    };
    eprintln!("lost in order: {in_order}; lost runs (start,end): {:?}; total on conn1 {}; new before first lost {}", &runs[..runs.len().min(20)], republished.len(), first);
    assert!(
        intruders.is_empty(),
        "newer messages were published between QoS 2 messages lost with the session: {} intruders, first {:?}",
        intruders.len(),
        &intruders[..intruders.len().min(5)]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn messages_beyond_packet_identifiers_wait_instead_of_being_dropped() {
    let (remote, addr) = FakeRemote::start(u16::MAX, true).await;
    let (bridge, task) = connected_bridge("ids", addr).await;
    let count = usize::from(u16::MAX) + 200;
    for i in 0..count {
        let packet = PublishPacket::new("out/x", format!("m{i}").into_bytes(), QoS::AtLeastOnce);
        bridge.forward_message(&packet);
    }
    wait_until(Duration::from_secs(60), || {
        remote.payloads(0).len() >= usize::from(u16::MAX)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before_acks = remote.payloads(0).len();
    remote.ack_from.store(0, Ordering::SeqCst);
    wait_until(Duration::from_secs(30), || {
        remote.payloads(0).len() >= count
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let delivered = remote.payloads(0).len();
    let sent = bridge.get_stats().await.messages_sent;
    let _ = tokio::time::timeout(Duration::from_secs(5), bridge.stop()).await;
    task.abort();
    assert_eq!(
        delivered, count,
        "messages forwarded while every packet identifier was in flight were dropped: {before_acks} before acks, {delivered} of {count} reached the remote, messages_sent {sent}"
    );
}
