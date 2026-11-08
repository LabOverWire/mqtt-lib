#![cfg(feature = "broker")]

use bytes::BytesMut;
use mqtt5::broker::auth::{AuthProvider, AuthResult};
use mqtt5::broker::config::{BrokerConfig, StorageBackend as BackendKind, StorageConfig};
use mqtt5::broker::server::MqttBroker;
use mqtt5::broker::storage::{DynamicStorage, StorageBackend};
use mqtt5::error::Result;
use mqtt5::time::Duration;
use mqtt5::transport::packet_io::read_packet_from_stream;
use mqtt5_protocol::packet::connack::ConnAckPacket;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::{MqttPacket, Packet};
use mqtt5_protocol::types::ConnectOptions;
use mqtt5_protocol::QoS;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time::sleep;

const NEVER: Duration = Duration::from_secs(3600);

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
    router: Arc<mqtt5::broker::router::MessageRouter>,
    storage: Option<Arc<DynamicStorage>>,
    provider: Arc<Provider>,
}

impl Broker {
    async fn start(config: BrokerConfig, slow_subscribe: Duration) -> Self {
        let config = config.with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap());
        let provider = Arc::new(Provider {
            slow_subscribe,
            deny_secret: AtomicBool::new(false),
        });
        let mut broker = MqttBroker::with_config(config)
            .await
            .unwrap()
            .with_auth_provider(Arc::clone(&provider) as Arc<dyn AuthProvider>);
        let addr = broker.local_addr().unwrap().to_string();
        let router = broker.router();
        let storage = broker.storage();
        tokio::spawn(async move {
            let _ = broker.run().await;
        });
        sleep(Duration::from_millis(100)).await;
        Self {
            addr,
            router,
            storage,
            provider,
        }
    }

    async fn instance(&self, client_id: &str, topic_filter: &str) -> u64 {
        self.storage
            .as_deref()
            .unwrap()
            .get_session(client_id)
            .await
            .unwrap()
            .unwrap()
            .subscriptions[topic_filter]
            .instance
    }
}

fn memory() -> BrokerConfig {
    BrokerConfig::default().with_storage(StorageConfig {
        backend: BackendKind::Memory,
        enable_persistence: true,
        cleanup_interval: NEVER,
        ..Default::default()
    })
}

fn options(id: &str, clean: bool, expiry: u32) -> ConnectOptions {
    ConnectOptions::new(id)
        .with_clean_start(clean)
        .with_session_expiry_interval(expiry)
}

struct Wire {
    stream: TcpStream,
    buffer: BytesMut,
}

impl Wire {
    async fn write_all(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).await.unwrap();
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
}

async fn connect(addr: &str, opts: ConnectOptions) -> (Wire, ConnAckPacket) {
    let mut wire = Wire {
        stream: TcpStream::connect(addr).await.unwrap(),
        buffer: BytesMut::new(),
    };
    let mut bytes = Vec::new();
    ConnectPacket::new(opts).encode(&mut bytes).unwrap();
    wire.write_all(&bytes).await;
    match wire.next(Duration::from_secs(60)).await {
        Some(Packet::ConnAck(connack)) => (wire, connack),
        other => panic!("expected CONNACK, got {other:?}"),
    }
}

fn subscribe_bytes(packet_id: u8, topic: &str, qos: u8) -> Vec<u8> {
    let topic_len = u8::try_from(topic.len()).unwrap();
    let mut bytes = vec![0x82, 6 + topic_len, 0x00, packet_id, 0x00, 0x00, topic_len];
    bytes.extend_from_slice(topic.as_bytes());
    bytes.push(qos);
    bytes
}

async fn subscribe(wire: &mut Wire, topic: &str, qos: u8) {
    wire.write_all(&subscribe_bytes(1, topic, qos)).await;
    assert!(matches!(
        wire.next(Duration::from_secs(10)).await,
        Some(Packet::SubAck(_))
    ));
}

fn publish_bytes(topic: &str, qos: QoS, id: u16, payload: &[u8]) -> Vec<u8> {
    let mut packet = PublishPacket::new(topic, payload.to_vec(), qos);
    if qos != QoS::AtMostOnce {
        packet.packet_id = Some(id);
    }
    let mut bytes = Vec::new();
    packet.encode(&mut bytes).unwrap();
    bytes
}

async fn publish1(wire: &mut Wire, topic: &str, id: u16, payload: &[u8]) {
    wire.write_all(&publish_bytes(topic, QoS::AtLeastOnce, id, payload))
        .await;
    assert!(matches!(
        wire.next(Duration::from_secs(10)).await,
        Some(Packet::PubAck(_))
    ));
}

async fn puback(wire: &mut Wire, packet_id: Option<u16>) {
    let id = packet_id.unwrap().to_be_bytes();
    wire.write_all(&[0x40, 0x02, id[0], id[1]]).await;
}

async fn end(mut wire: Wire) {
    wire.write_all(&[0xE0, 0x00]).await;
    sleep(Duration::from_millis(100)).await;
}

async fn wedged_first(broker: &Broker, id: &str) -> Wire {
    let (mut first, _) = connect(&broker.addr, options(id, true, 3600)).await;
    subscribe(&mut first, "secret/x", 1).await;
    subscribe(&mut first, "open/x", 1).await;
    broker.provider.deny_secret.store(true, Ordering::SeqCst);
    first.write_all(&subscribe_bytes(2, "slow/a", 1)).await;
    sleep(Duration::from_millis(100)).await;
    first
}

async fn publishes_until_silence(wire: &mut Wire) -> Vec<String> {
    let mut publishes = Vec::new();
    while let Some(packet) = wire.next(Duration::from_millis(500)).await {
        if let Packet::Publish(publish) = packet {
            publishes.push(format!("{} dup={}", publish.topic_name, publish.dup));
        }
    }
    publishes
}

/// The payloads received, acknowledging each, until none comes for half a second.
async fn acknowledged_payloads(wire: &mut Wire) -> Vec<String> {
    let mut payloads = Vec::new();
    while let Some(packet) = wire.next(Duration::from_millis(500)).await {
        if let Packet::Publish(publish) = packet {
            payloads.push(String::from_utf8_lossy(&publish.payload).into_owned());
            puback(wire, publish.packet_id).await;
        }
    }
    payloads
}

#[tokio::test]
async fn a_subscribe_during_a_handoff_doesnt_hold_up_the_connection() {
    let broker = Broker::start(memory(), Duration::from_secs(3)).await;
    let first = wedged_first(&broker, "prompt").await;
    let (mut second, connack) = connect(&broker.addr, options("prompt", false, 3600)).await;
    assert!(connack.session_present);
    let started = Instant::now();
    second.write_all(&subscribe_bytes(7, "other/y", 1)).await;
    second.write_all(&[0xC0, 0x00]).await;
    let mut answered = 0;
    while let Some(packet) = second.next(Duration::from_secs(1)).await {
        if matches!(packet, Packet::SubAck(_) | Packet::PingResp) {
            answered += 1;
        }
    }
    assert_eq!(answered, 2);
    assert!(started.elapsed() < Duration::from_secs(2));
    drop(first);
}

#[tokio::test]
async fn a_third_connection_isnt_held_up_by_a_subscribe_during_a_handoff() {
    let broker = Broker::start(memory(), Duration::from_secs(3)).await;
    let (mut publisher, _) = connect(&broker.addr, options("third-pub", true, 0)).await;
    let first = wedged_first(&broker, "third").await;
    let (mut second, _) = connect(&broker.addr, options("third", false, 3600)).await;
    second.write_all(&subscribe_bytes(7, "other/y", 1)).await;
    sleep(Duration::from_millis(100)).await;
    let started = Instant::now();
    let (mut third, connack) = connect(&broker.addr, options("third", false, 3600)).await;
    assert!(connack.session_present);
    publish1(&mut publisher, "open/x", 1, b"m").await;
    let delivered = third.next(Duration::from_secs(10)).await;
    assert!(
        matches!(&delivered, Some(Packet::Publish(publish)) if !publish.dup),
        "{delivered:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    let mut old_socket = Vec::new();
    while let Some(packet) = second.next(Duration::from_millis(200)).await {
        if let Packet::Publish(publish) = packet {
            old_socket.push(publish.topic_name);
        }
    }
    assert!(old_socket.is_empty(), "{old_socket:?}");
    drop(first);
}

#[tokio::test]
async fn a_displaced_subscribe_leaves_a_clean_start_owners_queue() {
    let broker = Broker::start(memory(), Duration::from_millis(1500)).await;
    let (mut publisher, _) = connect(&broker.addr, options("clean-pub", true, 0)).await;
    let (mut sub, _) = connect(&broker.addr, options("clean", true, 3600)).await;
    subscribe(&mut sub, "secret/x", 1).await;
    end(sub).await;
    sleep(Duration::from_millis(200)).await;
    broker.provider.deny_secret.store(true, Ordering::SeqCst);
    let (mut displaced, connack) = connect(&broker.addr, options("clean", false, 3600)).await;
    assert!(connack.session_present);
    displaced.write_all(&subscribe_bytes(2, "slow/a", 1)).await;
    sleep(Duration::from_millis(100)).await;
    broker.provider.deny_secret.store(false, Ordering::SeqCst);
    let (mut owner, _) = connect(&broker.addr, options("clean", true, 3600)).await;
    subscribe(&mut owner, "secret/x", 1).await;
    publish1(&mut publisher, "secret/x", 1, b"m").await;
    sleep(Duration::from_millis(2500)).await;
    assert_eq!(publishes_until_silence(&mut owner).await.len(), 1);
    drop(displaced);
}

async fn handback_with_other_clients_traffic(revoke: bool, pushes: u16) -> Vec<String> {
    let broker = Broker::start(memory(), Duration::from_millis(1500)).await;
    let (mut publisher, _) = connect(&broker.addr, options("traffic-pub", true, 0)).await;
    let (mut other, _) = connect(&broker.addr, options("traffic-other", true, 3600)).await;
    subscribe(&mut other, "other/z", 1).await;
    end(other).await;
    let (mut first, _) = connect(&broker.addr, options("traffic", true, 3600)).await;
    subscribe(&mut first, "secret/x", 1).await;
    broker.provider.deny_secret.store(revoke, Ordering::SeqCst);
    first.write_all(&subscribe_bytes(2, "slow/a", 1)).await;
    sleep(Duration::from_millis(100)).await;
    publish1(&mut publisher, "secret/x", 1, b"m").await;
    let (mut second, _) = connect(&broker.addr, options("traffic", false, 3600)).await;
    broker.provider.deny_secret.store(false, Ordering::SeqCst);
    subscribe(&mut second, "secret/x", 1).await;
    for id in 0..pushes {
        publish1(&mut publisher, "other/z", 100 + id, b"m").await;
    }
    sleep(Duration::from_millis(2500)).await;
    let delivered = publishes_until_silence(&mut second).await;
    drop(first);
    delivered
}

#[tokio::test]
async fn a_hand_back_is_dropped_whatever_other_clients_queue() {
    assert_eq!(
        handback_with_other_clients_traffic(true, 3).await,
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn control_a_hand_back_is_delivered_whatever_other_clients_queue() {
    assert_eq!(
        handback_with_other_clients_traffic(false, 3).await,
        ["secret/x dup=false"]
    );
}

async fn handback_reaching_a_later_connection(
    revoke: bool,
    second_disconnects: bool,
) -> Vec<String> {
    let broker = Broker::start(memory(), Duration::from_millis(1500)).await;
    let (mut publisher, _) = connect(&broker.addr, options("later-pub", true, 0)).await;
    let (mut first, _) = connect(&broker.addr, options("later", true, 3600)).await;
    subscribe(&mut first, "secret/x", 1).await;
    broker.provider.deny_secret.store(revoke, Ordering::SeqCst);
    first.write_all(&subscribe_bytes(2, "slow/a", 1)).await;
    sleep(Duration::from_millis(100)).await;
    publish1(&mut publisher, "secret/x", 1, b"m").await;
    let (mut second, _) = connect(&broker.addr, options("later", false, 3600)).await;
    broker.provider.deny_secret.store(false, Ordering::SeqCst);
    subscribe(&mut second, "secret/x", 1).await;
    if second_disconnects {
        end(second).await;
    }
    let (mut third, _) = connect(&broker.addr, options("later", false, 3600)).await;
    sleep(Duration::from_millis(2500)).await;
    third.write_all(&[0xC0, 0x00]).await;
    let delivered = publishes_until_silence(&mut third).await;
    drop(first);
    delivered
}

#[tokio::test]
async fn a_hand_back_is_dropped_when_it_reaches_a_later_takeover() {
    assert_eq!(
        handback_reaching_a_later_connection(true, false).await,
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn a_hand_back_is_dropped_when_it_reaches_the_next_connection() {
    assert_eq!(
        handback_reaching_a_later_connection(true, true).await,
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn control_a_hand_back_is_delivered_to_a_later_takeover_holding_the_subscription() {
    assert_eq!(
        handback_reaching_a_later_connection(false, false).await,
        ["secret/x dup=false"]
    );
}

#[tokio::test]
async fn control_a_hand_back_is_delivered_to_the_next_connection_holding_the_subscription() {
    assert_eq!(
        handback_reaching_a_later_connection(false, true).await,
        ["secret/x dup=false"]
    );
}

#[tokio::test]
async fn a_message_published_after_the_resubscribe_survives_the_hand_back() {
    let broker = Broker::start(memory(), Duration::from_millis(1500)).await;
    let (mut publisher, _) = connect(&broker.addr, options("after-pub", true, 0)).await;
    let (mut first, _) = connect(&broker.addr, options("after", true, 3600)).await;
    subscribe(&mut first, "secret/x", 1).await;
    broker.provider.deny_secret.store(true, Ordering::SeqCst);
    first.write_all(&subscribe_bytes(2, "slow/a", 1)).await;
    sleep(Duration::from_millis(100)).await;
    publish1(&mut publisher, "secret/x", 1, b"before").await;
    let (mut second, _) = connect(&broker.addr, options("after", false, 3600)).await;
    broker.provider.deny_secret.store(false, Ordering::SeqCst);
    subscribe(&mut second, "secret/x", 1).await;
    publish1(&mut publisher, "secret/x", 2, b"after").await;
    sleep(Duration::from_millis(2500)).await;
    let mut payloads = Vec::new();
    while let Some(packet) = second.next(Duration::from_millis(500)).await {
        if let Packet::Publish(publish) = packet {
            payloads.push(String::from_utf8_lossy(&publish.payload).into_owned());
        }
    }
    drop(first);
    assert_eq!(payloads, ["after"]);
}

async fn lane_message_after_a_resubscribe(revoke: bool) -> Vec<String> {
    let broker = Broker::start(memory(), Duration::ZERO).await;
    let (mut publisher, _) = connect(&broker.addr, options("lane-pub", true, 0)).await;
    let (mut sub, _) = connect(&broker.addr, options("lane", true, 3600)).await;
    subscribe(&mut sub, "secret/x", 1).await;
    subscribe(&mut sub, "open/x", 1).await;
    end(sub).await;
    sleep(Duration::from_millis(200)).await;
    let routed_for = broker.instance("lane", "secret/x").await;

    broker.provider.deny_secret.store(revoke, Ordering::SeqCst);
    publish1(&mut publisher, "open/x", 1, b"m").await;
    let (mut sub, connack) = connect(
        &broker.addr,
        options("lane", false, 3600).with_receive_maximum(1),
    )
    .await;
    assert!(connack.session_present);
    let started = sub.next(Duration::from_secs(2)).await;
    let Some(Packet::Publish(started)) = started else {
        panic!("{started:?}");
    };
    broker
        .router
        .subscribe(
            mqtt5::broker::router::SubscriptionRequest::new("lane", "secret/x", QoS::AtLeastOnce)
                .with_instance(routed_for),
        )
        .await
        .unwrap();
    let on_the_lane = PublishPacket::new("secret/x", &b"m"[..], QoS::AtLeastOnce);
    broker.router.route_message(&on_the_lane, None).await;
    broker.provider.deny_secret.store(false, Ordering::SeqCst);
    subscribe(&mut sub, "secret/x", 1).await;
    puback(&mut sub, started.packet_id).await;
    publishes_until_silence(&mut sub).await
}

#[tokio::test]
async fn a_resubscribe_doesnt_release_what_a_lane_holds_for_a_lost_subscription() {
    assert_eq!(
        lane_message_after_a_resubscribe(true).await,
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn control_a_resubscribe_keeps_what_a_lane_holds_for_a_held_subscription() {
    assert_eq!(
        lane_message_after_a_resubscribe(false).await,
        ["secret/x dup=false"]
    );
}

#[tokio::test]
async fn a_plain_resubscribe_keeps_what_was_queued_and_on_the_lane_in_order() {
    let broker = Broker::start(memory(), Duration::ZERO).await;
    let (mut publisher, _) = connect(&broker.addr, options("plain-pub", true, 0)).await;
    let (mut sub, _) = connect(&broker.addr, options("plain", true, 3600)).await;
    subscribe(&mut sub, "open/x", 1).await;
    end(sub).await;
    sleep(Duration::from_millis(200)).await;
    for (id, payload) in (1..).zip(["q0", "q1", "q2", "q3"]) {
        publish1(&mut publisher, "open/x", id, payload.as_bytes()).await;
    }

    let (mut sub, connack) = connect(
        &broker.addr,
        options("plain", false, 3600).with_receive_maximum(1),
    )
    .await;
    assert!(connack.session_present);
    let Some(Packet::Publish(started)) = sub.next(Duration::from_secs(2)).await else {
        panic!("the first queued message was not sent");
    };
    subscribe(&mut sub, "open/x", 1).await;
    puback(&mut sub, started.packet_id).await;
    let mut payloads = vec![String::from_utf8_lossy(&started.payload).into_owned()];
    payloads.extend(acknowledged_payloads(&mut sub).await);

    publish1(&mut publisher, "open/x", 10, b"l0").await;
    let Some(Packet::Publish(started)) = sub.next(Duration::from_secs(2)).await else {
        panic!("the first lane message was not sent");
    };
    for (id, payload) in (11..).zip(["l1", "l2", "l3"]) {
        publish1(&mut publisher, "open/x", id, payload.as_bytes()).await;
    }
    subscribe(&mut sub, "open/x", 1).await;
    puback(&mut sub, started.packet_id).await;
    payloads.push(String::from_utf8_lossy(&started.payload).into_owned());
    payloads.extend(acknowledged_payloads(&mut sub).await);

    assert_eq!(
        payloads,
        ["q0", "q1", "q2", "q3", "l0", "l1", "l2", "l3"],
        "a SUBSCRIBE refreshing a held subscription dropped or reordered what was routed before it"
    );
}

const ORDERED_RUN: u32 = 5000;

/// Publishes `ORDERED_RUN` numbered messages to a session that lost a subscription on resume,
/// while it subscribes to other filters every 10 messages it receives, one unacknowledged
/// message at a time so that what is routed meanwhile waits on its lane. Returns how many it
/// received and how many came before an earlier-numbered one.
async fn ordered_run_with_subscribes(qos: QoS) -> (usize, usize) {
    let broker = Broker::start(memory(), Duration::ZERO).await;
    let client_id = format!("ordered-{}", qos as u8);
    let (mut sub, _) = connect(&broker.addr, options(&client_id, true, 3600)).await;
    subscribe(&mut sub, "secret/x", 1).await;
    subscribe(&mut sub, "open/x", qos as u8).await;
    end(sub).await;
    sleep(Duration::from_millis(200)).await;
    broker.provider.deny_secret.store(true, Ordering::SeqCst);
    let (mut sub, connack) = connect(
        &broker.addr,
        options(&client_id, false, 3600).with_receive_maximum(1),
    )
    .await;
    assert!(connack.session_present);

    let (mut publisher, _) =
        connect(&broker.addr, options(&format!("{client_id}-pub"), true, 0)).await;
    let publishing = tokio::spawn(async move {
        for index in 0..ORDERED_RUN {
            let id = u16::try_from(index % 60_000 + 1).unwrap();
            let payload = index.to_be_bytes();
            if qos == QoS::AtMostOnce {
                publisher
                    .write_all(&publish_bytes("open/x", qos, id, &payload))
                    .await;
                if index % 50 == 0 {
                    tokio::task::yield_now().await;
                }
            } else {
                publish1(&mut publisher, "open/x", id, &payload).await;
            }
        }
        publisher
    });

    let mut received = Vec::new();
    let mut subscribes = 0u8;
    while let Some(packet) = sub.next(Duration::from_secs(2)).await {
        let Packet::Publish(publish) = packet else {
            continue;
        };
        received.push(u32::from_be_bytes(publish.payload[..4].try_into().unwrap()));
        if publish.qos != QoS::AtMostOnce {
            puback(&mut sub, publish.packet_id).await;
        }
        if received.len() % 10 == 0 {
            subscribes = subscribes.wrapping_add(1);
            sub.write_all(&subscribe_bytes(
                subscribes.max(1),
                &format!("other/{subscribes}"),
                1,
            ))
            .await;
        }
    }
    drop(publishing.await.unwrap());
    let inversions = received
        .windows(2)
        .filter(|pair| pair[1] <= pair[0])
        .count();
    (received.len(), inversions)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscribes_neither_reorder_nor_drop_at_most_once_messages_on_other_topics() {
    assert_eq!(
        ordered_run_with_subscribes(QoS::AtMostOnce).await,
        (usize::try_from(ORDERED_RUN).unwrap(), 0)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscribes_neither_reorder_nor_drop_at_least_once_messages_on_other_topics() {
    assert_eq!(
        ordered_run_with_subscribes(QoS::AtLeastOnce).await,
        (usize::try_from(ORDERED_RUN).unwrap(), 0)
    );
}
