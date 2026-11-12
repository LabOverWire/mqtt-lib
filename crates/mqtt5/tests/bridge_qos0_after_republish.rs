#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::router::MessageRouter;
use mqtt5::time::Duration;
use mqtt5::QoS;
use mqtt5_protocol::packet::publish::PublishPacket;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

const NEVER: usize = usize::MAX;

struct FakeRemote {
    ack_from: AtomicUsize,
    close: AtomicUsize,
    connections: AtomicUsize,
    later_receive_maximum: AtomicUsize,
    later_ack_delay_ms: AtomicU64,
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
            ack_from: AtomicUsize::new(0),
            close: AtomicUsize::new(NEVER),
            connections: AtomicUsize::new(0),
            later_receive_maximum: AtomicUsize::new(100),
            later_ack_delay_ms: AtomicU64::new(0),
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

    fn connack(&self, index: usize) -> Vec<u8> {
        let receive_maximum = if index == 0 {
            100u16
        } else {
            u16::try_from(self.later_receive_maximum.load(Ordering::SeqCst)).unwrap()
        };
        let [hi, lo] = receive_maximum.to_be_bytes();
        vec![0x20, 6, 0, 0, 3, 0x21, hi, lo]
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
                    1 => Some(self.connack(index)),
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
                        let acks = self.ack_from.load(Ordering::SeqCst) <= index;
                        let delay = self.later_ack_delay_ms.load(Ordering::SeqCst);
                        if acks && qos > 0 && index > 0 && delay > 0 {
                            tokio::time::sleep(Duration::from_millis(delay)).await;
                        }
                        id.filter(|_| acks)
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

fn base_config(name: &str, addr: SocketAddr) -> BridgeConfig {
    let mut config = BridgeConfig::new(name, addr.to_string());
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

async fn forward(bridge: &BridgeConnection, topic: &str, payload: &str, qos: QoS) {
    let packet = PublishPacket::new(topic, payload.as_bytes().to_vec(), qos);
    bridge.forward_message(&packet);
    tokio::task::yield_now().await;
}

async fn wait_connected(bridge: &BridgeConnection) {
    let mut up = false;
    for _ in 0..500 {
        if bridge.get_stats().await.connected {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(up, "setup: bridge never connected");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qos0_message_does_not_overtake_qos1_messages_republished_after_reconnect() {
    let (remote, addr) = FakeRemote::start().await;
    remote.ack_from.store(1, Ordering::SeqCst);
    remote.later_receive_maximum.store(1, Ordering::SeqCst);
    remote.later_ack_delay_ms.store(300, Ordering::SeqCst);
    let config = base_config("qos0-after-republish", addr)
        .add_topic("a/#", BridgeDirection::Out, QoS::AtLeastOnce)
        .add_topic("b/#", BridgeDirection::Out, QoS::AtMostOnce);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    let task = launch(&bridge);
    wait_connected(&bridge).await;

    forward(&bridge, "a/x", "a1", QoS::AtLeastOnce).await;
    forward(&bridge, "a/x", "a2", QoS::AtLeastOnce).await;
    assert!(
        wait_until(Duration::from_secs(2), || remote.payloads(0).len() >= 2).await,
        "setup: messages not sent on the first connection"
    );
    remote.close.store(0, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(100)).await;
    forward(&bridge, "b/x", "b1", QoS::AtMostOnce).await;
    assert!(
        wait_until(Duration::from_secs(5), || remote
            .connections
            .load(Ordering::SeqCst)
            >= 2)
        .await,
        "setup: the bridge never reconnected"
    );
    wait_until(Duration::from_secs(5), || remote.payloads(1).len() >= 3).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let delivered = remote.payloads(1);
    let _ = tokio::time::timeout(Duration::from_secs(5), bridge.stop()).await;
    task.abort();
    assert_eq!(
        delivered,
        vec!["a1", "a2", "b1"],
        "the bridge published b1 after a1 and a2, but the remote received it between them"
    );
}
