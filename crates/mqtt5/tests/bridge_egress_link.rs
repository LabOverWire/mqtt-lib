#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::router::MessageRouter;
use mqtt5::time::Duration;
use mqtt5::QoS;
use mqtt5_protocol::packet::publish::PublishPacket;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::Layer;

const NEVER: usize = usize::MAX;

struct FakeRemote {
    refuse: AtomicBool,
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
            refuse: AtomicBool::new(false),
            close: AtomicUsize::new(NEVER),
            connections: AtomicUsize::new(0),
            log: Mutex::new(Vec::new()),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let serving = Arc::clone(&remote);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                if serving.refuse.load(Ordering::SeqCst) {
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
        let mut chunk = [0u8; 8192];
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
                        id.map(|[hi, lo]| vec![if qos == 1 { 0x40 } else { 0x50 }, 2, hi, lo])
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

async fn connected_bridge(config: BridgeConfig) -> (Arc<BridgeConnection>, JoinHandle<()>) {
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move {
        let _ = running.run().await;
    });
    let mut up = false;
    for _ in 0..500 {
        if bridge.get_stats().await.connected {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(up, "setup: bridge never connected");
    (bridge, task)
}

fn forward(bridge: &BridgeConnection, topic: &str, payload: &str, qos: QoS) {
    let packet = PublishPacket::new(topic, payload.as_bytes().to_vec(), qos);
    bridge.forward_message(&packet);
}

async fn open_undetected_window(remote: &FakeRemote, bridge: &BridgeConnection, qos: QoS) {
    forward(bridge, "out/x", "before", qos);
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
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qos1_burst_before_the_bridge_notices_a_drop_waits_in_the_outbound_queue() {
    let (remote, addr) = FakeRemote::start().await;
    let mut config = bridge_config("egress-burst", addr, QoS::AtLeastOnce);
    config.outbound_queue_capacity = 5000;
    let (bridge, task) = connected_bridge(config).await;
    open_undetected_window(&remote, &bridge, QoS::AtLeastOnce).await;

    let expected: Vec<String> = (0..1500).map(|i| format!("m{i}")).collect();
    for payload in &expected {
        forward(&bridge, "out/x", payload, QoS::AtLeastOnce);
    }

    wait_until(Duration::from_secs(20), || {
        remote.payloads(1).len() >= expected.len()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let delivered = remote.payloads(1);
    let _ = tokio::time::timeout(Duration::from_secs(5), bridge.stop()).await;
    task.abort();

    let missing: Vec<&String> = expected.iter().filter(|m| !delivered.contains(m)).collect();
    assert!(
        missing.is_empty(),
        "outbound_queue_capacity is 5000 but {} of {} messages forwarded before the bridge noticed the drop were never published; first missing: {:?}",
        missing.len(),
        expected.len(),
        missing.first()
    );
    assert_eq!(delivered, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qos1_then_qos0_before_the_bridge_notices_a_drop_keep_their_order() {
    let (remote, addr) = FakeRemote::start().await;
    let mut config = BridgeConfig::new("egress-mixed", addr.to_string())
        .add_topic("out/one/#", BridgeDirection::Out, QoS::AtLeastOnce)
        .add_topic("out/x", BridgeDirection::Out, QoS::AtLeastOnce)
        .add_topic("out/zero/#", BridgeDirection::Out, QoS::AtMostOnce);
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(50);
    config.initial_reconnect_delay = Duration::from_millis(100);
    config.max_reconnect_delay = Duration::from_millis(200);
    let (bridge, task) = connected_bridge(config).await;
    open_undetected_window(&remote, &bridge, QoS::AtLeastOnce).await;

    let mut expected = Vec::new();
    for i in 0..20 {
        let one = format!("one{i}");
        let zero = format!("zero{i}");
        forward(&bridge, "out/one/t", &one, QoS::AtLeastOnce);
        forward(&bridge, "out/zero/t", &zero, QoS::AtMostOnce);
        expected.push(one);
        expected.push(zero);
    }

    wait_until(Duration::from_secs(10), || {
        remote.payloads(1).len() >= expected.len()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let delivered = remote.payloads(1);
    let _ = tokio::time::timeout(Duration::from_secs(5), bridge.stop()).await;
    task.abort();
    assert_eq!(delivered, expected);
}

type CallCounts = Arc<Mutex<HashMap<String, usize>>>;

struct BeginPublishCounter(CallCounts);

impl<S: tracing::Subscriber> Layer<S> for BeginPublishCounter {
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _id: &tracing::span::Id,
        _ctx: Context<'_, S>,
    ) {
        if attrs.metadata().name() != "begin_publish" {
            return;
        }
        if let Some(name) = std::thread::current().name() {
            *self.0.lock().unwrap().entry(name.to_string()).or_default() += 1;
        }
    }
}

fn begin_publish_counts() -> &'static CallCounts {
    static COUNTS: OnceLock<CallCounts> = OnceLock::new();
    COUNTS.get_or_init(|| {
        let counts = CallCounts::default();
        let subscriber =
            tracing_subscriber::registry().with(BeginPublishCounter(Arc::clone(&counts)));
        tracing::subscriber::set_global_default(subscriber).unwrap();
        counts
    })
}

fn begin_publish_calls(thread_name: &str) -> usize {
    begin_publish_counts()
        .lock()
        .unwrap()
        .get(thread_name)
        .copied()
        .unwrap_or(0)
}

fn on_named_runtime<T>(thread_name: &'static str, test: impl std::future::Future<Output = T>) -> T {
    begin_publish_counts();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name(thread_name)
        .enable_all()
        .build()
        .unwrap()
        .block_on(test)
}

async fn retries_over_two_seconds(thread_name: &str) -> usize {
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = begin_publish_calls(thread_name);
    tokio::time::sleep(Duration::from_secs(2)).await;
    begin_publish_calls(thread_name) - before
}

#[test]
fn stopped_bridge_does_not_keep_retrying_a_qos0_message() {
    let thread_name = "egress-stop-worker";
    let retries = on_named_runtime(thread_name, async {
        let (remote, addr) = FakeRemote::start().await;
        let (bridge, task) =
            connected_bridge(bridge_config("egress-stop", addr, QoS::AtMostOnce)).await;
        open_undetected_window(&remote, &bridge, QoS::AtMostOnce).await;
        forward(&bridge, "out/x", "stuck", QoS::AtMostOnce);
        tokio::time::sleep(Duration::from_millis(100)).await;
        bridge.stop().await.unwrap();
        task.abort();
        let retries = retries_over_two_seconds(thread_name).await;
        drop(bridge);
        retries
    });
    assert!(
        retries <= 1,
        "the stopped bridge called begin_publish {retries} times in 2 s for a message it cannot send"
    );
}

#[test]
fn bridge_that_gave_up_reconnecting_does_not_keep_retrying_a_qos0_message() {
    let thread_name = "egress-giveup-worker";
    let (retries, run_ended) = on_named_runtime(thread_name, async {
        let (remote, addr) = FakeRemote::start().await;
        let mut config = bridge_config("egress-giveup", addr, QoS::AtMostOnce);
        config.max_reconnect_attempts = Some(2);
        let (bridge, task) = connected_bridge(config).await;
        open_undetected_window(&remote, &bridge, QoS::AtMostOnce).await;
        remote.refuse.store(true, Ordering::SeqCst);
        forward(&bridge, "out/x", "stuck", QoS::AtMostOnce);
        let ended = tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .is_ok();
        let retries = retries_over_two_seconds(thread_name).await;
        drop(bridge);
        (retries, ended)
    });
    assert!(run_ended, "setup: the bridge never gave up reconnecting");
    assert!(
        retries <= 1,
        "the bridge gave up reconnecting but called begin_publish {retries} times in 2 s for a message it cannot send"
    );
}
