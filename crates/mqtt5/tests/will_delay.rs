#![cfg(feature = "broker")]
mod common;

use common::{MessageCollector, TestBroker};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::server::MqttBroker;
use mqtt5::broker::storage::{DynamicStorage, StorageBackend as _};
use mqtt5::time::Duration;
use mqtt5::MqttClient;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::disconnect::DisconnectPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use mqtt5_protocol::types::{ConnectOptions, WillMessage};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{sleep, Instant};

const SESSION_EXPIRY: u32 = 60;

fn will_topic(client_id: &str) -> String {
    format!("will/{client_id}")
}

fn delayed_will(client_id: &str, delay: u32) -> WillMessage {
    let mut will = WillMessage::new(will_topic(client_id), "offline");
    will.properties.will_delay_interval = Some(delay);
    will
}

async fn connect_with_options(addr: &str, options: ConnectOptions) -> (TcpStream, bool) {
    let mut stream = TcpStream::connect(addr).await.expect("connect tcp");
    let mut buf = Vec::new();
    ConnectPacket::new(options)
        .encode(&mut buf)
        .expect("encode CONNECT");
    stream.write_all(&buf).await.expect("write CONNECT");

    let mut connack = [0u8; 64];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut connack))
        .await
        .expect("CONNACK timed out")
        .expect("read CONNACK");
    assert!(read >= 4, "broker closed before CONNACK");
    assert_eq!(connack[0], 0x20, "expected CONNACK");
    assert_eq!(connack[3], 0x00, "CONNACK reason must be Success");
    (stream, connack[2] & 0x01 == 0x01)
}

fn connect_options(
    client_id: &str,
    clean_start: bool,
    session_expiry: Option<u32>,
    will: Option<WillMessage>,
) -> ConnectOptions {
    let options = ConnectOptions::new(client_id).with_clean_start(clean_start);
    let options = match session_expiry {
        Some(expiry) => options.with_session_expiry_interval(expiry),
        None => options,
    };
    match will {
        Some(will) => options.with_will(will),
        None => options,
    }
}

fn broker_addr(broker: &TestBroker) -> &str {
    broker.address().trim_start_matches("mqtt://")
}

async fn raw_connect(
    broker: &TestBroker,
    client_id: &str,
    clean_start: bool,
    session_expiry: u32,
    will: Option<WillMessage>,
) -> TcpStream {
    let options = connect_options(client_id, clean_start, Some(session_expiry), will);
    connect_with_options(broker_addr(broker), options).await.0
}

async fn send_disconnect(stream: TcpStream, reason: ReasonCode) {
    send_disconnect_packet(stream, DisconnectPacket::new(reason)).await;
}

async fn send_disconnect_packet(mut stream: TcpStream, disconnect: DisconnectPacket) {
    let mut buf = Vec::new();
    disconnect.encode(&mut buf).expect("encode DISCONNECT");
    stream.write_all(&buf).await.expect("write DISCONNECT");
    stream.flush().await.expect("flush DISCONNECT");
    let mut drain = [0u8; 16];
    let closed = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut drain)).await;
    assert!(
        matches!(closed, Ok(Ok(0))),
        "broker must close after DISCONNECT"
    );
}

fn disconnect_with_expiry(reason: ReasonCode, session_expiry: u32) -> DisconnectPacket {
    let mut disconnect = DisconnectPacket::new(reason);
    disconnect
        .properties
        .set_session_expiry_interval(session_expiry);
    disconnect
}

async fn watch_will(broker: &TestBroker, client_id: &str) -> (MqttClient, MessageCollector) {
    watch_will_at(broker.address(), client_id).await
}

async fn watch_will_at(address: &str, client_id: &str) -> (MqttClient, MessageCollector) {
    let watcher = MqttClient::new(format!("{client_id}-watcher"));
    watcher.connect(address).await.expect("watcher connect");
    let collector = MessageCollector::new();
    watcher
        .subscribe(&will_topic(client_id), collector.callback())
        .await
        .expect("watcher subscribe");
    sleep(Duration::from_millis(100)).await;
    (watcher, collector)
}

async fn assert_no_will_until(collector: &MessageCollector, deadline: Instant, context: &str) {
    sleep(deadline.saturating_duration_since(Instant::now())).await;
    assert_eq!(collector.count().await, 0, "{context}");
}

async fn reconnect_within_delay_cancels_will(clean_start: bool) {
    let broker = TestBroker::start().await;
    let client_id = format!("wd-reconnect-{clean_start}");
    let (watcher, collector) = watch_will(&broker, &client_id).await;

    let first = raw_connect(
        &broker,
        &client_id,
        true,
        SESSION_EXPIRY,
        Some(delayed_will(&client_id, 2)),
    )
    .await;
    let dropped_at = Instant::now();
    drop(first);
    sleep(Duration::from_millis(300)).await;

    let second = raw_connect(&broker, &client_id, clean_start, SESSION_EXPIRY, None).await;

    assert_no_will_until(
        &collector,
        dropped_at + Duration::from_secs(4),
        "a reconnect within the Will Delay Interval must cancel the Will",
    )
    .await;

    send_disconnect(second, ReasonCode::Success).await;
    watcher.disconnect().await.expect("watcher disconnect");
}

#[tokio::test]
async fn resume_within_delay_cancels_will() {
    Box::pin(reconnect_within_delay_cancels_will(false)).await;
}

#[tokio::test]
async fn clean_start_within_delay_cancels_will() {
    Box::pin(reconnect_within_delay_cancels_will(true)).await;
}

#[tokio::test]
async fn will_published_after_delay_without_reconnect() {
    let broker = TestBroker::start().await;
    let client_id = "wd-no-reconnect";
    let (watcher, collector) = watch_will(&broker, client_id).await;

    let conn = raw_connect(
        &broker,
        client_id,
        true,
        SESSION_EXPIRY,
        Some(delayed_will(client_id, 2)),
    )
    .await;
    let dropped_at = Instant::now();
    drop(conn);

    assert_no_will_until(
        &collector,
        dropped_at + Duration::from_millis(1500),
        "the Will must not be published before the Will Delay Interval",
    )
    .await;
    assert!(
        collector.wait_for_messages(1, Duration::from_secs(3)).await,
        "the Will must be published once the Will Delay Interval elapses"
    );
    sleep(Duration::from_millis(500)).await;
    assert_eq!(collector.count().await, 1, "the Will is published once");

    watcher.disconnect().await.expect("watcher disconnect");
}

#[tokio::test]
async fn session_expiry_zero_publishes_will_immediately() {
    let broker = TestBroker::start().await;
    let client_id = "wd-expiry-zero";
    let (watcher, collector) = watch_will(&broker, client_id).await;

    let conn = raw_connect(
        &broker,
        client_id,
        true,
        0,
        Some(delayed_will(client_id, 5)),
    )
    .await;
    drop(conn);

    assert!(
        collector
            .wait_for_messages(1, Duration::from_millis(1500))
            .await,
        "the Session ends at disconnect, so the Will must be published without waiting for the delay"
    );

    watcher.disconnect().await.expect("watcher disconnect");
}

#[tokio::test]
async fn session_expiry_shorter_than_delay_publishes_at_session_end() {
    let broker = TestBroker::start().await;
    let client_id = "wd-expiry-short";
    let (watcher, collector) = watch_will(&broker, client_id).await;

    let conn = raw_connect(
        &broker,
        client_id,
        true,
        2,
        Some(delayed_will(client_id, 10)),
    )
    .await;
    let dropped_at = Instant::now();
    drop(conn);

    assert_no_will_until(
        &collector,
        dropped_at + Duration::from_millis(1500),
        "the Will must not be published before the Session ends",
    )
    .await;
    assert!(
        collector.wait_for_messages(1, Duration::from_secs(3)).await,
        "the Will must be published when the Session expires, before the Will Delay Interval"
    );

    watcher.disconnect().await.expect("watcher disconnect");
}

#[tokio::test]
async fn reconnect_then_drop_again_publishes_one_will_after_second_delay() {
    let broker = TestBroker::start().await;
    let client_id = "wd-redrop";
    let (watcher, collector) = watch_will(&broker, client_id).await;

    let first = raw_connect(
        &broker,
        client_id,
        true,
        SESSION_EXPIRY,
        Some(delayed_will(client_id, 2)),
    )
    .await;
    let first_dropped_at = Instant::now();
    drop(first);
    sleep(Duration::from_millis(300)).await;

    let second = raw_connect(
        &broker,
        client_id,
        false,
        SESSION_EXPIRY,
        Some(delayed_will(client_id, 2)),
    )
    .await;
    sleep(Duration::from_millis(500)).await;
    let second_dropped_at = Instant::now();
    drop(second);

    assert_no_will_until(
        &collector,
        first_dropped_at + Duration::from_millis(2400),
        "the first connection's Will was cancelled by the reconnect",
    )
    .await;
    assert_no_will_until(
        &collector,
        second_dropped_at + Duration::from_millis(1500),
        "the second Will must wait for its own delay",
    )
    .await;
    assert!(
        collector.wait_for_messages(1, Duration::from_secs(3)).await,
        "the second connection's Will must be published after its delay"
    );
    sleep(Duration::from_millis(700)).await;
    assert_eq!(collector.count().await, 1, "exactly one Will is published");

    watcher.disconnect().await.expect("watcher disconnect");
}

#[tokio::test]
async fn normal_disconnect_discards_will() {
    let broker = TestBroker::start().await;
    let client_id = "wd-normal";
    let (watcher, collector) = watch_will(&broker, client_id).await;

    let conn = raw_connect(
        &broker,
        client_id,
        true,
        SESSION_EXPIRY,
        Some(delayed_will(client_id, 1)),
    )
    .await;
    send_disconnect(conn, ReasonCode::Success).await;

    assert_no_will_until(
        &collector,
        Instant::now() + Duration::from_millis(2500),
        "DISCONNECT 0x00 must delete the Will",
    )
    .await;

    watcher.disconnect().await.expect("watcher disconnect");
}

#[tokio::test]
async fn disconnect_with_will_message_follows_delay() {
    let broker = TestBroker::start().await;
    let client_id = "wd-disconnect-with-will";
    let (watcher, collector) = watch_will(&broker, client_id).await;

    let conn = raw_connect(
        &broker,
        client_id,
        true,
        SESSION_EXPIRY,
        Some(delayed_will(client_id, 2)),
    )
    .await;
    let disconnected_at = Instant::now();
    send_disconnect(conn, ReasonCode::DisconnectWithWillMessage).await;

    assert_no_will_until(
        &collector,
        disconnected_at + Duration::from_millis(1500),
        "DISCONNECT 0x04 must still honour the Will Delay Interval",
    )
    .await;
    assert!(
        collector.wait_for_messages(1, Duration::from_secs(3)).await,
        "DISCONNECT 0x04 must publish the Will after the delay"
    );

    watcher.disconnect().await.expect("watcher disconnect");
}

#[tokio::test]
async fn takeover_cancels_delayed_will() {
    let broker = TestBroker::start().await;
    let client_id = "wd-takeover-delayed";
    let (watcher, collector) = watch_will(&broker, client_id).await;

    let first = raw_connect(
        &broker,
        client_id,
        true,
        SESSION_EXPIRY,
        Some(delayed_will(client_id, 2)),
    )
    .await;
    let taken_over_at = Instant::now();
    let second = raw_connect(&broker, client_id, false, SESSION_EXPIRY, None).await;
    drop(first);

    assert_no_will_until(
        &collector,
        taken_over_at + Duration::from_secs(4),
        "the taking-over connection opened before the Will Delay Interval elapsed",
    )
    .await;

    send_disconnect(second, ReasonCode::Success).await;
    watcher.disconnect().await.expect("watcher disconnect");
}

#[tokio::test]
async fn takeover_publishes_undelayed_will() {
    let broker = TestBroker::start().await;
    let client_id = "wd-takeover-immediate";
    let (watcher, collector) = watch_will(&broker, client_id).await;

    let first = raw_connect(
        &broker,
        client_id,
        true,
        SESSION_EXPIRY,
        Some(delayed_will(client_id, 0)),
    )
    .await;
    let second = raw_connect(&broker, client_id, false, SESSION_EXPIRY, None).await;

    assert!(
        collector
            .wait_for_messages(1, Duration::from_millis(1500))
            .await,
        "the displaced connection's Will without a delay must be published on takeover"
    );

    drop(first);
    send_disconnect(second, ReasonCode::Success).await;
    watcher.disconnect().await.expect("watcher disconnect");
}

#[tokio::test]
async fn resume_with_longer_expiry_keeps_full_will_delay() {
    let broker = TestBroker::start().await;
    let client_id = "wd-resume-longer";
    let (watcher, collector) = watch_will(&broker, client_id).await;

    let first = raw_connect(&broker, client_id, true, 2, None).await;
    send_disconnect(first, ReasonCode::Success).await;
    sleep(Duration::from_millis(300)).await;

    let second = raw_connect(
        &broker,
        client_id,
        false,
        SESSION_EXPIRY,
        Some(delayed_will(client_id, 5)),
    )
    .await;
    let dropped_at = Instant::now();
    drop(second);

    assert_no_will_until(
        &collector,
        dropped_at + Duration::from_millis(3500),
        "the resuming CONNECT set Session Expiry 60, so the 5s Will Delay must be honoured",
    )
    .await;
    watcher.disconnect().await.expect("watcher disconnect");
}

#[tokio::test]
async fn resume_with_expiry_zero_publishes_will_immediately() {
    let broker = TestBroker::start().await;
    let client_id = "wd-resume-zero";
    let (watcher, collector) = watch_will(&broker, client_id).await;

    let first = raw_connect(&broker, client_id, true, SESSION_EXPIRY, None).await;
    send_disconnect(first, ReasonCode::Success).await;
    sleep(Duration::from_millis(200)).await;

    let second = raw_connect(
        &broker,
        client_id,
        false,
        0,
        Some(delayed_will(client_id, 5)),
    )
    .await;
    drop(second);

    assert!(
        collector
            .wait_for_messages(1, Duration::from_millis(1500))
            .await,
        "the resuming CONNECT set Session Expiry 0, so the session ends at close and the Will must not wait for the delay"
    );
    watcher.disconnect().await.expect("watcher disconnect");
}

#[tokio::test]
async fn absent_session_expiry_publishes_will_immediately() {
    let broker = TestBroker::start().await;
    let client_id = "wd-absent-expiry";
    let (watcher, collector) = watch_will(&broker, client_id).await;

    let options = connect_options(client_id, true, None, Some(delayed_will(client_id, 3)));
    let (stream, _) = connect_with_options(broker_addr(&broker), options).await;
    drop(stream);

    assert!(
        collector
            .wait_for_messages(1, Duration::from_millis(1500))
            .await,
        "an absent Session Expiry Interval means 0: the session ends at disconnect so the Will must be published immediately"
    );
    watcher.disconnect().await.expect("watcher disconnect");
}

#[tokio::test]
async fn absent_session_expiry_session_is_not_resumed() {
    let broker = TestBroker::start().await;
    let client_id = "wd-absent-resume";

    let (first, _) = connect_with_options(
        broker_addr(&broker),
        connect_options(client_id, true, None, None),
    )
    .await;
    send_disconnect(first, ReasonCode::Success).await;
    sleep(Duration::from_millis(200)).await;

    let (second, session_present) = connect_with_options(
        broker_addr(&broker),
        connect_options(client_id, false, Some(SESSION_EXPIRY), None),
    )
    .await;
    assert!(
        !session_present,
        "an absent Session Expiry Interval means 0, so no session survives the disconnect"
    );
    send_disconnect(second, ReasonCode::Success).await;
}

#[tokio::test]
async fn disconnect_session_expiry_zero_ends_session_and_releases_delayed_will() {
    let broker = TestBroker::start().await;
    let client_id = "wd-disconnect-expiry-zero";
    let (watcher, collector) = watch_will(&broker, client_id).await;

    let conn = raw_connect(
        &broker,
        client_id,
        true,
        SESSION_EXPIRY,
        Some(delayed_will(client_id, 5)),
    )
    .await;
    send_disconnect_packet(
        conn,
        disconnect_with_expiry(ReasonCode::DisconnectWithWillMessage, 0),
    )
    .await;

    assert!(
        collector
            .wait_for_messages(1, Duration::from_millis(1500))
            .await,
        "Session Expiry 0 on DISCONNECT ends the session now, so the Will must not wait for the delay"
    );

    let (second, session_present) = connect_with_options(
        broker_addr(&broker),
        connect_options(client_id, false, Some(SESSION_EXPIRY), None),
    )
    .await;
    assert!(
        !session_present,
        "Session Expiry 0 on DISCONNECT must end the session"
    );
    send_disconnect(second, ReasonCode::Success).await;
    watcher.disconnect().await.expect("watcher disconnect");
}

#[tokio::test]
async fn disconnect_session_expiry_extends_session() {
    let broker = TestBroker::start().await;
    let client_id = "wd-disconnect-expiry-extend";

    let first = raw_connect(&broker, client_id, true, 1, None).await;
    send_disconnect_packet(
        first,
        disconnect_with_expiry(ReasonCode::Success, SESSION_EXPIRY),
    )
    .await;
    sleep(Duration::from_millis(2500)).await;

    let (second, session_present) = connect_with_options(
        broker_addr(&broker),
        connect_options(client_id, false, Some(SESSION_EXPIRY), None),
    )
    .await;
    assert!(
        session_present,
        "the DISCONNECT Session Expiry of 60s replaces the CONNECT value of 1s"
    );
    send_disconnect(second, ReasonCode::Success).await;
}

#[tokio::test]
async fn disconnect_session_expiry_after_zero_is_protocol_error() {
    let broker = TestBroker::start().await;
    let client_id = "wd-disconnect-expiry-error";
    let (watcher, collector) = watch_will(&broker, client_id).await;

    let mut conn = raw_connect(
        &broker,
        client_id,
        true,
        0,
        Some(delayed_will(client_id, 0)),
    )
    .await;
    let mut buf = Vec::new();
    disconnect_with_expiry(ReasonCode::Success, SESSION_EXPIRY)
        .encode(&mut buf)
        .expect("encode DISCONNECT");
    conn.write_all(&buf).await.expect("write DISCONNECT");

    let mut reply = [0u8; 64];
    let read = tokio::time::timeout(Duration::from_secs(5), conn.read(&mut reply))
        .await
        .expect("no reply to the invalid DISCONNECT")
        .expect("read reply");
    assert!(read >= 3, "the server must answer with DISCONNECT");
    assert_eq!(reply[0], 0xE0, "the server must send DISCONNECT");
    assert_eq!(
        reply[2], 0x82,
        "a non-zero Session Expiry on DISCONNECT after CONNECT sent 0 is a Protocol Error"
    );
    let mut drain = [0u8; 16];
    let closed = tokio::time::timeout(Duration::from_secs(5), conn.read(&mut drain)).await;
    assert!(
        matches!(closed, Ok(Ok(0))),
        "the server must close the connection"
    );

    assert!(
        collector
            .wait_for_messages(1, Duration::from_millis(1500))
            .await,
        "an invalid DISCONNECT is not a normal disconnection, so the Will is published"
    );

    let (second, session_present) = connect_with_options(
        broker_addr(&broker),
        connect_options(client_id, false, Some(SESSION_EXPIRY), None),
    )
    .await;
    assert!(
        !session_present,
        "the session still ends because its Session Expiry stays 0"
    );
    send_disconnect(second, ReasonCode::Success).await;
    watcher.disconnect().await.expect("watcher disconnect");
}

struct StoredBroker {
    address: String,
    storage: Arc<DynamicStorage>,
    handle: tokio::task::JoinHandle<()>,
}

impl StoredBroker {
    async fn start() -> Self {
        let storage_config = StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        };
        let config = BrokerConfig::default()
            .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("bind address"))
            .with_storage(storage_config);
        let mut broker = MqttBroker::with_config(config).await.expect("start broker");
        let address = broker.local_addr().expect("broker address").to_string();
        let storage = broker.storage().expect("persistence is enabled");
        let handle = tokio::spawn(async move {
            if let Err(e) = broker.run().await {
                tracing::debug!("broker stopped: {e}");
            }
        });
        sleep(Duration::from_millis(100)).await;
        Self {
            address,
            storage,
            handle,
        }
    }

    async fn stored_will_present(&self, client_id: &str) -> bool {
        self.storage
            .get_session(client_id)
            .await
            .expect("read session")
            .expect("the session outlives the connection")
            .will_message
            .is_some()
    }
}

impl Drop for StoredBroker {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

#[tokio::test]
async fn published_will_is_removed_from_stored_session() {
    let broker = StoredBroker::start().await;
    let client_id = "wd-stored-published";
    let (watcher, collector) =
        watch_will_at(&format!("mqtt://{}", broker.address), client_id).await;

    let (conn, _) = connect_with_options(
        &broker.address,
        connect_options(
            client_id,
            true,
            Some(SESSION_EXPIRY),
            Some(delayed_will(client_id, 0)),
        ),
    )
    .await;
    assert!(broker.stored_will_present(client_id).await);
    drop(conn);

    assert!(
        collector
            .wait_for_messages(1, Duration::from_millis(1500))
            .await,
        "the Will is published at disconnect"
    );
    sleep(Duration::from_millis(100)).await;
    assert!(
        !broker.stored_will_present(client_id).await,
        "a published Will must be removed from the stored session"
    );
    watcher.disconnect().await.expect("watcher disconnect");
}

#[tokio::test]
async fn normal_disconnect_removes_stored_will() {
    let broker = StoredBroker::start().await;
    let client_id = "wd-stored-normal";

    let (conn, _) = connect_with_options(
        &broker.address,
        connect_options(
            client_id,
            true,
            Some(SESSION_EXPIRY),
            Some(delayed_will(client_id, 0)),
        ),
    )
    .await;
    assert!(broker.stored_will_present(client_id).await);
    send_disconnect(conn, ReasonCode::Success).await;
    sleep(Duration::from_millis(200)).await;

    assert!(
        !broker.stored_will_present(client_id).await,
        "DISCONNECT 0x00 must remove the Will from the stored session"
    );
}
