#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::router::MessageRouter;
use mqtt5::time::Duration;
use mqtt5::QoS;
use mqtt5_protocol::packet::publish::PublishPacket;
use std::cell::Cell;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::span::{Attributes, Id};
use tracing::Subscriber;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::Layer;

const NEVER: usize = usize::MAX;

struct FakeRemote {
    close: AtomicUsize,
    connections: AtomicUsize,
    suback_code: AtomicU8,
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
            close: AtomicUsize::new(NEVER),
            connections: AtomicUsize::new(0),
            suback_code: AtomicU8::new(0),
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
                    1 => Some(vec![0x20, 3, 0, 0, 0]),
                    3 => {
                        let qos = (header >> 1) & 3;
                        let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
                        let pos = 2 + topic_len;
                        (qos > 0).then(|| {
                            vec![
                                if qos == 1 { 0x40 } else { 0x50 },
                                2,
                                body[pos],
                                body[pos + 1],
                            ]
                        })
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

fn base_config(name: &str, addr: SocketAddr) -> BridgeConfig {
    let mut config = BridgeConfig::new(name, addr.to_string());
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(50);
    config.initial_reconnect_delay = Duration::from_millis(100);
    config.max_reconnect_delay = Duration::from_millis(200);
    config
}

async fn forward(bridge: &BridgeConnection, topic: &str, payload: &str, qos: QoS) {
    let packet = PublishPacket::new(topic, payload.as_bytes().to_vec(), qos);
    bridge.forward_message(&packet);
    tokio::task::yield_now().await;
}

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

static PUBLISH_SPANS: AtomicUsize = AtomicUsize::new(0);

struct PublishSpans;

impl<S: Subscriber> Layer<S> for PublishSpans {
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        let meta = attrs.metadata();
        if COUNTING.with(Cell::get)
            && meta.target().starts_with("mqtt5::client")
            && meta.name().contains("publish")
        {
            PUBLISH_SPANS.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn bridge_whose_run_failed_does_not_keep_retrying_a_publish() {
    let _ =
        tracing::subscriber::set_global_default(tracing_subscriber::registry().with(PublishSpans));
    COUNTING.with(|counting| counting.set(true));

    let (remote, addr) = FakeRemote::start().await;
    remote.suback_code.store(0x87, Ordering::SeqCst);
    let config = base_config("run-failure-run-failed", addr)
        .add_topic("in/#", BridgeDirection::In, QoS::AtLeastOnce)
        .add_topic("out/#", BridgeDirection::Out, QoS::AtLeastOnce);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let result = tokio::time::timeout(Duration::from_secs(5), async move { running.run().await })
        .await
        .expect("setup: run() did not return");
    assert!(
        result.is_err(),
        "setup: run() did not fail on the refused subscription"
    );

    remote.close.store(0, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let before = PUBLISH_SPANS.load(Ordering::SeqCst);
    forward(&bridge, "out/x", "m1", QoS::AtLeastOnce).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let attempts = PUBLISH_SPANS.load(Ordering::SeqCst) - before;
    let stats = bridge.get_stats().await;
    assert!(
        attempts <= 1,
        "bridge whose run() ended with {result:?} made {attempts} publish attempts in one second for one message to a closed connection (stats.connected = {})",
        stats.connected
    );
}
