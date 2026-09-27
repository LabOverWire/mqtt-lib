#![cfg(feature = "broker")]
mod common;

use bytes::BytesMut;
use common::MessageCollector;
use mqtt5::broker::config::{BrokerConfig, StorageBackend as BackendKind, StorageConfig};
use mqtt5::broker::server::MqttBroker;
use mqtt5::broker::storage::{ClientSession, DynamicStorage, FileBackend, StorageBackend};
use mqtt5::time::Duration;
use mqtt5::transport::packet_io::read_packet_from_stream;
use mqtt5::MqttClient;
use mqtt5_protocol::packet::connack::ConnAckPacket;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::disconnect::DisconnectPacket;
use mqtt5_protocol::packet::{MqttPacket, Packet};
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use mqtt5_protocol::types::{ConnectOptions, WillMessage};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time::sleep;

struct Broker {
    addr: String,
    storage: Option<Arc<DynamicStorage>>,
    handle: tokio::task::JoinHandle<()>,
}

impl Broker {
    async fn start(config: BrokerConfig) -> Self {
        let config =
            config.with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("bind address"));
        let mut broker = MqttBroker::with_config(config).await.expect("start broker");
        let addr = broker.local_addr().expect("broker address").to_string();
        let storage = broker.storage();
        let handle = tokio::spawn(async move {
            if let Err(e) = broker.run().await {
                tracing::debug!("broker stopped: {e}");
            }
        });
        sleep(Duration::from_millis(100)).await;
        Self {
            addr,
            storage,
            handle,
        }
    }

    async fn persistent() -> Self {
        Self::start(BrokerConfig::default().with_storage(memory_storage())).await
    }

    fn url(&self) -> String {
        format!("mqtt://{}", self.addr)
    }

    fn storage(&self) -> &DynamicStorage {
        self.storage.as_deref().expect("persistence is enabled")
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn memory_storage() -> StorageConfig {
    StorageConfig {
        backend: BackendKind::Memory,
        enable_persistence: true,
        ..Default::default()
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
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            buffer: BytesMut::new(),
        }
    }

    async fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.stream.write_all(bytes).await
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

async fn read_connack(wire: &mut Wire) -> ConnAckPacket {
    match wire.next(Duration::from_secs(5)).await {
        Some(Packet::ConnAck(connack)) => {
            assert_eq!(connack.reason_code, ReasonCode::Success);
            connack
        }
        other => panic!("expected CONNACK, got {other:?}"),
    }
}

async fn connect(addr: &str, options: ConnectOptions) -> (Wire, ConnAckPacket) {
    connect_packet(addr, ConnectPacket::new(options)).await
}

async fn connect_packet(addr: &str, packet: ConnectPacket) -> (Wire, ConnAckPacket) {
    let mut wire = Wire::new(TcpStream::connect(addr).await.expect("connect tcp"));
    let mut buf = Vec::new();
    packet.encode(&mut buf).expect("encode CONNECT");
    wire.write_all(&buf).await.expect("write CONNECT");
    let connack = read_connack(&mut wire).await;
    (wire, connack)
}

async fn disconnect(mut wire: Wire, disconnect: DisconnectPacket) {
    let mut buf = Vec::new();
    disconnect.encode(&mut buf).expect("encode DISCONNECT");
    wire.write_all(&buf).await.expect("write DISCONNECT");
    assert!(
        wire.closed(Duration::from_secs(5)).await,
        "broker must close after DISCONNECT"
    );
}

fn disconnect_with_expiry(session_expiry: u32) -> DisconnectPacket {
    let mut packet = DisconnectPacket::new(ReasonCode::Success);
    packet
        .properties
        .set_session_expiry_interval(session_expiry);
    packet
}

async fn resumes(addr: &str, client_id: &str) -> bool {
    let (stream, connack) = connect(addr, options(client_id, false, Some(60))).await;
    disconnect(stream, DisconnectPacket::new(ReasonCode::Success)).await;
    connack.session_present
}

#[tokio::test]
async fn session_expiry_counts_from_disconnect() {
    let broker = Broker::persistent().await;
    let (stream, _) = connect(&broker.addr, options("long-lived", true, Some(2))).await;
    sleep(Duration::from_secs(3)).await;
    disconnect(stream, DisconnectPacket::new(ReasonCode::Success)).await;

    assert!(
        resumes(&broker.addr, "long-lived").await,
        "a 2s Session Expiry counts from the disconnect, not from CONNECT"
    );
}

#[tokio::test]
async fn disconnect_expiry_extends_a_long_connection() {
    let broker = Broker::persistent().await;
    let (stream, _) = connect(&broker.addr, options("long-extend", true, Some(1))).await;
    sleep(Duration::from_millis(2500)).await;
    disconnect(stream, disconnect_with_expiry(60)).await;
    sleep(Duration::from_millis(1500)).await;

    assert!(
        resumes(&broker.addr, "long-extend").await,
        "the DISCONNECT Session Expiry of 60s applies from the disconnect"
    );
}

#[tokio::test]
async fn expiry_sweep_keeps_the_session_of_a_connected_client() {
    let mut storage = memory_storage();
    storage.cleanup_interval = Duration::from_millis(200);
    let broker = Broker::start(BrokerConfig::default().with_storage(storage)).await;
    let (stream, _) = connect(&broker.addr, options("swept", true, Some(1))).await;
    sleep(Duration::from_millis(2500)).await;

    assert!(
        broker
            .storage()
            .get_session("swept")
            .await
            .expect("read session")
            .is_some(),
        "the session of a connected client must not expire"
    );
    disconnect(stream, DisconnectPacket::new(ReasonCode::Success)).await;
    assert!(resumes(&broker.addr, "swept").await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expiry_zero_departure_keeps_the_successor_session() {
    let broker = Broker::persistent().await;
    let rounds = 100;
    let mut lost = 0;
    for round in 0..rounds {
        let client_id = format!("race-{round}");
        let (mut first, _) = connect(&broker.addr, options(&client_id, true, None)).await;
        let mut second = Wire::new(TcpStream::connect(&broker.addr).await.expect("connect tcp"));
        let mut connect_bytes = Vec::new();
        ConnectPacket::new(options(&client_id, true, Some(3600)))
            .encode(&mut connect_bytes)
            .expect("encode CONNECT");
        let (first_sent, second_sent) = tokio::join!(
            first.write_all(&[0xE0, 0x00]),
            second.write_all(&connect_bytes)
        );
        first_sent.expect("first DISCONNECT");
        second_sent.expect("second CONNECT");
        read_connack(&mut second).await;
        sleep(Duration::from_millis(20)).await;
        second
            .write_all(&[0xE0, 0x00])
            .await
            .expect("second DISCONNECT");
        sleep(Duration::from_millis(20)).await;
        if broker
            .storage()
            .get_session(&client_id)
            .await
            .expect("read session")
            .is_none()
        {
            lost += 1;
        }
    }
    assert_eq!(
        lost, 0,
        "{lost}/{rounds} successor sessions with Session Expiry 3600 were deleted by a departing expiry-0 connection"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn takeover_of_an_expiry_zero_session_keeps_the_successor_session() {
    let broker = Broker::persistent().await;
    let rounds = 50;
    let mut lost = 0;
    for round in 0..rounds {
        let client_id = format!("takeover-{round}");
        let (first, _) = connect(&broker.addr, options(&client_id, true, None)).await;
        let (mut second, _) = connect(&broker.addr, options(&client_id, true, Some(3600))).await;
        sleep(Duration::from_millis(20)).await;
        second.write_all(&[0xE0, 0x00]).await.expect("DISCONNECT");
        sleep(Duration::from_millis(50)).await;
        drop(first);
        if broker
            .storage()
            .get_session(&client_id)
            .await
            .expect("read session")
            .is_none()
        {
            lost += 1;
        }
    }
    assert_eq!(
        lost, 0,
        "{lost}/{rounds} successor sessions lost after takeover"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_racing_a_disconnect_with_expiry_zero_keeps_its_subscriptions() {
    let broker = Broker::persistent().await;
    let (mut publisher, _) = connect(&broker.addr, options("rr-pub", true, None)).await;
    let rounds = 60;
    let mut resumed = 0;
    let mut inconsistent = 0;
    for round in 0..rounds {
        let client_id = format!("rr-{round}");
        let topic = format!("t/{round:03}");
        let (mut first, _) = connect(&broker.addr, options(&client_id, true, Some(3600))).await;
        let mut subscribe = vec![0x82, 0x0B, 0x00, 0x01, 0x00, 0x00, 0x05];
        subscribe.extend_from_slice(topic.as_bytes());
        subscribe.push(0x00);
        first.write_all(&subscribe).await.expect("SUBSCRIBE");
        assert!(
            matches!(
                first.next(Duration::from_secs(5)).await,
                Some(Packet::SubAck(_))
            ),
            "expected SUBACK"
        );

        let mut second = Wire::new(TcpStream::connect(&broker.addr).await.expect("connect tcp"));
        let mut connect_bytes = Vec::new();
        ConnectPacket::new(options(&client_id, false, Some(3600)))
            .encode(&mut connect_bytes)
            .expect("encode CONNECT");
        let mut end_session = Vec::new();
        disconnect_with_expiry(0)
            .encode(&mut end_session)
            .expect("encode DISCONNECT");
        let (first_sent, second_sent) = tokio::join!(
            first.write_all(&end_session),
            second.write_all(&connect_bytes)
        );
        first_sent.expect("first DISCONNECT");
        second_sent.expect("second CONNECT");
        let session_present = read_connack(&mut second).await.session_present;
        sleep(Duration::from_millis(50)).await;

        let mut publish = vec![0x30, 0x0A, 0x00, 0x05];
        publish.extend_from_slice(topic.as_bytes());
        publish.extend_from_slice(&[0x00, b'h', b'i']);
        publisher.write_all(&publish).await.expect("PUBLISH");
        let delivered = matches!(
            second.next(Duration::from_millis(300)).await,
            Some(Packet::Publish(_))
        );
        let stored = broker
            .storage()
            .get_session(&client_id)
            .await
            .expect("read session");
        if session_present {
            resumed += 1;
            let stored_has_subscription = stored
                .as_ref()
                .is_some_and(|session| session.subscriptions.contains_key(&topic));
            if !delivered || !stored_has_subscription {
                inconsistent += 1;
            }
        }
        second.write_all(&[0xE0, 0x00]).await.expect("DISCONNECT");
    }
    assert_eq!(
        inconsistent, 0,
        "{inconsistent}/{resumed} resumed sessions lost their subscription"
    );
}

#[tokio::test]
async fn version_one_session_files_migrate_into_the_session_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sessions = dir.path().join("sessions");
    std::fs::create_dir_all(&sessions).expect("sessions dir");
    std::fs::write(dir.path().join(".storage_version"), "1").expect("version");
    let mut value =
        serde_json::to_value(ClientSession::new("legacy", true, Some(3600))).expect("json");
    value
        .as_object_mut()
        .expect("object")
        .remove("connection_token")
        .expect("connection token field");
    std::fs::write(
        sessions.join("legacy.json"),
        serde_json::to_string(&value).expect("json"),
    )
    .expect("write legacy file");
    std::fs::write(sessions.join("broken.json"), b"{\"client_id\":").expect("write broken file");

    let backend = FileBackend::new(dir.path()).await.expect("file backend");
    let loaded = backend
        .get_session("legacy")
        .await
        .expect("read")
        .expect("migrated session");
    assert_eq!(loaded.connection_token, 0);
    assert_eq!(loaded.expiry_interval, Some(3600));
    assert!(backend.get_session("broken").await.expect("read").is_none());
    drop(backend);

    assert!(!sessions.join("legacy.json").exists());
    assert!(!sessions.join("broken.json").exists());
    assert!(
        std::fs::read_dir(&sessions)
            .expect("list sessions dir")
            .filter_map(std::result::Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("broken.json.corrupt-")),
        "the unreadable legacy file was not kept aside"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join(".storage_version")).expect("version"),
        "2"
    );
    let reopened = FileBackend::new(dir.path()).await.expect("file backend");
    assert!(reopened
        .get_session("legacy")
        .await
        .expect("read")
        .is_some());
}

fn no_persistence() -> BrokerConfig {
    BrokerConfig::default().with_storage(StorageConfig {
        enable_persistence: false,
        ..Default::default()
    })
}

#[tokio::test]
async fn will_is_published_without_persistence() {
    let broker = Broker::start(no_persistence()).await;
    let watcher = MqttClient::new("np-will-watcher");
    watcher
        .connect(&broker.url())
        .await
        .expect("watcher connect");
    let collector = MessageCollector::new();
    watcher
        .subscribe("will/np-will", collector.callback())
        .await
        .expect("subscribe");
    sleep(Duration::from_millis(100)).await;

    let mut will = WillMessage::new("will/np-will", "offline");
    will.properties.will_delay_interval = Some(5);
    let (stream, _) = connect(
        &broker.addr,
        options("np-will", true, Some(60)).with_will(will),
    )
    .await;
    drop(stream);

    assert!(
        collector
            .wait_for_messages(1, Duration::from_millis(1500))
            .await,
        "without persistence the session ends at disconnect, so the Will is published at once"
    );
    watcher.disconnect().await.expect("watcher disconnect");
}

#[tokio::test]
async fn disconnect_expiry_after_zero_is_a_protocol_error_without_persistence() {
    let broker = Broker::start(no_persistence()).await;
    let (mut stream, _) = connect(&broker.addr, options("np-error", true, Some(0))).await;
    let mut buf = Vec::new();
    disconnect_with_expiry(60)
        .encode(&mut buf)
        .expect("encode DISCONNECT");
    stream.write_all(&buf).await.expect("write DISCONNECT");
    match stream.next(Duration::from_secs(5)).await {
        Some(Packet::Disconnect(reply)) => {
            assert_eq!(reply.reason_code, ReasonCode::ProtocolError);
        }
        other => panic!("the broker must send DISCONNECT, got {other:?}"),
    }
}

#[tokio::test]
async fn sessions_do_not_survive_disconnect_without_persistence() {
    let broker = Broker::start(no_persistence()).await;
    let (stream, connack) = connect(&broker.addr, options("np-resume", true, Some(60))).await;
    assert_eq!(
        connack.properties.get_session_expiry_interval(),
        Some(0),
        "the broker cannot keep the session, so it grants Session Expiry 0"
    );
    disconnect(stream, DisconnectPacket::new(ReasonCode::Success)).await;
    assert!(!resumes(&broker.addr, "np-resume").await);
}

fn capped(maximum: u64) -> BrokerConfig {
    BrokerConfig::default()
        .with_storage(memory_storage())
        .with_session_expiry(Duration::from_secs(maximum))
}

#[tokio::test]
async fn session_expiry_above_the_broker_maximum_is_capped_and_advertised() {
    let broker = Broker::start(capped(10)).await;
    let (stream, connack) = connect(&broker.addr, options("cap-high", true, Some(3600))).await;
    assert_eq!(connack.properties.get_session_expiry_interval(), Some(10));
    disconnect(stream, DisconnectPacket::new(ReasonCode::Success)).await;
    let stored = broker
        .storage()
        .get_session("cap-high")
        .await
        .expect("read session")
        .expect("session");
    assert_eq!(stored.expiry_interval, Some(10));
}

#[tokio::test]
async fn session_expiry_within_the_broker_maximum_is_not_advertised() {
    let broker = Broker::start(capped(10)).await;
    let (stream, connack) = connect(&broker.addr, options("cap-low", true, Some(5))).await;
    assert_eq!(connack.properties.get_session_expiry_interval(), None);
    disconnect(stream, DisconnectPacket::new(ReasonCode::Success)).await;
    let stored = broker
        .storage()
        .get_session("cap-low")
        .await
        .expect("read session")
        .expect("session");
    assert_eq!(stored.expiry_interval, Some(5));
}

#[tokio::test]
async fn persistent_v311_session_is_capped_by_the_broker_maximum() {
    let broker = Broker::start(capped(10)).await;
    let mut packet = ConnectPacket::new(options("cap-v311", false, None));
    packet.protocol_version = 4;
    let (stream, _) = connect_packet(&broker.addr, packet).await;
    drop(stream);
    sleep(Duration::from_millis(200)).await;
    let stored = broker
        .storage()
        .get_session("cap-v311")
        .await
        .expect("read session")
        .expect("session");
    assert_eq!(stored.expiry_interval, Some(10));
}

#[tokio::test]
async fn disconnect_expiry_is_capped_by_the_broker_maximum() {
    let broker = Broker::start(capped(10)).await;
    let (stream, _) = connect(&broker.addr, options("cap-disconnect", true, Some(5))).await;
    disconnect(stream, disconnect_with_expiry(3600)).await;
    let stored = broker
        .storage()
        .get_session("cap-disconnect")
        .await
        .expect("read session")
        .expect("session");
    assert_eq!(stored.expiry_interval, Some(10));
}

#[tokio::test]
async fn default_broker_does_not_cap_or_advertise_session_expiry() {
    let broker = Broker::persistent().await;
    let (stream, connack) = connect(&broker.addr, options("uncapped", true, Some(u32::MAX))).await;
    assert_eq!(connack.properties.get_session_expiry_interval(), None);
    disconnect(stream, DisconnectPacket::new(ReasonCode::Success)).await;
}
