use bytes::BytesMut;
use mqtt5::packet::connack::ConnAckPacket;
use mqtt5::packet::connect::ConnectPacket;
use mqtt5::packet::publish::PublishPacket;
use mqtt5::packet::{FixedHeader, MqttPacket, Packet, PacketType};
use mqtt5::protocol::v5::reason_codes::ReasonCode;
use mqtt5::session::TopicAliasManager;
use mqtt5::{
    AckToken, ConnectOptions, Message, MqttClient, MqttError, PublishOptions, PublishProperties,
    QoS, SubscribeOptions,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

const CONNECT: u8 = 1;
const PUBLISH: u8 = 3;
const PUBACK: u8 = 4;
const PUBREC: u8 = 5;
const PUBREL: u8 = 6;
const PUBCOMP: u8 = 7;
const SUBSCRIBE: u8 = 8;
const UNSUBSCRIBE: u8 = 10;
const PINGREQ: u8 = 12;
const DISCONNECT: u8 = 14;

const SHORT: Duration = Duration::from_millis(600);
const MEDIUM: Duration = Duration::from_secs(3);

struct Frame {
    first: u8,
    body: Vec<u8>,
}

impl Frame {
    fn kind(&self) -> u8 {
        self.first >> 4
    }

    fn dup(&self) -> bool {
        self.first & 0x08 != 0
    }

    fn packet_id(&self) -> u16 {
        if self.kind() == PUBLISH {
            return self
                .publish()
                .packet_id
                .expect("QoS > 0 PUBLISH carries a packet id");
        }
        u16::from_be_bytes([self.body[0], self.body[1]])
    }

    fn reason_code(&self) -> u8 {
        match self.kind() {
            DISCONNECT => self.body.first().copied().unwrap_or(0),
            _ => self.body.get(2).copied().unwrap_or(0),
        }
    }

    fn packet(&self) -> Packet {
        let packet_type = PacketType::from_u8(self.kind()).expect("known packet type");
        let header = FixedHeader::new(
            packet_type,
            self.first & 0x0F,
            u32::try_from(self.body.len()).expect("body fits u32"),
        );
        let mut buf = BytesMut::from(&self.body[..]);
        Packet::decode_from_body_with_version(packet_type, &header, &mut buf, 5)
            .expect("client sent a decodable packet")
    }

    fn publish(&self) -> PublishPacket {
        match self.packet() {
            Packet::Publish(publish) => publish,
            other => panic!("expected PUBLISH, got {}", other.packet_type_name()),
        }
    }
}

enum Next {
    Frame(Frame),
    Closed,
    Timeout,
}

async fn read_frame(stream: &mut TcpStream) -> Option<Frame> {
    let mut first = [0u8; 1];
    stream.read_exact(&mut first).await.ok()?;
    let mut multiplier = 1usize;
    let mut remaining = 0usize;
    loop {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).await.ok()?;
        remaining += usize::from(byte[0] & 0x7F) * multiplier;
        if byte[0] & 0x80 == 0 {
            break;
        }
        multiplier *= 128;
    }
    let mut body = vec![0u8; remaining];
    stream.read_exact(&mut body).await.ok()?;
    Some(Frame {
        first: first[0],
        body,
    })
}

async fn next_frame(stream: &mut TcpStream, wait: Duration) -> Next {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match tokio::time::timeout_at(deadline, read_frame(stream)).await {
            Err(_) => return Next::Timeout,
            Ok(None) => return Next::Closed,
            Ok(Some(frame)) if frame.kind() == PINGREQ => {
                let _ = stream.write_all(&[0xD0, 0x00]).await;
            }
            Ok(Some(frame)) => return Next::Frame(frame),
        }
    }
}

async fn expect_kind(stream: &mut TcpStream, kind: u8, wait: Duration) -> Frame {
    match next_frame(stream, wait).await {
        Next::Frame(frame) if frame.kind() == kind => frame,
        Next::Frame(frame) => panic!("expected packet type {kind}, got {}", frame.kind()),
        Next::Closed => panic!("connection closed while waiting for packet type {kind}"),
        Next::Timeout => panic!("timed out waiting for packet type {kind}"),
    }
}

async fn collect_frames(stream: &mut TcpStream, wait: Duration) -> (Vec<Frame>, bool) {
    let deadline = tokio::time::Instant::now() + wait;
    let mut frames = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match next_frame(stream, left).await {
            Next::Frame(frame) => frames.push(frame),
            Next::Closed => return (frames, true),
            Next::Timeout => return (frames, false),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Termination {
    Disconnect(u8),
    Closed,
    StillOpen,
}

async fn termination(stream: &mut TcpStream, wait: Duration) -> Termination {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match next_frame(stream, left).await {
            Next::Frame(frame) if frame.kind() == DISCONNECT => {
                return Termination::Disconnect(frame.reason_code())
            }
            Next::Frame(_) => {}
            Next::Closed => return Termination::Closed,
            Next::Timeout => return Termination::StillOpen,
        }
    }
}

fn encode_varint(mut value: usize, out: &mut Vec<u8>) {
    loop {
        let mut byte = u8::try_from(value % 128).expect("remainder fits u8");
        value /= 128;
        if value > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn frame_bytes(first: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![first];
    encode_varint(body.len(), &mut out);
    out.extend_from_slice(body);
    out
}

fn raw_publish(
    qos: u8,
    dup: bool,
    topic: &str,
    packet_id: Option<u16>,
    props: &[u8],
    payload: &[u8],
) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(
        &u16::try_from(topic.len())
            .expect("topic fits u16")
            .to_be_bytes(),
    );
    body.extend_from_slice(topic.as_bytes());
    if let Some(id) = packet_id {
        body.extend_from_slice(&id.to_be_bytes());
    }
    encode_varint(props.len(), &mut body);
    body.extend_from_slice(props);
    body.extend_from_slice(payload);
    frame_bytes(0x30 | (u8::from(dup) << 3) | (qos << 1), &body)
}

fn topic_alias_prop(alias: u16) -> Vec<u8> {
    let mut out = vec![0x23];
    out.extend_from_slice(&alias.to_be_bytes());
    out
}

fn subscription_id_prop(id: usize) -> Vec<u8> {
    let mut out = vec![0x0B];
    encode_varint(id, &mut out);
    out
}

fn ack_bytes(first: u8, packet_id: u16) -> Vec<u8> {
    frame_bytes(first, &packet_id.to_be_bytes())
}

fn suback_bytes(packet_id: u16, code: u8) -> Vec<u8> {
    let mut body = packet_id.to_be_bytes().to_vec();
    body.push(0);
    body.push(code);
    frame_bytes(0x90, &body)
}

fn unsuback_bytes(packet_id: u16) -> Vec<u8> {
    let mut body = packet_id.to_be_bytes().to_vec();
    body.push(0);
    body.push(0);
    frame_bytes(0xB0, &body)
}

fn connack(session_present: bool, configure: impl FnOnce(&mut ConnAckPacket)) -> Vec<u8> {
    let mut packet = ConnAckPacket::new(session_present, ReasonCode::Success);
    configure(&mut packet);
    let mut encoded = Vec::new();
    packet.encode(&mut encoded).expect("CONNACK encodes");
    encoded
}

fn plain_connack() -> Vec<u8> {
    connack(false, |_| {})
}

async fn bind() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("mqtt://{}", listener.local_addr().expect("local addr"));
    (listener, url)
}

async fn accept_session(
    listener: &TcpListener,
    connack_bytes: &[u8],
) -> (TcpStream, ConnectPacket) {
    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .expect("client connected within 10s")
        .expect("accept");
    let frame = expect_kind(&mut stream, CONNECT, MEDIUM).await;
    let connect = match frame.packet() {
        Packet::Connect(connect) => *connect,
        other => panic!("expected CONNECT, got {}", other.packet_type_name()),
    };
    stream
        .write_all(connack_bytes)
        .await
        .expect("write CONNACK");
    (stream, connect)
}

fn base_options(client_id: &str) -> ConnectOptions {
    ConnectOptions::new(client_id).with_automatic_reconnect(false)
}

fn resumable_options(client_id: &str) -> ConnectOptions {
    ConnectOptions::new(client_id)
        .with_clean_start(false)
        .with_session_expiry_interval(300)
        .with_automatic_reconnect(true)
        .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(200))
}

async fn connect_client(
    options: ConnectOptions,
    connack_bytes: &[u8],
) -> (MqttClient, TcpStream, TcpListener) {
    let (listener, url) = bind().await;
    let client = MqttClient::with_options(options);
    let connecting = client.clone();
    let task = tokio::spawn(async move { connecting.connect(&url).await });
    let (stream, _) = accept_session(&listener, connack_bytes).await;
    task.await
        .expect("connect task")
        .expect("client connects to fake broker");
    (client, stream, listener)
}

async fn subscribe(
    client: &MqttClient,
    stream: &mut TcpStream,
    filter: &str,
    options: SubscribeOptions,
) -> mpsc::UnboundedReceiver<Message> {
    let (tx, rx) = mpsc::unbounded_channel();
    let subscriber = client.clone();
    let filter = filter.to_string();
    let granted = options.qos as u8;
    let task = tokio::spawn(async move {
        subscriber
            .subscribe_with_options(filter, options, move |message| {
                let _ = tx.send(message);
            })
            .await
    });
    let frame = expect_kind(stream, SUBSCRIBE, MEDIUM).await;
    stream
        .write_all(&suback_bytes(frame.packet_id(), granted))
        .await
        .expect("write SUBACK");
    task.await.expect("subscribe task").expect("subscribe");
    rx
}

fn qos_options(qos: QoS) -> SubscribeOptions {
    SubscribeOptions {
        qos,
        ..Default::default()
    }
}

async fn drain(rx: &mut mpsc::UnboundedReceiver<Message>, wait: Duration) -> Vec<Message> {
    let mut messages = Vec::new();
    while let Ok(Some(message)) = tokio::time::timeout(wait, rx.recv()).await {
        messages.push(message);
    }
    messages
}

async fn wait_until_disconnected(client: &MqttClient) {
    for _ in 0..100 {
        if !client.is_connected().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("client did not observe the dropped connection");
}

async fn published_frame_for(
    options: PublishOptions,
    topic: &str,
) -> (std::result::Result<(), MqttError>, Option<Frame>) {
    published_frame_with_connack(options, topic, &plain_connack()).await
}

async fn published_frame_with_connack(
    options: PublishOptions,
    topic: &str,
    connack_bytes: &[u8],
) -> (std::result::Result<(), MqttError>, Option<Frame>) {
    let (client, mut stream, _listener) =
        connect_client(base_options("conf-b-pub"), connack_bytes).await;
    let result = client
        .publish_with_options(topic.to_string(), b"x".to_vec(), options)
        .await
        .map(|_| ());
    let frame = match next_frame(&mut stream, SHORT).await {
        Next::Frame(frame) if frame.kind() == PUBLISH => Some(frame),
        _ => None,
    };
    (result, frame)
}

fn qos0_with(properties: PublishProperties) -> PublishOptions {
    PublishOptions {
        qos: QoS::AtMostOnce,
        properties,
        ..Default::default()
    }
}

#[tokio::test]
async fn mqtt_3_3_2_2_client_publish_topic_must_not_contain_wildcards() {
    let (result, frame) =
        published_frame_for(qos0_with(PublishProperties::default()), "a/+/b").await;
    if let Some(frame) = frame {
        panic!(
            "[MQTT-3.3.2-2] client sent PUBLISH with wildcard Topic Name {:?} (publish returned {result:?})",
            frame.publish().topic_name
        );
    }
}

#[tokio::test]
async fn mqtt_3_3_2_1_prose_zero_length_topic_requires_topic_alias() {
    let (result, frame) = published_frame_for(qos0_with(PublishProperties::default()), "").await;
    if let Some(frame) = frame {
        let publish = frame.publish();
        assert!(
            publish.topic_alias().is_some(),
            "[MQTT-3.3.2-1 / §3.3.2.3.4] client sent PUBLISH with zero-length Topic Name and no Topic Alias (publish returned {result:?})"
        );
    }
}

#[tokio::test]
async fn mqtt_3_3_2_8_client_must_not_send_topic_alias_zero() {
    let properties = PublishProperties {
        topic_alias: Some(0),
        ..Default::default()
    };
    let (result, frame) = published_frame_for(qos0_with(properties), "t/alias").await;
    if let Some(frame) = frame {
        assert_ne!(
            frame.publish().topic_alias(),
            Some(0),
            "[MQTT-3.3.2-8] client sent PUBLISH with Topic Alias 0 (publish returned {result:?})"
        );
    }
}

#[tokio::test]
async fn mqtt_3_3_2_9_client_must_not_exceed_server_topic_alias_maximum() {
    let properties = PublishProperties {
        topic_alias: Some(3),
        ..Default::default()
    };
    let connack_bytes = connack(false, |c| c.properties.set_topic_alias_maximum(2));
    let (result, frame) =
        published_frame_with_connack(qos0_with(properties), "t/alias", &connack_bytes).await;
    if let Some(frame) = frame {
        let alias = frame.publish().topic_alias();
        assert!(
            alias.is_none_or(|a| a <= 2),
            "[MQTT-3.3.2-9] client sent Topic Alias {alias:?} above the server Topic Alias Maximum 2 (publish returned {result:?})"
        );
    }
}

#[tokio::test]
async fn mqtt_3_3_4_6_client_publish_must_not_contain_subscription_identifier() {
    let properties = PublishProperties {
        subscription_identifiers: vec![7],
        ..Default::default()
    };
    let (result, frame) = published_frame_for(qos0_with(properties), "t/subid").await;
    if let Some(frame) = frame {
        let ids = frame
            .publish()
            .properties
            .get_all(mqtt5::PropertyId::SubscriptionIdentifier)
            .map_or(0, <[mqtt5::PropertyValue]>::len);
        assert_eq!(
            ids, 0,
            "[MQTT-3.3.4-6] client sent PUBLISH containing a Subscription Identifier (publish returned {result:?})"
        );
    }
}

#[tokio::test]
async fn mqtt_3_3_2_14_response_topic_must_not_contain_wildcards() {
    let properties = PublishProperties {
        response_topic: Some("reply/#".to_string()),
        ..Default::default()
    };
    let (result, frame) = published_frame_for(qos0_with(properties), "t/req").await;
    if let Some(frame) = frame {
        panic!(
            "[MQTT-3.3.2-14] client sent PUBLISH with wildcard Response Topic {:?} (publish returned {result:?})",
            frame.publish().properties.get(mqtt5::PropertyId::ResponseTopic)
        );
    }
}

#[tokio::test]
async fn mqtt_3_3_2_1_13_19_utf8_string_fields_reject_nul() {
    let cases = [
        (
            "t/\0nul",
            PublishProperties::default(),
            "MQTT-3.3.2-1 Topic Name",
        ),
        (
            "t/ok",
            PublishProperties {
                response_topic: Some("r/\0".to_string()),
                ..Default::default()
            },
            "MQTT-3.3.2-13 Response Topic",
        ),
        (
            "t/ok",
            PublishProperties {
                content_type: Some("text/\0".to_string()),
                ..Default::default()
            },
            "MQTT-3.3.2-19 Content Type",
        ),
    ];
    for (topic, properties, id) in cases {
        let (result, frame) = published_frame_for(qos0_with(properties), topic).await;
        assert!(
            frame.is_none(),
            "[{id}] client put a U+0000 string on the wire"
        );
        assert!(result.is_err(), "[{id}] publish with U+0000 must fail");
    }
}

#[tokio::test]
async fn mqtt_3_8_2_1_2_prose_subscribe_subscription_identifier_zero() {
    let (client, mut stream, _listener) =
        connect_client(base_options("conf-b-subid0"), &plain_connack()).await;
    let subscriber = client.clone();
    let task = tokio::spawn(async move {
        subscriber
            .subscribe_with_options(
                "t/x",
                SubscribeOptions {
                    subscription_identifier: Some(0),
                    ..Default::default()
                },
                |_| {},
            )
            .await
    });
    let sent = match next_frame(&mut stream, SHORT).await {
        Next::Frame(frame) if frame.kind() == SUBSCRIBE => match frame.packet() {
            Packet::Subscribe(sub) => sub.properties.get_subscription_identifier(),
            _ => None,
        },
        _ => None,
    };
    task.abort();
    assert_ne!(
        sent,
        Some(0),
        "[§3.8.2.1.2] client sent SUBSCRIBE with Subscription Identifier 0 (Protocol Error)"
    );
}

#[tokio::test]
async fn mqtt_3_8_1_1_3_10_1_1_3_6_1_1_reserved_flags_and_reason_codes_on_wire() {
    let (client, mut stream, _listener) =
        connect_client(base_options("conf-b-flags"), &plain_connack()).await;

    let subscriber = client.clone();
    let sub_task = tokio::spawn(async move { subscriber.subscribe("t/flags", |_| {}).await });
    let sub = expect_kind(&mut stream, SUBSCRIBE, MEDIUM).await;
    assert_eq!(
        sub.first, 0x82,
        "[MQTT-3.8.1-1] SUBSCRIBE fixed header flags"
    );
    match sub.packet() {
        Packet::Subscribe(packet) => assert!(
            !packet.filters.is_empty(),
            "[MQTT-3.8.3-2] SUBSCRIBE must carry at least one filter"
        ),
        _ => unreachable!(),
    }
    stream
        .write_all(&suback_bytes(sub.packet_id(), 0))
        .await
        .unwrap();
    sub_task.await.unwrap().expect("subscribe");

    client.publish("t/q0", b"x".to_vec()).await.expect("QoS0");
    let q0 = expect_kind(&mut stream, PUBLISH, MEDIUM).await;
    assert!(!q0.dup(), "[MQTT-3.3.1-2] QoS0 PUBLISH must have DUP=0");

    let publisher = client.clone();
    let q2_task = tokio::spawn(async move { publisher.publish_qos2("t/q2", b"x".to_vec()).await });
    let q2 = expect_kind(&mut stream, PUBLISH, MEDIUM).await;
    assert!(
        !q2.dup(),
        "[MQTT-4.3.3-2] first QoS2 transmission must have DUP=0"
    );
    stream
        .write_all(&ack_bytes(0x50, q2.packet_id()))
        .await
        .unwrap();
    let rel = expect_kind(&mut stream, PUBREL, MEDIUM).await;
    assert_eq!(rel.first, 0x62, "[MQTT-3.6.1-1] PUBREL fixed header flags");
    assert!(
        matches!(rel.reason_code(), 0x00 | 0x92),
        "[MQTT-3.6.2-1] PUBREL reason code 0x{:02X}",
        rel.reason_code()
    );
    assert_eq!(rel.packet_id(), q2.packet_id());
    stream
        .write_all(&ack_bytes(0x70, q2.packet_id()))
        .await
        .unwrap();
    q2_task.await.unwrap().expect("QoS2 publish completes");

    let unsubscriber = client.clone();
    let unsub_task = tokio::spawn(async move { unsubscriber.unsubscribe("t/flags").await });
    let unsub = expect_kind(&mut stream, UNSUBSCRIBE, MEDIUM).await;
    assert_eq!(
        unsub.first, 0xA2,
        "[MQTT-3.10.1-1] UNSUBSCRIBE fixed header flags"
    );
    match unsub.packet() {
        Packet::Unsubscribe(packet) => assert!(
            !packet.filters.is_empty(),
            "[MQTT-3.10.3-2] UNSUBSCRIBE must carry at least one filter"
        ),
        _ => unreachable!(),
    }
    stream
        .write_all(&unsuback_bytes(unsub.packet_id()))
        .await
        .unwrap();
    unsub_task.await.unwrap().expect("unsubscribe");
}

#[tokio::test]
async fn mqtt_3_8_4_2_suback_with_foreign_packet_id_is_not_accepted() {
    let (client, mut stream, _listener) =
        connect_client(base_options("conf-b-suback"), &plain_connack()).await;
    let subscriber = client.clone();
    let task = tokio::spawn(async move { subscriber.subscribe("t/s", |_| {}).await });
    let sub = expect_kind(&mut stream, SUBSCRIBE, MEDIUM).await;
    let foreign = sub.packet_id().wrapping_add(100);
    stream.write_all(&suback_bytes(foreign, 0)).await.unwrap();
    tokio::time::sleep(SHORT).await;
    assert!(
        !task.is_finished(),
        "[MQTT-3.8.4-2] subscribe completed on a SUBACK carrying a different Packet Identifier"
    );
    stream
        .write_all(&suback_bytes(sub.packet_id(), 0))
        .await
        .unwrap();
    tokio::time::timeout(MEDIUM, task)
        .await
        .expect("[MQTT-3.8.4-2] matching SUBACK completes subscribe")
        .unwrap()
        .expect("subscribe");
}

#[tokio::test]
async fn mqtt_3_3_4_1_3_4_2_3_5_2_3_7_2_client_acks_inbound_by_qos() {
    let (client, mut stream, _listener) =
        connect_client(base_options("conf-b-acks"), &plain_connack()).await;
    let mut rx = subscribe(&client, &mut stream, "t/in", qos_options(QoS::ExactlyOnce)).await;

    stream
        .write_all(&raw_publish(1, false, "t/in", Some(11), &[], b"q1"))
        .await
        .unwrap();
    let puback = expect_kind(&mut stream, PUBACK, MEDIUM).await;
    assert_eq!(puback.packet_id(), 11, "[MQTT-3.3.4-1] PUBACK id");
    assert_eq!(puback.first, 0x40);
    assert!(
        puback.body.len() <= 3,
        "[MQTT-3.4.2-2/3] client added PUBACK properties"
    );
    match puback.packet() {
        Packet::PubAck(ack) => assert!(
            mqtt5::packet::is_valid_publish_ack_reason_code(ack.reason_code),
            "[MQTT-3.4.2-1] PUBACK reason code {:?}",
            ack.reason_code
        ),
        _ => unreachable!(),
    }

    stream
        .write_all(&raw_publish(2, false, "t/in", Some(12), &[], b"q2"))
        .await
        .unwrap();
    let pubrec = expect_kind(&mut stream, PUBREC, MEDIUM).await;
    assert_eq!(pubrec.packet_id(), 12, "[MQTT-3.3.4-1] PUBREC id");
    assert!(
        pubrec.body.len() <= 3,
        "[MQTT-3.5.2-2/3] client added PUBREC properties"
    );
    assert!(
        mqtt5::packet::is_valid_publish_ack_reason_code(
            ReasonCode::from_u8(pubrec.reason_code()).expect("known reason code")
        ),
        "[MQTT-3.5.2-1] PUBREC reason code 0x{:02X}",
        pubrec.reason_code()
    );
    stream.write_all(&ack_bytes(0x62, 12)).await.unwrap();
    let pubcomp = expect_kind(&mut stream, PUBCOMP, MEDIUM).await;
    assert_eq!(pubcomp.packet_id(), 12, "[MQTT-4.3.3-11] PUBCOMP id");
    assert!(
        matches!(pubcomp.reason_code(), 0x00 | 0x92),
        "[MQTT-3.7.2-1] PUBCOMP reason code 0x{:02X}",
        pubcomp.reason_code()
    );
    assert!(
        pubcomp.body.len() <= 3,
        "[MQTT-3.7.2-2/3] client added PUBCOMP properties"
    );

    assert_eq!(drain(&mut rx, SHORT).await.len(), 2);
}

#[tokio::test]
async fn mqtt_4_3_3_10_duplicate_qos2_before_pubrel_delivered_once_then_id_reusable() {
    let (client, mut stream, _listener) =
        connect_client(base_options("conf-b-dupq2"), &plain_connack()).await;
    let mut rx = subscribe(&client, &mut stream, "t/q2", qos_options(QoS::ExactlyOnce)).await;

    stream
        .write_all(&raw_publish(2, false, "t/q2", Some(5), &[], b"first"))
        .await
        .unwrap();
    assert_eq!(
        expect_kind(&mut stream, PUBREC, MEDIUM).await.packet_id(),
        5
    );
    stream
        .write_all(&raw_publish(2, true, "t/q2", Some(5), &[], b"first"))
        .await
        .unwrap();
    assert_eq!(
        expect_kind(&mut stream, PUBREC, MEDIUM).await.packet_id(),
        5,
        "[MQTT-4.3.3-10] duplicate before PUBREL must be re-acknowledged with PUBREC"
    );
    assert_eq!(
        drain(&mut rx, SHORT).await.len(),
        1,
        "[MQTT-4.3.3-10] duplicate QoS2 PUBLISH before PUBREL delivered more than once"
    );

    stream.write_all(&ack_bytes(0x62, 5)).await.unwrap();
    assert_eq!(
        expect_kind(&mut stream, PUBCOMP, MEDIUM).await.packet_id(),
        5
    );

    stream
        .write_all(&raw_publish(2, false, "t/q2", Some(5), &[], b"second"))
        .await
        .unwrap();
    assert_eq!(
        expect_kind(&mut stream, PUBREC, MEDIUM).await.packet_id(),
        5
    );
    let reused = drain(&mut rx, SHORT).await;
    assert_eq!(
        reused.len(),
        1,
        "[MQTT-4.3.3-12] PUBLISH reusing a completed QoS2 id must be a new message"
    );
    assert_eq!(reused[0].payload, b"second");
}

#[tokio::test]
async fn mqtt_4_3_2_5_qos1_id_reuse_after_puback_is_new_message() {
    let (client, mut stream, _listener) =
        connect_client(base_options("conf-b-q1reuse"), &plain_connack()).await;
    let mut rx = subscribe(&client, &mut stream, "t/q1", qos_options(QoS::AtLeastOnce)).await;
    for (dup, payload) in [(false, &b"a"[..]), (true, &b"b"[..])] {
        stream
            .write_all(&raw_publish(1, dup, "t/q1", Some(9), &[], payload))
            .await
            .unwrap();
        assert_eq!(
            expect_kind(&mut stream, PUBACK, MEDIUM).await.packet_id(),
            9
        );
    }
    assert_eq!(
        drain(&mut rx, SHORT).await.len(),
        2,
        "[MQTT-4.3.2-5] QoS1 PUBLISH reusing an acknowledged id must be treated as new"
    );
}

#[tokio::test]
async fn mqtt_3_7_2_1_pubrel_for_unknown_id_is_completed() {
    let (_client, mut stream, _listener) =
        connect_client(base_options("conf-b-pubrel"), &plain_connack()).await;
    stream.write_all(&ack_bytes(0x62, 77)).await.unwrap();
    let pubcomp = expect_kind(&mut stream, PUBCOMP, MEDIUM).await;
    assert_eq!(pubcomp.packet_id(), 77, "[MQTT-4.3.3-11] PUBCOMP id");
    assert!(
        matches!(pubcomp.reason_code(), 0x00 | 0x92),
        "[MQTT-3.7.2-1] PUBCOMP reason code 0x{:02X}",
        pubcomp.reason_code()
    );
}

#[tokio::test]
async fn mqtt_3_3_1_4_inbound_qos3_publish_is_malformed() {
    let (client, mut stream, _listener) =
        connect_client(base_options("conf-b-qos3"), &plain_connack()).await;
    let mut rx = subscribe(&client, &mut stream, "t/q3", qos_options(QoS::ExactlyOnce)).await;
    stream
        .write_all(&raw_publish(3, false, "t/q3", Some(3), &[], b"bad"))
        .await
        .unwrap();
    let delivered = drain(&mut rx, SHORT).await.len();
    let end = termination(&mut stream, MEDIUM).await;
    assert_eq!(
        delivered, 0,
        "[MQTT-3.3.1-4] QoS 3 PUBLISH was delivered to the application"
    );
    assert!(
        matches!(end, Termination::Closed | Termination::Disconnect(0x80..)),
        "[MQTT-3.3.1-4 / §4.13] client did not close the Network Connection after a malformed QoS 3 PUBLISH: {end:?}"
    );
}

fn alias_client_options(client_id: &str) -> ConnectOptions {
    let mut options = base_options(client_id);
    options.properties.topic_alias_maximum = Some(2);
    options
}

#[tokio::test]
async fn mqtt_3_3_2_10_client_accepts_topic_alias_within_its_maximum() {
    let (listener, url) = bind().await;
    let client = MqttClient::with_options(alias_client_options("conf-b-alias-ok"));
    let connecting = client.clone();
    let task = tokio::spawn(async move { connecting.connect(&url).await });
    let (mut stream, connect) = accept_session(&listener, &plain_connack()).await;
    task.await.unwrap().expect("connect");
    assert_eq!(connect.properties.get_topic_alias_maximum(), Some(2));

    let mut rx = subscribe(&client, &mut stream, "t/a", qos_options(QoS::AtMostOnce)).await;
    stream
        .write_all(&raw_publish(
            0,
            false,
            "t/a",
            None,
            &topic_alias_prop(1),
            b"one",
        ))
        .await
        .unwrap();
    stream
        .write_all(&raw_publish(
            0,
            false,
            "",
            None,
            &topic_alias_prop(1),
            b"two",
        ))
        .await
        .unwrap();
    let received = drain(&mut rx, SHORT).await;
    let topics: Vec<&str> = received.iter().map(|m| m.topic.as_str()).collect();
    assert_eq!(
        topics,
        vec!["t/a", "t/a"],
        "[MQTT-3.3.2-10] client did not resolve an inbound Topic Alias (1 <= its maximum 2)"
    );
}

async fn alias_violation(alias: u16, client_id: &str) -> (usize, Termination) {
    let (client, mut stream, _listener) =
        connect_client(alias_client_options(client_id), &plain_connack()).await;
    let mut rx = subscribe(&client, &mut stream, "t/a", qos_options(QoS::AtMostOnce)).await;
    stream
        .write_all(&raw_publish(
            0,
            false,
            "t/a",
            None,
            &topic_alias_prop(alias),
            b"x",
        ))
        .await
        .unwrap();
    let delivered = drain(&mut rx, SHORT).await.len();
    (delivered, termination(&mut stream, MEDIUM).await)
}

#[tokio::test]
async fn mqtt_3_3_2_8_receiver_treats_inbound_topic_alias_zero_as_protocol_error() {
    let (delivered, end) = alias_violation(0, "conf-b-alias0").await;
    assert!(
        delivered == 0 && end == Termination::Disconnect(0x94),
        "[MQTT-3.3.2-8 / §3.3.2.3.4] inbound Topic Alias 0 must be a Protocol Error (DISCONNECT 0x94); delivered={delivered} termination={end:?}"
    );
}

#[tokio::test]
async fn mqtt_3_3_2_11_receiver_treats_topic_alias_above_its_maximum_as_protocol_error() {
    let (delivered, end) = alias_violation(3, "conf-b-alias3").await;
    assert!(
        delivered == 0 && end == Termination::Disconnect(0x94),
        "[MQTT-3.3.2-11 / §3.3.2.3.4] inbound Topic Alias 3 > client maximum 2 must be a Protocol Error (DISCONNECT 0x94); delivered={delivered} termination={end:?}"
    );
}

#[tokio::test]
async fn crosscheck3_inbound_subscription_identifier_zero_is_protocol_error() {
    let (client, mut stream, _listener) =
        connect_client(base_options("conf-b-inbound-subid0"), &plain_connack()).await;
    let mut rx = subscribe(&client, &mut stream, "t/sid", qos_options(QoS::AtMostOnce)).await;
    stream
        .write_all(&raw_publish(
            0,
            false,
            "t/sid",
            None,
            &subscription_id_prop(0),
            b"x",
        ))
        .await
        .unwrap();
    let delivered = drain(&mut rx, SHORT).await.len();
    let end = termination(&mut stream, MEDIUM).await;
    assert!(
        delivered == 0 && matches!(end, Termination::Closed | Termination::Disconnect(0x80..)),
        "[§3.3.2.3.8] inbound PUBLISH with Subscription Identifier 0 must be a Protocol Error; delivered={delivered} termination={end:?}"
    );
}

#[tokio::test]
async fn crosscheck2_mqtt_3_3_4_9_server_exceeding_client_receive_maximum_gets_disconnect_0x93() {
    let options = base_options("conf-b-rm-in").with_receive_maximum(1);
    let (client, mut stream, _listener) = connect_client(options, &plain_connack()).await;
    let mut rx = subscribe(&client, &mut stream, "t/rm", qos_options(QoS::ExactlyOnce)).await;
    stream
        .write_all(&raw_publish(2, false, "t/rm", Some(1), &[], b"1"))
        .await
        .unwrap();
    assert_eq!(
        expect_kind(&mut stream, PUBREC, MEDIUM).await.packet_id(),
        1
    );
    stream
        .write_all(&raw_publish(2, false, "t/rm", Some(2), &[], b"2"))
        .await
        .unwrap();
    let end = termination(&mut stream, MEDIUM).await;
    let delivered = drain(&mut rx, SHORT).await.len();
    assert!(
        end == Termination::Disconnect(0x93),
        "[MQTT-3.3.4-9 / §3.3.4] second unacknowledged QoS2 PUBLISH with client Receive Maximum 1 must draw DISCONNECT 0x93; termination={end:?} delivered={delivered}"
    );
}

#[tokio::test]
async fn mqtt_3_1_2_24_prose_inbound_packet_above_client_maximum_packet_size() {
    let mut options = base_options("conf-b-maxpkt");
    options.properties.maximum_packet_size = Some(128);
    let (client, mut stream, _listener) = connect_client(options, &plain_connack()).await;
    let mut rx = subscribe(&client, &mut stream, "t/big", qos_options(QoS::AtMostOnce)).await;
    stream
        .write_all(&raw_publish(0, false, "t/big", None, &[], &[b'x'; 1000]))
        .await
        .unwrap();
    let delivered = drain(&mut rx, SHORT).await.len();
    let end = termination(&mut stream, MEDIUM).await;
    assert!(
        delivered == 0 && matches!(end, Termination::Closed | Termination::Disconnect(0x95)),
        "[§3.1.2.11.4] packet larger than client Maximum Packet Size 128 must be a Protocol Error (DISCONNECT 0x95); delivered={delivered} termination={end:?}"
    );
}

#[tokio::test]
async fn crosscheck1_mqtt_3_3_4_7_session_resume_with_smaller_receive_maximum() {
    let (listener, url) = bind().await;
    let client = MqttClient::with_options(resumable_options("conf-b-resume-rm"));
    let connecting = client.clone();
    let connect_task = tokio::spawn(async move { connecting.connect(&url).await });
    let (mut first, _) = accept_session(
        &listener,
        &connack(false, |c| c.properties.set_receive_maximum(2)),
    )
    .await;
    connect_task.await.unwrap().expect("connect");

    let mut old = Vec::new();
    for topic in ["t/old/1", "t/old/2"] {
        let publisher = client.clone();
        old.push(tokio::spawn(async move {
            publisher.publish_qos1(topic, b"old".to_vec()).await
        }));
    }
    let (frames, _) = collect_frames(&mut first, SHORT).await;
    let old_ids: Vec<u16> = frames
        .iter()
        .filter(|f| f.kind() == PUBLISH)
        .map(Frame::packet_id)
        .collect();
    assert_eq!(
        old_ids.len(),
        2,
        "both QoS1 publishes reach the wire on RM=2"
    );
    drop(first);
    wait_until_disconnected(&client).await;

    let (mut second, connect) = accept_session(
        &listener,
        &connack(true, |c| c.properties.set_receive_maximum(1)),
    )
    .await;
    assert!(!connect.clean_start, "reconnect uses Clean Start 0");
    for _ in 0..100 {
        if client.is_connected().await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let mut new = Vec::new();
    for topic in ["t/new/1", "t/new/2"] {
        let publisher = client.clone();
        new.push(tokio::spawn(async move {
            publisher.publish_qos1(topic, b"new".to_vec()).await
        }));
    }

    let (frames, closed) = collect_frames(&mut second, Duration::from_millis(1500)).await;
    assert!(!closed, "client dropped the resumed connection");
    let unacked: Vec<(u16, bool)> = frames
        .iter()
        .filter(|f| f.kind() == PUBLISH)
        .map(|f| (f.packet_id(), f.dup()))
        .collect();
    assert!(
        unacked.len() <= 1,
        "[MQTT-3.3.4-7 / MQTT-4.9.0-2] client had {} unacknowledged QoS1 PUBLISH on the wire with server Receive Maximum 1: {unacked:?}",
        unacked.len()
    );

    let mut acked = 0;
    let mut pending = unacked;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while acked < old_ids.len() + 2 && tokio::time::Instant::now() < deadline {
        for (id, _) in pending.drain(..) {
            second.write_all(&ack_bytes(0x40, id)).await.unwrap();
            acked += 1;
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if let Next::Frame(frame) = next_frame(&mut second, left).await {
            if frame.kind() == PUBLISH {
                pending.push((frame.packet_id(), frame.dup()));
            }
        }
    }
    for handle in new {
        let outcome = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("[MQTT-4.9.0-2] new publish deadlocked after session resume");
        let result = outcome.expect("publish task must not panic");
        assert!(
            result.is_ok(),
            "new publish after resume failed: {result:?}"
        );
    }
    for handle in old {
        let outcome = tokio::time::timeout(Duration::from_secs(1), handle).await;
        if let Ok(joined) = outcome {
            assert!(
                !joined.is_err_and(|e| e.is_panic()),
                "publish task panicked"
            );
        }
    }
}

#[tokio::test]
async fn mqtt_4_4_0_1_mqtt_3_3_1_1_unacked_publish_resent_with_dup_on_session_resume() {
    let (listener, url) = bind().await;
    let client = MqttClient::with_options(resumable_options("conf-b-resend"));
    let connecting = client.clone();
    let connect_task = tokio::spawn(async move { connecting.connect(&url).await });
    let (mut first, _) = accept_session(&listener, &plain_connack()).await;
    connect_task.await.unwrap().expect("connect");

    let publisher = client.clone();
    let pending =
        tokio::spawn(async move { publisher.publish_qos1("t/resend", b"m".to_vec()).await });
    let original = expect_kind(&mut first, PUBLISH, MEDIUM).await;
    drop(first);
    wait_until_disconnected(&client).await;

    let (mut second, _) = accept_session(&listener, &connack(true, |_| {})).await;
    let resent = match next_frame(&mut second, MEDIUM).await {
        Next::Frame(frame) if frame.kind() == PUBLISH => Some(frame),
        _ => None,
    };
    pending.abort();
    let resent = resent.unwrap_or_else(|| {
        panic!(
            "[MQTT-4.4.0-1] client did not resend unacknowledged QoS1 PUBLISH id {} after reconnecting with Clean Start 0 and Session Present 1",
            original.packet_id()
        )
    });
    assert_eq!(
        resent.packet_id(),
        original.packet_id(),
        "[MQTT-4.4.0-1] original packet id"
    );
    assert!(
        resent.dup(),
        "[MQTT-3.3.1-1] re-delivered PUBLISH must have DUP=1"
    );
}

#[tokio::test]
async fn mqtt_4_9_0_1_send_quota_reinitialized_on_new_connection() {
    let (listener, url) = bind().await;
    let options = ConnectOptions::new("conf-b-quota")
        .with_automatic_reconnect(true)
        .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(200));
    let client = MqttClient::with_options(options);
    let connecting = client.clone();
    let connect_task = tokio::spawn(async move { connecting.connect(&url).await });
    let rm1 = connack(false, |c| c.properties.set_receive_maximum(1));
    let (mut first, _) = accept_session(&listener, &rm1).await;
    connect_task.await.unwrap().expect("connect");

    let timed_out = client
        .publish_qos1("t/quota", b"never-acked".to_vec())
        .await;
    assert!(
        matches!(timed_out, Err(MqttError::Timeout)),
        "unacknowledged publish times out: {timed_out:?}"
    );
    let _ = collect_frames(&mut first, Duration::from_millis(50)).await;
    drop(first);
    wait_until_disconnected(&client).await;

    let (mut second, _) = accept_session(&listener, &rm1).await;
    for _ in 0..100 {
        if client.is_connected().await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let publisher = client.clone();
    let fresh =
        tokio::spawn(async move { publisher.publish_qos1("t/quota", b"fresh".to_vec()).await });
    let sent = matches!(
        next_frame(&mut second, Duration::from_secs(2)).await,
        Next::Frame(ref f) if f.kind() == PUBLISH
    );
    fresh.abort();
    assert!(
        sent,
        "[MQTT-4.9.0-1] new connection (Session Present 0, Receive Maximum 1) started with a send quota of 0: a permit held by a timed-out publish on the previous connection was never reclaimed"
    );
}

async fn queued_flush(client_id: &str) -> Vec<Frame> {
    let (listener, url) = bind().await;
    let options = ConnectOptions::new(client_id)
        .with_clean_start(false)
        .with_session_expiry_interval(300)
        .with_automatic_reconnect(true)
        .with_reconnect_delay(Duration::from_millis(1500), Duration::from_millis(2000));
    let client = MqttClient::with_options(options);
    client.set_queue_on_disconnect(true).await;
    let connecting = client.clone();
    let connect_task = tokio::spawn(async move { connecting.connect(&url).await });
    let (first, _) = accept_session(&listener, &plain_connack()).await;
    connect_task.await.unwrap().expect("connect");
    drop(first);
    wait_until_disconnected(&client).await;

    for i in 0..3u8 {
        let queued = client
            .publish_qos1("t/queued", vec![i])
            .await
            .expect("publish while disconnected is queued");
        assert!(queued.packet_id().is_some());
    }

    let (mut second, _) = accept_session(
        &listener,
        &connack(true, |c| c.properties.set_receive_maximum(1)),
    )
    .await;
    let (frames, _) = collect_frames(&mut second, Duration::from_millis(1500)).await;
    frames.into_iter().filter(|f| f.kind() == PUBLISH).collect()
}

#[tokio::test]
async fn mqtt_3_3_4_7_queued_messages_flushed_within_server_receive_maximum() {
    let publishes = queued_flush("conf-b-queue-rm").await;
    assert!(
        !publishes.is_empty(),
        "queued messages were never sent after reconnect"
    );
    assert!(
        publishes.len() <= 1,
        "[MQTT-3.3.4-7] client flushed {} unacknowledged queued QoS1 PUBLISH packets with server Receive Maximum 1",
        publishes.len()
    );
}

#[tokio::test]
async fn mqtt_4_3_2_2_queued_message_first_transmission_has_dup_zero() {
    let publishes = queued_flush("conf-b-queue-dup").await;
    let first = publishes
        .first()
        .expect("queued messages were never sent after reconnect");
    assert!(
        !first.dup(),
        "[MQTT-4.3.2-2] queued QoS1 message sent for the first time with DUP=1 (packet id {})",
        first.packet_id()
    );
}

#[tokio::test]
async fn mqtt_3_3_4_8_disconnect_not_delayed_by_exhausted_send_quota() {
    let rm1 = connack(false, |c| c.properties.set_receive_maximum(1));
    let (client, mut stream, _listener) =
        connect_client(base_options("conf-b-nodelay"), &rm1).await;

    let mut blocked = Vec::new();
    for i in 0..2u8 {
        let publisher = client.clone();
        blocked.push(tokio::spawn(async move {
            publisher.publish_qos1("t/hold", vec![i]).await
        }));
    }
    let _ = expect_kind(&mut stream, PUBLISH, MEDIUM).await;

    let subscriber = client.clone();
    let sub_task = tokio::spawn(async move { subscriber.subscribe("t/other", |_| {}).await });
    let sub = expect_kind(&mut stream, SUBSCRIBE, Duration::from_secs(2)).await;
    stream
        .write_all(&suback_bytes(sub.packet_id(), 0))
        .await
        .unwrap();
    sub_task
        .await
        .unwrap()
        .expect("subscribe while quota is exhausted");

    let disconnecting = client.clone();
    let disconnect_task = tokio::spawn(async move { disconnecting.disconnect().await });
    let end = termination(&mut stream, Duration::from_secs(2)).await;
    disconnect_task.abort();
    for handle in blocked {
        handle.abort();
    }
    assert!(
        matches!(end, Termination::Disconnect(_)),
        "[MQTT-3.3.4-8] DISCONNECT was delayed while the send quota was exhausted (nothing within 2s): {end:?}"
    );
}

fn deferred_options(client_id: &str, receive_maximum: u16) -> ConnectOptions {
    ConnectOptions::new(client_id)
        .with_clean_start(false)
        .with_session_expiry_interval(300)
        .with_receive_maximum(receive_maximum)
        .with_deferred_ack(true)
        .with_automatic_reconnect(false)
}

async fn deferred_reject_code(qos: QoS, reject_with: ReasonCode) -> Frame {
    let (client, mut stream, _listener) =
        connect_client(deferred_options("conf-b-reject", 10), &plain_connack()).await;
    let subscriber = client.clone();
    let task = tokio::spawn(async move {
        subscriber
            .subscribe_with_ack("t/rej", qos_options(qos), move |_, token: AckToken| {
                token.reject(reject_with);
            })
            .await
    });
    let sub = expect_kind(&mut stream, SUBSCRIBE, MEDIUM).await;
    stream
        .write_all(&suback_bytes(sub.packet_id(), qos as u8))
        .await
        .unwrap();
    task.await.unwrap().expect("subscribe_with_ack");
    stream
        .write_all(&raw_publish(qos as u8, false, "t/rej", Some(21), &[], b"x"))
        .await
        .unwrap();
    let kind = if qos == QoS::AtLeastOnce {
        PUBACK
    } else {
        PUBREC
    };
    expect_kind(&mut stream, kind, MEDIUM).await
}

#[tokio::test]
async fn mqtt_3_4_2_1_deferred_reject_puback_reason_code_must_be_valid() {
    let ack = deferred_reject_code(QoS::AtLeastOnce, ReasonCode::ServerBusy).await;
    let code = ReasonCode::from_u8(ack.reason_code()).expect("known reason code");
    assert!(
        mqtt5::packet::is_valid_publish_ack_reason_code(code),
        "[MQTT-3.4.2-1] AckToken::reject put PUBACK reason code {code:?} (0x{:02X}) on the wire, which is not a PUBACK Reason Code",
        ack.reason_code()
    );
}

#[tokio::test]
async fn mqtt_3_5_2_1_deferred_reject_pubrec_reason_code_must_be_valid() {
    let ack = deferred_reject_code(QoS::ExactlyOnce, ReasonCode::ServerBusy).await;
    let code = ReasonCode::from_u8(ack.reason_code()).expect("known reason code");
    assert!(
        mqtt5::packet::is_valid_publish_ack_reason_code(code),
        "[MQTT-3.5.2-1] AckToken::reject put PUBREC reason code {code:?} (0x{:02X}) on the wire, which is not a PUBREC Reason Code",
        ack.reason_code()
    );
}

#[tokio::test]
async fn mqtt_4_4_0_1_deferred_ack_redelivery_at_receive_maximum_after_resume() {
    let (listener, url) = bind().await;
    let mut options = deferred_options("conf-b-deferred-resume", 1);
    options.reconnect_config.enabled = true;
    options.reconnect_config.initial_delay = Duration::from_millis(100);
    options.reconnect_config.max_delay = Duration::from_millis(200);
    let client = MqttClient::with_options(options);
    let connecting = client.clone();
    let connect_task = tokio::spawn(async move { connecting.connect(&url).await });
    let (mut first, _) = accept_session(&listener, &plain_connack()).await;
    connect_task.await.unwrap().expect("connect");

    let held: Arc<Mutex<Vec<AckToken>>> = Arc::new(Mutex::new(Vec::new()));
    let deliveries = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let subscriber = client.clone();
    let (held_cb, deliveries_cb) = (Arc::clone(&held), Arc::clone(&deliveries));
    let sub_task = tokio::spawn(async move {
        subscriber
            .subscribe_with_ack(
                "t/deferred",
                qos_options(QoS::AtLeastOnce),
                move |publish, token| {
                    deliveries_cb.lock().unwrap().push(publish.payload.to_vec());
                    held_cb.lock().unwrap().push(token);
                },
            )
            .await
    });
    let sub = expect_kind(&mut first, SUBSCRIBE, MEDIUM).await;
    first
        .write_all(&suback_bytes(sub.packet_id(), 1))
        .await
        .unwrap();
    sub_task.await.unwrap().expect("subscribe_with_ack");

    first
        .write_all(&raw_publish(1, false, "t/deferred", Some(1), &[], b"held"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        held.lock().unwrap().len(),
        1,
        "first delivery holds its token"
    );
    drop(first);
    wait_until_disconnected(&client).await;

    let (mut second, _) = accept_session(&listener, &connack(true, |_| {})).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    second
        .write_all(&raw_publish(1, true, "t/deferred", Some(1), &[], b"held"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let tokens: Vec<AckToken> = held.lock().unwrap().drain(..).collect();
    for token in tokens {
        token.ack();
    }
    let _ = collect_frames(&mut second, Duration::from_millis(300)).await;

    second
        .write_all(&raw_publish(1, false, "t/deferred", Some(2), &[], b"next"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let got_next = deliveries
        .lock()
        .unwrap()
        .iter()
        .any(|payload| payload == b"next");
    assert!(
        got_next,
        "[MQTT-4.4.0-1 / §4.9] a resumed-session DUP redelivery of a still-held message at client Receive Maximum 1 broke the connection: later PUBLISH id 2 was never delivered"
    );
}

#[tokio::test]
async fn crosscheck4_topic_alias_boundary_no_panic_via_public_api() {
    let outcome = tokio::spawn(async move {
        let mut inbound = TopicAliasManager::new(u16::MAX);
        inbound
            .register_alias(u16::MAX, "t/max")
            .expect("alias == maximum is valid");
        let mut outbound = TopicAliasManager::new(u16::MAX);
        let mut last = None;
        for i in 0..u32::from(u16::MAX) {
            last = outbound.get_or_create_alias(&format!("t/{i}"));
        }
        last
    })
    .await;
    match outcome {
        Ok(last) => assert_eq!(last, Some(u16::MAX)),
        Err(e) => panic!(
            "[cross-check 4] mqtt5::session::TopicAliasManager panicked assigning outbound Topic Alias == Topic Alias Maximum (65535): {e}"
        ),
    }

    let (client, mut stream, _listener) = connect_client(
        base_options("conf-b-alias-boundary-wire"),
        &connack(false, |c| c.properties.set_topic_alias_maximum(u16::MAX)),
    )
    .await;
    let properties = PublishProperties {
        topic_alias: Some(u16::MAX),
        ..Default::default()
    };
    client
        .publish_with_options("t/max", b"x".to_vec(), qos0_with(properties))
        .await
        .expect("publish with alias == server maximum");
    let frame = expect_kind(&mut stream, PUBLISH, MEDIUM).await;
    assert_eq!(frame.publish().topic_alias(), Some(u16::MAX));
}

#[tokio::test]
async fn mqtt_3_2_2_5_queued_acks_discarded_when_session_not_present() {
    let (listener, url) = bind().await;
    let mut options = deferred_options("conf-b-sp0-acks", 4);
    options.reconnect_config.enabled = true;
    options.reconnect_config.initial_delay = Duration::from_millis(100);
    options.reconnect_config.max_delay = Duration::from_millis(200);
    let client = MqttClient::with_options(options);
    let connecting = client.clone();
    let connect_task = tokio::spawn(async move { connecting.connect(&url).await });
    let (mut first, _) = accept_session(&listener, &plain_connack()).await;
    connect_task.await.unwrap().expect("connect");

    let held: Arc<Mutex<Vec<AckToken>>> = Arc::new(Mutex::new(Vec::new()));
    let subscriber = client.clone();
    let held_cb = Arc::clone(&held);
    let sub_task = tokio::spawn(async move {
        subscriber
            .subscribe_with_ack(
                "t/deferred",
                qos_options(QoS::AtLeastOnce),
                move |_, token| held_cb.lock().unwrap().push(token),
            )
            .await
    });
    let sub = expect_kind(&mut first, SUBSCRIBE, MEDIUM).await;
    first
        .write_all(&suback_bytes(sub.packet_id(), 1))
        .await
        .unwrap();
    sub_task.await.unwrap().expect("subscribe_with_ack");
    let _auto = subscribe(&client, &mut first, "t/auto", qos_options(QoS::AtLeastOnce)).await;

    first
        .write_all(&raw_publish(1, false, "t/deferred", Some(1), &[], b"held"))
        .await
        .unwrap();
    first
        .write_all(&raw_publish(1, false, "t/auto", Some(2), &[], b"auto"))
        .await
        .unwrap();
    let (before, _) = collect_frames(&mut first, Duration::from_millis(400)).await;
    assert!(
        before.iter().all(|f| f.kind() != PUBACK),
        "setup: the automatic PUBACK for id 2 is held behind the unresolved token"
    );
    assert_eq!(held.lock().unwrap().len(), 1, "setup: token held");
    drop(first);
    wait_until_disconnected(&client).await;

    let (mut second, _) = accept_session(&listener, &plain_connack()).await;
    for _ in 0..100 {
        if client.is_connected().await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let tokens: Vec<AckToken> = held.lock().unwrap().drain(..).collect();
    for token in tokens {
        token.ack();
    }
    let (frames, _) = collect_frames(&mut second, Duration::from_millis(800)).await;
    let pubacks: Vec<u16> = frames
        .iter()
        .filter(|f| f.kind() == PUBACK)
        .map(Frame::packet_id)
        .collect();
    assert!(
        pubacks.is_empty(),
        "[MQTT-3.2.2-5] acknowledgements from the discarded session were sent on a Session Present=0 connection: {pubacks:?}"
    );
}
