use crate::broker::config::{BrokerConfig, StorageBackend as BackendKind, StorageConfig};
use crate::broker::router::MessageRouter;
use crate::broker::server::MqttBroker;
use crate::broker::storage::{DynamicStorage, FileBackend, StorageBackend};
use crate::packet::connect::ConnectPacket;
use crate::packet::subscribe::SubscribePacket;
use crate::packet::unsubscribe::UnsubscribePacket;
use crate::packet::{MqttPacket, Packet};
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::time::Duration;
use crate::transport::packet_io::read_packet_from_stream;
use crate::types::ConnectOptions;
use crate::{QoS, WillMessage};
use bytes::BytesMut;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time::sleep;

struct Broker {
    addr: String,
    storage: Arc<DynamicStorage>,
    router: Arc<MessageRouter>,
    handle: tokio::task::JoinHandle<()>,
}

impl Broker {
    async fn start(dir: &Path, cleanup_interval: Duration) -> Self {
        let config = BrokerConfig::default()
            .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
            .with_storage(StorageConfig {
                backend: BackendKind::File,
                base_dir: dir.to_path_buf(),
                enable_persistence: true,
                cleanup_interval,
                ..Default::default()
            });
        let mut broker = MqttBroker::with_config(config).await.unwrap();
        let addr = broker.local_addr().unwrap().to_string();
        let storage = broker.storage().unwrap();
        let router = broker.router();
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
            handle,
        }
    }

    fn file_backend(&self) -> &FileBackend {
        let DynamicStorage::File(backend) = &*self.storage else {
            panic!("file storage expected");
        };
        backend
    }

    async fn break_next_session_write(&self) {
        self.file_backend().break_next_session_write().await;
    }

    async fn pause_session_writes(&self) -> impl Sized {
        self.file_backend().pause_session_writes().await
    }

    async fn token(&self, client_id: &str) -> u64 {
        self.storage
            .get_session(client_id)
            .await
            .unwrap()
            .unwrap()
            .connection_token
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

struct Conn {
    stream: TcpStream,
    buffer: BytesMut,
}

impl Conn {
    async fn send(&mut self, packet: &impl MqttPacket) {
        let mut bytes = Vec::new();
        packet.encode(&mut bytes).unwrap();
        self.stream.write_all(&bytes).await.unwrap();
    }

    async fn send_raw(&mut self, bytes: &[u8]) -> bool {
        self.stream.write_all(bytes).await.is_ok()
    }

    async fn next(&mut self, millis: u64) -> Option<Packet> {
        tokio::time::timeout(
            Duration::from_millis(millis),
            read_packet_from_stream(&mut self.stream, 5, &mut self.buffer, 1 << 20),
        )
        .await
        .ok()?
        .ok()
    }

    async fn closed(&mut self, millis: u64) -> bool {
        tokio::time::timeout(
            Duration::from_millis(millis),
            read_packet_from_stream(&mut self.stream, 5, &mut self.buffer, 1 << 20),
        )
        .await
        .is_ok_and(|read| read.is_err())
    }

    async fn replies(&mut self, millis: u64) -> Vec<Packet> {
        let mut replies = Vec::new();
        while let Some(packet) = self.next(millis).await {
            replies.push(packet);
        }
        replies
    }

    async fn subscribed(&mut self, topic: &str) {
        self.send(&SubscribePacket::new(1).add_filter(topic, QoS::AtLeastOnce))
            .await;
        assert!(matches!(self.next(3000).await, Some(Packet::SubAck(_))));
    }

    async fn still_served(&mut self) -> bool {
        self.send_raw(&[0xC0, 0x00]).await
            && matches!(self.next(1000).await, Some(Packet::PingResp))
    }
}

async fn connect_with(addr: &str, options: ConnectOptions) -> (Conn, Option<Packet>) {
    let mut conn = Conn {
        stream: TcpStream::connect(addr).await.unwrap(),
        buffer: BytesMut::new(),
    };
    conn.send(&ConnectPacket::new(options.protocol_options))
        .await;
    let connack = conn.next(3000).await;
    (conn, connack)
}

async fn open(
    addr: &str,
    client_id: &str,
    clean_start: bool,
    expiry: u32,
) -> (Conn, Option<Packet>) {
    connect_with(
        addr,
        ConnectOptions::new(client_id)
            .with_clean_start(clean_start)
            .with_session_expiry_interval(expiry),
    )
    .await
}

fn connack_reason(packet: Option<&Packet>) -> Option<ReasonCode> {
    match packet {
        Some(Packet::ConnAck(connack)) => Some(connack.reason_code),
        _ => None,
    }
}

async fn publish(conn: &mut Conn, topic: &str) {
    let topic_len = u8::try_from(topic.len()).unwrap();
    let mut bytes = vec![0x30, 5 + topic_len, 0x00, topic_len];
    bytes.extend_from_slice(topic.as_bytes());
    bytes.extend_from_slice(&[0x00, b'h', b'i']);
    assert!(conn.send_raw(&bytes).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_claim_write_leaves_the_live_owner_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let broker = Broker::start(dir.path(), Duration::from_secs(3600)).await;
    let (mut owner, connack) = open(&broker.addr, "held", true, 60).await;
    assert_eq!(connack_reason(connack.as_ref()), Some(ReasonCode::Success));
    owner.subscribed("held/t").await;
    let owner_token = broker.token("held").await;

    broker.break_next_session_write().await;
    let (mut intruder, reply) = open(&broker.addr, "held", false, 60).await;
    assert_eq!(
        connack_reason(reply.as_ref()),
        Some(ReasonCode::ServerUnavailable),
        "the failed claim is answered with a CONNACK refusing it as Server unavailable"
    );
    assert!(intruder.next(500).await.is_none());

    assert!(
        owner.still_served().await,
        "a claim whose write failed must not displace the live owner"
    );
    assert!(broker.router.is_current_owner("held", owner_token).await);
    assert_eq!(broker.token("held").await, owner_token);
    assert!(broker.router.has_subscription("held", "held/t").await);

    let (mut publisher, _) = open(&broker.addr, "held-pub", true, 0).await;
    publish(&mut publisher, "held/t").await;
    assert!(matches!(owner.next(1000).await, Some(Packet::Publish(_))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_subscribe_write_is_not_acknowledged_and_installs_no_route() {
    let dir = tempfile::tempdir().unwrap();
    let broker = Broker::start(dir.path(), Duration::from_secs(3600)).await;
    let (mut client, _) = open(&broker.addr, "subfail", true, 60).await;
    client.subscribed("kept/t").await;

    broker.break_next_session_write().await;
    client
        .send(
            &SubscribePacket::new(2)
                .add_filter("lost/a", QoS::AtLeastOnce)
                .add_filter("lost/b", QoS::AtLeastOnce)
                .add_filter("kept/t", QoS::AtMostOnce),
        )
        .await;
    let replies = client.replies(1000).await;
    assert!(
        !replies
            .iter()
            .any(|packet| matches!(packet, Packet::SubAck(_))),
        "a SUBSCRIBE whose write failed was acknowledged: {replies:?}"
    );
    for lost in ["lost/a", "lost/b"] {
        assert!(!broker.router.has_subscription("subfail", lost).await);
    }
    assert!(broker.router.has_subscription("subfail", "kept/t").await);
    drop(client);
    sleep(Duration::from_millis(200)).await;

    let stored = broker
        .storage
        .get_session("subfail")
        .await
        .unwrap()
        .unwrap();
    assert!(!stored.subscriptions.contains_key("lost/a"));
    assert!(!stored.subscriptions.contains_key("lost/b"));
    assert_eq!(
        stored.subscriptions.get("kept/t").map(|sub| sub.qos),
        Some(QoS::AtLeastOnce),
        "the failed SUBSCRIBE changed a stored subscription"
    );
    assert!(!broker.router.has_subscription("subfail", "lost/a").await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_unsubscribe_write_is_not_acknowledged_and_keeps_the_route() {
    let dir = tempfile::tempdir().unwrap();
    let broker = Broker::start(dir.path(), Duration::from_secs(3600)).await;
    let (mut client, _) = open(&broker.addr, "unsubfail", true, 60).await;
    client.subscribed("kept/u").await;

    broker.break_next_session_write().await;
    client
        .send(&UnsubscribePacket::new(3).add_filter("kept/u"))
        .await;
    let replies = client.replies(1000).await;
    assert!(
        !replies
            .iter()
            .any(|packet| matches!(packet, Packet::UnsubAck(_))),
        "an UNSUBSCRIBE whose write failed was acknowledged: {replies:?}"
    );
    assert!(broker.router.has_subscription("unsubfail", "kept/u").await);
    drop(client);
    sleep(Duration::from_millis(200)).await;
    let stored = broker
        .storage
        .get_session("unsubfail")
        .await
        .unwrap()
        .unwrap();
    assert!(stored.subscriptions.contains_key("kept/u"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_session_write_sends_disconnect_and_publishes_the_will() {
    let dir = tempfile::tempdir().unwrap();
    let broker = Broker::start(dir.path(), Duration::from_secs(3600)).await;
    let (mut watcher, _) = open(&broker.addr, "will-watcher", true, 0).await;
    watcher.subscribed("wills/failed").await;
    let (mut client, _) = connect_with(
        &broker.addr,
        ConnectOptions::new("will-failed")
            .with_clean_start(true)
            .with_session_expiry_interval(60)
            .with_will(WillMessage::new("wills/failed", "gone")),
    )
    .await;

    broker.break_next_session_write().await;
    client
        .send(&SubscribePacket::new(4).add_filter("never/t", QoS::AtMostOnce))
        .await;
    match client.next(2000).await {
        Some(Packet::Disconnect(disconnect)) => {
            assert_eq!(disconnect.reason_code, ReasonCode::UnspecifiedError);
        }
        other => panic!("expected DISCONNECT before the close, got {other:?}"),
    }
    assert!(client.closed(2000).await);
    assert!(
        matches!(watcher.next(2000).await, Some(Packet::Publish(will)) if will.topic_name == "wills/failed"),
        "a server-initiated close for an error must publish the Will"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscribe_with_many_filters_is_one_session_write() {
    let dir = tempfile::tempdir().unwrap();
    let broker = Broker::start(dir.path(), Duration::from_secs(3600)).await;
    let (mut client, _) = open(&broker.addr, "many", true, 60).await;
    let before = broker.file_backend().session_flushes().await;
    let subscribe = (0..20).fold(SubscribePacket::new(5), |packet, i| {
        packet.add_filter(format!("many/{i}"), QoS::AtLeastOnce)
    });
    client.send(&subscribe).await;
    match client.next(3000).await {
        Some(Packet::SubAck(suback)) => assert_eq!(suback.reason_codes.len(), 20),
        other => panic!("expected SUBACK, got {other:?}"),
    }
    assert_eq!(
        broker.file_backend().session_flushes().await - before,
        1,
        "every filter of one SUBSCRIBE must be stored in one write"
    );
    let stored = broker.storage.get_session("many").await.unwrap().unwrap();
    assert_eq!(stored.subscriptions.len(), 20);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unsubscribe_with_many_filters_is_one_session_write() {
    let dir = tempfile::tempdir().unwrap();
    let broker = Broker::start(dir.path(), Duration::from_secs(3600)).await;
    let (mut client, _) = open(&broker.addr, "many-un", true, 60).await;
    let subscribe = (0..20).fold(SubscribePacket::new(6), |packet, i| {
        packet.add_filter(format!("many/{i}"), QoS::AtLeastOnce)
    });
    client.send(&subscribe).await;
    assert!(matches!(client.next(3000).await, Some(Packet::SubAck(_))));
    let before = broker.file_backend().session_flushes().await;
    let unsubscribe = (0..20).fold(UnsubscribePacket::new(7), |packet, i| {
        packet.add_filter(format!("many/{i}"))
    });
    client.send(&unsubscribe).await;
    assert!(matches!(client.next(3000).await, Some(Packet::UnsubAck(_))));
    assert_eq!(broker.file_backend().session_flushes().await - before, 1);
    let stored = broker
        .storage
        .get_session("many-un")
        .await
        .unwrap()
        .unwrap();
    assert!(stored.subscriptions.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_left_connected_by_a_failed_release_write_still_expires() {
    let dir = tempfile::tempdir().unwrap();
    let broker = Broker::start(dir.path(), Duration::from_millis(200)).await;
    let (mut client, _) = open(&broker.addr, "stranded", true, 1).await;
    client.subscribed("stranded/t").await;

    broker.break_next_session_write().await;
    assert!(client.send_raw(&[0xE0, 0x00]).await);
    assert!(client.next(2000).await.is_none());
    sleep(Duration::from_millis(3000)).await;

    assert!(
        broker
            .storage
            .get_session("stranded")
            .await
            .unwrap()
            .is_none(),
        "a session whose release write failed must still end and expire"
    );
    assert_eq!(
        broker
            .router
            .subscription_count_for_client("stranded")
            .await,
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connack_waits_for_the_flush_that_covers_the_claim() {
    let dir = tempfile::tempdir().unwrap();
    let broker = Broker::start(dir.path(), Duration::from_secs(3600)).await;
    let paused = broker.pause_session_writes().await;
    let mut conn = Conn {
        stream: TcpStream::connect(&broker.addr).await.unwrap(),
        buffer: BytesMut::new(),
    };
    conn.send(&ConnectPacket::new(
        ConnectOptions::new("covered")
            .with_clean_start(true)
            .with_session_expiry_interval(60)
            .protocol_options,
    ))
    .await;
    assert!(
        conn.next(500).await.is_none(),
        "CONNACK was sent before the claim was durable"
    );
    drop(paused);
    assert_eq!(
        connack_reason(conn.next(3000).await.as_ref()),
        Some(ReasonCode::Success)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn suback_waits_for_the_flush_that_covers_the_subscription() {
    let dir = tempfile::tempdir().unwrap();
    let broker = Broker::start(dir.path(), Duration::from_secs(3600)).await;
    let (mut client, _) = open(&broker.addr, "sub-covered", true, 60).await;
    let paused = broker.pause_session_writes().await;
    client
        .send(&SubscribePacket::new(8).add_filter("covered/t", QoS::AtLeastOnce))
        .await;
    assert!(
        client.next(500).await.is_none(),
        "SUBACK was sent before the subscription was durable"
    );
    drop(paused);
    assert!(matches!(client.next(3000).await, Some(Packet::SubAck(_))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disconnect_completes_only_after_its_writes_are_durable() {
    let dir = tempfile::tempdir().unwrap();
    let broker = Broker::start(dir.path(), Duration::from_secs(3600)).await;
    let (mut client, _) = open(&broker.addr, "disc-covered", true, 60).await;
    let paused = broker.pause_session_writes().await;
    assert!(
        client
            .send_raw(&[0xE0, 0x07, 0x00, 0x05, 0x11, 0x00, 0x00, 0x00, 0x00])
            .await
    );
    assert!(
        !client.closed(500).await,
        "DISCONNECT processing completed before its writes were durable"
    );
    drop(paused);
    assert!(client.closed(3000).await);
    assert!(broker
        .storage
        .session_client_ids()
        .await
        .unwrap()
        .is_empty());
}
