#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::router::MessageRouter;
use mqtt5::time::Duration;
use mqtt5::QoS;
use mqtt5_protocol::packet::publish::PublishPacket;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

struct FakeRemote {
    connections: AtomicUsize,
    suback_code: AtomicU8,
    payloads: Mutex<Vec<String>>,
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

fn split_packet(buf: &[u8]) -> Option<(u8, Vec<u8>, usize)> {
    let header = *buf.first()?;
    let (len, start) = varint(buf, 1)?;
    let end = start + len;
    (buf.len() >= end).then(|| (header, buf[start..end].to_vec(), end))
}

impl FakeRemote {
    async fn start() -> (Arc<Self>, SocketAddr) {
        let remote = Arc::new(Self {
            connections: AtomicUsize::new(0),
            suback_code: AtomicU8::new(0),
            payloads: Mutex::new(Vec::new()),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let serving = Arc::clone(&remote);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                serving.connections.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(Arc::clone(&serving).serve(stream));
            }
        });
        (remote, addr)
    }

    fn payloads(&self) -> Vec<String> {
        self.payloads.lock().unwrap().clone()
    }

    async fn serve(self: Arc<Self>, mut stream: TcpStream) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
            while let Some((header, body, used)) = split_packet(&buf) {
                buf.drain(..used);
                let reply: Option<Vec<u8>> = match header >> 4 {
                    1 => Some(vec![0x20, 3, 0, 0, 0]),
                    3 => {
                        let qos = (header >> 1) & 3;
                        let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
                        let mut pos = 2 + topic_len;
                        let packet_id = (qos > 0).then(|| [body[pos], body[pos + 1]]);
                        if qos > 0 {
                            pos += 2;
                        }
                        let (props_len, props_start) = varint(&body, pos).unwrap();
                        let payload =
                            String::from_utf8_lossy(&body[props_start + props_len..]).into_owned();
                        self.payloads.lock().unwrap().push(payload);
                        packet_id
                            .map(|id| vec![if qos == 1 { 0x40 } else { 0x50 }, 2, id[0], id[1]])
                    }
                    6 => Some(vec![0x70, 2, body[0], body[1]]),
                    8 => Some(vec![
                        0x90,
                        4,
                        body[0],
                        body[1],
                        0,
                        self.suback_code.load(Ordering::SeqCst),
                    ]),
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

fn config(name: &str, addr: SocketAddr) -> BridgeConfig {
    let mut config = BridgeConfig::new(name, addr.to_string())
        .add_topic("in/#", BridgeDirection::In, QoS::AtLeastOnce)
        .add_topic("out/#", BridgeDirection::Out, QoS::AtLeastOnce);
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

async fn wait_for_payload(remote: &FakeRemote, payload: &str) -> bool {
    for _ in 0..60 {
        if remote.payloads().iter().any(|seen| seen == payload) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

async fn failed_run(name: &str) -> (Arc<FakeRemote>, Arc<BridgeConnection>) {
    let (remote, addr) = FakeRemote::start().await;
    remote.suback_code.store(0x87, Ordering::SeqCst);
    let bridge = Arc::new(
        BridgeConnection::new(config(name, addr), Arc::new(MessageRouter::new())).unwrap(),
    );
    bridge.start();
    let running = Arc::clone(&bridge);
    let result = tokio::time::timeout(Duration::from_secs(5), async move { running.run().await })
        .await
        .expect("setup: run() did not return");
    assert!(
        result.is_err(),
        "setup: run() did not fail on the refused subscription"
    );
    remote.suback_code.store(0, Ordering::SeqCst);
    (remote, bridge)
}

#[tokio::test]
async fn bridge_run_again_after_failed_run_forwards() {
    let (remote, bridge) = failed_run("run-again-rerun").await;

    bridge.start();
    let running = Arc::clone(&bridge);
    let rerun = tokio::spawn(async move { running.run().await });
    tokio::time::sleep(Duration::from_millis(300)).await;

    forward(&bridge, "after-rerun");
    let delivered = wait_for_payload(&remote, "after-rerun").await;
    let stats = bridge.get_stats().await;
    let connections = remote.connections.load(Ordering::SeqCst);
    let seen = remote.payloads();
    rerun.abort();
    assert!(
        delivered,
        "bridge restarted with start() and run() after run() failed did not forward within 3 s: remote saw {seen:?}, connections = {connections}, stats.connected = {}",
        stats.connected
    );
}

#[tokio::test]
async fn bridge_stopped_and_restarted_after_failed_run_forwards() {
    let (remote, bridge) = failed_run("run-again-stop-restart").await;

    bridge.stop().await.unwrap();
    bridge.start();
    let running = Arc::clone(&bridge);
    let rerun = tokio::spawn(async move { running.run().await });
    tokio::time::sleep(Duration::from_millis(300)).await;

    forward(&bridge, "after-restart");
    let delivered = wait_for_payload(&remote, "after-restart").await;
    let seen = remote.payloads();
    rerun.abort();
    assert!(
        delivered,
        "bridge stopped, started and run after run() failed did not forward within 3 s: remote saw {seen:?}"
    );
}
