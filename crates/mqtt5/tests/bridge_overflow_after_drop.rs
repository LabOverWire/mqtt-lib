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
        QoS::AtLeastOnce,
    );
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(50);
    config.initial_reconnect_delay = Duration::from_millis(100);
    config.max_reconnect_delay = Duration::from_millis(200);
    config
}

fn forward(bridge: &BridgeConnection, payload: &str) {
    let packet = PublishPacket::new("out/x", payload.as_bytes().to_vec(), QoS::AtLeastOnce);
    bridge.forward_message(&packet);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overflow_after_a_detected_drop_drops_the_oldest_message() {
    let (remote, addr) = FakeRemote::start(100, true).await;
    remote.ack_from.store(0, Ordering::SeqCst);
    let bridge = Arc::new(
        BridgeConnection::new(
            bridge_config("overflow-after-drop", addr),
            Arc::new(MessageRouter::new()),
        )
        .unwrap(),
    );
    bridge.start();
    let running = Arc::clone(&bridge);
    let task: JoinHandle<()> = tokio::spawn(async move {
        let _ = running.run().await;
    });
    let up = {
        let mut up = false;
        for _ in 0..100 {
            if bridge.get_stats().await.connected {
                up = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        up
    };
    assert!(up, "setup: bridge never connected");
    forward(&bridge, "before");
    assert!(
        wait_until(Duration::from_secs(5), || remote
            .payloads(0)
            .contains(&"before".to_string()))
        .await,
        "setup: first message not delivered"
    );

    remote.accept.store(false, Ordering::SeqCst);
    remote.close.store(0, Ordering::SeqCst);
    let down = {
        let mut down = false;
        for _ in 0..200 {
            if !bridge.get_stats().await.connected {
                down = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        down
    };
    assert!(down, "setup: bridge never noticed the disconnect");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let capacity = 1000;
    forward(&bridge, "p0");
    tokio::time::sleep(Duration::from_millis(200)).await;
    for i in 1..=capacity + 1 {
        forward(&bridge, &format!("p{i}"));
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    remote.close.store(NEVER, Ordering::SeqCst);
    remote.accept.store(true, Ordering::SeqCst);

    let last = format!("p{}", capacity + 1);
    wait_until(Duration::from_secs(15), || {
        remote.payloads(1).contains(&last)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let delivered = remote.payloads(1);
    let _ = tokio::time::timeout(Duration::from_secs(5), bridge.stop()).await;
    task.abort();

    assert!(
        delivered.contains(&last),
        "setup: newest message not delivered after reconnect: {} delivered",
        delivered.len()
    );
    assert!(
        !delivered.contains(&"p0".to_string()) && delivered.len() <= capacity,
        "{} messages forwarded while the bridge was known to be disconnected, outbound queue capacity {capacity}: {} delivered after reconnect, first {:?}; expected at most {capacity} with the oldest (p0) dropped",
        capacity + 2,
        delivered.len(),
        delivered.first()
    );
}
