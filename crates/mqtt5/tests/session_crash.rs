#![cfg(feature = "broker")]

use bytes::BytesMut;
use mqtt5::broker::config::{BrokerConfig, StorageBackend as BackendKind, StorageConfig};
use mqtt5::broker::server::MqttBroker;
use mqtt5::broker::storage::{FileBackend, StorageBackend};
use mqtt5::time::Duration;
use mqtt5::transport::packet_io::read_packet_from_stream;
use mqtt5_protocol::packet::connack::ConnAckPacket;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::{MqttPacket, Packet};
use mqtt5_protocol::types::ConnectOptions;
use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;

const HARNESS_DIR: &str = "MQTT5_CRASH_HARNESS_DIR";
const HARNESS_CLEANUP_MS: &str = "MQTT5_CRASH_HARNESS_CLEANUP_MS";
const LISTENING: &str = "CRASH-HARNESS-LISTENING ";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crash_harness_broker() {
    let Ok(dir) = std::env::var(HARNESS_DIR) else {
        return;
    };
    let cleanup_ms = std::env::var(HARNESS_CLEANUP_MS)
        .ok()
        .and_then(|ms| ms.parse().ok())
        .unwrap_or(3_600_000);
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("bind address"))
        .with_storage(StorageConfig {
            backend: BackendKind::File,
            base_dir: dir.into(),
            enable_persistence: true,
            cleanup_interval: Duration::from_millis(cleanup_ms),
            ..Default::default()
        });
    let mut broker = MqttBroker::with_config(config).await.expect("start broker");
    let addr = broker.local_addr().expect("broker address");
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{LISTENING}{addr}").expect("announce address");
    stdout.flush().expect("flush address");
    broker.run().await.expect("broker run");
}

struct CrashBroker {
    addr: String,
    child: Child,
}

impl CrashBroker {
    async fn start(dir: &Path, cleanup: Duration) -> Self {
        let mut child = Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "crash_harness_broker",
                "--exact",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(HARNESS_DIR, dir)
            .env(HARNESS_CLEANUP_MS, cleanup.as_millis().to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn broker process");
        let stdout = child.stdout.take().expect("broker stdout");
        let mut lines = BufReader::new(stdout).lines();
        let addr = tokio::time::timeout(Duration::from_secs(30), async {
            while let Some(line) = lines.next_line().await.expect("read broker stdout") {
                if let Some((_, addr)) = line.split_once(LISTENING) {
                    return addr.to_string();
                }
            }
            panic!("broker process exited before listening");
        })
        .await
        .expect("broker process did not start");
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        Self { addr, child }
    }

    async fn crash(mut self) {
        self.child.start_kill().expect("kill broker process");
        self.child.wait().await.expect("reap broker process");
    }
}

fn options(client_id: &str, clean_start: bool, session_expiry: Option<u32>) -> ConnectOptions {
    let options = ConnectOptions::new(client_id).with_clean_start(clean_start);
    match session_expiry {
        Some(expiry) => options.with_session_expiry_interval(expiry),
        None => options,
    }
}

struct Wire {
    stream: TcpStream,
    buffer: BytesMut,
}

impl Wire {
    async fn send(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).await.expect("write packet");
    }

    async fn next(&mut self, wait: Duration) -> Option<Packet> {
        tokio::time::timeout(
            wait,
            read_packet_from_stream(&mut self.stream, 5, &mut self.buffer, 1 << 20),
        )
        .await
        .ok()?
        .ok()
    }

    async fn closed(&mut self, wait: Duration) -> bool {
        tokio::time::timeout(
            wait,
            read_packet_from_stream(&mut self.stream, 5, &mut self.buffer, 1 << 20),
        )
        .await
        .is_ok_and(|read| read.is_err())
    }
}

async fn connect(addr: &str, options: ConnectOptions) -> (Wire, ConnAckPacket) {
    let mut wire = Wire {
        stream: TcpStream::connect(addr).await.expect("connect tcp"),
        buffer: BytesMut::new(),
    };
    let mut bytes = Vec::new();
    ConnectPacket::new(options)
        .encode(&mut bytes)
        .expect("encode CONNECT");
    wire.send(&bytes).await;
    match wire.next(Duration::from_secs(10)).await {
        Some(Packet::ConnAck(connack)) => (wire, connack),
        other => panic!("expected CONNACK, got {other:?}"),
    }
}

async fn subscribe(wire: &mut Wire, topic: &str) {
    let topic_len = u8::try_from(topic.len()).expect("short topic");
    let mut bytes = vec![0x82, 6 + topic_len, 0x00, 1, 0x00, 0x00, topic_len];
    bytes.extend_from_slice(topic.as_bytes());
    bytes.push(1);
    wire.send(&bytes).await;
    assert!(
        matches!(
            wire.next(Duration::from_secs(5)).await,
            Some(Packet::SubAck(_))
        ),
        "expected SUBACK"
    );
}

async fn publish(wire: &mut Wire, topic: &str) {
    let topic_len = u8::try_from(topic.len()).expect("short topic");
    let mut bytes = vec![0x32, 7 + topic_len, 0x00, topic_len];
    bytes.extend_from_slice(topic.as_bytes());
    bytes.extend_from_slice(&[0x00, 0x01, 0x00, b'h', b'i']);
    wire.send(&bytes).await;
    assert!(
        matches!(
            wire.next(Duration::from_secs(5)).await,
            Some(Packet::PubAck(_))
        ),
        "expected PUBACK"
    );
}

async fn received_publish(wire: &mut Wire) -> bool {
    matches!(
        wire.next(Duration::from_millis(500)).await,
        Some(Packet::Publish(_))
    )
}

async fn end(mut wire: Wire, disconnect: &[u8]) {
    wire.send(disconnect).await;
    assert!(
        wire.closed(Duration::from_secs(5)).await,
        "broker must close after DISCONNECT"
    );
}

const DISCONNECT: [u8; 2] = [0xE0, 0x00];
const DISCONNECT_EXPIRY_ZERO: [u8; 9] = [0xE0, 0x07, 0x00, 0x05, 0x11, 0x00, 0x00, 0x00, 0x00];
const NEVER: Duration = Duration::from_secs(3600);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crashed_broker_serves_no_connection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let broker = CrashBroker::start(dir.path(), NEVER).await;
    let (mut wire, _) = connect(&broker.addr, options("survivor", true, Some(60))).await;
    broker.crash().await;
    let pinged = wire.stream.write_all(&[0xC0, 0x00]).await;
    assert!(
        pinged.is_err() || wire.closed(Duration::from_secs(1)).await,
        "a connection outlived the crash"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subacked_subscription_survives_a_crash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let broker = CrashBroker::start(dir.path(), NEVER).await;
    let (mut stream, _) = connect(&broker.addr, options("durable", true, Some(60))).await;
    subscribe(&mut stream, "durable/t").await;
    broker.crash().await;
    drop(stream);

    let broker = CrashBroker::start(dir.path(), NEVER).await;
    let (mut publisher, _) = connect(&broker.addr, options("durable-pub", true, None)).await;
    let (mut stream, connack) = connect(&broker.addr, options("durable", false, Some(60))).await;
    assert!(
        connack.session_present,
        "the session was claimed before CONNACK and must survive a crash"
    );
    publish(&mut publisher, "durable/t").await;
    assert!(
        received_publish(&mut stream).await,
        "a SUBACKed subscription must survive a crash"
    );
    broker.crash().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clean_start_discard_is_not_undone_by_a_crash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let broker = CrashBroker::start(dir.path(), NEVER).await;
    let (mut stream, _) = connect(&broker.addr, options("discard", true, Some(60))).await;
    subscribe(&mut stream, "discard/old").await;
    end(stream, &DISCONNECT).await;
    let (_stream, connack) = connect(&broker.addr, options("discard", true, Some(60))).await;
    assert!(!connack.session_present);
    broker.crash().await;

    let broker = CrashBroker::start(dir.path(), NEVER).await;
    let (mut publisher, _) = connect(&broker.addr, options("discard-pub", true, None)).await;
    let (mut stream, connack) = connect(&broker.addr, options("discard", false, Some(60))).await;
    assert!(
        connack.session_present,
        "the clean-start session was acknowledged and must survive"
    );
    publish(&mut publisher, "discard/old").await;
    assert!(
        !received_publish(&mut stream).await,
        "the discarded session's subscription came back after a crash"
    );
    broker.crash().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ended_session_expires_at_its_disconnect_time_after_a_crash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let broker = CrashBroker::start(dir.path(), NEVER).await;
    let (stream, _) = connect(&broker.addr, options("ended", true, Some(1))).await;
    end(stream, &DISCONNECT).await;
    sleep(Duration::from_millis(2500)).await;
    broker.crash().await;

    let broker = CrashBroker::start(dir.path(), NEVER).await;
    let (stream, connack) = connect(&broker.addr, options("ended", false, Some(60))).await;
    end(stream, &DISCONNECT).await;
    broker.crash().await;
    assert!(
        !connack.session_present,
        "the session ended 2.5s before the crash with a 1s expiry, so it must not be resumed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_session_queues_messages_published_after_a_crash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let broker = CrashBroker::start(dir.path(), NEVER).await;
    let (mut stream, _) = connect(&broker.addr, options("offline", true, Some(60))).await;
    subscribe(&mut stream, "offline/t").await;
    end(stream, &DISCONNECT).await;
    broker.crash().await;

    let broker = CrashBroker::start(dir.path(), NEVER).await;
    let (mut publisher, _) = connect(&broker.addr, options("offline-pub", true, None)).await;
    publish(&mut publisher, "offline/t").await;
    let (mut stream, connack) = connect(&broker.addr, options("offline", false, Some(60))).await;
    assert!(connack.session_present);
    assert!(
        received_publish(&mut stream).await,
        "a message published after the restart, before the client came back, must be queued for it"
    );
    broker.crash().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_connected_at_a_crash_expires_from_the_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let broker = CrashBroker::start(dir.path(), NEVER).await;
    let (stream, _) = connect(&broker.addr, options("crashed", true, Some(1))).await;
    broker.crash().await;
    drop(stream);

    let broker = CrashBroker::start(dir.path(), NEVER).await;
    sleep(Duration::from_millis(2500)).await;
    let (_stream, connack) = connect(&broker.addr, options("crashed", false, Some(60))).await;
    broker.crash().await;
    assert!(
        !connack.session_present,
        "a session connected at the crash with a 1s expiry must be gone 2.5s after the restart"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expiry_of_a_session_connected_at_a_crash_survives_a_second_crash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let broker = CrashBroker::start(dir.path(), NEVER).await;
    let (stream, _) = connect(&broker.addr, options("twice", true, Some(2))).await;
    broker.crash().await;
    drop(stream);

    let broker = CrashBroker::start(dir.path(), NEVER).await;
    sleep(Duration::from_millis(2500)).await;
    broker.crash().await;

    let broker = CrashBroker::start(dir.path(), NEVER).await;
    let (_stream, connack) = connect(&broker.addr, options("twice", false, Some(60))).await;
    broker.crash().await;
    assert!(
        !connack.session_present,
        "the session ended at the first restart; a second crash must not restart its expiry"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expiry_zero_session_is_not_rebuilt_after_a_crash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let broker = CrashBroker::start(dir.path(), NEVER).await;
    let (mut stream, _) = connect(&broker.addr, options("zero-crash", true, Some(0))).await;
    subscribe(&mut stream, "zc/t").await;
    broker.crash().await;
    drop(stream);

    let backend = FileBackend::new(dir.path()).await.expect("file backend");
    let stored_at_crash = backend.session_client_ids().await.expect("list sessions");
    drop(backend);
    assert_eq!(
        stored_at_crash,
        ["zero-crash"],
        "the claim is durable before CONNACK"
    );

    let broker = CrashBroker::start(dir.path(), NEVER).await;
    let (mut publisher, _) = connect(&broker.addr, options("zc-pub", true, None)).await;
    let (mut stream, connack) = connect(&broker.addr, options("zero-crash", false, Some(60))).await;
    assert!(!connack.session_present);
    publish(&mut publisher, "zc/t").await;
    assert!(!received_publish(&mut stream).await);
    broker.crash().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ended_expiry_zero_session_is_removed_before_the_connection_closes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let broker = CrashBroker::start(dir.path(), NEVER).await;
    let (mut stream, _) = connect(&broker.addr, options("gone", true, Some(60))).await;
    subscribe(&mut stream, "gone/t").await;
    end(stream, &DISCONNECT_EXPIRY_ZERO).await;
    broker.crash().await;

    let backend = FileBackend::new(dir.path()).await.expect("file backend");
    assert!(
        backend
            .session_client_ids()
            .await
            .expect("list sessions")
            .is_empty(),
        "the session ended by DISCONNECT with expiry 0 is still stored after a crash"
    );
}
