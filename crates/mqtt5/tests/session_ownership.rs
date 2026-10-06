#![cfg(feature = "broker")]

use bytes::BytesMut;
use mqtt5::broker::auth::{AuthProvider, AuthResult};
use mqtt5::broker::config::{BrokerConfig, StorageBackend as BackendKind, StorageConfig};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::server::{BrokerShutdownHandle, MqttBroker};
use mqtt5::broker::storage::{
    ClientSession, DynamicStorage, FileBackend, StorageBackend, StoredSubscription,
};
use mqtt5::error::Result;
use mqtt5::time::Duration;
use mqtt5::transport::packet_io::read_packet_from_stream;
use mqtt5_protocol::packet::connack::ConnAckPacket;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::{MqttPacket, Packet};
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use mqtt5_protocol::types::ConnectOptions;
use std::future::Future;
use std::net::SocketAddr;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time::sleep;

struct Provider {
    slow_subscribe: Duration,
    deny_secret: AtomicBool,
}

impl AuthProvider for Provider {
    fn authenticate<'a>(
        &'a self,
        _connect: &'a ConnectPacket,
        _client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        Box::pin(async move { Ok(AuthResult::success()) })
    }

    fn authorize_publish<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { true })
    }

    fn authorize_subscribe<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            if topic_filter.starts_with("slow/") {
                sleep(self.slow_subscribe).await;
            }
            !(topic_filter.starts_with("secret/") && self.deny_secret.load(Ordering::SeqCst))
        })
    }
}

struct Broker {
    addr: String,
    storage: Option<Arc<DynamicStorage>>,
    router: Arc<MessageRouter>,
    provider: Arc<Provider>,
    shutdown: BrokerShutdownHandle,
    handle: tokio::task::JoinHandle<()>,
}

impl Broker {
    async fn start(config: BrokerConfig, slow_subscribe: Duration) -> Self {
        let config =
            config.with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("bind address"));
        let provider = Arc::new(Provider {
            slow_subscribe,
            deny_secret: AtomicBool::new(false),
        });
        let mut broker = MqttBroker::with_config(config)
            .await
            .expect("start broker")
            .with_auth_provider(Arc::clone(&provider) as Arc<dyn AuthProvider>);
        let addr = broker.local_addr().expect("broker address").to_string();
        let storage = broker.storage();
        let router = broker.router();
        let shutdown = broker.shutdown_handle();
        let handle = tokio::spawn(async move {
            if let Err(e) = broker.run().await {
                tracing::debug!("broker stopped: {e}");
            }
        });
        sleep(Duration::from_millis(100)).await;
        Self {
            addr,
            storage,
            router,
            provider,
            shutdown,
            handle,
        }
    }

    async fn stop(self) {
        self.handle.abort();
        let aborted = self.handle.await;
        assert!(aborted.is_err_and(|e| e.is_cancelled()));
    }

    async fn shut_down(self) {
        self.shutdown.shutdown();
        self.handle.await.expect("broker task joins");
    }

    fn storage(&self) -> &DynamicStorage {
        self.storage.as_deref().expect("persistence is enabled")
    }
}

fn memory(cleanup_interval: Duration) -> BrokerConfig {
    BrokerConfig::default().with_storage(StorageConfig {
        backend: BackendKind::Memory,
        enable_persistence: true,
        cleanup_interval,
        ..Default::default()
    })
}

fn file(dir: &Path, cleanup_interval: Duration) -> BrokerConfig {
    BrokerConfig::default().with_storage(StorageConfig {
        backend: BackendKind::File,
        base_dir: dir.to_path_buf(),
        enable_persistence: true,
        cleanup_interval,
        ..Default::default()
    })
}

fn no_persistence() -> BrokerConfig {
    BrokerConfig::default().with_storage(StorageConfig {
        enable_persistence: false,
        ..Default::default()
    })
}

const SWEEP: Duration = Duration::from_millis(200);
const NEVER: Duration = Duration::from_secs(3600);

fn options(client_id: &str, clean_start: bool, session_expiry: Option<u32>) -> ConnectOptions {
    let options = ConnectOptions::new(client_id).with_clean_start(clean_start);
    match session_expiry {
        Some(expiry) => options.with_session_expiry_interval(expiry),
        None => options,
    }
}

fn connect_bytes(options: ConnectOptions) -> Vec<u8> {
    let mut bytes = Vec::new();
    ConnectPacket::new(options)
        .encode(&mut bytes)
        .expect("encode CONNECT");
    bytes
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
        Some(Packet::ConnAck(connack)) => connack,
        other => panic!("expected CONNACK, got {other:?}"),
    }
}

async fn connect(addr: &str, options: ConnectOptions) -> (Wire, ConnAckPacket) {
    let mut wire = Wire::new(TcpStream::connect(addr).await.expect("connect tcp"));
    wire.write_all(&connect_bytes(options))
        .await
        .expect("write CONNECT");
    let connack = read_connack(&mut wire).await;
    (wire, connack)
}

fn subscribe_bytes(packet_id: u8, topic: &str, qos: u8) -> Vec<u8> {
    let topic_len = u8::try_from(topic.len()).expect("short topic");
    let mut bytes = vec![0x82, 6 + topic_len, 0x00, packet_id, 0x00, 0x00, topic_len];
    bytes.extend_from_slice(topic.as_bytes());
    bytes.push(qos);
    bytes
}

async fn subscribe(wire: &mut Wire, topic: &str, qos: u8) {
    wire.write_all(&subscribe_bytes(1, topic, qos))
        .await
        .expect("SUBSCRIBE");
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
    let mut bytes = vec![0x30, 5 + topic_len, 0x00, topic_len];
    bytes.extend_from_slice(topic.as_bytes());
    bytes.extend_from_slice(&[0x00, b'h', b'i']);
    wire.write_all(&bytes).await.expect("PUBLISH");
}

async fn received_publish(wire: &mut Wire) -> bool {
    matches!(
        wire.next(Duration::from_millis(500)).await,
        Some(Packet::Publish(_))
    )
}

async fn end(mut wire: Wire) {
    wire.write_all(&[0xE0, 0x00]).await.expect("DISCONNECT");
    assert!(
        wire.closed(Duration::from_secs(5)).await,
        "broker must close after DISCONNECT"
    );
}

async fn still_served(wire: &mut Wire) -> bool {
    wire.write_all(&[0xC0, 0x00]).await.is_ok()
        && matches!(
            wire.next(Duration::from_millis(300)).await,
            Some(Packet::PingResp)
        )
}

#[tokio::test]
async fn subscribe_from_a_displaced_connection_does_not_keep_the_session_alive() {
    let broker = Broker::start(memory(SWEEP), Duration::from_millis(600)).await;
    let (mut first, _) = connect(&broker.addr, options("leak", true, Some(1))).await;
    first
        .write_all(&subscribe_bytes(1, "slow/a", 1))
        .await
        .expect("SUBSCRIBE");
    sleep(Duration::from_millis(100)).await;

    let (second, _) = connect(&broker.addr, options("leak", true, Some(1))).await;
    sleep(Duration::from_millis(1000)).await;
    end(second).await;
    drop(first);
    sleep(Duration::from_millis(3000)).await;

    let stored = broker.storage().get_session("leak").await.expect("read");
    assert!(
        stored.is_none(),
        "no connection is live and the 1s expiry passed, yet the session is still stored"
    );
}

#[tokio::test]
async fn subscribe_from_a_displaced_connection_does_not_keep_an_expiry_zero_session() {
    let broker = Broker::start(memory(SWEEP), Duration::from_millis(600)).await;
    let (mut first, _) = connect(&broker.addr, options("leak0", true, Some(0))).await;
    first
        .write_all(&subscribe_bytes(1, "slow/a", 1))
        .await
        .expect("SUBSCRIBE");
    sleep(Duration::from_millis(100)).await;

    let (second, _) = connect(&broker.addr, options("leak0", true, Some(0))).await;
    sleep(Duration::from_millis(1000)).await;
    end(second).await;
    drop(first);
    sleep(Duration::from_millis(1000)).await;

    assert!(
        broker
            .storage()
            .get_session("leak0")
            .await
            .expect("read")
            .is_none(),
        "an expiry-0 session outlived every connection"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscribe_racing_a_takeover_leaks_no_session() {
    let broker = Broker::start(memory(SWEEP), Duration::ZERO).await;
    let rounds: u64 = 60;
    for round in 0..rounds {
        let client_id = format!("sr-{round}");
        let (mut first, _) = connect(&broker.addr, options(&client_id, true, Some(0))).await;
        let mut burst = Vec::new();
        for i in 0..2000u16 {
            let topic = format!("t/{i:05}");
            let packet_id = u8::try_from(i % 250 + 1).expect("small packet id");
            burst.extend_from_slice(&subscribe_bytes(packet_id, &topic, 0));
        }
        let writer = tokio::spawn(async move {
            let written = first.write_all(&burst).await;
            sleep(Duration::from_millis(300)).await;
            drop(first);
            written
        });
        sleep(Duration::from_micros((round % 10) * 200)).await;
        let (mut second, _) = connect(&broker.addr, options(&client_id, true, Some(0))).await;
        sleep(Duration::from_millis(50)).await;
        second.write_all(&[0xE0, 0x00]).await.expect("DISCONNECT");
        drop(second);
        if let Err(e) = writer.await.expect("writer task") {
            tracing::debug!("burst ended early: {e}");
        }
    }
    sleep(Duration::from_millis(1500)).await;
    let mut leaked = Vec::new();
    for round in 0..rounds {
        let client_id = format!("sr-{round}");
        if let Some(session) = broker
            .storage()
            .get_session(&client_id)
            .await
            .expect("read")
        {
            leaked.push((client_id, session.connected, session.subscriptions.len()));
        }
    }
    assert!(
        leaked.is_empty(),
        "{} expiry-0 sessions outlived all their connections: {leaked:?}",
        leaked.len()
    );
}

#[tokio::test]
async fn resumed_session_drops_a_revoked_subscription_from_routing() {
    let broker = Broker::start(memory(NEVER), Duration::ZERO).await;
    let (mut publisher, _) = connect(&broker.addr, options("rv-pub", true, Some(0))).await;
    let (mut sub, _) = connect(&broker.addr, options("rv", true, Some(3600))).await;
    subscribe(&mut sub, "secret/x", 1).await;
    end(sub).await;
    sleep(Duration::from_millis(200)).await;

    broker.provider.deny_secret.store(true, Ordering::SeqCst);
    let (mut sub, connack) = connect(&broker.addr, options("rv", false, Some(3600))).await;
    assert!(connack.session_present);
    sleep(Duration::from_millis(100)).await;
    publish(&mut publisher, "secret/x").await;
    assert!(
        !received_publish(&mut sub).await,
        "a subscription dropped on restore as no longer authorized must not deliver"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn aborted_takeover_never_replaces_a_live_owners_session() {
    let broker = Broker::start(memory(NEVER), Duration::ZERO).await;
    let mut orphaned = 0;
    let mut kept = 0;
    for round in 0..40 {
        let client_id = format!("abort-{round}");
        let (mut first, _) = connect(&broker.addr, options(&client_id, true, Some(60))).await;
        subscribe(&mut first, "abort/t", 0).await;
        let before = broker
            .storage()
            .get_session(&client_id)
            .await
            .expect("read")
            .expect("session")
            .connection_token;

        let mut intruder = TcpStream::connect(&broker.addr).await.expect("connect tcp");
        intruder.set_zero_linger().expect("linger");
        intruder
            .write_all(&connect_bytes(options(&client_id, true, Some(60))))
            .await
            .expect("write CONNECT");
        drop(intruder);
        sleep(Duration::from_millis(100)).await;

        let after = broker
            .storage()
            .get_session(&client_id)
            .await
            .expect("read")
            .map(|session| {
                (
                    session.connection_token,
                    session.subscriptions.contains_key("abort/t"),
                )
            });
        if still_served(&mut first).await {
            kept += 1;
            if after != Some((before, true)) {
                orphaned += 1;
            }
        }
    }
    assert_eq!(
        orphaned, 0,
        "{orphaned}/{kept} aborted takeovers changed the stored session of a connection that stayed live"
    );
}

#[tokio::test]
async fn takeover_of_a_live_expiry_zero_session_is_not_resumed() {
    let broker = Broker::start(memory(NEVER), Duration::ZERO).await;
    let (first, _) = connect(&broker.addr, options("live-zero", true, Some(0))).await;
    sleep(Duration::from_millis(200)).await;
    let (second, connack) = connect(&broker.addr, options("live-zero", false, Some(60))).await;
    drop(second);
    drop(first);
    assert!(!connack.session_present);
}

#[tokio::test]
async fn takeover_without_persistence_does_not_inherit_subscriptions() {
    let broker = Broker::start(no_persistence(), Duration::ZERO).await;
    let (mut publisher, _) = connect(&broker.addr, options("np-pub", true, None)).await;
    let (mut first, _) = connect(&broker.addr, options("np-take", true, Some(60))).await;
    subscribe(&mut first, "np/t", 0).await;
    let (mut second, connack) = connect(&broker.addr, options("np-take", false, Some(60))).await;
    sleep(Duration::from_millis(200)).await;
    publish(&mut publisher, "np/t").await;
    let delivered = received_publish(&mut second).await;
    drop(first);
    assert!(!connack.session_present);
    assert!(
        !delivered,
        "Session Present 0, yet the new connection receives the old connection's subscription"
    );
}

#[tokio::test]
async fn takeover_of_an_expiry_zero_session_does_not_inherit_subscriptions() {
    let broker = Broker::start(memory(NEVER), Duration::ZERO).await;
    let (mut publisher, _) = connect(&broker.addr, options("p0-pub", true, None)).await;
    let (mut first, _) = connect(&broker.addr, options("p0-take", true, Some(0))).await;
    subscribe(&mut first, "p0/t", 0).await;
    let (mut second, connack) = connect(&broker.addr, options("p0-take", false, Some(60))).await;
    sleep(Duration::from_millis(200)).await;
    publish(&mut publisher, "p0/t").await;
    let delivered = received_publish(&mut second).await;
    drop(first);
    assert!(!connack.session_present);
    assert!(!delivered);
}

#[tokio::test]
async fn clean_start_after_an_offline_session_drops_its_subscriptions() {
    let broker = Broker::start(memory(NEVER), Duration::ZERO).await;
    let (mut publisher, _) = connect(&broker.addr, options("cs-pub", true, None)).await;
    let (mut first, _) = connect(&broker.addr, options("cs-take", true, Some(60))).await;
    subscribe(&mut first, "cs/t", 0).await;
    end(first).await;
    let (mut second, connack) = connect(&broker.addr, options("cs-take", true, Some(60))).await;
    sleep(Duration::from_millis(200)).await;
    publish(&mut publisher, "cs/t").await;
    let delivered = received_publish(&mut second).await;
    assert!(!connack.session_present);
    assert!(!delivered);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_from_a_previous_run_expires_after_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let backend = FileBackend::new(dir.path()).await.expect("file backend");
        let session = ClientSession::new("stale", true, Some(1));
        backend.store_session(session).await.expect("store");
    }
    let broker = Broker::start(file(dir.path(), SWEEP), Duration::ZERO).await;
    sleep(Duration::from_millis(2500)).await;
    let stored = broker
        .storage()
        .session_client_ids()
        .await
        .expect("list sessions");
    broker.shut_down().await;
    assert!(
        !stored.iter().any(|id| id == "stale"),
        "a 1s-expiry session written by a previous run must expire after the restart"
    );
    let reopened = FileBackend::new(dir.path()).await.expect("file backend");
    assert!(reopened
        .session_client_ids()
        .await
        .expect("list sessions")
        .is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_backend_expiry_zero_departure_keeps_the_successor_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let broker = Broker::start(file(dir.path(), NEVER), Duration::ZERO).await;
    let rounds = 30;
    let mut lost = 0;
    for round in 0..rounds {
        let client_id = format!("frace-{round}");
        let (mut first, _) = connect(&broker.addr, options(&client_id, true, None)).await;
        let mut second = Wire::new(TcpStream::connect(&broker.addr).await.expect("connect tcp"));
        let claim = connect_bytes(options(&client_id, true, Some(3600)));
        let (first_sent, second_sent) =
            tokio::join!(first.write_all(&[0xE0, 0x00]), second.write_all(&claim));
        first_sent.expect("first DISCONNECT");
        second_sent.expect("second CONNECT");
        read_connack(&mut second).await;
        sleep(Duration::from_millis(20)).await;
        second
            .write_all(&[0xE0, 0x00])
            .await
            .expect("second DISCONNECT");
        sleep(Duration::from_millis(30)).await;
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
    broker.stop().await;
    assert_eq!(
        lost, 0,
        "{lost}/{rounds} successor sessions were deleted by a departing expiry-0 connection"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_backend_displaced_subscribe_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let broker = Broker::start(file(dir.path(), SWEEP), Duration::from_millis(600)).await;
    let (mut first, _) = connect(&broker.addr, options("fleak", true, Some(1))).await;
    first
        .write_all(&subscribe_bytes(1, "slow/a", 1))
        .await
        .expect("SUBSCRIBE");
    sleep(Duration::from_millis(100)).await;
    let (second, _) = connect(&broker.addr, options("fleak", true, Some(1))).await;
    sleep(Duration::from_millis(1000)).await;
    let stored = broker
        .storage()
        .get_session("fleak")
        .await
        .expect("read")
        .expect("session of the live successor");
    assert!(
        !stored.subscriptions.contains_key("slow/a"),
        "the displaced connection's late SUBSCRIBE must not reach the successor's stored session"
    );
    end(second).await;
    drop(first);
    sleep(Duration::from_millis(2500)).await;
    let stored = broker
        .storage()
        .session_client_ids()
        .await
        .expect("list sessions");
    broker.stop().await;
    assert!(!stored.iter().any(|id| id == "fleak"));
}

fn unsubscribe_bytes(topic: &str) -> Vec<u8> {
    let topic_len = u8::try_from(topic.len()).expect("short topic");
    let mut bytes = vec![0xA2, 5 + topic_len, 0x00, 2, 0x00, 0x00, topic_len];
    bytes.extend_from_slice(topic.as_bytes());
    bytes
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_claims_agree_on_the_owner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let broker = Broker::start(file(dir.path(), NEVER), Duration::ZERO).await;
    let rounds = 60;
    let mut disagreements = Vec::new();
    for round in 0..rounds {
        let client_id = format!("race-{round}");
        let claim = connect_bytes(options(&client_id, round % 2 == 0, Some(60)));
        let mut streams = Vec::new();
        for _ in 0..3 {
            streams.push(Wire::new(
                TcpStream::connect(&broker.addr).await.expect("connect tcp"),
            ));
        }
        futures::future::join_all(streams.iter_mut().map(|stream| stream.write_all(&claim)))
            .await
            .into_iter()
            .for_each(|written| written.expect("write CONNECT"));
        for stream in &mut streams {
            read_connack(stream).await;
        }
        let stored = broker
            .storage()
            .get_session(&client_id)
            .await
            .expect("read")
            .expect("claimed session");
        if !broker
            .router
            .is_current_owner(&client_id, stored.connection_token)
            .await
        {
            disagreements.push(client_id);
        }
    }
    broker.stop().await;
    assert!(
        disagreements.is_empty(),
        "stored session token is not the router owner: {disagreements:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn displaced_subscribe_installs_no_route_for_the_successor() {
    let broker = Broker::start(memory(NEVER), Duration::from_millis(600)).await;
    let (mut publisher, _) = connect(&broker.addr, options("dsub-pub", true, Some(0))).await;
    let (mut first, _) = connect(&broker.addr, options("dsub", true, Some(60))).await;
    first
        .write_all(&subscribe_bytes(1, "slow/a", 1))
        .await
        .expect("SUBSCRIBE");
    sleep(Duration::from_millis(100)).await;
    let (mut second, _) = connect(&broker.addr, options("dsub", true, Some(60))).await;
    sleep(Duration::from_millis(1000)).await;
    let routed = broker.router.has_subscription("dsub", "slow/a").await;
    publish(&mut publisher, "slow/a").await;
    let delivered = received_publish(&mut second).await;
    drop(first);
    broker.stop().await;
    assert!(
        !routed,
        "a displaced connection's SUBSCRIBE installed a route for the successor"
    );
    assert!(!delivered);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn displaced_unsubscribe_keeps_the_successors_route() {
    let broker = Broker::start(memory(NEVER), Duration::from_millis(600)).await;
    let (mut first, _) = connect(&broker.addr, options("dunsub", true, Some(60))).await;
    subscribe(&mut first, "keep/x", 1).await;
    let mut burst = subscribe_bytes(1, "slow/a", 1);
    burst.extend_from_slice(&unsubscribe_bytes("keep/x"));
    first
        .write_all(&burst)
        .await
        .expect("SUBSCRIBE and UNSUBSCRIBE");
    sleep(Duration::from_millis(100)).await;
    let (_second, connack) = connect(&broker.addr, options("dunsub", false, Some(60))).await;
    assert!(connack.session_present);
    sleep(Duration::from_millis(1000)).await;
    let kept = broker.router.has_subscription("dunsub", "keep/x").await;
    let stored = broker
        .storage()
        .get_session("dunsub")
        .await
        .expect("read")
        .expect("session");
    drop(first);
    broker.stop().await;
    assert!(
        kept,
        "a displaced connection's UNSUBSCRIBE removed the successor's route"
    );
    assert!(stored.subscriptions.contains_key("keep/x"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn displaced_unsubscribe_waiting_on_the_slot_is_fenced() {
    let broker = Broker::start(memory(NEVER), Duration::ZERO).await;
    let (mut first, _) = connect(&broker.addr, options("dunsub2", true, Some(60))).await;
    subscribe(&mut first, "keep/x", 1).await;
    let slot = broker.router.lock_session("dunsub2").await;
    let mut second = Wire::new(TcpStream::connect(&broker.addr).await.expect("connect tcp"));
    second
        .write_all(&connect_bytes(options("dunsub2", false, Some(60))))
        .await
        .expect("write CONNECT");
    sleep(Duration::from_millis(200)).await;
    first
        .write_all(&unsubscribe_bytes("keep/x"))
        .await
        .expect("UNSUBSCRIBE");
    sleep(Duration::from_millis(200)).await;
    drop(slot);
    let connack = read_connack(&mut second).await;
    assert!(connack.session_present);
    sleep(Duration::from_millis(300)).await;
    let kept = broker.router.has_subscription("dunsub2", "keep/x").await;
    let stored = broker
        .storage()
        .get_session("dunsub2")
        .await
        .expect("read")
        .expect("session");
    drop(first);
    broker.stop().await;
    assert!(
        kept,
        "an UNSUBSCRIBE queued behind the successor's claim removed the successor's route"
    );
    assert!(stored.subscriptions.contains_key("keep/x"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sweep_without_persistence_keeps_a_live_connections_routes() {
    let config = BrokerConfig::default().with_storage(StorageConfig {
        enable_persistence: false,
        cleanup_interval: Duration::from_millis(100),
        ..Default::default()
    });
    let broker = Broker::start(config, Duration::ZERO).await;
    let (mut stream, _) = connect(&broker.addr, options("np-live", true, Some(0))).await;
    subscribe(&mut stream, "np/t", 1).await;
    sleep(Duration::from_millis(600)).await;
    let kept = broker.router.has_subscription("np-live", "np/t").await;
    broker.stop().await;
    assert!(kept, "the sweep stripped a live connection's route");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claim_waiting_past_the_connect_timeout_still_completes() {
    let broker = Broker::start(memory(NEVER), Duration::ZERO).await;
    let slot = broker.router.lock_session("late").await;
    let mut stream = Wire::new(TcpStream::connect(&broker.addr).await.expect("connect tcp"));
    stream
        .write_all(&connect_bytes(options("late", true, Some(60))))
        .await
        .expect("write CONNECT");
    sleep(Duration::from_millis(10_500)).await;
    drop(slot);
    let connack = read_connack(&mut stream).await;
    assert_eq!(connack.reason_code, ReasonCode::Success);
    let token = broker
        .storage()
        .get_session("late")
        .await
        .expect("read")
        .expect("claimed session")
        .connection_token;
    let owner = broker.router.is_current_owner("late", token).await;
    let served = still_served(&mut stream).await;
    broker.stop().await;
    assert!(
        owner && served,
        "the claim must finish once the CONNECT was read"
    );
}

fn seeded(client_id: &str, expiry: u32, topic: Option<&str>) -> ClientSession {
    let mut session = ClientSession::new(client_id, true, Some(expiry));
    session.mark_connected(7);
    if let Some(topic) = topic {
        session.add_subscription(topic, StoredSubscription::new(mqtt5::QoS::AtLeastOnce));
    }
    session
}

async fn seed(dir: &Path, sessions: Vec<ClientSession>) {
    let backend = FileBackend::new(dir).await.expect("file backend");
    futures::future::join_all(
        sessions
            .into_iter()
            .map(|session| backend.store_session(session)),
    )
    .await
    .into_iter()
    .for_each(|stored| stored.expect("store"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_left_connected_by_a_previous_run_expires_after_boot() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed(dir.path(), vec![seeded("seeded", 1, None)]).await;
    let broker = Broker::start(file(dir.path(), NEVER), Duration::ZERO).await;
    sleep(Duration::from_millis(2500)).await;
    let (_stream, connack) = connect(&broker.addr, options("seeded", false, Some(60))).await;
    broker.stop().await;
    assert!(
        !connack.session_present,
        "a session connected when the broker stopped ends at boot"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expiry_zero_session_from_a_previous_run_is_dropped_at_boot() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed(dir.path(), vec![seeded("seeded0", 0, Some("s0/t"))]).await;
    let broker = Broker::start(file(dir.path(), NEVER), Duration::ZERO).await;
    let routes = broker.router.subscription_count_for_client("seeded0").await;
    let stored = broker
        .storage()
        .session_client_ids()
        .await
        .expect("list sessions");
    broker.stop().await;
    assert_eq!(routes, 0);
    assert!(stored.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_caps_stored_expiry_to_the_configured_maximum() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut long_ended = seeded("long-ended", 3600, Some("le/t"));
    long_ended.mark_disconnected(mqtt5::broker::storage::unix_millis_now() - 120_000);
    seed(
        dir.path(),
        vec![long_ended, seeded("capped", 3600, Some("c/t"))],
    )
    .await;
    let config = file(dir.path(), NEVER).with_session_expiry(Duration::from_secs(60));
    let broker = Broker::start(config, Duration::ZERO).await;
    let capped = broker
        .storage()
        .get_session("capped")
        .await
        .expect("read")
        .expect("capped session");
    let ended = broker
        .storage()
        .get_session("long-ended")
        .await
        .expect("read");
    let ended_routes = broker
        .router
        .subscription_count_for_client("long-ended")
        .await;
    broker.stop().await;
    assert_eq!(capped.expiry_interval, Some(60));
    assert!(
        ended.is_none() && ended_routes == 0,
        "a session that ended 120s ago has outlived the 60s maximum"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn boot_with_many_sessions_left_connected_accepts_connections_promptly() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed(
        dir.path(),
        (0..500)
            .map(|i| seeded(&format!("boot-{i}"), 3600, Some("boot/t")))
            .collect(),
    )
    .await;
    let started = std::time::Instant::now();
    let broker = Broker::start(file(dir.path(), NEVER), Duration::ZERO).await;
    let (_stream, connack) = connect(&broker.addr, options("first", true, Some(0))).await;
    let elapsed = started.elapsed();
    let routes = broker
        .router
        .subscription_count_for_client("boot-499")
        .await;
    broker.stop().await;
    assert_eq!(connack.reason_code, ReasonCode::Success);
    assert_eq!(routes, 1);
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "recovering 500 sessions delayed the first CONNACK by {elapsed:?}"
    );
}

async fn seed_unacked(dir: &Path, client_id: &str) {
    use mqtt5::broker::storage::{InflightDirection, InflightMessage, InflightPhase};
    let backend = FileBackend::new(dir).await.expect("file backend");
    let mut session = ClientSession::new(client_id, true, Some(3600));
    session.mark_disconnected(mqtt5::broker::storage::unix_millis_now());
    backend.store_session(session).await.expect("store session");
    let mut publish = mqtt5::packet::publish::PublishPacket::new(
        "unacked/t".to_string(),
        b"unacked".to_vec(),
        mqtt5::QoS::AtLeastOnce,
    );
    publish.packet_id = Some(7);
    backend
        .store_inflight_message(InflightMessage::from_publish(
            &publish,
            client_id.to_string(),
            InflightDirection::Outbound,
            InflightPhase::AwaitingPubrec,
        ))
        .await
        .expect("store inflight");
    backend.flush_queue_writes().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_after_restart_redelivers_the_persisted_inflight_message() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_unacked(dir.path(), "unacked").await;
    let broker = Broker::start(file(dir.path(), NEVER), Duration::ZERO).await;
    let (mut wire, connack) = connect(&broker.addr, options("unacked", false, Some(3600))).await;
    let redelivered = wire.next(Duration::from_secs(2)).await;
    broker.stop().await;
    assert!(connack.session_present);
    match redelivered {
        Some(Packet::Publish(publish)) => {
            assert!(publish.dup);
            assert_eq!(publish.topic_name, "unacked/t");
        }
        other => panic!("the persisted unacknowledged message was not redelivered: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clean_start_after_restart_drops_the_persisted_inflight_message() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_unacked(dir.path(), "unacked-clean").await;
    let broker = Broker::start(file(dir.path(), NEVER), Duration::ZERO).await;
    let (mut wire, connack) =
        connect(&broker.addr, options("unacked-clean", true, Some(3600))).await;
    let delivered = wire.next(Duration::from_millis(500)).await;
    broker.stop().await;
    assert!(!connack.session_present);
    assert!(
        delivered.is_none(),
        "a clean start delivered the old session's message: {delivered:?}"
    );
}
