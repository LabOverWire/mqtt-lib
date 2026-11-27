use mqtt5::{
    ConnectOptions, ConnectionEvent, Delivery, MqttClient, PublishOptions, PublishOutcome,
    PublishProperties, PublishResult, QoS,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;

const CONNACK: [u8; 5] = [0x20, 0x03, 0x00, 0x00, 0x00];
const PUBLISH_TYPE: u8 = 3;
const LARGE_PAYLOAD: usize = 16 * 1024 * 1024;
const SERVER_STALL: Duration = Duration::from_millis(1500);
const CANCEL_AFTER: Duration = Duration::from_millis(300);
const SERVER_IDLE: Duration = Duration::from_secs(1);
const SETTLE_WITHIN: Duration = Duration::from_secs(10);
const MIB: isize = 1 << 20;

struct ThreadCountingAllocator;

thread_local! {
    static THREAD_HEAP: Cell<isize> = const { Cell::new(0) };
}

fn count_on_thread(delta: isize) {
    let _ = THREAD_HEAP.try_with(|heap| heap.set(heap.get() + delta));
}

fn signed(size: usize) -> isize {
    isize::try_from(size).unwrap_or(isize::MAX)
}

unsafe impl GlobalAlloc for ThreadCountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_on_thread(signed(layout.size()));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        count_on_thread(-signed(layout.size()));
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count_on_thread(signed(new_size) - signed(layout.size()));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: ThreadCountingAllocator = ThreadCountingAllocator;

fn thread_heap() -> isize {
    THREAD_HEAP.with(Cell::get)
}

#[derive(Debug, PartialEq)]
struct Received {
    publishes: Vec<String>,
    unfinished: Option<UnfinishedFrame>,
    finished: bool,
}

#[derive(Debug, PartialEq)]
struct UnfinishedFrame {
    declared_len: Option<usize>,
    received_len: usize,
}

fn frame_bounds(bytes: &[u8]) -> Option<(u8, usize, usize)> {
    let first = *bytes.first()?;
    let mut remaining = 0usize;
    let mut multiplier = 1usize;
    for (index, byte) in bytes.iter().enumerate().skip(1).take(4) {
        remaining += usize::from(byte & 0x7f) * multiplier;
        if byte & 0x80 == 0 {
            return Some((first, index + 1, index + 1 + remaining));
        }
        multiplier *= 128;
    }
    None
}

fn complete_frame(bytes: &[u8]) -> Option<(u8, usize, usize)> {
    frame_bounds(bytes).filter(|&(_, _, total)| bytes.len() >= total)
}

fn publish_topic_and_id(first: u8, body: &[u8]) -> (String, Option<u16>) {
    let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
    let topic = String::from_utf8_lossy(&body[2..2 + topic_len]).into_owned();
    let packet_id =
        (first & 0x06 != 0).then(|| u16::from_be_bytes([body[2 + topic_len], body[3 + topic_len]]));
    (topic, packet_id)
}

async fn accept_connect<R, W>(reader: &mut R, writer: &mut W) -> Vec<u8>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    accept_connect_with(reader, writer, &CONNACK).await
}

async fn accept_connect_with<R, W>(reader: &mut R, writer: &mut W, connack: &[u8]) -> Vec<u8>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buffer = Vec::new();
    let mut chunk = vec![0u8; 4096];
    let connect_len = loop {
        if let Some((first, _, total)) = complete_frame(&buffer) {
            assert_eq!(first >> 4, 1, "first packet must be CONNECT");
            break total;
        }
        let read = reader.read(&mut chunk).await.unwrap();
        assert!(read > 0, "connection closed before CONNECT");
        buffer.extend_from_slice(&chunk[..read]);
    };
    buffer.drain(..connect_len);
    writer.write_all(connack).await.unwrap();
    writer.flush().await.unwrap();
    buffer
}

async fn read_publishes<R, W>(reader: &mut R, mut buffer: Vec<u8>, acks: &mut W) -> Received
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut chunk = vec![0u8; 1 << 20];
    let mut publishes = Vec::new();
    let finished = loop {
        while let Some((first, header_len, total)) = complete_frame(&buffer) {
            if first >> 4 == PUBLISH_TYPE {
                let (topic, packet_id) = publish_topic_and_id(first, &buffer[header_len..total]);
                if let Some(packet_id) = packet_id {
                    let [high, low] = packet_id.to_be_bytes();
                    acks.write_all(&[0x40, 0x02, high, low]).await.unwrap();
                    acks.flush().await.unwrap();
                }
                publishes.push(topic);
            }
            buffer.drain(..total);
        }
        match tokio::time::timeout(SERVER_IDLE, reader.read(&mut chunk)).await {
            Ok(Ok(0)) => break true,
            Ok(Ok(read)) => buffer.extend_from_slice(&chunk[..read]),
            _ => break false,
        }
    };
    let unfinished = (!buffer.is_empty()).then(|| UnfinishedFrame {
        declared_len: frame_bounds(&buffer).map(|(_, _, total)| total),
        received_len: buffer.len(),
    });
    Received {
        publishes,
        unfinished,
        finished,
    }
}

async fn serve_stalled<R, W>(mut reader: R, mut writer: W) -> Received
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let buffer = accept_connect(&mut reader, &mut writer).await;
    tokio::time::sleep(SERVER_STALL).await;
    read_publishes(&mut reader, buffer, &mut writer).await
}

async fn stalled_tcp_server() -> (String, tokio::task::JoinHandle<Received>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (reader, writer) = socket.into_split();
        serve_stalled(reader, writer).await
    });
    (url, server)
}

fn qos1() -> PublishOptions {
    PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    }
}

fn large_payload() -> Vec<u8> {
    vec![7u8; LARGE_PAYLOAD]
}

async fn connected_client(name: &str, url: &str) -> MqttClient {
    let options = ConnectOptions::new(name).with_automatic_reconnect(false);
    let client = MqttClient::with_options(options.clone());
    client.set_insecure_tls(true).await;
    client.connect_with_options(url, options).await.unwrap();
    client
}

fn assert_only_complete_packets(received: &Received, expected: &[&str]) {
    assert_eq!(
        received.unfinished, None,
        "the server was left with an unfinished frame; packets parsed before it: {:?}",
        received.publishes
    );
    assert_eq!(received.publishes, expected);
}

async fn cancelled_publish_then_publish(
    client: &MqttClient,
    big_topic: &str,
    next_topic: &str,
) -> mqtt5::Result<PublishResult> {
    let cancelled = tokio::time::timeout(
        CANCEL_AFTER,
        client.publish_with_options(big_topic, large_payload(), qos1()),
    )
    .await;
    assert!(
        cancelled.is_err(),
        "the large publish must still be writing when it is cancelled"
    );
    tokio::time::timeout(
        SETTLE_WITHIN,
        client.publish_with_options(next_topic, b"next".to_vec(), qos1()),
    )
    .await
    .expect("the publish after a cancelled one must settle")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publish_after_a_cancelled_publish_is_framed_and_acknowledged() {
    let (url, server) = stalled_tcp_server().await;
    let client = connected_client("cancel-publish", &url).await;
    let next = cancelled_publish_then_publish(&client, "t/big", "t/next").await;
    let received = server.await.unwrap();

    assert_only_complete_packets(&received, &["t/big", "t/next"]);
    assert!(
        matches!(next, Ok(PublishResult::Sent(Delivery::AtLeastOnce { .. }))),
        "{next:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn begin_publish_after_a_cancelled_begin_publish_is_framed_and_acknowledged() {
    let (url, server) = stalled_tcp_server().await;
    let client = connected_client("cancel-begin", &url).await;
    let cancelled = tokio::time::timeout(
        CANCEL_AFTER,
        client.begin_publish("t/big", large_payload(), qos1()),
    )
    .await;
    assert!(
        cancelled.is_err(),
        "the large publish must still be writing when it is cancelled"
    );
    let next = tokio::time::timeout(SETTLE_WITHIN, async {
        match client
            .begin_publish("t/next", b"next".to_vec(), qos1())
            .await
        {
            Ok(handle) => Ok(handle.outcome().await),
            Err(e) => Err(e),
        }
    })
    .await
    .expect("the publish after a cancelled one must settle");
    let received = server.await.unwrap();

    assert_only_complete_packets(&received, &["t/big", "t/next"]);
    assert!(
        matches!(
            next,
            Ok(PublishOutcome::Delivered(Delivery::AtLeastOnce { .. }))
        ),
        "{next:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_publish_frame_is_completed_without_another_write() {
    let (url, server) = stalled_tcp_server().await;
    let client = connected_client("cancel-alone", &url).await;
    let cancelled = tokio::time::timeout(
        CANCEL_AFTER,
        client.publish_with_options("t/big", large_payload(), qos1()),
    )
    .await;
    assert!(
        cancelled.is_err(),
        "the large publish must still be writing when it is cancelled"
    );
    let received = server.await.unwrap();

    assert_only_complete_packets(&received, &["t/big"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publish_cancelled_while_waiting_for_the_connection_is_still_sent() {
    let (url, server) = stalled_tcp_server().await;
    let client = connected_client("cancel-waiting", &url).await;
    let writing = client.clone();
    let first = tokio::spawn(async move {
        match writing
            .begin_publish("t/big", large_payload(), qos1())
            .await
        {
            Ok(handle) => Ok(handle.outcome().await),
            Err(e) => Err(e),
        }
    });
    tokio::time::sleep(CANCEL_AFTER).await;
    let cancelled = tokio::time::timeout(
        CANCEL_AFTER,
        client.publish_with_options("t/waiting", b"waiting".to_vec(), qos1()),
    )
    .await;
    assert!(
        cancelled.is_err(),
        "the publish must still be waiting for the connection when it is cancelled"
    );
    let received = server.await.unwrap();
    let first = first.await.unwrap();

    assert_only_complete_packets(&received, &["t/big", "t/waiting"]);
    assert!(
        matches!(
            first,
            Ok(PublishOutcome::Delivered(Delivery::AtLeastOnce { .. }))
        ),
        "{first:?}"
    );
}

const CONNACK_TOPIC_ALIAS_MAXIMUM_5: [u8; 8] = [0x20, 0x06, 0x00, 0x00, 0x03, 0x22, 0x00, 0x05];

#[derive(Debug)]
struct AliasedPublish {
    topic: String,
    alias: Option<u16>,
    resolved: String,
    payload: String,
    packet_len: usize,
}

fn parse_qos0_publish(body: &[u8]) -> (String, Option<u16>, Vec<u8>) {
    let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
    let topic = String::from_utf8_lossy(&body[2..2 + topic_len]).into_owned();
    let properties_at = 3 + topic_len;
    let properties_len = usize::from(body[2 + topic_len]);
    let properties = &body[properties_at..properties_at + properties_len];
    let alias = (properties.len() >= 3 && properties[0] == 0x23)
        .then(|| u16::from_be_bytes([properties[1], properties[2]]));
    (
        topic,
        alias,
        body[properties_at + properties_len..].to_vec(),
    )
}

async fn alias_server() -> (String, tokio::task::JoinHandle<Vec<AliasedPublish>>) {
    alias_server_with(CONNACK_TOPIC_ALIAS_MAXIMUM_5.to_vec()).await
}

async fn alias_server_with(
    connack: Vec<u8>,
) -> (String, tokio::task::JoinHandle<Vec<AliasedPublish>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (mut reader, mut writer) = socket.split();
        let mut buffer = Vec::new();
        let mut chunk = vec![0u8; 1 << 20];
        let connect_len = loop {
            if let Some((_, _, total)) = complete_frame(&buffer) {
                break total;
            }
            let read = reader.read(&mut chunk).await.unwrap();
            assert!(read > 0, "connection closed before CONNECT");
            buffer.extend_from_slice(&chunk[..read]);
        };
        buffer.drain(..connect_len);
        writer.write_all(&connack).await.unwrap();
        tokio::time::sleep(SERVER_STALL).await;
        let mut aliases: HashMap<u16, String> = HashMap::new();
        let mut seen = Vec::new();
        loop {
            while let Some((first, header_len, total)) = complete_frame(&buffer) {
                if first >> 4 == PUBLISH_TYPE {
                    let (topic, alias, payload) = parse_qos0_publish(&buffer[header_len..total]);
                    if let (Some(alias), false) = (alias, topic.is_empty()) {
                        aliases.insert(alias, topic.clone());
                    }
                    let resolved = if topic.is_empty() {
                        alias
                            .and_then(|alias| aliases.get(&alias).cloned())
                            .unwrap_or_default()
                    } else {
                        topic.clone()
                    };
                    let payload = if payload.len() < 64 {
                        String::from_utf8_lossy(&payload).into_owned()
                    } else {
                        "<large>".to_string()
                    };
                    seen.push(AliasedPublish {
                        topic,
                        alias,
                        resolved,
                        payload,
                        packet_len: total,
                    });
                }
                buffer.drain(..total);
            }
            match tokio::time::timeout(SERVER_IDLE, reader.read(&mut chunk)).await {
                Ok(Ok(read)) if read > 0 => buffer.extend_from_slice(&chunk[..read]),
                _ => break,
            }
        }
        seen
    });
    (url, server)
}

fn with_alias(qos: QoS, alias: u16) -> PublishOptions {
    PublishOptions {
        qos,
        properties: PublishProperties {
            topic_alias: Some(alias),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn resolved_payloads(seen: &[AliasedPublish]) -> Vec<(String, String)> {
    seen.iter()
        .map(|publish| (publish.resolved.clone(), publish.payload.clone()))
        .collect()
}

async fn hold_the_connection(
    client: &MqttClient,
) -> tokio::task::JoinHandle<mqtt5::Result<PublishResult>> {
    let writing = client.clone();
    let large = tokio::spawn(async move { writing.publish_qos0("t/large", large_payload()).await });
    tokio::time::sleep(CANCEL_AFTER).await;
    large
}

async fn cancel_while_waiting(client: &MqttClient, topic: &str, payload: &[u8], alias: u16) {
    let cancelled = tokio::time::timeout(
        CANCEL_AFTER,
        client.publish_with_options(topic, payload.to_vec(), with_alias(QoS::AtMostOnce, alias)),
    )
    .await;
    assert!(
        cancelled.is_err(),
        "the publish must still be waiting for the connection when it is cancelled"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_alias_definition_keeps_its_alias_in_commit_order() {
    let (url, server) = alias_server().await;
    let client = connected_client("cancel-alias-define", &url).await;
    let large = hold_the_connection(&client).await;
    let cancelling = client.clone();
    let cancelled = tokio::spawn(async move {
        cancel_while_waiting(&cancelling, "t/a", b"for-a", 1).await;
    });
    tokio::time::sleep(CANCEL_AFTER / 3).await;
    let later =
        client.publish_with_options("t/b", b"for-b".to_vec(), with_alias(QoS::AtMostOnce, 1));
    let later = tokio::time::timeout(SETTLE_WITHIN, later).await.unwrap();
    cancelled.await.unwrap();
    large.await.unwrap().unwrap();
    let alias_only = client
        .publish_with_options("", b"alias-only".to_vec(), with_alias(QoS::AtMostOnce, 1))
        .await;
    let seen = server.await.unwrap();

    assert!(later.is_ok(), "{later:?}");
    assert!(alias_only.is_ok(), "{alias_only:?}");
    let cancelled_publish = seen
        .iter()
        .find(|publish| publish.payload == "for-a")
        .expect("the cancelled publish is still sent");
    assert_eq!(cancelled_publish.topic, "t/a");
    assert_eq!(cancelled_publish.alias, Some(1), "{seen:?}");
    assert_eq!(
        resolved_payloads(&seen),
        vec![
            ("t/large".to_string(), "<large>".to_string()),
            ("t/a".to_string(), "for-a".to_string()),
            ("t/b".to_string(), "for-b".to_string()),
            ("t/b".to_string(), "alias-only".to_string()),
        ]
    );
    assert_eq!(
        seen.last()
            .map(|publish| (publish.resolved.as_str(), publish.payload.as_str())),
        Some(("t/b", "alias-only")),
        "{:?}",
        resolved_payloads(&seen)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_alias_redefinition_is_applied_in_commit_order() {
    let (url, server) = alias_server().await;
    let client = connected_client("cancel-alias-redefine", &url).await;
    client
        .publish_with_options("t/x", b"for-x".to_vec(), with_alias(QoS::AtMostOnce, 1))
        .await
        .unwrap();
    let large = hold_the_connection(&client).await;
    cancel_while_waiting(&client, "t/a", b"for-a", 1).await;
    large.await.unwrap().unwrap();
    let alias_only = client
        .publish_with_options("", b"alias-only".to_vec(), with_alias(QoS::AtMostOnce, 1))
        .await;
    let seen = server.await.unwrap();

    assert!(alias_only.is_ok(), "{alias_only:?}");
    assert_eq!(
        resolved_payloads(&seen),
        vec![
            ("t/x".to_string(), "for-x".to_string()),
            ("t/large".to_string(), "<large>".to_string()),
            ("t/a".to_string(), "for-a".to_string()),
            ("t/a".to_string(), "alias-only".to_string()),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alias_defined_by_a_publish_cancelled_mid_write_is_kept_by_the_client() {
    let (url, server) = alias_server().await;
    let client = connected_client("cancel-alias-midframe", &url).await;
    let cancelled = tokio::time::timeout(
        CANCEL_AFTER,
        client.publish_with_options("t/large", large_payload(), with_alias(QoS::AtMostOnce, 2)),
    )
    .await;
    assert!(
        cancelled.is_err(),
        "the large publish must still be writing when it is cancelled"
    );
    let alias_only = tokio::time::timeout(
        SETTLE_WITHIN,
        client.publish_with_options("", b"alias-only".to_vec(), with_alias(QoS::AtMostOnce, 2)),
    )
    .await
    .unwrap();
    let seen = server.await.unwrap();

    assert!(alias_only.is_ok(), "{alias_only:?}");
    assert_eq!(
        resolved_payloads(&seen),
        vec![
            ("t/large".to_string(), "<large>".to_string()),
            ("t/large".to_string(), "alias-only".to_string()),
        ]
    );
}

const MAXIMUM_PACKET_SIZE: usize = 17 * 1024 * 1024;

fn connack_with_aliases_and_maximum_packet_size() -> Vec<u8> {
    let [a, b, c, d] = u32::try_from(MAXIMUM_PACKET_SIZE).unwrap().to_be_bytes();
    vec![
        0x20, 0x0B, 0x00, 0x00, 0x08, 0x22, 0x00, 0x05, 0x27, a, b, c, d,
    ]
}

fn long_topic() -> String {
    format!("t/{}", "x".repeat(4000))
}

fn assert_within_maximum_packet_size(seen: &[AliasedPublish]) {
    for publish in seen {
        assert!(
            publish.packet_len <= MAXIMUM_PACKET_SIZE,
            "a {} byte PUBLISH (topic {} bytes, alias {:?}) exceeds the Maximum Packet Size {MAXIMUM_PACKET_SIZE}",
            publish.packet_len,
            publish.topic.len(),
            publish.alias
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_alias_only_publish_stays_within_maximum_packet_size() {
    let (url, server) = alias_server_with(connack_with_aliases_and_maximum_packet_size()).await;
    let client = connected_client("cancel-alias-mps", &url).await;
    client
        .publish_with_options(
            &long_topic(),
            b"define".to_vec(),
            with_alias(QoS::AtMostOnce, 1),
        )
        .await
        .unwrap();
    let large = hold_the_connection(&client).await;
    let cancelled = tokio::time::timeout(
        CANCEL_AFTER,
        client.publish_with_options(
            "",
            vec![1u8; MAXIMUM_PACKET_SIZE - 100],
            with_alias(QoS::AtMostOnce, 1),
        ),
    )
    .await;
    assert!(
        cancelled.is_err(),
        "the publish must still be waiting for the connection when it is cancelled"
    );
    large.await.unwrap().unwrap();
    let seen = server.await.unwrap();

    assert_eq!(seen.len(), 3, "{:?}", resolved_payloads(&seen));
    assert_within_maximum_packet_size(&seen);
    assert_eq!(seen[2].topic, "");
    assert_eq!(seen[2].alias, Some(1));
    assert_eq!(seen[2].resolved, long_topic());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alias_only_publish_cancelled_mid_write_keeps_its_alias_within_maximum_packet_size() {
    let (url, server) = alias_server_with(connack_with_aliases_and_maximum_packet_size()).await;
    let client = connected_client("cancel-alias-midwrite-mps", &url).await;
    client
        .publish_with_options(
            &long_topic(),
            b"define".to_vec(),
            with_alias(QoS::AtMostOnce, 1),
        )
        .await
        .unwrap();
    let cancelled = tokio::time::timeout(
        CANCEL_AFTER,
        client.publish_with_options(
            "",
            vec![1u8; MAXIMUM_PACKET_SIZE - 100],
            with_alias(QoS::AtMostOnce, 1),
        ),
    )
    .await;
    assert!(
        cancelled.is_err(),
        "the publish must still be writing when it is cancelled"
    );
    let seen = server.await.unwrap();

    assert_eq!(seen.len(), 2, "{:?}", resolved_payloads(&seen));
    assert_eq!(seen[1].topic, "");
    assert_eq!(seen[1].alias, Some(1));
    assert_eq!(seen[1].resolved, long_topic());
    assert_within_maximum_packet_size(&seen);
}

async fn publishes_committed_while_stalled(options: PublishOptions) -> usize {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("mqtt://{}", socket.local_addr().unwrap());
    let listener = socket.listen(1).unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (mut reader, mut writer) = socket.into_split();
        let buffer =
            accept_connect_with(&mut reader, &mut writer, &CONNACK_TOPIC_ALIAS_MAXIMUM_5).await;
        tokio::time::sleep(SERVER_STALL * 3).await;
        read_publishes(&mut reader, buffer, &mut tokio::io::sink())
            .await
            .publishes
            .len()
    });
    let client = connected_client("room-overshoot", &url).await;
    let mut tasks = Vec::new();
    for _ in 0..300 {
        let publishing = client.clone();
        let options = options.clone();
        tasks.push(tokio::spawn(async move {
            let _ = tokio::time::timeout(
                SERVER_STALL / 2,
                publishing.publish_with_options("t/x", vec![1u8; 64 * 1024], options),
            )
            .await;
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    server.await.unwrap()
}

const DISCONNECT_WITHIN: Duration = Duration::from_secs(3);

async fn spawn_stuck_publishes(
    client: &MqttClient,
    count: usize,
    topic: &'static str,
    size: usize,
) -> Vec<tokio::task::JoinHandle<mqtt5::Result<PublishResult>>> {
    let tasks: Vec<_> = (0..count)
        .map(|_| {
            let publishing = client.clone();
            tokio::spawn(async move { publishing.publish_qos0(topic, vec![7u8; size]).await })
        })
        .collect();
    tokio::time::sleep(CANCEL_AFTER * 2).await;
    assert!(
        tasks.iter().any(|task| !task.is_finished()),
        "the publishes must be stuck on the stalled link"
    );
    tasks
}

async fn disconnect_while_publishes_are_stuck(
    client: &MqttClient,
    tasks: Vec<tokio::task::JoinHandle<mqtt5::Result<PublishResult>>>,
) -> (bool, Vec<String>) {
    let disconnected = tokio::time::timeout(DISCONNECT_WITHIN, client.disconnect())
        .await
        .is_ok();
    let mut outcomes = Vec::new();
    for task in tasks {
        let outcome = tokio::time::timeout(DISCONNECT_WITHIN, task).await;
        outcomes.push(match outcome {
            Ok(Ok(Ok(_))) => "sent".to_string(),
            Ok(Ok(Err(e))) => format!("error {e}"),
            other => format!("{other:?}"),
        });
    }
    (disconnected, outcomes)
}

fn assert_stuck_publishes_returned_errors(disconnected: bool, outcomes: &[String]) {
    assert!(
        disconnected,
        "disconnect blocked behind publishes stuck on the stalled link: {outcomes:?}"
    );
    assert!(
        outcomes
            .iter()
            .all(|outcome| outcome == "sent" || outcome.starts_with("error")),
        "every publish returns once disconnect completes: {outcomes:?}"
    );
    assert!(
        outcomes.iter().any(|outcome| outcome.starts_with("error")),
        "the stuck publishes return an error: {outcomes:?}"
    );
}

fn server_that_stops_reading_after_connack() -> (String, tokio::task::JoinHandle<()>) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("mqtt://{}", socket.local_addr().unwrap());
    let listener = socket.listen(1).unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (mut reader, mut writer) = socket.into_split();
        accept_connect(&mut reader, &mut writer).await;
        tokio::time::sleep(SETTLE_WITHIN).await;
    });
    (url, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_is_not_blocked_by_publishes_stuck_on_a_stalled_link() {
    let (url, server) = server_that_stops_reading_after_connack();
    let client = connected_client("disconnect-stalled-tcp", &url).await;
    let tasks = spawn_stuck_publishes(&client, 40, "t/x", 64 * 1024).await;

    let (disconnected, outcomes) = disconnect_while_publishes_are_stuck(&client, tasks).await;
    server.abort();

    assert_stuck_publishes_returned_errors(disconnected, &outcomes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disconnect_is_not_blocked_by_a_setting_change_queued_behind_stuck_publishes() {
    let (url, server) = server_that_stops_reading_after_connack();
    let client = connected_client("disconnect-behind-setting", &url).await;
    let tasks = spawn_stuck_publishes(&client, 40, "t/x", 64 * 1024).await;
    let setting = client.clone();
    let queued = tokio::spawn(async move { setting.set_queue_on_disconnect(true).await });
    tokio::time::sleep(CANCEL_AFTER).await;
    let queued_behind_publishes = !queued.is_finished();

    let (disconnected, outcomes) = disconnect_while_publishes_are_stuck(&client, tasks).await;
    let setting_applied = tokio::time::timeout(DISCONNECT_WITHIN, queued)
        .await
        .is_ok();
    server.abort();

    assert!(
        queued_behind_publishes,
        "the setting change must wait for the stuck publishes"
    );
    assert_stuck_publishes_returned_errors(disconnected, &outcomes);
    assert!(setting_applied, "the queued setting change never ran");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_abnormally_is_not_blocked_by_publishes_stuck_on_a_stalled_link() {
    let (url, server) = server_that_stops_reading_after_connack();
    let client = connected_client("abnormal-disconnect-stalled-tcp", &url).await;
    let tasks = spawn_stuck_publishes(&client, 40, "t/x", 64 * 1024).await;

    let disconnected = tokio::time::timeout(DISCONNECT_WITHIN, client.disconnect_abnormally())
        .await
        .is_ok();
    let mut returned = 0;
    for task in tasks {
        if tokio::time::timeout(DISCONNECT_WITHIN, task).await.is_ok() {
            returned += 1;
        }
    }
    server.abort();

    assert!(
        disconnected,
        "disconnect_abnormally blocked behind publishes stuck on the stalled link"
    );
    assert_eq!(
        returned, 40,
        "every stuck publish returns once the client disconnects"
    );
}

async fn stalled_link_kernel_capacity_in_publishes() -> usize {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = socket.local_addr().unwrap();
    let listener = socket.listen(1).unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        tokio::time::sleep(SERVER_STALL * 3).await;
        socket
    });
    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    let chunk = vec![1u8; 64 * 1024];
    let mut written = 0usize;
    while tokio::time::timeout(CANCEL_AFTER, client.write_all(&chunk))
        .await
        .is_ok()
    {
        written += chunk.len();
    }
    server.abort();
    written.div_ceil(chunk.len()) + 1
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_publishes_on_a_stalled_link_stay_within_the_pending_limit() {
    let kernel = stalled_link_kernel_capacity_in_publishes().await;
    let mut committed = Vec::new();
    for _ in 0..3 {
        committed.push((
            publishes_committed_while_stalled(with_alias(QoS::AtMostOnce, 1)).await,
            publishes_committed_while_stalled(qos1()).await,
        ));
    }
    let pending_or_taken = 2 * (1024 * 1024 / (64 * 1024) + 1);
    let bound = pending_or_taken + kernel;

    assert!(
        committed
            .iter()
            .all(|&(aliased, acknowledged)| aliased <= bound && acknowledged <= bound),
        "64 KiB publishes committed during each stall (aliased QoS 0, QoS 1): {committed:?}, \
         bound {bound} ({kernel} held by the kernel on a stalled link)"
    );
}

#[tokio::test]
async fn a_large_publish_does_not_keep_its_buffer_after_it_is_written() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut chunk = vec![0u8; 1 << 20];
        let read = socket.read(&mut chunk).await.unwrap();
        assert!(read > 0, "connection closed before CONNECT");
        socket.write_all(&CONNACK).await.unwrap();
        while matches!(socket.read(&mut chunk).await, Ok(read) if read > 0) {}
    });
    let client = connected_client("release-buffer", &url).await;
    let before = thread_heap();
    client
        .publish_qos0("t/huge", vec![7u8; 128 << 20])
        .await
        .unwrap();
    tokio::time::sleep(CANCEL_AFTER).await;
    let retained = thread_heap() - before;
    server.abort();

    assert!(
        retained < 16 * MIB,
        "a 128 MiB publish left {} MiB allocated after it was written",
        retained / MIB
    );
}

#[tokio::test]
async fn timed_out_qos0_publishes_on_a_stalled_link_stay_bounded() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut chunk = vec![0u8; 4096];
        let read = socket.read(&mut chunk).await.unwrap();
        assert!(read > 0, "connection closed before CONNECT");
        socket.write_all(&CONNACK).await.unwrap();
        tokio::time::sleep(SETTLE_WITHIN * 3).await;
    });
    let client = connected_client("cancel-growth", &url).await;
    let before = thread_heap();
    let mut cancelled = 0;
    for _ in 0..1000 {
        if tokio::time::timeout(
            Duration::from_millis(1),
            client.publish_qos0("t/x", vec![1u8; 64 * 1024]),
        )
        .await
        .is_err()
        {
            cancelled += 1;
        }
    }
    let retained = thread_heap() - before;
    server.abort();

    assert!(cancelled > 900, "only {cancelled} publishes were cancelled");
    assert!(
        retained < 16 * MIB,
        "{cancelled} timed-out QoS 0 publishes left {} MiB buffered in the client",
        retained / MIB
    );
}

async fn server_reading_after(delay: Duration) -> (String, tokio::task::JoinHandle<Received>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (mut reader, mut writer) = socket.into_split();
        let buffer = accept_connect(&mut reader, &mut writer).await;
        tokio::time::sleep(delay).await;
        read_publishes(&mut reader, buffer, &mut tokio::io::sink()).await
    });
    (url, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qos0_publish_that_returns_an_error_on_disconnect_is_not_delivered() {
    let (url, server) = server_reading_after(CANCEL_AFTER + CANCEL_AFTER / 3).await;
    let client = connected_client("qos0-disconnect", &url).await;
    let publishing = client.clone();
    let publish =
        tokio::spawn(async move { publishing.publish_qos0("t/big", large_payload()).await });
    tokio::time::sleep(CANCEL_AFTER / 2).await;
    assert!(!publish.is_finished(), "the publish must still be writing");
    let disconnected = tokio::time::timeout(SETTLE_WITHIN / 2, client.disconnect()).await;
    let outcome = publish.await.unwrap();
    let received = server.await.unwrap();
    let delivered = received.publishes.iter().any(|topic| topic == "t/big");

    assert!(disconnected.is_ok(), "disconnect did not complete");
    assert!(
        outcome.is_ok() || !delivered,
        "the publish returned {outcome:?}, yet the server received it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qos0_publish_written_before_disconnect_is_ok_and_delivered() {
    let (url, server) = server_reading_after(Duration::ZERO).await;
    let client = connected_client("qos0-before-disconnect", &url).await;
    let outcome = client.publish_qos0("t/big", large_payload()).await;
    let disconnected = tokio::time::timeout(SETTLE_WITHIN / 2, client.disconnect()).await;
    let received = server.await.unwrap();

    assert!(outcome.is_ok(), "{outcome:?}");
    assert!(disconnected.is_ok(), "disconnect did not complete");
    assert!(received.publishes.iter().any(|topic| topic == "t/big"));
}

fn server_stalled_then_watching_for_close() -> (String, tokio::task::JoinHandle<bool>) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("mqtt://{}", socket.local_addr().unwrap());
    let listener = socket.listen(1).unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (mut reader, mut writer) = socket.into_split();
        accept_connect(&mut reader, &mut writer).await;
        tokio::time::sleep(SERVER_STALL).await;
        let mut chunk = vec![0u8; 1 << 20];
        loop {
            match tokio::time::timeout(SETTLE_WITHIN / 2, reader.read(&mut chunk)).await {
                Ok(Ok(0) | Err(_)) => return true,
                Ok(Ok(_)) => {}
                Err(_) => return false,
            }
        }
    });
    (url, server)
}

async fn disconnect_attempt_with_a_stuck_publish(cancel_after: Duration) -> (bool, bool, bool) {
    let (url, server) = server_stalled_then_watching_for_close();
    let client = connected_client("disconnect-while-closing", &url).await;
    let publishing = client.clone();
    let stuck =
        tokio::spawn(async move { publishing.publish_qos0("t/big", vec![7u8; 8 << 20]).await });
    tokio::time::sleep(CANCEL_AFTER).await;
    assert!(
        !stuck.is_finished(),
        "the publish must be stuck on the stalled link"
    );
    let cancelled = tokio::time::timeout(cancel_after, client.disconnect())
        .await
        .is_err();
    let connected = client.is_connected().await;
    let closed = server.await.unwrap();
    (cancelled, connected, closed)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_cancelled_while_closing_leaves_a_usable_client_or_a_closed_connection() {
    let (cancelled, connected, closed) =
        disconnect_attempt_with_a_stuck_publish(CANCEL_AFTER).await;

    assert!(
        cancelled,
        "the disconnect must still be closing when it is cancelled"
    );
    assert!(
        connected || closed,
        "after the cancelled disconnect is_connected() is false, yet the connection was never closed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_disconnect_left_to_finish_disconnects() {
    let (cancelled, connected, closed) =
        disconnect_attempt_with_a_stuck_publish(SETTLE_WITHIN / 2).await;

    assert!(!cancelled, "disconnect did not finish");
    assert!(!connected, "the client is still connected after disconnect");
    assert!(closed, "the connection was never closed");
}

const CONNACK_WITH_AUTH_METHOD: [u8; 13] = [
    0x20, 0x0b, 0x00, 0x00, 0x08, 0x15, 0x00, 0x05, b'P', b'L', b'A', b'I', b'N',
];

fn server_that_stops_reading(connack: &'static [u8]) -> (String, tokio::task::JoinHandle<()>) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("mqtt://{}", socket.local_addr().unwrap());
    let listener = socket.listen(1).unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (mut reader, mut writer) = socket.into_split();
        accept_connect_with(&mut reader, &mut writer, connack).await;
        tokio::time::sleep(SETTLE_WITHIN * 3).await;
    });
    (url, server)
}

async fn stuck_publish(
    client: &MqttClient,
) -> tokio::task::JoinHandle<mqtt5::Result<PublishResult>> {
    let publishing = client.clone();
    let publish =
        tokio::spawn(async move { publishing.publish_qos0("t/big", vec![7u8; 8 << 20]).await });
    tokio::time::sleep(CANCEL_AFTER).await;
    assert!(
        !publish.is_finished(),
        "the publish must be stuck on the stalled link"
    );
    publish
}

async fn disconnect_with_a_stuck_call<T: Send + 'static>(
    client: &MqttClient,
    call: Option<tokio::task::JoinHandle<T>>,
) -> bool {
    let publish = stuck_publish(client).await;
    tokio::time::sleep(CANCEL_AFTER).await;
    if let Some(call) = &call {
        assert!(!call.is_finished(), "the call must be stuck on its write");
    }
    let disconnected = tokio::time::timeout(SETTLE_WITHIN / 2, client.disconnect())
        .await
        .is_ok();
    publish.abort();
    if let Some(call) = call {
        call.abort();
    }
    disconnected
}

async fn disconnect_with_a_stuck_subscribe(subscribing: bool) -> bool {
    let (url, server) = server_that_stops_reading(&CONNACK);
    let client = connected_client("stuck-subscribe", &url).await;
    let subscribe = subscribing.then(|| {
        let client = client.clone();
        tokio::spawn(async move {
            tokio::time::sleep(CANCEL_AFTER).await;
            client.subscribe("t/sub", |_| {}).await
        })
    });
    let disconnected = disconnect_with_a_stuck_call(&client, subscribe).await;
    server.abort();
    disconnected
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_is_not_blocked_by_a_subscribe_stuck_on_a_stalled_write() {
    assert!(
        disconnect_with_a_stuck_subscribe(true).await,
        "disconnect did not complete while a SUBSCRIBE write was stuck on the stalled link"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_with_only_a_stuck_publish_completes() {
    assert!(disconnect_with_a_stuck_subscribe(false).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_is_not_blocked_by_an_unsubscribe_stuck_on_a_stalled_write() {
    let (url, server) = server_that_stops_reading(&CONNACK);
    let client = connected_client("stuck-unsubscribe", &url).await;
    let unsubscribing = client.clone();
    let unsubscribe = tokio::spawn(async move {
        tokio::time::sleep(CANCEL_AFTER).await;
        unsubscribing.unsubscribe("t/sub").await
    });
    let disconnected = disconnect_with_a_stuck_call(&client, Some(unsubscribe)).await;
    server.abort();

    assert!(
        disconnected,
        "disconnect did not complete while an UNSUBSCRIBE write was stuck on the stalled link"
    );
}

async fn disconnect_with_a_stuck_reauthenticate(reauthenticating: bool) -> bool {
    let (url, server) = server_that_stops_reading(&CONNACK_WITH_AUTH_METHOD);
    let mut options = ConnectOptions::new("stuck-reauthenticate").with_automatic_reconnect(false);
    options.properties.authentication_method = Some("PLAIN".to_string());
    let client = MqttClient::with_options(options.clone());
    client
        .set_auth_handler(mqtt5::PlainAuthHandler::new("u", "p"))
        .await;
    client.connect_with_options(&url, options).await.unwrap();
    let reauthenticate = reauthenticating.then(|| {
        let client = client.clone();
        tokio::spawn(async move {
            tokio::time::sleep(CANCEL_AFTER).await;
            client.reauthenticate().await
        })
    });
    let disconnected = disconnect_with_a_stuck_call(&client, reauthenticate).await;
    server.abort();
    disconnected
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_is_not_blocked_by_a_reauthenticate_stuck_on_a_stalled_write() {
    assert!(
        disconnect_with_a_stuck_reauthenticate(true).await,
        "disconnect did not complete while an AUTH write was stuck on the stalled link"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_with_auth_and_only_a_stuck_publish_completes() {
    assert!(disconnect_with_a_stuck_reauthenticate(false).await);
}

async fn disconnected_events_after(cancel_after: Option<Duration>) -> usize {
    let (url, server) = server_that_stops_reading(&CONNACK);
    let client = connected_client("disconnected-event", &url).await;
    let events = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counting = std::sync::Arc::clone(&events);
    client
        .on_connection_event(move |event| {
            if matches!(event, ConnectionEvent::Disconnected { .. }) {
                counting.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        })
        .await
        .unwrap();
    let publish = stuck_publish(&client).await;
    match cancel_after {
        Some(after) => assert!(
            tokio::time::timeout(after, client.disconnect())
                .await
                .is_err(),
            "the disconnect must still be closing when it is cancelled"
        ),
        None => client.disconnect().await.unwrap(),
    }
    tokio::time::sleep(SETTLE_WITHIN / 2).await;
    let connected = client.is_connected().await;
    publish.abort();
    server.abort();
    assert!(!connected, "the disconnect never finished");
    events.load(std::sync::atomic::Ordering::SeqCst)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_cancelled_while_closing_still_reports_disconnected() {
    let events = disconnected_events_after(Some(CANCEL_AFTER)).await;

    assert_eq!(
        events, 1,
        "the cancelled disconnect finished in the background but fired {events} Disconnected events"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_left_to_finish_reports_disconnected() {
    assert_eq!(disconnected_events_after(None).await, 1);
}

const SUBSCRIBE_TYPE: u8 = 8;
const UNSUBSCRIBE_TYPE: u8 = 10;
const AUTH_TYPE: u8 = 15;

fn server_recording_packet_types(
    connack: &'static [u8],
    reads_after: Duration,
) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("mqtt://{}", socket.local_addr().unwrap());
    let listener = socket.listen(1).unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (mut reader, mut writer) = socket.into_split();
        let mut buffer = accept_connect_with(&mut reader, &mut writer, connack).await;
        tokio::time::sleep(reads_after).await;
        let mut types = Vec::new();
        let mut chunk = vec![0u8; 1 << 20];
        loop {
            while let Some((first, _, total)) = complete_frame(&buffer) {
                if first >> 4 != PUBLISH_TYPE {
                    types.push(first >> 4);
                }
                buffer.drain(..total);
            }
            match tokio::time::timeout(SERVER_IDLE * 3, reader.read(&mut chunk)).await {
                Ok(Ok(read)) if read > 0 => buffer.extend_from_slice(&chunk[..read]),
                _ => break,
            }
        }
        drop(writer);
        types
    });
    (url, server)
}

async fn request_interrupted_by_disconnect<F, R>(
    client: MqttClient,
    server: tokio::task::JoinHandle<Vec<u8>>,
    request: F,
) -> (String, Vec<u8>)
where
    F: FnOnce(MqttClient) -> R,
    R: std::future::Future<Output = String> + Send + 'static,
{
    let publishing = client.clone();
    let publish =
        tokio::spawn(async move { publishing.publish_qos0("t/big", vec![7u8; 8 << 20]).await });
    tokio::time::sleep(CANCEL_AFTER).await;
    let pending = tokio::spawn(request(client.clone()));
    tokio::time::sleep(CANCEL_AFTER).await;
    let disconnected = tokio::time::timeout(SETTLE_WITHIN / 2, client.disconnect()).await;
    let outcome = tokio::time::timeout(SETTLE_WITHIN / 2, pending).await;
    publish.abort();
    let seen = server.await.unwrap();
    assert!(disconnected.is_ok(), "disconnect did not complete");
    (format!("{outcome:?}"), seen)
}

fn assert_not_connected_means_not_sent(outcome: &str, seen: &[u8], packet_type: u8) {
    let sent = seen.contains(&packet_type);
    assert!(
        sent,
        "the packet must reach the server while the disconnect closes the connection: {seen:?}, call returned {outcome}"
    );
    assert!(
        !(sent && outcome.contains("NotConnected")),
        "the call returned {outcome}, yet the server received the packet: {seen:?}"
    );
    assert!(
        !sent || outcome.contains("Ok(Ok(\"Ok(") || outcome.contains("OutcomeUnknown"),
        "the server received the packet, so the call must return Ok or an unknown outcome, not {outcome}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_that_returns_not_connected_was_not_sent() {
    let (url, server) = server_recording_packet_types(&CONNACK, CANCEL_AFTER * 3);
    let client = connected_client("subscribe-contract", &url).await;
    let (outcome, seen) = request_interrupted_by_disconnect(client, server, |client| async move {
        format!("{:?}", client.subscribe("t/sub", |_| {}).await)
    })
    .await;

    assert_not_connected_means_not_sent(&outcome, &seen, SUBSCRIBE_TYPE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsubscribe_that_returns_not_connected_was_not_sent() {
    let (url, server) = server_recording_packet_types(&CONNACK, CANCEL_AFTER * 3);
    let client = connected_client("unsubscribe-contract", &url).await;
    let (outcome, seen) = request_interrupted_by_disconnect(client, server, |client| async move {
        format!("{:?}", client.unsubscribe("t/sub").await)
    })
    .await;

    assert_not_connected_means_not_sent(&outcome, &seen, UNSUBSCRIBE_TYPE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reauthenticate_that_returns_not_connected_was_not_sent() {
    let (url, server) = server_recording_packet_types(&CONNACK_WITH_AUTH_METHOD, CANCEL_AFTER * 3);
    let mut options =
        ConnectOptions::new("reauthenticate-contract").with_automatic_reconnect(false);
    options.properties.authentication_method = Some("PLAIN".to_string());
    let client = MqttClient::with_options(options.clone());
    client
        .set_auth_handler(mqtt5::PlainAuthHandler::new("u", "p"))
        .await;
    client.connect_with_options(&url, options).await.unwrap();
    let (outcome, seen) = request_interrupted_by_disconnect(client, server, |client| async move {
        format!("{:?}", client.reauthenticate().await)
    })
    .await;

    assert_not_connected_means_not_sent(&outcome, &seen, AUTH_TYPE);
}

const AUTH_CONTINUE_PLAIN: [u8; 16] = [
    0xF0, 0x0e, 0x18, 0x0c, 0x15, 0x00, 0x05, b'P', b'L', b'A', b'I', b'N', 0x16, 0x00, 0x01, b'c',
];

struct CountingChallengeHandler(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl mqtt5::AuthHandler for CountingChallengeHandler {
    fn handle_challenge<'a>(
        &'a self,
        auth_method: &'a str,
        challenge_data: Option<&'a [u8]>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = mqtt5::Result<mqtt5::AuthResponse>> + Send + 'a>,
    > {
        if auth_method == "PLAIN" && challenge_data == Some(b"c".as_slice()) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        Box::pin(async move { Ok(mqtt5::AuthResponse::Continue(b"r".to_vec())) })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reader_keeps_reading_while_an_auth_answer_waits_behind_queued_publishes() {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("mqtt://{}", socket.local_addr().unwrap());
    let listener = socket.listen(1).unwrap();
    let (stalled_tx, stalled_rx) = tokio::sync::oneshot::channel::<()>();
    let (publish_stuck_tx, publish_stuck_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (mut reader, mut writer) = socket.into_split();
        let mut buffer =
            accept_connect_with(&mut reader, &mut writer, &CONNACK_WITH_AUTH_METHOD).await;
        let subscribe = next_of(&mut reader, &mut buffer, SUBSCRIBE_TYPE).await;
        writer
            .write_all(&[0x90, 0x04, subscribe[0], subscribe[1], 0x00, 0x00])
            .await
            .unwrap();
        next_of(&mut reader, &mut buffer, AUTH_TYPE).await;
        stalled_tx.send(()).unwrap();
        publish_stuck_rx.await.unwrap();
        writer.write_all(&AUTH_CONTINUE_PLAIN).await.unwrap();
        writer.write_all(&PUBLISH_T_SUB).await.unwrap();
        writer.flush().await.unwrap();
        tokio::time::sleep(SETTLE_WITHIN * 3).await;
        drop(reader);
    });

    let mut options = ConnectOptions::new("auth-answer-reader").with_automatic_reconnect(false);
    options.properties.authentication_method = Some("PLAIN".to_string());
    let client = MqttClient::with_options(options.clone());
    let challenges = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    client
        .set_auth_handler(CountingChallengeHandler(std::sync::Arc::clone(&challenges)))
        .await;
    client.connect_with_options(&url, options).await.unwrap();
    let (delivered, on_message) = counting_callback();
    client.subscribe("t/sub", on_message).await.unwrap();
    client.reauthenticate().await.unwrap();
    stalled_rx.await.unwrap();
    let publish = stuck_publish(&client).await;
    publish_stuck_tx.send(()).unwrap();

    wait_for_total(&[&challenges], 1).await;
    wait_for_total(&[&delivered], 1).await;
    let handled = challenges.load(std::sync::atomic::Ordering::SeqCst);
    let received = delivered.load(std::sync::atomic::Ordering::SeqCst);
    publish.abort();
    server.abort();

    assert_eq!(
        handled, 1,
        "the client must hand the server's PLAIN challenge to its auth handler"
    );
    assert_eq!(
        received,
        1,
        "the PUBLISH that follows an AUTH challenge must be delivered while the AUTH answer waits behind the client's own stuck publish, so the packet reader keeps reading instead of waiting for that answer's flush"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_is_not_delayed_by_a_subscribe_waiting_for_its_suback() {
    let (url, server) = server_recording_packet_types(&CONNACK, Duration::ZERO);
    let client = connected_client("suback-never-sent", &url).await;
    let subscribing = client.clone();
    let subscribe = tokio::spawn(async move { subscribing.subscribe("t/sub", |_| {}).await });
    tokio::time::sleep(CANCEL_AFTER).await;
    assert!(
        !subscribe.is_finished(),
        "the subscribe must wait for its SUBACK"
    );

    let disconnected = tokio::time::timeout(SERVER_IDLE, client.disconnect()).await;
    let outcome = tokio::time::timeout(SERVER_IDLE, subscribe).await;
    let seen = server.await.unwrap();

    assert!(
        disconnected.is_ok(),
        "disconnect waited behind a subscribe whose SUBACK never came"
    );
    assert!(seen.contains(&SUBSCRIBE_TYPE), "{seen:?}");
    assert!(
        format!("{outcome:?}").contains("OutcomeUnknown"),
        "a subscribe sent but never acknowledged returned {outcome:?}"
    );
}

fn server_dropping_the_first_connection_after_subscribe(
) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("mqtt://{}", socket.local_addr().unwrap());
    let listener = socket.listen(2).unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (mut reader, mut writer) = socket.into_split();
        let mut buffer = accept_connect(&mut reader, &mut writer).await;
        let mut chunk = vec![0u8; 4096];
        loop {
            match complete_frame(&buffer) {
                Some((first, _, _)) if first >> 4 == SUBSCRIBE_TYPE => break,
                Some((_, _, total)) => {
                    buffer.drain(..total);
                }
                None => {
                    let read = reader.read(&mut chunk).await.unwrap();
                    assert!(read > 0);
                    buffer.extend_from_slice(&chunk[..read]);
                }
            }
        }
        drop((reader, writer));
        let (socket, _) = tokio::time::timeout(SETTLE_WITHIN, listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut reader, mut writer) = socket.into_split();
        let mut buffer = accept_connect(&mut reader, &mut writer).await;
        let mut types = Vec::new();
        loop {
            while let Some((first, _, total)) = complete_frame(&buffer) {
                types.push(first >> 4);
                buffer.drain(..total);
            }
            match tokio::time::timeout(SERVER_IDLE * 2, reader.read(&mut chunk)).await {
                Ok(Ok(read)) if read > 0 => buffer.extend_from_slice(&chunk[..read]),
                _ => break,
            }
        }
        types
    });
    (url, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_with_an_unknown_outcome_is_not_restored_after_reconnect() {
    let (url, server) = server_dropping_the_first_connection_after_subscribe();
    let options = ConnectOptions::new("unknown-subscribe-restore")
        .with_automatic_reconnect(true)
        .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(200));
    let client = MqttClient::with_options(options.clone());
    client.connect_with_options(&url, options).await.unwrap();

    let subscribed =
        tokio::time::timeout(SETTLE_WITHIN / 2, client.subscribe("t/sub", |_| {})).await;
    let second_connection = server.await.unwrap();
    client.disconnect().await.ok();

    assert!(
        format!("{subscribed:?}").contains("OutcomeUnknown"),
        "a SUBSCRIBE whose connection closed before its SUBACK returned {subscribed:?}"
    );
    assert!(
        !second_connection.contains(&SUBSCRIBE_TYPE),
        "the subscribe was reported as failed, yet the reconnect sent it again: {second_connection:?}"
    );
}

const PUBLISH_T_SUB: [u8; 10] = [0x30, 0x08, 0x00, 0x05, b't', b'/', b's', b'u', b'b', 0x00];

async fn answer_until(
    reader: &mut tokio::net::tcp::OwnedReadHalf,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    buffer: &mut Vec<u8>,
    answers: &mut std::collections::VecDeque<Option<u8>>,
    stop_after: u8,
) -> Vec<u8> {
    let mut chunk = vec![0u8; 4096];
    let mut types = Vec::new();
    loop {
        while let Some((first, header_len, total)) = complete_frame(buffer) {
            let kind = first >> 4;
            types.push(kind);
            if kind == SUBSCRIBE_TYPE || kind == UNSUBSCRIBE_TYPE {
                let [high, low] = [buffer[header_len], buffer[header_len + 1]];
                if let Some(Some(reason)) = answers.pop_front() {
                    let ack = if kind == SUBSCRIBE_TYPE { 0x90 } else { 0xb0 };
                    writer
                        .write_all(&[ack, 0x04, high, low, 0x00, reason])
                        .await
                        .unwrap();
                }
            }
            buffer.drain(..total);
            if kind == stop_after {
                return types;
            }
        }
        match tokio::time::timeout(SERVER_IDLE, reader.read(&mut chunk)).await {
            Ok(Ok(read)) if read > 0 => buffer.extend_from_slice(&chunk[..read]),
            _ => return types,
        }
    }
}

type PacketTypesPerConnection = tokio::task::JoinHandle<(Vec<u8>, Vec<u8>)>;

fn reconnecting_server(
    first_answers: Vec<Option<u8>>,
    first_stop_after: u8,
) -> (
    String,
    tokio::sync::oneshot::Sender<()>,
    PacketTypesPerConnection,
) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("mqtt://{}", socket.local_addr().unwrap());
    let listener = socket.listen(2).unwrap();
    let (drop_tx, drop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (mut reader, mut writer) = socket.into_split();
        let mut buffer = accept_connect(&mut reader, &mut writer).await;
        let mut answers = first_answers.into_iter().collect();
        let first = answer_until(
            &mut reader,
            &mut writer,
            &mut buffer,
            &mut answers,
            first_stop_after,
        )
        .await;
        let _ = drop_rx.await;
        drop((reader, writer));
        let (socket, _) = tokio::time::timeout(SETTLE_WITHIN, listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut reader, mut writer) = socket.into_split();
        let mut buffer = accept_connect(&mut reader, &mut writer).await;
        let second = answer_until(
            &mut reader,
            &mut writer,
            &mut buffer,
            &mut std::collections::VecDeque::new(),
            u8::MAX,
        )
        .await;
        writer.write_all(&PUBLISH_T_SUB).await.unwrap();
        tokio::time::sleep(CANCEL_AFTER).await;
        (first, second)
    });
    (url, drop_tx, server)
}

async fn reconnecting_client(name: &str, url: &str) -> MqttClient {
    let options = ConnectOptions::new(name)
        .with_automatic_reconnect(true)
        .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(200));
    let client = MqttClient::with_options(options.clone());
    client.connect_with_options(url, options).await.unwrap();
    client
}

fn counting_callback() -> (
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    impl Fn(mqtt5::types::Message) + Send + Sync + 'static,
) {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counting = std::sync::Arc::clone(&calls);
    (calls, move |_| {
        counting.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_cancelled_after_commit_keeps_nothing_and_is_not_restored() {
    let (url, drop_first, server) = reconnecting_server(vec![None], SUBSCRIBE_TYPE);
    let client = reconnecting_client("cancelled-subscribe", &url).await;
    let (calls, callback) = counting_callback();

    let cancelled = tokio::time::timeout(CANCEL_AFTER, client.subscribe("t/sub", callback)).await;
    drop_first.send(()).unwrap();
    let (first, second) = server.await.unwrap();
    let delivered = calls.load(std::sync::atomic::Ordering::SeqCst);
    client.disconnect().await.ok();

    assert!(
        cancelled.is_err(),
        "the subscribe must be cancelled while waiting for its SUBACK"
    );
    assert!(first.contains(&SUBSCRIBE_TYPE), "{first:?}");
    assert!(
        !second.contains(&SUBSCRIBE_TYPE) && delivered == 0,
        "a subscribe cancelled after commit was kept: resent on reconnect {second:?}, callback invoked {delivered} times"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsubscribe_cancelled_after_commit_keeps_nothing_and_is_not_restored() {
    let (url, drop_first, server) = reconnecting_server(vec![Some(0x00), None], UNSUBSCRIBE_TYPE);
    let client = reconnecting_client("cancelled-unsubscribe", &url).await;
    let (calls, callback) = counting_callback();

    let subscribed = client.subscribe("t/sub", callback).await;
    let cancelled = tokio::time::timeout(CANCEL_AFTER, client.unsubscribe("t/sub")).await;
    drop_first.send(()).unwrap();
    let (first, second) = server.await.unwrap();
    let delivered = calls.load(std::sync::atomic::Ordering::SeqCst);
    client.disconnect().await.ok();

    assert!(subscribed.is_ok(), "{subscribed:?}");
    assert!(
        cancelled.is_err(),
        "the unsubscribe must be cancelled while waiting for its UNSUBACK"
    );
    assert!(first.contains(&UNSUBSCRIBE_TYPE), "{first:?}");
    assert!(
        !second.contains(&SUBSCRIBE_TYPE) && delivered == 0,
        "an unsubscribe cancelled after commit left the subscription: resent on reconnect {second:?}, callback invoked {delivered} times"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_or_cancelled_subscribe_keeps_an_earlier_subscription_to_the_same_filter() {
    let (url, drop_first, server) =
        reconnecting_server(vec![Some(0x00), Some(0x87), None], u8::MAX);
    let client = reconnecting_client("same-filter", &url).await;
    let (granted_calls, granted) = counting_callback();
    let (denied_calls, denied) = counting_callback();
    let (cancelled_calls, cancelled) = counting_callback();

    let first = client.subscribe("t/sub", granted).await;
    let second = client.subscribe("t/sub", denied).await;
    let third = tokio::time::timeout(CANCEL_AFTER, client.subscribe("t/sub", cancelled)).await;
    drop_first.send(()).unwrap();
    server.await.unwrap();
    let calls = [&granted_calls, &denied_calls, &cancelled_calls]
        .map(|calls| calls.load(std::sync::atomic::Ordering::SeqCst));
    client.disconnect().await.ok();

    assert!(first.is_ok(), "{first:?}");
    assert!(second.is_err(), "{second:?}");
    assert!(third.is_err(), "{third:?}");
    assert_eq!(
        calls,
        [1, 0, 0],
        "after a denied and a cancelled subscribe to the same filter, callback calls were [granted, denied, cancelled] = {calls:?}"
    );
}

async fn next_frame(
    reader: &mut tokio::net::tcp::OwnedReadHalf,
    buffer: &mut Vec<u8>,
) -> Option<(u8, Vec<u8>)> {
    let mut chunk = vec![0u8; 4096];
    loop {
        if let Some((first, header_len, total)) = complete_frame(buffer) {
            let body = buffer[header_len..total].to_vec();
            buffer.drain(..total);
            return Some((first >> 4, body));
        }
        match tokio::time::timeout(SERVER_IDLE * 2, reader.read(&mut chunk)).await {
            Ok(Ok(read)) if read > 0 => buffer.extend_from_slice(&chunk[..read]),
            _ => return None,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ack_subscription_unsubscribed_before_its_suback_is_not_restored_after_reconnect() {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("mqtt://{}", socket.local_addr().unwrap());
    let listener = socket.listen(2).unwrap();
    let (subscribe_seen_tx, subscribe_seen_rx) = tokio::sync::oneshot::channel::<()>();
    let (drop_tx, drop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (mut reader, mut writer) = socket.into_split();
        let mut buffer = accept_connect(&mut reader, &mut writer).await;
        let subscribe = loop {
            let (kind, body) = next_frame(&mut reader, &mut buffer).await.unwrap();
            if kind == SUBSCRIBE_TYPE {
                break body;
            }
        };
        subscribe_seen_tx.send(()).unwrap();
        let unsubscribe = loop {
            let (kind, body) = next_frame(&mut reader, &mut buffer).await.unwrap();
            if kind == UNSUBSCRIBE_TYPE {
                break body;
            }
        };
        writer
            .write_all(&[0x90, 0x04, subscribe[0], subscribe[1], 0x00, 0x00])
            .await
            .unwrap();
        writer
            .write_all(&[0xb0, 0x04, unsubscribe[0], unsubscribe[1], 0x00, 0x00])
            .await
            .unwrap();
        let _ = drop_rx.await;
        drop((reader, writer));
        let (socket, _) = tokio::time::timeout(SETTLE_WITHIN, listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut reader, mut writer) = socket.into_split();
        let mut buffer = accept_connect(&mut reader, &mut writer).await;
        let mut types = Vec::new();
        while let Some((kind, _)) = next_frame(&mut reader, &mut buffer).await {
            types.push(kind);
        }
        types
    });
    let options = ConnectOptions::new("ack-unsubscribed-before-suback")
        .with_clean_start(false)
        .with_session_expiry_interval(300)
        .with_receive_maximum(10)
        .with_deferred_ack(true)
        .with_automatic_reconnect(true)
        .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(200));
    let client = MqttClient::with_options(options.clone());
    client.connect_with_options(&url, options).await.unwrap();

    let subscribing = client.clone();
    let subscribe = tokio::spawn(async move {
        subscribing
            .subscribe_with_ack("t/ack", mqtt5::SubscribeOptions::default(), |_, _| {})
            .await
    });
    subscribe_seen_rx.await.unwrap();
    let unsubscribed = tokio::time::timeout(SETTLE_WITHIN, client.unsubscribe("t/ack")).await;
    let subscribed = tokio::time::timeout(SETTLE_WITHIN, subscribe).await;
    drop_tx.send(()).unwrap();
    let second_connection = tokio::time::timeout(SETTLE_WITHIN, server)
        .await
        .unwrap()
        .unwrap();
    client.disconnect().await.ok();

    assert!(
        matches!(subscribed, Ok(Ok(Ok(_)))),
        "subscribe_with_ack: {subscribed:?}"
    );
    assert!(
        matches!(unsubscribed, Ok(Ok(()))),
        "unsubscribe: {unsubscribed:?}"
    );
    assert!(
        !second_connection.contains(&SUBSCRIBE_TYPE),
        "the filter was unsubscribed after its SUBSCRIBE, yet the reconnect subscribed it again: packet types on the second connection {second_connection:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn denied_subscribe_with_ack_falls_back_to_the_granted_callback() {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("mqtt://{}", socket.local_addr().unwrap());
    let listener = socket.listen(1).unwrap();
    let (publish_tx, publish_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (mut reader, mut writer) = socket.into_split();
        let mut buffer = accept_connect(&mut reader, &mut writer).await;
        for reason in [0x00, 0x87] {
            let subscribe = loop {
                let (kind, body) = next_frame(&mut reader, &mut buffer).await.unwrap();
                if kind == SUBSCRIBE_TYPE {
                    break body;
                }
            };
            writer
                .write_all(&[0x90, 0x04, subscribe[0], subscribe[1], 0x00, reason])
                .await
                .unwrap();
        }
        let _ = publish_rx.await;
        writer
            .write_all(&[
                0x32, 0x0a, 0x00, 0x05, b't', b'/', b'a', b'c', b'k', 0x00, 0x01, 0x00,
            ])
            .await
            .unwrap();
        tokio::time::sleep(CANCEL_AFTER * 2).await;
    });
    let options = ConnectOptions::new("ack-replaced")
        .with_clean_start(false)
        .with_session_expiry_interval(300)
        .with_receive_maximum(10)
        .with_deferred_ack(true)
        .with_automatic_reconnect(false);
    let client = MqttClient::with_options(options.clone());
    client.connect_with_options(&url, options).await.unwrap();
    let granted_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counting = std::sync::Arc::clone(&granted_calls);

    let first = client
        .subscribe_with_ack(
            "t/ack",
            mqtt5::SubscribeOptions::default(),
            move |_, token| {
                counting.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                drop(token);
            },
        )
        .await;
    let second = client
        .subscribe_with_ack("t/ack", mqtt5::SubscribeOptions::default(), |_, _| {})
        .await;
    publish_tx.send(()).unwrap();
    server.await.unwrap();
    let delivered = granted_calls.load(std::sync::atomic::Ordering::SeqCst);
    client.disconnect().await.ok();

    assert!(first.is_ok(), "{first:?}");
    assert!(second.is_err(), "{second:?}");
    assert_eq!(
        delivered, 1,
        "a denied subscribe_with_ack on the same filter left the granted subscription without its callback"
    );
}

const PUBLISH_T_ACK_QOS1: [u8; 12] = [
    0x32, 0x0a, 0x00, 0x05, b't', b'/', b'a', b'c', b'k', 0x00, 0x01, 0x00,
];

async fn next_of(
    reader: &mut tokio::net::tcp::OwnedReadHalf,
    buffer: &mut Vec<u8>,
    wanted: u8,
) -> Vec<u8> {
    loop {
        let (kind, body) = next_frame(reader, buffer).await.unwrap();
        if kind == wanted {
            return body;
        }
    }
}

async fn accept_on(
    listener: &TcpListener,
) -> (
    tokio::net::tcp::OwnedReadHalf,
    tokio::net::tcp::OwnedWriteHalf,
    Vec<u8>,
) {
    let (socket, _) = tokio::time::timeout(SETTLE_WITHIN, listener.accept())
        .await
        .unwrap()
        .unwrap();
    let (mut reader, mut writer) = socket.into_split();
    let buffer = accept_connect(&mut reader, &mut writer).await;
    (reader, writer, buffer)
}

async fn suback(writer: &mut tokio::net::tcp::OwnedWriteHalf, subscribe: &[u8], reason: u8) {
    writer
        .write_all(&[0x90, 0x04, subscribe[0], subscribe[1], 0x00, reason])
        .await
        .unwrap();
}

fn deferred_ack_options(name: &str, reconnect: bool) -> ConnectOptions {
    ConnectOptions::new(name)
        .with_clean_start(false)
        .with_session_expiry_interval(300)
        .with_receive_maximum(10)
        .with_deferred_ack(true)
        .with_automatic_reconnect(reconnect)
        .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(200))
}

fn counting_ack_callback() -> (
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    impl Fn(mqtt5::packet::publish::PublishPacket, mqtt5::AckToken) + Send + Sync + 'static,
) {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counting = std::sync::Arc::clone(&calls);
    (calls, move |_, token: mqtt5::AckToken| {
        counting.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        token.ack();
    })
}

fn spawn_ack_subscribe(
    client: &MqttClient,
    callback: impl Fn(mqtt5::packet::publish::PublishPacket, mqtt5::AckToken) + Send + Sync + 'static,
) -> tokio::task::JoinHandle<mqtt5::Result<(u16, QoS)>> {
    let subscribing = client.clone();
    tokio::spawn(async move {
        subscribing
            .subscribe_with_ack("t/ack", mqtt5::SubscribeOptions::default(), callback)
            .await
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsubscribe_committed_before_a_denied_subscribe_with_ack_is_not_undone() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let (publish_tx, publish_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (mut reader, mut writer, mut buffer) = accept_on(&listener).await;
        let first = next_of(&mut reader, &mut buffer, SUBSCRIBE_TYPE).await;
        suback(&mut writer, &first, 0x01).await;
        let second = next_of(&mut reader, &mut buffer, SUBSCRIBE_TYPE).await;
        seen_tx.send(()).unwrap();
        let unsubscribe = next_of(&mut reader, &mut buffer, UNSUBSCRIBE_TYPE).await;
        suback(&mut writer, &second, 0x87).await;
        writer
            .write_all(&[0xb0, 0x04, unsubscribe[0], unsubscribe[1], 0x00, 0x00])
            .await
            .unwrap();
        let _ = publish_rx.await;
        writer.write_all(&PUBLISH_T_ACK_QOS1).await.unwrap();
        tokio::time::sleep(CANCEL_AFTER * 2).await;
    });
    let options = deferred_ack_options("ack-unsubscribe-then-denied", false);
    let client = MqttClient::with_options(options.clone());
    client.connect_with_options(&url, options).await.unwrap();

    let (first_calls, first_callback) = counting_ack_callback();
    let first = client
        .subscribe_with_ack("t/ack", mqtt5::SubscribeOptions::default(), first_callback)
        .await;
    let second = spawn_ack_subscribe(&client, |_, token| token.ack());
    seen_rx.recv().await.unwrap();
    let unsubscribed = tokio::time::timeout(SETTLE_WITHIN, client.unsubscribe("t/ack")).await;
    let second = tokio::time::timeout(SETTLE_WITHIN, second).await;
    publish_tx.send(()).unwrap();
    tokio::time::timeout(SETTLE_WITHIN, server)
        .await
        .unwrap()
        .unwrap();
    let delivered = first_calls.load(std::sync::atomic::Ordering::SeqCst);
    client.disconnect().await.ok();

    assert!(first.is_ok(), "first: {first:?}");
    assert!(
        matches!(unsubscribed, Ok(Ok(()))),
        "unsubscribe: {unsubscribed:?}"
    );
    assert!(matches!(second, Ok(Ok(Err(_)))), "second: {second:?}");
    assert_eq!(
        delivered, 0,
        "the unsubscribe was committed after the second SUBSCRIBE, yet the denied second subscribe_with_ack brought the first callback back and it received a message"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_failed_subscribe_with_ack_calls_leave_only_the_granted_callback() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let (second_done_tx, second_done_rx) = tokio::sync::oneshot::channel::<()>();
    let (publish_tx, publish_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (mut reader, mut writer, mut buffer) = accept_on(&listener).await;
        let first = next_of(&mut reader, &mut buffer, SUBSCRIBE_TYPE).await;
        suback(&mut writer, &first, 0x01).await;
        let second = next_of(&mut reader, &mut buffer, SUBSCRIBE_TYPE).await;
        seen_tx.send(()).unwrap();
        let third = next_of(&mut reader, &mut buffer, SUBSCRIBE_TYPE).await;
        seen_tx.send(()).unwrap();
        suback(&mut writer, &second, 0x87).await;
        let _ = second_done_rx.await;
        suback(&mut writer, &third, 0x87).await;
        let _ = publish_rx.await;
        writer.write_all(&PUBLISH_T_ACK_QOS1).await.unwrap();
        tokio::time::sleep(CANCEL_AFTER * 2).await;
    });
    let options = deferred_ack_options("ack-two-denied", false);
    let client = MqttClient::with_options(options.clone());
    client.connect_with_options(&url, options).await.unwrap();

    let (first_calls, first_callback) = counting_ack_callback();
    let (second_calls, second_callback) = counting_ack_callback();
    let (third_calls, third_callback) = counting_ack_callback();
    let first = client
        .subscribe_with_ack("t/ack", mqtt5::SubscribeOptions::default(), first_callback)
        .await;
    let second = spawn_ack_subscribe(&client, second_callback);
    seen_rx.recv().await.unwrap();
    let third = spawn_ack_subscribe(&client, third_callback);
    seen_rx.recv().await.unwrap();
    let second = tokio::time::timeout(SETTLE_WITHIN, second).await;
    second_done_tx.send(()).unwrap();
    let third = tokio::time::timeout(SETTLE_WITHIN, third).await;
    publish_tx.send(()).unwrap();
    tokio::time::timeout(SETTLE_WITHIN, server)
        .await
        .unwrap()
        .unwrap();
    let calls = [&first_calls, &second_calls, &third_calls]
        .map(|calls| calls.load(std::sync::atomic::Ordering::SeqCst));
    client.disconnect().await.ok();

    assert!(first.is_ok(), "first: {first:?}");
    assert!(matches!(second, Ok(Ok(Err(_)))), "second: {second:?}");
    assert!(matches!(third, Ok(Ok(Err(_)))), "third: {third:?}");
    assert_eq!(
        calls,
        [1, 0, 0],
        "after two denied subscribe_with_ack calls only the granted callback must receive the message: callback calls [granted, denied 2nd, denied 3rd] = {calls:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_granted_subscribe_with_ack_calls_are_restored_after_a_lost_session() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let (first_done_tx, first_done_rx) = tokio::sync::oneshot::channel::<()>();
    let (drop_tx, drop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (mut reader, mut writer, mut buffer) = accept_on(&listener).await;
        let first = next_of(&mut reader, &mut buffer, SUBSCRIBE_TYPE).await;
        seen_tx.send(()).unwrap();
        let second = next_of(&mut reader, &mut buffer, SUBSCRIBE_TYPE).await;
        seen_tx.send(()).unwrap();
        suback(&mut writer, &first, 0x01).await;
        let _ = first_done_rx.await;
        suback(&mut writer, &second, 0x01).await;
        let _ = drop_rx.await;
        drop((reader, writer));
        let (mut reader, mut writer, mut buffer) = accept_on(&listener).await;
        let mut types = Vec::new();
        while let Some((kind, body)) = next_frame(&mut reader, &mut buffer).await {
            if kind == SUBSCRIBE_TYPE {
                suback(&mut writer, &body, 0x01).await;
            }
            types.push(kind);
        }
        types
    });
    let options = deferred_ack_options("ack-two-granted", true);
    let client = MqttClient::with_options(options.clone());
    client.connect_with_options(&url, options).await.unwrap();

    let first = spawn_ack_subscribe(&client, |_, token| token.ack());
    seen_rx.recv().await.unwrap();
    let second = spawn_ack_subscribe(&client, |_, token| token.ack());
    seen_rx.recv().await.unwrap();
    let first = tokio::time::timeout(SETTLE_WITHIN, first).await;
    first_done_tx.send(()).unwrap();
    let second = tokio::time::timeout(SETTLE_WITHIN, second).await;
    drop_tx.send(()).unwrap();
    let second_connection = tokio::time::timeout(SETTLE_WITHIN * 2, server)
        .await
        .unwrap()
        .unwrap();
    client.disconnect().await.ok();

    assert!(matches!(first, Ok(Ok(Ok(_)))), "first: {first:?}");
    assert!(matches!(second, Ok(Ok(Ok(_)))), "second: {second:?}");
    assert!(
        second_connection.contains(&SUBSCRIBE_TYPE),
        "both subscribe_with_ack calls were granted, yet the reconnect after a lost session did not subscribe t/ack again: packet types on the second connection {second_connection:?}"
    );
}

fn publish_a_b_c_qos1(packet_id: u8) -> [u8; 12] {
    [
        0x32, 0x0a, 0x00, 0x05, b'a', b'/', b'b', b'/', b'c', 0x00, packet_id, 0x00,
    ]
}

async fn wait_for_total(counts: &[&std::sync::Arc<std::sync::atomic::AtomicUsize>], total: usize) {
    let deadline = tokio::time::Instant::now() + SETTLE_WITHIN / 2;
    while counts
        .iter()
        .map(|count| count.load(std::sync::atomic::Ordering::SeqCst))
        .sum::<usize>()
        < total
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn denied_subscribe_with_ack_keeps_the_wildcard_precedence_of_the_granted_filter() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let (second_publish_tx, second_publish_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (mut reader, mut writer, mut buffer) = accept_on(&listener).await;
        let first = next_of(&mut reader, &mut buffer, SUBSCRIBE_TYPE).await;
        suback(&mut writer, &first, 0x01).await;
        let second = next_of(&mut reader, &mut buffer, SUBSCRIBE_TYPE).await;
        suback(&mut writer, &second, 0x01).await;
        writer.write_all(&publish_a_b_c_qos1(1)).await.unwrap();
        let third = next_of(&mut reader, &mut buffer, SUBSCRIBE_TYPE).await;
        suback(&mut writer, &third, 0x87).await;
        let _ = second_publish_rx.await;
        writer.write_all(&publish_a_b_c_qos1(2)).await.unwrap();
        tokio::time::sleep(CANCEL_AFTER * 2).await;
    });
    let options = deferred_ack_options("ack-wildcard-precedence", false);
    let client = MqttClient::with_options(options.clone());
    client.connect_with_options(&url, options).await.unwrap();

    let (hash_calls, hash_callback) = counting_ack_callback();
    let (plus_calls, plus_callback) = counting_ack_callback();
    let (denied_calls, denied_callback) = counting_ack_callback();
    let hash = client
        .subscribe_with_ack("a/#", mqtt5::SubscribeOptions::default(), hash_callback)
        .await;
    let plus = client
        .subscribe_with_ack("a/+/c", mqtt5::SubscribeOptions::default(), plus_callback)
        .await;
    wait_for_total(&[&hash_calls, &plus_calls], 1).await;
    let before =
        [&hash_calls, &plus_calls].map(|calls| calls.load(std::sync::atomic::Ordering::SeqCst));
    let denied = client
        .subscribe_with_ack("a/#", mqtt5::SubscribeOptions::default(), denied_callback)
        .await;
    second_publish_tx.send(()).unwrap();
    wait_for_total(&[&hash_calls, &plus_calls, &denied_calls], 2).await;
    tokio::time::timeout(SETTLE_WITHIN, server)
        .await
        .unwrap()
        .unwrap();
    let after = [&hash_calls, &plus_calls, &denied_calls]
        .map(|calls| calls.load(std::sync::atomic::Ordering::SeqCst));
    client.disconnect().await.ok();

    assert!(hash.is_ok(), "a/#: {hash:?}");
    assert!(plus.is_ok(), "a/+/c: {plus:?}");
    assert!(denied.is_err(), "denied a/#: {denied:?}");
    assert_eq!(
        before,
        [1, 0],
        "before the denied call a/b/c went to [a/#, a/+/c] = {before:?}"
    );
    assert_eq!(
        after,
        [2, 0, 0],
        "a denied subscribe_with_ack on a/# changed which callback receives a/b/c: calls [a/#, a/+/c, denied a/#] = {after:?}"
    );
}

fn long_filter() -> String {
    format!("t/{}", "f".repeat(32 * 1024))
}

#[tokio::test]
async fn timed_out_subscribes_on_a_stalled_link_stay_bounded() {
    let (url, server) = server_that_stops_reading(&CONNACK);
    let client = connected_client("subscribe-growth", &url).await;
    let filter = long_filter();
    let before = thread_heap();
    let mut cancelled = 0;
    for _ in 0..1000 {
        if tokio::time::timeout(
            Duration::from_millis(1),
            client.subscribe(filter.as_str(), |_| {}),
        )
        .await
        .is_err()
        {
            cancelled += 1;
        }
    }
    let retained = thread_heap() - before;
    server.abort();

    assert!(
        cancelled > 900,
        "only {cancelled} subscribes were cancelled"
    );
    assert!(
        retained < 16 * MIB,
        "{cancelled} timed-out 32 KiB subscribes left {} MiB buffered in the client (pending limit is 1 MiB)",
        retained / MIB
    );
}

#[tokio::test]
async fn timed_out_unsubscribes_on_a_stalled_link_stay_bounded() {
    let (url, server) = server_that_stops_reading(&CONNACK);
    let client = connected_client("unsubscribe-growth", &url).await;
    let filter = long_filter();
    let before = thread_heap();
    let mut cancelled = 0;
    for _ in 0..1000 {
        if tokio::time::timeout(
            Duration::from_millis(1),
            client.unsubscribe(filter.as_str()),
        )
        .await
        .is_err()
        {
            cancelled += 1;
        }
    }
    let retained = thread_heap() - before;
    server.abort();

    assert!(
        cancelled > 900,
        "only {cancelled} unsubscribes were cancelled"
    );
    assert!(
        retained < 16 * MIB,
        "{cancelled} timed-out 32 KiB unsubscribes left {} MiB buffered in the client (pending limit is 1 MiB)",
        retained / MIB
    );
}

fn acknowledging_server() -> (String, tokio::task::JoinHandle<Vec<u8>>) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("mqtt://{}", socket.local_addr().unwrap());
    let listener = socket.listen(1).unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (mut reader, mut writer) = socket.into_split();
        let mut buffer = accept_connect(&mut reader, &mut writer).await;
        let mut types = Vec::new();
        let mut chunk = vec![0u8; 4096];
        loop {
            while let Some((first, header_len, total)) = complete_frame(&buffer) {
                let kind = first >> 4;
                types.push(kind);
                if kind == SUBSCRIBE_TYPE {
                    let [high, low] = [buffer[header_len], buffer[header_len + 1]];
                    writer
                        .write_all(&[0x90, 0x04, high, low, 0x00, 0x00])
                        .await
                        .unwrap();
                    writer
                        .write_all(&[0x30, 0x08, 0x00, 0x05, b't', b'/', b's', b'u', b'b', 0x00])
                        .await
                        .unwrap();
                } else if kind == UNSUBSCRIBE_TYPE {
                    let [high, low] = [buffer[header_len], buffer[header_len + 1]];
                    writer
                        .write_all(&[0xb0, 0x04, high, low, 0x00, 0x00])
                        .await
                        .unwrap();
                }
                buffer.drain(..total);
            }
            match tokio::time::timeout(SERVER_IDLE, reader.read(&mut chunk)).await {
                Ok(Ok(read)) if read > 0 => buffer.extend_from_slice(&chunk[..read]),
                _ => break,
            }
        }
        types
    });
    (url, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledged_subscribe_and_unsubscribe_return_ok_and_deliver() {
    let (url, server) = acknowledging_server();
    let client = connected_client("acknowledged-subscribe", &url).await;
    let (delivered_tx, delivered_rx) = tokio::sync::oneshot::channel();
    let delivered_tx = std::sync::Mutex::new(Some(delivered_tx));
    let subscribed = client
        .subscribe("t/sub", move |message| {
            if let Some(tx) = delivered_tx.lock().unwrap().take() {
                let _ = tx.send(message.topic);
            }
        })
        .await;
    let delivered = tokio::time::timeout(SETTLE_WITHIN / 2, delivered_rx).await;
    let unsubscribed = client.unsubscribe("t/sub").await;
    client.disconnect().await.unwrap();
    let seen = server.await.unwrap();

    assert!(subscribed.is_ok(), "{subscribed:?}");
    assert!(
        matches!(&delivered, Ok(Ok(topic)) if topic == "t/sub"),
        "{delivered:?}"
    );
    assert!(unsubscribed.is_ok(), "{unsubscribed:?}");
    assert!(
        seen.contains(&SUBSCRIBE_TYPE) && seen.contains(&UNSUBSCRIBE_TYPE),
        "{seen:?}"
    );
}

fn publish_topics(mut buffer: &[u8]) -> Vec<String> {
    let mut topics = Vec::new();
    while let Some((first, header_len, total)) = complete_frame(buffer) {
        if first >> 4 == PUBLISH_TYPE {
            topics.push(publish_topic_and_id(first, &buffer[header_len..total]).0);
        }
        buffer = &buffer[total..];
    }
    topics
}

fn server_stalling_after_the_first_publish(
    stalls: bool,
) -> (
    String,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<Vec<String>>,
) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("mqtt://{}", socket.local_addr().unwrap());
    let listener = socket.listen(1).unwrap();
    let (resume, resumed) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (mut reader, mut writer) = socket.into_split();
        let mut buffer = accept_connect(&mut reader, &mut writer).await;
        tokio::time::sleep(CANCEL_AFTER + CANCEL_AFTER * 2 / 3).await;
        let mut chunk = vec![0u8; 64 * 1024];
        if stalls {
            while complete_frame(&buffer).is_none() {
                let read = reader.read(&mut chunk).await.unwrap();
                assert!(read > 0);
                buffer.extend_from_slice(&chunk[..read]);
            }
            let _ = resumed.await;
        }
        while let Ok(Ok(read)) =
            tokio::time::timeout(SERVER_IDLE * 2, reader.read(&mut chunk)).await
        {
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        publish_topics(&buffer)
    });
    (url, resume, server)
}

type BatchedServer = (
    String,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<Vec<String>>,
);

async fn small_publish_batched_before_a_stuck_one(
    stalls: bool,
) -> ([String; 2], bool, Vec<String>) {
    batched_publishes_cut_short_by_disconnect(server_stalling_after_the_first_publish(stalls)).await
}

async fn batched_publishes_cut_short_by_disconnect(
    (url, resume, server): BatchedServer,
) -> ([String; 2], bool, Vec<String>) {
    let client = connected_client("batched-qos0", &url).await;

    let first = client.clone();
    let large = tokio::spawn(async move { first.publish_qos0("t/c", vec![1u8; 8 << 20]).await });
    tokio::time::sleep(CANCEL_AFTER / 3).await;
    assert!(
        !large.is_finished(),
        "the first publish must still be writing"
    );
    let small = client.clone();
    let batched = tokio::spawn(async move { small.publish_qos0("t/a", b"small".to_vec()).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let second = client.clone();
    let stuck = tokio::spawn(async move { second.publish_qos0("t/b", vec![2u8; 32 << 20]).await });
    tokio::time::sleep(CANCEL_AFTER * 3).await;

    let disconnected = tokio::time::timeout(SETTLE_WITHIN / 2, client.disconnect())
        .await
        .is_ok();
    let outcomes = [
        format!("{:?}", large.await.unwrap()),
        format!("{:?}", batched.await.unwrap()),
    ];
    stuck.abort();
    let _ = resume.send(());
    let received = server.await.unwrap();
    (outcomes, disconnected, received)
}

fn assert_batched_outcomes_match_delivery(
    outcomes: &[String; 2],
    disconnected: bool,
    received: &[String],
) {
    assert!(disconnected, "disconnect did not complete");
    for (topic, outcome) in ["t/c", "t/a"].iter().zip(outcomes) {
        let delivered = received.iter().any(|seen| seen == topic);
        assert!(
            outcome.starts_with("Ok") || !delivered,
            "publish {topic} returned {outcome}, yet the server received it whole: {received:?}"
        );
    }
}

const SERVER_START_DELAY: Duration = Duration::from_millis(500);

type StallingServer = (
    String,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<(Vec<String>, Vec<String>)>,
);

fn without_connect(mut buffer: Vec<u8>) -> Vec<u8> {
    let (first, _, total) = complete_frame(&buffer).unwrap();
    assert_eq!(first >> 4, 1, "first packet must be CONNECT");
    buffer.drain(..total);
    buffer
}

fn tcp_server_stalling_once_it_has_read(stall_after: &'static str) -> StallingServer {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let (resume, resumed) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let listener = TcpListener::from_std(listener).unwrap();
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = Vec::new();
        let mut chunk = vec![0u8; 4096];
        while complete_frame(&buffer).is_none() {
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0);
            buffer.extend_from_slice(&chunk[..read]);
        }
        let mut buffer = without_connect(buffer);
        stream.write_all(&CONNACK).await.unwrap();
        tokio::time::sleep(SERVER_START_DELAY).await;
        while !publish_topics(&buffer)
            .iter()
            .any(|topic| topic == stall_after)
        {
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0);
            buffer.extend_from_slice(&chunk[..read]);
        }
        let before_stall = publish_topics(&buffer);
        let _ = resumed.await;
        while let Ok(Ok(read)) =
            tokio::time::timeout(SERVER_IDLE * 2, stream.read(&mut chunk)).await
        {
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        (before_stall, publish_topics(&buffer))
    });
    (url, resume, server)
}

async fn small_publish_read_by_the_server_before_a_stall(
    (url, resume, server): StallingServer,
) -> (String, bool, Vec<String>, Vec<String>) {
    let client = connected_client("read-before-stall", &url).await;

    let first = client.clone();
    let large = tokio::spawn(async move { first.publish_qos0("t/c", vec![1u8; 8 << 20]).await });
    tokio::time::sleep(SERVER_START_DELAY / 5).await;
    assert!(
        !large.is_finished(),
        "the first publish must still be writing"
    );
    let small = client.clone();
    let batched = tokio::spawn(async move { small.publish_qos0("t/a", b"small".to_vec()).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let second = client.clone();
    let stuck = tokio::spawn(async move { second.publish_qos0("t/b", vec![2u8; 32 << 20]).await });
    tokio::time::sleep(SERVER_START_DELAY * 3).await;

    let disconnected = tokio::time::timeout(SETTLE_WITHIN, client.disconnect())
        .await
        .is_ok();
    let outcome = format!("{:?}", batched.await.unwrap());
    stuck.abort();
    let _ = resume.send(());
    let (before_stall, received) = server.await.unwrap();
    (outcome, disconnected, before_stall, received)
}

fn assert_read_publish_is_not_reported_unsent(
    outcome: &str,
    disconnected: bool,
    before_stall: &[String],
    received: &[String],
) {
    assert!(disconnected, "disconnect did not complete");
    assert!(
        before_stall.iter().any(|topic| topic == "t/a"),
        "the server read t/a before it stopped reading: {before_stall:?}"
    );
    assert!(
        !outcome.contains("NotConnected"),
        "publish t/a returned {outcome}, which means it was never sent, yet the server had already read it whole before it stopped reading: {before_stall:?} (all received: {received:?})"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tcp_qos0_publish_the_server_read_is_not_reported_unsent() {
    let (outcome, disconnected, before_stall, received) =
        small_publish_read_by_the_server_before_a_stall(tcp_server_stalling_once_it_has_read(
            "t/a",
        ))
        .await;

    assert_read_publish_is_not_reported_unsent(&outcome, disconnected, &before_stall, &received);
}

fn tls_server_stalling_after_the_first_publish() -> BatchedServer {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("mqtts://{}", socket.local_addr().unwrap());
    let listener = socket.listen(1).unwrap();
    let acceptor = tls_acceptor();
    let (resume, resumed) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let tls = acceptor.accept(socket).await.unwrap();
        let (mut reader, mut writer) = tokio::io::split(tls);
        let mut buffer = accept_connect(&mut reader, &mut writer).await;
        tokio::time::sleep(CANCEL_AFTER + CANCEL_AFTER * 2 / 3).await;
        let mut chunk = vec![0u8; 64 * 1024];
        while complete_frame(&buffer).is_none() {
            let read = reader.read(&mut chunk).await.unwrap();
            assert!(read > 0);
            buffer.extend_from_slice(&chunk[..read]);
        }
        let _ = resumed.await;
        while let Ok(Ok(read)) =
            tokio::time::timeout(SERVER_IDLE * 2, reader.read(&mut chunk)).await
        {
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        publish_topics(&buffer)
    });
    (url, resume, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_qos0_publish_written_in_a_batch_cut_short_by_disconnect_is_not_reported_unsent() {
    let (outcomes, disconnected, received) =
        batched_publishes_cut_short_by_disconnect(tls_server_stalling_after_the_first_publish())
            .await;

    assert_batched_outcomes_match_delivery(&outcomes, disconnected, &received);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qos0_publish_written_in_a_batch_cut_short_by_disconnect_is_not_reported_unsent() {
    let (outcomes, disconnected, received) = small_publish_batched_before_a_stuck_one(true).await;

    assert_batched_outcomes_match_delivery(&outcomes, disconnected, &received);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qos0_publish_in_a_batch_that_completes_is_ok_and_delivered() {
    let (outcomes, disconnected, received) = small_publish_batched_before_a_stuck_one(false).await;

    assert!(disconnected, "disconnect did not complete");
    assert!(
        outcomes.iter().all(|outcome| outcome.starts_with("Ok")),
        "{outcomes:?}"
    );
    assert!(received.iter().any(|topic| topic == "t/a"), "{received:?}");
}

fn tls_acceptor() -> tokio_rustls::TlsAcceptor {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let chain = vec![rustls::pki_types::CertificateDer::from(
        cert.cert.der().to_vec(),
    )];
    let key = rustls::pki_types::PrivateKeyDer::try_from(cert.signing_key.serialize_der()).unwrap();
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap();
    tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(config))
}

fn tls_server(stalled: bool) -> (String, tokio::task::JoinHandle<()>) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("mqtts://{}", socket.local_addr().unwrap());
    let listener = socket.listen(1).unwrap();
    let acceptor = tls_acceptor();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let tls = acceptor.accept(socket).await.unwrap();
        let (mut reader, mut writer) = tokio::io::split(tls);
        let buffer = accept_connect(&mut reader, &mut writer).await;
        if stalled {
            tokio::time::sleep(SETTLE_WITHIN * 3).await;
        } else {
            read_publishes(&mut reader, buffer, &mut tokio::io::sink()).await;
        }
    });
    (url, server)
}

async fn publishes_before_one_blocks(client: &MqttClient, limit: usize) -> Option<usize> {
    for count in 0..limit {
        let publishing = client.clone();
        let task =
            tokio::spawn(async move { publishing.publish_qos0("t/s", vec![1u8; 1024]).await });
        match tokio::time::timeout(CANCEL_AFTER, task).await {
            Ok(result) => drop(result.unwrap().unwrap()),
            Err(_) => return Some(count),
        }
    }
    None
}

async fn tls_disconnect_after_publishes(stalled: bool, publishes: usize) -> (bool, Option<usize>) {
    let (url, server) = tls_server(stalled);
    let client = connected_client("tls-disconnect", &url).await;
    let blocked = publishes_before_one_blocks(&client, publishes).await;
    let disconnected = tokio::time::timeout(SETTLE_WITHIN / 2, client.disconnect())
        .await
        .is_ok();
    server.abort();
    (disconnected, blocked)
}

fn slow_reading_tls_server(expected: usize) -> (String, tokio::task::JoinHandle<usize>) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("mqtts://{}", socket.local_addr().unwrap());
    let listener = socket.listen(1).unwrap();
    let acceptor = tls_acceptor();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let tls = acceptor.accept(socket).await.unwrap();
        let (mut reader, mut writer) = tokio::io::split(tls);
        let mut buffer = accept_connect(&mut reader, &mut writer).await;
        tokio::time::sleep(CANCEL_AFTER + CANCEL_AFTER * 2 / 3).await;
        let mut chunk = vec![0u8; 1 << 16];
        while buffer.len() < expected {
            match tokio::time::timeout(SETTLE_WITHIN / 2, reader.read(&mut chunk)).await {
                Ok(Ok(read)) if read > 0 => buffer.extend_from_slice(&chunk[..read]),
                _ => break,
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        buffer.len()
    });
    (url, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_qos0_publish_that_returned_ok_reaches_the_server_without_further_writes() {
    let payload = 2 << 20;
    let expected = 1 + 4 + 2 + 3 + 1 + payload;
    let (url, server) = slow_reading_tls_server(expected);
    let client = connected_client("tls-flush", &url).await;

    client
        .publish_qos0("t/a", vec![7u8; payload])
        .await
        .unwrap();
    let received = server.await.unwrap();

    assert!(
        received >= expected,
        "the publish returned Ok, yet the server received {received} of {expected} bytes and no more arrived"
    );
}

fn stalled_then_counting_tls_server() -> (String, tokio::task::JoinHandle<(usize, usize)>) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("mqtts://{}", socket.local_addr().unwrap());
    let listener = socket.listen(1).unwrap();
    let acceptor = tls_acceptor();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let tls = acceptor.accept(socket).await.unwrap();
        let (mut reader, mut writer) = tokio::io::split(tls);
        let mut buffer = accept_connect(&mut reader, &mut writer).await;
        tokio::time::sleep(SERVER_STALL * 2 + SERVER_STALL * 2 / 3).await;
        let mut publishes = 0;
        let mut chunk = vec![0u8; 1 << 16];
        loop {
            while let Some((first, _, total)) = complete_frame(&buffer) {
                if first >> 4 == PUBLISH_TYPE {
                    publishes += 1;
                }
                buffer.drain(..total);
            }
            match tokio::time::timeout(SERVER_STALL, reader.read(&mut chunk)).await {
                Ok(Ok(read)) if read > 0 => buffer.extend_from_slice(&chunk[..read]),
                _ => break,
            }
        }
        (publishes, buffer.len())
    });
    (url, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_qos0_publish_cancelled_during_the_tls_flush_is_still_delivered() {
    let (url, server) = stalled_then_counting_tls_server();
    let client = connected_client("tls-cancel-flush", &url).await;

    let mut returned_ok = 0;
    while let Ok(result) =
        tokio::time::timeout(CANCEL_AFTER, client.publish_qos0("t/s", vec![1u8; 1024])).await
    {
        result.unwrap();
        returned_ok += 1;
        assert!(returned_ok < 100_000, "no publish ever blocked");
    }
    let (publishes, leftover) = server.await.unwrap();

    assert_eq!(
        (publishes, leftover),
        (returned_ok + 1, 0),
        "{returned_ok} publishes returned Ok and one more was committed then cancelled; the server parsed {publishes} PUBLISH packets and was left with {leftover} bytes of an unfinished one"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_disconnect_on_a_stalled_link_completes_promptly() {
    let (_, calibration) = tls_disconnect_after_publishes(true, 100_000).await;
    let blocked_at = calibration.expect("a publish must block on the stalled link");
    let publishes = blocked_at.saturating_sub(40);
    let (disconnected, blocked) = tls_disconnect_after_publishes(true, publishes).await;

    assert_eq!(
        blocked, None,
        "a publish blocked within {publishes}, first block was at {blocked_at}"
    );
    assert!(
        disconnected,
        "disconnect did not complete on a stalled TLS link after {publishes} publishes returned"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_disconnect_on_a_reading_link_completes() {
    let (disconnected, blocked) = tls_disconnect_after_publishes(false, 300).await;

    assert_eq!(blocked, None);
    assert!(disconnected);
}

#[cfg(feature = "broker")]
mod own_subscription {
    use super::*;
    use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
    use mqtt5::broker::MqttBroker;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn tcp_concurrent_large_publishes_to_an_own_qos1_subscription_complete() {
        let config = BrokerConfig::default()
            .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
            .with_bind_address(([127, 0, 0, 1], 0));
        let mut broker = MqttBroker::with_config(config).await.unwrap();
        let tcp_addr = broker.local_addr().unwrap();
        let shutdown = broker.shutdown_handle();
        let run = tokio::spawn(async move { broker.run().await });

        let client =
            connected_client("own-subscription-loopback", &format!("mqtt://{tcp_addr}")).await;
        client
            .subscribe_with_options(
                "loop/t",
                mqtt5::SubscribeOptions {
                    qos: QoS::AtLeastOnce,
                    ..Default::default()
                },
                |_| {},
            )
            .await
            .unwrap();
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let client = client.clone();
            tasks.push(tokio::spawn(async move {
                let mut sent = 0;
                for _ in 0..8 {
                    if client
                        .publish_with_options("loop/t", vec![9u8; 4 << 20], qos1())
                        .await
                        .is_ok()
                    {
                        sent += 1;
                    }
                }
                sent
            }));
        }
        let finished = tokio::time::timeout(Duration::from_secs(60), async {
            let mut sent = 0;
            for task in tasks {
                sent += task.await.unwrap();
            }
            sent
        })
        .await;

        shutdown.shutdown();
        let _ = tokio::time::timeout(SETTLE_WITHIN, run).await;
        assert_eq!(
            finished,
            Ok(64),
            "64 concurrent 4 MiB QoS 1 publishes to the client's own QoS 1 subscription must all be acknowledged within 60 s, so the client keeps reading while its acks wait behind its own publishes"
        );
    }
}

#[cfg(all(feature = "transport-websocket", feature = "broker"))]
mod websocket {
    use super::*;
    use futures::{SinkExt, StreamExt};
    use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig, WebSocketConfig};
    use mqtt5::broker::MqttBroker;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::handshake::server::{
        Callback, ErrorResponse, Request, Response,
    };
    use tokio_tungstenite::tungstenite::Message;

    struct Subprotocol;

    impl Callback for Subprotocol {
        fn on_request(
            self,
            req: &Request,
            mut resp: Response,
        ) -> std::result::Result<Response, ErrorResponse> {
            if let Some(first) = req
                .headers()
                .get("Sec-WebSocket-Protocol")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(',').next())
            {
                resp.headers_mut()
                    .insert("Sec-WebSocket-Protocol", first.trim().parse().unwrap());
            }
            Ok(resp)
        }
    }

    async fn stalled_ws_server() -> (String, tokio::task::JoinHandle<(Received, String)>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_hdr_async(stream, Subprotocol)
                .await
                .unwrap();
            let connect = ws.next().await;
            assert!(
                matches!(connect, Some(Ok(Message::Binary(_)))),
                "{connect:?}"
            );
            ws.send(Message::Binary(CONNACK.to_vec().into()))
                .await
                .unwrap();
            tokio::time::sleep(SERVER_STALL).await;
            let mut bytes = Vec::new();
            let ended = loop {
                match tokio::time::timeout(SERVER_IDLE * 3, ws.next()).await {
                    Ok(Some(Ok(Message::Binary(data)))) => bytes.extend_from_slice(&data),
                    Ok(Some(Ok(_))) => {}
                    other => break format!("{other:?}"),
                }
            };
            let received =
                read_publishes(&mut bytes.as_slice(), Vec::new(), &mut tokio::io::sink()).await;
            (received, ended)
        });
        (format!("ws://{addr}/mqtt"), server)
    }

    async fn ws_disconnect_after_publishes(publishes: usize) -> (bool, Option<usize>) {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.set_recv_buffer_size(4096).unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = socket.local_addr().unwrap();
        let listener = socket.listen(1).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_hdr_async(stream, Subprotocol)
                .await
                .unwrap();
            let connect = ws.next().await;
            assert!(matches!(connect, Some(Ok(Message::Binary(_)))));
            ws.send(Message::Binary(CONNACK.to_vec().into()))
                .await
                .unwrap();
            tokio::time::sleep(SETTLE_WITHIN * 3).await;
        });
        let client = connected_client("ws-disconnect", &format!("ws://{addr}/mqtt")).await;
        let blocked = publishes_before_one_blocks(&client, publishes).await;
        let disconnected = tokio::time::timeout(SETTLE_WITHIN / 2, client.disconnect())
            .await
            .is_ok();
        server.abort();
        (disconnected, blocked)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn websocket_disconnect_on_a_stalled_link_completes_promptly() {
        let (_, calibration) = ws_disconnect_after_publishes(100_000).await;
        let blocked_at = calibration.expect("a publish must block on the stalled link");
        let publishes = blocked_at.saturating_sub(40);
        let (disconnected, blocked) = ws_disconnect_after_publishes(publishes).await;

        assert_eq!(
            blocked, None,
            "a publish blocked within {publishes}, first block was at {blocked_at}"
        );
        assert!(
            disconnected,
            "disconnect did not complete on a stalled WebSocket link after {publishes} publishes returned"
        );
    }

    fn ws_server_stalling_after_the_first_publish() -> BatchedServer {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.set_recv_buffer_size(4096).unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let url = format!("ws://{}/mqtt", socket.local_addr().unwrap());
        let listener = socket.listen(1).unwrap();
        let (resume, resumed) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_hdr_async(stream, Subprotocol)
                .await
                .unwrap();
            let connect = ws.next().await;
            assert!(matches!(connect, Some(Ok(Message::Binary(_)))));
            ws.send(Message::Binary(CONNACK.to_vec().into()))
                .await
                .unwrap();
            tokio::time::sleep(CANCEL_AFTER + CANCEL_AFTER * 2 / 3).await;
            let mut buffer = Vec::new();
            while complete_frame(&buffer).is_none() {
                match ws.next().await {
                    Some(Ok(Message::Binary(data))) => buffer.extend_from_slice(&data),
                    Some(Ok(_)) => {}
                    other => panic!("{other:?}"),
                }
            }
            let _ = resumed.await;
            while let Ok(Some(Ok(message))) = tokio::time::timeout(SERVER_IDLE * 2, ws.next()).await
            {
                if let Message::Binary(data) = message {
                    buffer.extend_from_slice(&data);
                }
            }
            publish_topics(&buffer)
        });
        (url, resume, server)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn websocket_qos0_publish_written_in_a_batch_cut_short_by_disconnect_is_not_reported_unsent(
    ) {
        let (outcomes, disconnected, received) =
            batched_publishes_cut_short_by_disconnect(ws_server_stalling_after_the_first_publish())
                .await;

        assert_batched_outcomes_match_delivery(&outcomes, disconnected, &received);
    }

    fn ws_server_stalling_once_it_has_read(stall_after: &'static str) -> StallingServer {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("ws://{}/mqtt", listener.local_addr().unwrap());
        let (resume, resumed) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let listener = TcpListener::from_std(listener).unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_hdr_async(stream, Subprotocol)
                .await
                .unwrap();
            let mut buffer = Vec::new();
            while complete_frame(&buffer).is_none() {
                match ws.next().await {
                    Some(Ok(Message::Binary(data))) => buffer.extend_from_slice(&data),
                    Some(Ok(_)) => {}
                    other => panic!("{other:?}"),
                }
            }
            let mut buffer = without_connect(buffer);
            ws.send(Message::Binary(CONNACK.to_vec().into()))
                .await
                .unwrap();
            tokio::time::sleep(SERVER_START_DELAY).await;
            while !publish_topics(&buffer)
                .iter()
                .any(|topic| topic == stall_after)
            {
                match ws.next().await {
                    Some(Ok(Message::Binary(data))) => buffer.extend_from_slice(&data),
                    Some(Ok(_)) => {}
                    other => panic!("{other:?}"),
                }
            }
            let before_stall = publish_topics(&buffer);
            let _ = resumed.await;
            while let Ok(Some(Ok(message))) = tokio::time::timeout(SERVER_IDLE * 2, ws.next()).await
            {
                if let Message::Binary(data) = message {
                    buffer.extend_from_slice(&data);
                }
            }
            (before_stall, publish_topics(&buffer))
        });
        (url, resume, server)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn websocket_qos0_publish_the_server_read_is_not_reported_unsent() {
        let (outcome, disconnected, before_stall, received) =
            small_publish_read_by_the_server_before_a_stall(ws_server_stalling_once_it_has_read(
                "t/a",
            ))
            .await;

        assert_read_publish_is_not_reported_unsent(
            &outcome,
            disconnected,
            &before_stall,
            &received,
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_websocket_publishes_stay_within_default_frame_limits() {
        let (url, server) = stalled_ws_server().await;
        let client = connected_client("ws-batch", &url).await;
        let mut tasks = Vec::new();
        for index in 0..40 {
            let publishing = client.clone();
            tasks.push(tokio::spawn(async move {
                publishing
                    .publish_qos0(format!("t/{index}"), vec![7u8; 1 << 20])
                    .await
            }));
        }
        let (received, ended) = server.await.unwrap();
        for task in tasks {
            task.abort();
        }

        assert_eq!(received.unfinished, None, "server ended with {ended}");
        assert_eq!(received.publishes.len(), 40, "server ended with {ended}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn websocket_publish_cancelled_during_the_sink_flush_is_still_sent() {
        let (url, server) = stalled_ws_server().await;
        let client = connected_client("ws-cancel", &url).await;
        let cancelled = tokio::time::timeout(
            CANCEL_AFTER,
            client.publish_qos0("t/big", vec![7u8; 8 << 20]),
        )
        .await;
        assert!(
            cancelled.is_err(),
            "the publish must still be writing when it is cancelled"
        );
        let (received, ended) = server.await.unwrap();

        assert_only_complete_packets(&received, &["t/big"]);
        assert!(!ended.is_empty());
    }

    fn connect_frame() -> Vec<u8> {
        let client_id = b"ws-split";
        let mut body = vec![
            0x00, 0x04, b'M', b'Q', b'T', b'T', 0x05, 0x02, 0x00, 0x3C, 0x00,
        ];
        body.extend_from_slice(&u16::try_from(client_id.len()).unwrap().to_be_bytes());
        body.extend_from_slice(client_id);
        let mut frame = vec![0x10, u8::try_from(body.len()).unwrap()];
        frame.extend_from_slice(&body);
        frame
    }

    fn qos0_publish(topic: &str, payload: &[u8]) -> Vec<u8> {
        let mut body = u16::try_from(topic.len()).unwrap().to_be_bytes().to_vec();
        body.extend_from_slice(topic.as_bytes());
        body.push(0x00);
        body.extend_from_slice(payload);
        let mut frame = vec![0x30];
        let mut remaining = body.len();
        loop {
            let mut byte = u8::try_from(remaining % 128).unwrap();
            remaining /= 128;
            if remaining > 0 {
                byte |= 0x80;
            }
            frame.push(byte);
            if remaining == 0 {
                break;
            }
        }
        frame.extend_from_slice(&body);
        frame
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn broker_accepts_packets_split_across_websocket_frames() {
        let config = BrokerConfig::default()
            .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
            .with_bind_address(([127, 0, 0, 1], 0))
            .with_websocket(
                WebSocketConfig::new()
                    .with_bind_address(([127, 0, 0, 1], 0))
                    .with_path("/mqtt")
                    .with_tls(false),
            );
        let mut broker = MqttBroker::with_config(config).await.unwrap();
        let tcp_addr = broker.local_addr().unwrap();
        let ws_addr = broker.ws_local_addr().unwrap();
        let shutdown = broker.shutdown_handle();
        let run = tokio::spawn(async move { broker.run().await });

        let subscriber = MqttClient::new("ws-split-subscriber");
        subscriber
            .connect(&format!("mqtt://{tcp_addr}"))
            .await
            .unwrap();
        let (delivered_tx, mut delivered) = tokio::sync::mpsc::unbounded_channel();
        subscriber
            .subscribe("t/split", move |message| {
                let _ = delivered_tx.send(message.payload.clone());
            })
            .await
            .unwrap();

        let mut request = format!("ws://{ws_addr}/mqtt")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", "mqtt".parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        let connect = connect_frame();
        let (connect_head, connect_tail) = connect.split_at(3);
        ws.send(Message::Binary(connect_head.to_vec().into()))
            .await
            .unwrap();
        ws.send(Message::Binary(connect_tail.to_vec().into()))
            .await
            .unwrap();
        let connack = ws.next().await;
        assert!(
            matches!(connack, Some(Ok(Message::Binary(_)))),
            "{connack:?}"
        );

        let payload = vec![9u8; 3 * 1024 * 1024];
        let mut stream = qos0_publish("t/split", &payload);
        stream.extend_from_slice(&qos0_publish("t/split", b"second"));
        for chunk in stream.chunks(1024 * 1024 + 7) {
            ws.send(Message::Binary(chunk.to_vec().into()))
                .await
                .unwrap();
        }
        let first = tokio::time::timeout(SETTLE_WITHIN, delivered.recv())
            .await
            .unwrap()
            .unwrap();
        let second = tokio::time::timeout(SETTLE_WITHIN, delivered.recv())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(first, payload);
        assert_eq!(second, b"second".to_vec());
        subscriber.disconnect().await.unwrap();
        shutdown.shutdown();
        let _ = tokio::time::timeout(SETTLE_WITHIN, run).await;
    }
}

#[cfg(feature = "transport-quic")]
mod quic {
    use super::*;
    use mqtt5::packet::publish::PublishPacket;
    use mqtt5::packet::Packet;
    use mqtt5::transport::{FlowId, QuicConfig, QuicStreamManager, QuicTransport, StreamStrategy};
    use mqtt5::Transport;
    use quinn::crypto::rustls::QuicServerConfig;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use std::sync::Arc;

    fn endpoint() -> quinn::Endpoint {
        endpoint_with_uni_streams(100)
    }

    fn endpoint_with_uni_streams(max_uni_streams: u32) -> quinn::Endpoint {
        configured_endpoint(max_uni_streams, 1_048_576)
    }

    fn configured_endpoint(max_uni_streams: u32, receive_window: u32) -> quinn::Endpoint {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let chain = vec![CertificateDer::from(cert.cert.der().to_vec())];
        let key = PrivateKeyDer::try_from(cert.signing_key.serialize_der()).unwrap();
        let mut tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .unwrap();
        tls.alpn_protocols = vec![b"mqtt".to_vec()];
        let mut server =
            quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls).unwrap()));
        let mut transport = quinn::TransportConfig::default();
        transport.stream_receive_window(65_536u32.into());
        transport.max_concurrent_uni_streams(max_uni_streams.into());
        transport.receive_window(receive_window.into());
        server.transport_config(Arc::new(transport));
        quinn::Endpoint::server(server, "127.0.0.1:0".parse().unwrap()).unwrap()
    }

    async fn connected_quic_client(name: &str, url: &str, strategy: StreamStrategy) -> MqttClient {
        let options = ConnectOptions::new(name).with_automatic_reconnect(false);
        let client = MqttClient::with_options(options.clone());
        client.set_insecure_tls(true).await;
        client.set_quic_stream_strategy(strategy).await;
        client.connect_with_options(url, options).await.unwrap();
        client
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_control_stream_publish_after_a_cancelled_publish_is_framed() {
        let endpoint = endpoint();
        let url = format!("quic://{}", endpoint.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let (send, recv) = connection.accept_bi().await.unwrap();
            let received = serve_stalled(recv, send).await;
            (received, connection)
        });
        let client = connected_quic_client("cancel-quic", &url, StreamStrategy::ControlOnly).await;
        let next = cancelled_publish_then_publish(&client, "t/big", "t/next").await;
        let (received, connection) = server.await.unwrap();

        assert_only_complete_packets(&received, &["t/big", "t/next"]);
        assert!(
            matches!(next, Ok(PublishResult::Sent(Delivery::AtLeastOnce { .. }))),
            "{next:?}"
        );
        drop(connection);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_topic_stream_publish_after_a_cancelled_publish_is_framed() {
        let endpoint = endpoint();
        let url = format!("quic://{}", endpoint.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let (mut control_send, mut control_recv) = connection.accept_bi().await.unwrap();
            accept_connect(&mut control_recv, &mut control_send).await;
            let mut topic_stream = connection.accept_uni().await.unwrap();
            tokio::time::sleep(SERVER_STALL).await;
            let received = read_publishes(&mut topic_stream, Vec::new(), &mut control_send).await;
            (received, connection)
        });
        let client =
            connected_quic_client("cancel-topic-stream", &url, StreamStrategy::DataPerTopic).await;
        let next = cancelled_publish_then_publish(&client, "t/same", "t/same").await;
        let (received, connection) = server.await.unwrap();

        assert_only_complete_packets(&received, &["t/same", "t/same"]);
        assert!(
            matches!(next, Ok(PublishResult::Sent(Delivery::AtLeastOnce { .. }))),
            "{next:?}"
        );
        drop(connection);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_cancelled_publish_on_its_own_stream_is_completed_before_the_stream_ends() {
        let endpoint = endpoint();
        let url = format!("quic://{}", endpoint.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let (mut control_send, mut control_recv) = connection.accept_bi().await.unwrap();
            accept_connect(&mut control_recv, &mut control_send).await;
            let mut first_stream = connection.accept_uni().await.unwrap();
            tokio::time::sleep(SERVER_STALL).await;
            let first = read_publishes(&mut first_stream, Vec::new(), &mut control_send).await;
            let mut second_stream = connection.accept_uni().await.unwrap();
            let second = read_publishes(&mut second_stream, Vec::new(), &mut control_send).await;
            ((first, second), connection)
        });
        let client =
            connected_quic_client("cancel-own-stream", &url, StreamStrategy::DataPerPublish).await;
        let next = cancelled_publish_then_publish(&client, "t/big", "t/next").await;
        let ((first, second), connection) = server.await.unwrap();

        assert_only_complete_packets(&first, &["t/big"]);
        assert!(first.finished, "the stream must end after its packet");
        assert_only_complete_packets(&second, &["t/next"]);
        assert!(
            matches!(next, Ok(PublishResult::Sent(Delivery::AtLeastOnce { .. }))),
            "{next:?}"
        );
        drop(connection);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_flow_stream_packet_after_a_cancelled_write_is_framed() {
        let endpoint = endpoint();
        let addr = endpoint.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let mut flow_stream = connection.accept_uni().await.unwrap();
            tokio::time::sleep(SERVER_STALL).await;
            let received =
                read_publishes(&mut flow_stream, Vec::new(), &mut tokio::io::sink()).await;
            (received, connection)
        });
        let mut transport =
            QuicTransport::new(QuicConfig::new(addr, "localhost").with_verify_server_cert(false));
        transport.connect().await.unwrap();
        let split = transport.into_split().unwrap();
        let manager =
            QuicStreamManager::new(Arc::new(split.connection), StreamStrategy::DataPerTopic);
        let flow_id = FlowId::client(1);
        let stream = manager.open_data_stream().await.unwrap();
        manager.register_flow_stream(flow_id, stream).await;
        let big = Packet::Publish(PublishPacket::new(
            "t/big",
            large_payload(),
            QoS::AtMostOnce,
        ));
        let cancelled =
            tokio::time::timeout(CANCEL_AFTER, manager.send_on_flow(flow_id, big)).await;
        assert!(
            cancelled.is_err(),
            "the large packet must still be writing when it is cancelled"
        );
        let next = Packet::Publish(PublishPacket::new(
            "t/next",
            b"next".to_vec(),
            QoS::AtMostOnce,
        ));
        let sent = tokio::time::timeout(SETTLE_WITHIN, manager.send_on_flow(flow_id, next))
            .await
            .expect("the packet after a cancelled one must be written");
        let (received, connection) = server.await.unwrap();

        assert!(sent.is_ok(), "{sent:?}");
        assert_only_complete_packets(&received, &["t/big", "t/next"]);
        drop(connection);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_publish_cancelled_while_waiting_for_its_stream_is_still_sent() {
        let endpoint = endpoint_with_uni_streams(1);
        let url = format!("quic://{}", endpoint.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let (mut control_send, mut control_recv) = connection.accept_bi().await.unwrap();
            accept_connect(&mut control_recv, &mut control_send).await;
            let mut first_stream = connection.accept_uni().await.unwrap();
            tokio::time::sleep(SERVER_STALL).await;
            let first = read_publishes(&mut first_stream, Vec::new(), &mut control_send).await;
            let second = match tokio::time::timeout(SETTLE_WITHIN, connection.accept_uni()).await {
                Ok(Ok(mut stream)) => {
                    Some(read_publishes(&mut stream, Vec::new(), &mut control_send).await)
                }
                _ => None,
            };
            ((first, second), connection)
        });
        let client =
            connected_quic_client("cancel-opening", &url, StreamStrategy::DataPerPublish).await;
        let writing = client.clone();
        let big = tokio::spawn(async move {
            match writing
                .begin_publish("t/big", large_payload(), qos1())
                .await
            {
                Ok(handle) => Ok(handle.outcome().await),
                Err(e) => Err(e),
            }
        });
        tokio::time::sleep(CANCEL_AFTER).await;
        let cancelled = tokio::time::timeout(
            CANCEL_AFTER,
            client.publish_with_options("t/waiting", b"waiting".to_vec(), qos1()),
        )
        .await;
        assert!(
            cancelled.is_err(),
            "the publish must still be waiting for its stream when it is cancelled"
        );
        let ((first, second), connection) = server.await.unwrap();
        let big = big.await.unwrap();

        assert_only_complete_packets(&first, &["t/big"]);
        let second = second.expect("the cancelled publish must be sent on a stream of its own");
        assert_only_complete_packets(&second, &["t/waiting"]);
        assert!(second.finished, "the stream must end after its packet");
        assert!(
            matches!(
                big,
                Ok(PublishOutcome::Delivered(Delivery::AtLeastOnce { .. }))
            ),
            "{big:?}"
        );
        drop(connection);
    }

    async fn raw_manager(addr: std::net::SocketAddr) -> Arc<QuicStreamManager> {
        raw_manager_with_flow_headers(addr, false).await
    }

    async fn raw_manager_with_flow_headers(
        addr: std::net::SocketAddr,
        flow_headers: bool,
    ) -> Arc<QuicStreamManager> {
        let mut transport =
            QuicTransport::new(QuicConfig::new(addr, "localhost").with_verify_server_cert(false));
        transport.connect().await.unwrap();
        let split = transport.into_split().unwrap();
        Arc::new(
            QuicStreamManager::new(Arc::new(split.connection), StreamStrategy::DataPerTopic)
                .with_flow_headers(flow_headers),
        )
    }

    fn qos0(topic: &str, payload: Vec<u8>) -> Packet {
        Packet::Publish(PublishPacket::new(topic, payload, QoS::AtMostOnce))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_topic_stream_with_a_cancelled_packet_delivers_it_whole_under_cache_pressure() {
        let endpoint = configured_endpoint(500, 16_000_000);
        let addr = endpoint.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let mut first = connection.accept_uni().await.unwrap();
            tokio::time::sleep(SERVER_STALL).await;
            let received = read_publishes(&mut first, Vec::new(), &mut tokio::io::sink()).await;
            (received, connection)
        });
        let manager = raw_manager(addr).await;
        let writing = Arc::clone(&manager);
        let cancelled = tokio::spawn(async move {
            tokio::time::timeout(
                CANCEL_AFTER * 2,
                writing.send_on_topic_stream("t/0".into(), qos0("t/0", vec![7u8; 1 << 20])),
            )
            .await
            .is_err()
        });
        tokio::time::sleep(CANCEL_AFTER / 2).await;
        for index in 1..=100 {
            let topic = format!("t/{index}");
            tokio::time::timeout(
                SETTLE_WITHIN,
                manager.send_on_topic_stream(topic.clone(), qos0(&topic, b"x".to_vec())),
            )
            .await
            .expect("a publish on another topic must not wait for the busy stream")
            .unwrap();
        }
        assert!(
            cancelled.await.unwrap(),
            "the large packet must still be writing when it is cancelled"
        );
        let (received, connection) = server.await.unwrap();

        assert_only_complete_packets(&received, &["t/0"]);
        drop(connection);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_closing_all_streams_does_not_wait_for_a_stalled_stream() {
        let endpoint = endpoint();
        let addr = endpoint.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let stalled = connection.accept_uni().await.unwrap();
            (stalled, connection)
        });
        let manager = raw_manager(addr).await;
        let cancelled = tokio::time::timeout(
            CANCEL_AFTER,
            manager.send_on_topic_stream("t/0".into(), qos0("t/0", vec![7u8; 1 << 20])),
        )
        .await;
        assert!(
            cancelled.is_err(),
            "the large packet must still be writing when it is cancelled"
        );
        let (stalled, connection) = server.await.unwrap();
        tokio::time::sleep(CANCEL_AFTER / 3).await;
        let closed = tokio::time::timeout(Duration::from_secs(5), {
            let closing = Arc::clone(&manager);
            tokio::task::spawn_blocking(move || closing.close_all_streams())
        })
        .await;

        assert!(
            closed.is_ok(),
            "closing all streams waited for the stalled stream"
        );
        drop(stalled);
        drop(connection);
    }

    #[tokio::test]
    async fn quic_failed_writes_on_a_stopped_stream_do_not_accumulate() {
        let endpoint = endpoint();
        let addr = endpoint.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let mut stream = connection.accept_uni().await.unwrap();
            let mut first = [0u8; 4];
            stream.read_exact(&mut first).await.unwrap();
            stream.stop(0u32.into()).unwrap();
            connection
        });
        let manager = raw_manager(addr).await;
        manager
            .send_on_topic_stream("t/x".into(), qos0("t/x", b"hi".to_vec()))
            .await
            .unwrap();
        let connection = server.await.unwrap();
        tokio::time::sleep(CANCEL_AFTER).await;
        let payload = vec![7u8; 1 << 20];
        let before = thread_heap();
        let mut failed = 0;
        for _ in 0..64 {
            if manager
                .send_on_topic_stream("t/x".into(), qos0("t/x", payload.clone()))
                .await
                .is_err()
            {
                failed += 1;
            }
        }
        let grown = thread_heap() - before;
        drop(connection);

        assert_eq!(failed, 64);
        assert!(
            grown < 8 * MIB,
            "64 failed 1 MiB writes on a stopped stream left {} MiB allocated",
            grown / MIB
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_client_reconnects_while_a_data_stream_is_stalled() {
        let endpoint = endpoint();
        let url = format!("quic://{}", endpoint.local_addr().unwrap());
        let (reconnected_tx, reconnected) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let first = endpoint.accept().await.unwrap().await.unwrap();
            let (mut control_send, mut control_recv) = first.accept_bi().await.unwrap();
            accept_connect(&mut control_recv, &mut control_send).await;
            let stalled = first.accept_uni().await.unwrap();
            let second = endpoint.accept().await.unwrap().await.unwrap();
            let (mut second_send, mut second_recv) = second.accept_bi().await.unwrap();
            accept_connect(&mut second_recv, &mut second_send).await;
            let _ = reconnected_tx.send(());
            (
                first,
                stalled,
                control_send,
                control_recv,
                second,
                second_send,
                second_recv,
            )
        });
        let options = ConnectOptions::new("reconnect-stalled")
            .with_keep_alive(Duration::from_secs(1))
            .with_automatic_reconnect(true)
            .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(500));
        let client = MqttClient::with_options(options.clone());
        client.set_insecure_tls(true).await;
        client
            .set_quic_stream_strategy(StreamStrategy::DataPerTopic)
            .await;
        client.connect_with_options(&url, options).await.unwrap();
        let cancelled = tokio::time::timeout(
            CANCEL_AFTER,
            client.publish_with_options("t/big", large_payload(), qos1()),
        )
        .await;
        assert!(
            cancelled.is_err(),
            "the large publish must still be writing when it is cancelled"
        );
        let reconnect = tokio::time::timeout(Duration::from_secs(15), reconnected).await;
        let held = if matches!(reconnect, Ok(Ok(()))) {
            Some(server.await.unwrap())
        } else {
            server.abort();
            None
        };

        assert!(
            matches!(reconnect, Ok(Ok(()))),
            "the client did not reconnect while a data stream was stalled"
        );
        client.disconnect().await.ok();
        drop(held);
    }

    async fn record_arrivals(
        mut stream: quinn::RecvStream,
        stall: bool,
        arrivals: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    ) {
        if stall {
            tokio::time::sleep(SERVER_STALL).await;
        }
        let mut buffer = Vec::new();
        let mut chunk = vec![0u8; 1 << 16];
        loop {
            while let Some((first, header_len, total)) = complete_frame(&buffer) {
                if first >> 4 == PUBLISH_TYPE {
                    let (topic, _, payload) = parse_qos0_publish(&buffer[header_len..total]);
                    let tag =
                        String::from_utf8_lossy(&payload[..payload.len().min(6)]).into_owned();
                    arrivals.lock().unwrap().push((topic, tag));
                }
                buffer.drain(..total);
            }
            match stream.read(&mut chunk).await {
                Ok(Some(read)) if read > 0 => buffer.extend_from_slice(&chunk[..read]),
                _ => break,
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_topic_stream_keeps_publish_order_when_the_cache_is_full() {
        let endpoint = configured_endpoint(500, 64_000_000);
        let addr = endpoint.local_addr().unwrap();
        let arrivals = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = Arc::clone(&arrivals);
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let mut readers = Vec::new();
            while let Ok(Ok(stream)) =
                tokio::time::timeout(SERVER_STALL * 2, connection.accept_uni()).await
            {
                readers.push(tokio::spawn(record_arrivals(
                    stream,
                    readers.is_empty(),
                    Arc::clone(&recorded),
                )));
            }
            tokio::time::sleep(SERVER_IDLE).await;
            for reader in &readers {
                reader.abort();
            }
            connection
        });
        let manager = raw_manager(addr).await;
        let mut first = b"first-".to_vec();
        first.extend(std::iter::repeat_n(7u8, 1 << 20));
        let cancelled = tokio::time::timeout(
            CANCEL_AFTER,
            manager.send_on_topic_stream("t/same".into(), qos0("t/same", first)),
        )
        .await;
        assert!(
            cancelled.is_err(),
            "the first packet must still be writing when it is cancelled"
        );
        for index in 0..100 {
            let topic = format!("t/other{index}");
            manager
                .send_on_topic_stream(topic.clone(), qos0(&topic, b"other-".to_vec()))
                .await
                .unwrap();
        }
        tokio::time::timeout(
            SETTLE_WITHIN,
            manager.send_on_topic_stream("t/same".into(), qos0("t/same", b"second".to_vec())),
        )
        .await
        .expect("the second packet must be sent")
        .unwrap();
        let connection = server.await.unwrap();
        let same: Vec<String> = arrivals
            .lock()
            .unwrap()
            .iter()
            .filter(|(topic, _)| topic == "t/same")
            .map(|(_, tag)| tag.clone())
            .collect();

        assert_eq!(same, vec!["first-".to_string(), "second".to_string()]);
        drop(connection);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_topic_stream_alias_defined_by_a_publish_cancelled_mid_write_is_kept() {
        let endpoint = configured_endpoint(500, 64_000_000);
        let url = format!("quic://{}", endpoint.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let (mut control_send, mut control_recv) = connection.accept_bi().await.unwrap();
            let mut buffer = Vec::new();
            let mut chunk = vec![0u8; 4096];
            while complete_frame(&buffer).is_none() {
                let read = control_recv.read(&mut chunk).await.unwrap().unwrap();
                buffer.extend_from_slice(&chunk[..read]);
            }
            control_send
                .write_all(&CONNACK_TOPIC_ALIAS_MAXIMUM_5)
                .await
                .unwrap();
            let mut stream = connection.accept_uni().await.unwrap();
            tokio::time::sleep(SERVER_STALL).await;
            let received = read_publishes(&mut stream, Vec::new(), &mut tokio::io::sink()).await;
            (received, connection)
        });
        let client =
            connected_quic_client("cancel-quic-alias", &url, StreamStrategy::DataPerTopic).await;
        let cancelled = tokio::time::timeout(
            CANCEL_AFTER,
            client.publish_with_options("t/large", large_payload(), with_alias(QoS::AtMostOnce, 2)),
        )
        .await;
        assert!(
            cancelled.is_err(),
            "the large publish must still be writing when it is cancelled"
        );
        let alias_only = tokio::time::timeout(
            SETTLE_WITHIN,
            client.publish_with_options("", b"alias-only".to_vec(), with_alias(QoS::AtMostOnce, 2)),
        )
        .await
        .expect("the alias-only publish must be written");
        let (received, connection) = server.await.unwrap();

        assert!(alias_only.is_ok(), "{alias_only:?}");
        assert_only_complete_packets(&received, &["t/large", "t/large"]);
        drop(connection);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_topic_stream_cache_stays_within_its_limit_when_writes_are_cancelled() {
        let endpoint = configured_endpoint(1000, 65_536);
        let addr = endpoint.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            tokio::time::sleep(SETTLE_WITHIN * 3).await;
            connection
        });
        let manager = raw_manager(addr).await;
        let topics = 1000;
        for index in 0..topics {
            let topic = format!("t/{index}");
            let _ = tokio::time::timeout(
                Duration::from_millis(1),
                manager.send_on_topic_stream(topic.clone(), qos0(&topic, vec![7u8; 4096])),
            )
            .await;
        }
        let cached = (0..topics)
            .filter(|index| {
                manager
                    .get_flow_id_for_topic(&format!("t/{index}"))
                    .is_some()
            })
            .count();
        server.abort();

        assert!(
            cached <= 100,
            "the topic stream cache holds {cached} streams, above its limit of 100"
        );
    }

    async fn fill_cache_with_stalled_streams(manager: &QuicStreamManager) {
        for index in 0..100 {
            let topic = format!("t/{index}");
            let _ = tokio::time::timeout(
                Duration::from_millis(1),
                manager.send_on_topic_stream(topic.clone(), qos0(&topic, vec![7u8; 256 * 1024])),
            )
            .await;
        }
        tokio::time::sleep(CANCEL_AFTER).await;
    }

    async fn accept_streams(connection: &quinn::Connection) -> Vec<quinn::RecvStream> {
        let mut streams = Vec::new();
        while let Ok(Ok(stream)) =
            tokio::time::timeout(CANCEL_AFTER * 2, connection.accept_uni()).await
        {
            streams.push(stream);
        }
        streams
    }

    fn spawn_new_topic_publish(
        manager: &Arc<QuicStreamManager>,
    ) -> tokio::task::JoinHandle<mqtt5::Result<()>> {
        let waiting = Arc::clone(manager);
        tokio::spawn(async move {
            waiting
                .send_on_topic_stream("t/new".into(), qos0("t/new", b"x".to_vec()))
                .await
        })
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_publish_waiting_for_a_topic_stream_wakes_when_a_busy_stream_fails() {
        let endpoint = configured_endpoint(1000, 16_000_000);
        let addr = endpoint.local_addr().unwrap();
        let (fail_tx, fail_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let mut streams = accept_streams(&connection).await;
            let _ = fail_rx.await;
            streams[0].stop(0u32.into()).unwrap();
            tokio::time::sleep(SETTLE_WITHIN * 3).await;
            (streams, connection)
        });
        let manager = raw_manager(addr).await;
        fill_cache_with_stalled_streams(&manager).await;
        tokio::time::sleep(CANCEL_AFTER * 3).await;
        let waiter = spawn_new_topic_publish(&manager);
        tokio::time::sleep(CANCEL_AFTER).await;
        assert!(
            !waiter.is_finished(),
            "the new topic must wait for a stream"
        );
        fail_tx.send(()).unwrap();
        let outcome = tokio::time::timeout(SETTLE_WITHIN / 2, waiter).await;
        server.abort();

        assert!(
            outcome.is_ok(),
            "a cached stream failed, but the publish waiting for a stream was never woken"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_publish_waiting_for_a_topic_stream_wakes_when_a_busy_stream_drains() {
        let endpoint = configured_endpoint(1000, 16_000_000);
        let addr = endpoint.local_addr().unwrap();
        let (drain_tx, drain_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let mut streams = accept_streams(&connection).await;
            let _ = drain_rx.await;
            let mut first = streams.remove(0);
            let mut sink = vec![0u8; 1 << 16];
            while let Ok(Some(_)) = first.read(&mut sink).await {}
            tokio::time::sleep(SETTLE_WITHIN * 3).await;
            (streams, connection)
        });
        let manager = raw_manager(addr).await;
        fill_cache_with_stalled_streams(&manager).await;
        tokio::time::sleep(CANCEL_AFTER * 3).await;
        let waiter = spawn_new_topic_publish(&manager);
        tokio::time::sleep(CANCEL_AFTER).await;
        assert!(
            !waiter.is_finished(),
            "the new topic must wait for a stream"
        );
        drain_tx.send(()).unwrap();
        let outcome = tokio::time::timeout(SETTLE_WITHIN / 2, waiter).await;
        server.abort();

        assert!(
            outcome.is_ok(),
            "a cached stream drained, but the waiting publish was never woken"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_publish_waiting_for_a_topic_stream_returns_after_streams_and_connection_close() {
        let endpoint = configured_endpoint(1000, 65_536);
        let addr = endpoint.local_addr().unwrap();
        let (close_tx, close_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let _ = close_rx.await;
            connection.close(0u32.into(), b"bye");
            tokio::time::sleep(SETTLE_WITHIN * 3).await;
        });
        let manager = raw_manager(addr).await;
        fill_cache_with_stalled_streams(&manager).await;
        let waiter = spawn_new_topic_publish(&manager);
        tokio::time::sleep(CANCEL_AFTER).await;
        assert!(
            !waiter.is_finished(),
            "the new topic must wait for a stream"
        );
        manager.close_all_streams();
        close_tx.send(()).unwrap();
        let outcome = tokio::time::timeout(SETTLE_WITHIN / 2, waiter).await;
        server.abort();

        assert!(
            matches!(outcome, Ok(Ok(Err(_)))),
            "after the streams and the connection closed, the waiting publish returned {outcome:?}"
        );
    }

    async fn client_waiting_for_a_topic_stream(
        name: &str,
        receive_window: u32,
    ) -> (
        MqttClient,
        tokio::task::JoinHandle<mqtt5::Result<PublishResult>>,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let endpoint = configured_endpoint(1000, receive_window);
        let url = format!("quic://{}", endpoint.local_addr().unwrap());
        let (close_tx, close_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let (mut send, mut recv) = connection.accept_bi().await.unwrap();
            accept_connect(&mut recv, &mut send).await;
            let _ = close_rx.await;
            connection.close(0u32.into(), b"bye");
            tokio::time::sleep(SETTLE_WITHIN * 3).await;
        });
        let client = connected_quic_client(name, &url, StreamStrategy::DataPerTopic).await;
        for index in 0..100 {
            let _ = tokio::time::timeout(
                Duration::from_millis(1),
                client.publish_qos0(format!("t/{index}"), vec![7u8; 256 * 1024]),
            )
            .await;
        }
        tokio::time::sleep(CANCEL_AFTER).await;
        let publishing = client.clone();
        let waiter =
            tokio::spawn(async move { publishing.publish_qos0("t/new", b"x".to_vec()).await });
        tokio::time::sleep(CANCEL_AFTER).await;
        assert!(
            !waiter.is_finished(),
            "the new topic must wait for a stream"
        );
        (client, waiter, close_tx, server)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_client_publish_waiting_for_a_topic_stream_returns_when_the_connection_closes() {
        let (client, waiter, close_tx, server) =
            client_waiting_for_a_topic_stream("wait-server-close", 65_536).await;
        close_tx.send(()).unwrap();
        let outcome = tokio::time::timeout(SETTLE_WITHIN, waiter).await;
        let disconnect = tokio::time::timeout(SETTLE_WITHIN / 2, client.disconnect()).await;
        server.abort();

        assert!(
            outcome.is_ok(),
            "the waiting publish never returned after the server closed"
        );
        assert!(
            disconnect.is_ok(),
            "disconnect blocked behind the waiting publish"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_client_disconnect_is_not_blocked_by_a_publish_waiting_for_a_topic_stream() {
        let (client, waiter, close_tx, server) =
            client_waiting_for_a_topic_stream("wait-disconnect", 16_000_000).await;
        let disconnect = tokio::time::timeout(SETTLE_WITHIN / 2, client.disconnect()).await;
        let outcome = tokio::time::timeout(SETTLE_WITHIN / 2, waiter).await;
        drop(close_tx);
        server.abort();

        assert!(
            disconnect.is_ok(),
            "disconnect blocked behind the waiting publish"
        );
        assert!(
            matches!(outcome, Ok(Ok(Err(_)))),
            "the waiting publish returned {outcome:?} after disconnect"
        );
    }

    async fn drain_uni(mut stream: quinn::RecvStream) {
        let mut sink = vec![0u8; 1 << 16];
        while let Ok(Some(_)) = stream.read(&mut sink).await {}
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_disconnect_is_not_blocked_by_publishes_stuck_on_a_stalled_topic_stream() {
        let endpoint = configured_endpoint(1000, 16_000_000);
        let url = format!("quic://{}", endpoint.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let (mut send, mut recv) = connection.accept_bi().await.unwrap();
            accept_connect(&mut recv, &mut send).await;
            tokio::spawn(async move {
                let mut sink = vec![0u8; 4096];
                while let Ok(Some(_)) = recv.read(&mut sink).await {}
            });
            let mut held = Vec::new();
            while let Ok(stream) = connection.accept_uni().await {
                held.push(stream);
            }
        });
        let client = connected_quic_client(
            "disconnect-stalled-quic",
            &url,
            StreamStrategy::DataPerTopic,
        )
        .await;
        let tasks = spawn_stuck_publishes(&client, 8, "t/a", 256 * 1024).await;

        let (disconnected, outcomes) = disconnect_while_publishes_are_stuck(&client, tasks).await;
        server.abort();

        assert_stuck_publishes_returned_errors(disconnected, &outcomes);
    }

    async fn control_stream_publishes_committed_while_stalled(datagrams: bool) -> usize {
        let endpoint = configured_endpoint(100, 16_000_000);
        let url = format!("quic://{}", endpoint.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let (mut send, mut recv) = connection.accept_bi().await.unwrap();
            let buffer = accept_connect(&mut recv, &mut send).await;
            tokio::time::sleep(SERVER_STALL * 3).await;
            read_publishes(&mut recv, buffer, &mut tokio::io::sink())
                .await
                .publishes
                .len()
        });
        let options = ConnectOptions::new("datagram-fallback").with_automatic_reconnect(false);
        let client = MqttClient::with_options(options.clone());
        client.set_insecure_tls(true).await;
        client.set_quic_datagrams(datagrams).await;
        client
            .set_quic_stream_strategy(StreamStrategy::ControlOnly)
            .await;
        client.connect_with_options(&url, options).await.unwrap();
        let publish_options = PublishOptions {
            properties: PublishProperties {
                user_properties: vec![("k".to_string(), "v".repeat(60 * 1024))],
                ..Default::default()
            },
            ..Default::default()
        };
        let tasks: Vec<_> = (0..300)
            .map(|_| {
                let publishing = client.clone();
                let options = publish_options.clone();
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(
                        SERVER_STALL / 2,
                        publishing.publish_with_options("t/d", b"x".to_vec(), options),
                    )
                    .await;
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap();
        }
        server.await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn quic_publishes_that_do_not_fit_a_datagram_stay_within_the_pending_limit() {
        let committed = control_stream_publishes_committed_while_stalled(true).await;

        assert!(
            committed <= 40,
            "{committed} publishes of 60 KiB that did not fit a datagram were committed during the stall"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn quic_control_stream_publishes_without_datagrams_stay_within_the_pending_limit() {
        let committed = control_stream_publishes_committed_while_stalled(false).await;

        assert!(
            committed <= 40,
            "{committed} publishes of 60 KiB were committed during the stall"
        );
    }

    async fn waiter_is_woken_after_a_cached_stream_fails() -> bool {
        let endpoint = configured_endpoint(1000, 16_000_000);
        let addr = endpoint.local_addr().unwrap();
        let (fail_tx, mut fail_rx) = tokio::sync::oneshot::channel::<()>();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let mut streams = vec![connection.accept_uni().await.unwrap()];
            loop {
                tokio::select! {
                    biased;
                    failed = &mut fail_rx => {
                        failed.unwrap();
                        break;
                    }
                    accepted = connection.accept_uni() => streams.push(accepted.unwrap()),
                }
            }
            streams[0].stop(0u32.into()).unwrap();
            done_rx.await.unwrap();
            (streams, connection)
        });
        let manager = raw_manager(addr).await;
        for index in 0..100 {
            let topic = format!("t/{index}");
            let _ = tokio::time::timeout(
                Duration::from_millis(1),
                manager.send_on_topic_stream(topic.clone(), qos0(&topic, vec![7u8; 100 * 1024])),
            )
            .await;
        }
        tokio::time::sleep(CANCEL_AFTER * 3).await;
        let waiting = Arc::clone(&manager);
        let waiter = tokio::spawn(async move {
            waiting
                .send_on_topic_stream("new/0".to_string(), qos0("new/0", b"x".to_vec()))
                .await
        });
        tokio::time::sleep(CANCEL_AFTER).await;
        let was_waiting = !waiter.is_finished();
        fail_tx.send(()).unwrap();
        let woken = tokio::time::timeout(SETTLE_WITHIN / 2, waiter)
            .await
            .is_ok();
        done_tx.send(()).unwrap();
        tokio::time::timeout(SETTLE_WITHIN, server)
            .await
            .unwrap()
            .unwrap();
        assert!(was_waiting, "the new topic must wait for a stream");
        woken
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn waiters_for_a_topic_stream_are_all_woken_when_cached_streams_fail() {
        let rounds = 20;
        let mut asleep = 0;
        for _ in 0..rounds {
            let trials: Vec<_> = (0..8)
                .map(|_| tokio::spawn(waiter_is_woken_after_a_cached_stream_fails()))
                .collect();
            for trial in trials {
                if !trial.await.unwrap() {
                    asleep += 1;
                }
            }
        }

        assert_eq!(
            asleep,
            0,
            "{asleep} of {} trials left the waiter asleep after a cached stream failed",
            rounds * 8
        );
    }

    async fn remove_flow_stream_after_a_stalled_send(
        server_reads: bool,
    ) -> (bool, Result<usize, quinn::ReadToEndError>) {
        let endpoint = configured_endpoint(100, 16_000_000);
        let addr = endpoint.local_addr().unwrap();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let mut recv = connection.accept_uni().await.unwrap();
            if server_reads {
                let received = recv.read_to_end(16_000_000).await.map(|data| data.len());
                done_rx.await.unwrap();
                received
            } else {
                done_rx.await.unwrap();
                recv.read_to_end(16_000_000).await.map(|data| data.len())
            }
        });
        let manager = raw_manager(addr).await;
        let flow_id = FlowId::client(1);
        let stream = manager.open_data_stream().await.unwrap();
        manager.register_flow_stream(flow_id, stream).await;
        let _ = tokio::time::timeout(
            CANCEL_AFTER,
            manager.send_on_flow(flow_id, qos0("t/flow", vec![7u8; 1_000_000])),
        )
        .await;
        let removed = tokio::time::timeout(SETTLE_WITHIN / 2, manager.remove_flow_stream(flow_id))
            .await
            .is_ok_and(|removed| removed);
        done_tx.send(()).unwrap();
        let received = tokio::time::timeout(SETTLE_WITHIN, server)
            .await
            .unwrap()
            .unwrap();
        (removed, received)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn removing_a_flow_stream_the_peer_does_not_read_resets_it() {
        let (removed, received) = remove_flow_stream_after_a_stalled_send(false).await;

        assert!(
            removed,
            "remove_flow_stream blocked on a stream the peer does not read"
        );
        assert!(
            matches!(
                received,
                Err(quinn::ReadToEndError::Read(quinn::ReadError::Reset(_)))
            ),
            "a stream removed with a packet still unsent must be reset, not finished: {received:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn removing_a_flow_stream_the_peer_reads_delivers_its_packet() {
        let (removed, received) = remove_flow_stream_after_a_stalled_send(true).await;

        assert!(
            removed,
            "remove_flow_stream blocked on a stream the peer reads"
        );
        assert!(
            received.as_ref().is_ok_and(|len| *len > 1_000_000),
            "the peer received {received:?}"
        );
    }

    async fn new_topic_publish_after_sends_that_failed_to_encode(flow_headers: bool) -> String {
        let endpoint = configured_endpoint(1000, 16_000_000);
        let addr = endpoint.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            while let Ok(stream) = connection.accept_uni().await {
                tokio::spawn(drain_uni(stream));
            }
        });
        let manager = raw_manager_with_flow_headers(addr, flow_headers).await;
        for index in 0..100 {
            let topic = format!("bad/{index}");
            let unencodable = qos0(&format!("{topic}\0"), b"x".to_vec());
            assert!(manager
                .send_on_topic_stream(topic, unencodable)
                .await
                .is_err());
        }
        let sent = tokio::time::timeout(
            SETTLE_WITHIN / 2,
            manager.send_on_topic_stream("good".to_string(), qos0("good", b"y".to_vec())),
        )
        .await;
        server.abort();
        format!("{sent:?}")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_new_topic_publish_is_not_blocked_by_streams_left_holding_only_a_flow_header() {
        let sent = new_topic_publish_after_sends_that_failed_to_encode(true).await;

        assert_eq!(
            sent, "Ok(Ok(()))",
            "after 100 sends that failed to encode, the topic stream cache was full of streams that can never be evicted"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_new_topic_publish_after_sends_that_failed_to_encode_without_flow_headers() {
        let sent = new_topic_publish_after_sends_that_failed_to_encode(false).await;

        assert_eq!(sent, "Ok(Ok(()))");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_subscribe_cancelled_before_commit_leaves_no_callback() {
        let endpoint = configured_endpoint(1000, 16_000_000);
        let url = format!("quic://{}", endpoint.local_addr().unwrap());
        let (publish_tx, publish_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let (mut send, mut recv) = connection.accept_bi().await.unwrap();
            accept_connect(&mut recv, &mut send).await;
            tokio::spawn(async move {
                let mut sink = vec![0u8; 4096];
                while let Ok(Some(_)) = recv.read(&mut sink).await {}
            });
            let holding = connection.clone();
            let held = tokio::spawn(async move {
                let mut held = Vec::new();
                while let Ok(stream) = holding.accept_uni().await {
                    held.push(stream);
                }
                held
            });
            let _ = publish_rx.await;
            send.write_all(&[0x30, 0x06, 0x00, 0x03, b't', b'/', b'x', 0x00])
                .await
                .unwrap();
            tokio::time::sleep(CANCEL_AFTER * 2).await;
            held.abort();
        });
        let client =
            connected_quic_client("orphan-callback", &url, StreamStrategy::DataPerTopic).await;
        for index in 0..100 {
            let _ = tokio::time::timeout(
                Duration::from_millis(1),
                client.publish_qos0(format!("t/{index}"), vec![7u8; 256 * 1024]),
            )
            .await;
        }
        tokio::time::sleep(CANCEL_AFTER).await;
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counting = Arc::clone(&calls);
        let cancelled = tokio::time::timeout(
            CANCEL_AFTER,
            client.subscribe("t/+", move |_| {
                counting.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
        )
        .await;
        publish_tx.send(()).unwrap();
        server.await.unwrap();
        let delivered = calls.load(std::sync::atomic::Ordering::SeqCst);

        assert!(
            cancelled.is_err(),
            "the subscribe must be cancelled while waiting for a stream"
        );
        assert_eq!(
            delivered, 0,
            "a subscribe cancelled before its SUBSCRIBE was sent left a callback that received a message"
        );
    }

    async fn acknowledge_subscriptions(
        mut recv: quinn::RecvStream,
        send: Arc<tokio::sync::Mutex<quinn::SendStream>>,
    ) {
        let mut buffer = Vec::new();
        let mut chunk = vec![0u8; 1 << 16];
        while let Ok(Some(read)) = recv.read(&mut chunk).await {
            buffer.extend_from_slice(&chunk[..read]);
            while let Some((first, header_len, total)) = complete_frame(&buffer) {
                let ack = match first >> 4 {
                    SUBSCRIBE_TYPE => Some(0x90),
                    UNSUBSCRIBE_TYPE => Some(0xb0),
                    _ => None,
                };
                if let Some(ack) = ack {
                    let [high, low] = [buffer[header_len], buffer[header_len + 1]];
                    send.lock()
                        .await
                        .write_all(&[ack, 0x04, high, low, 0x00, 0x00])
                        .await
                        .unwrap();
                }
                buffer.drain(..total);
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_subscribe_granted_after_an_unsubscribe_overtook_it_keeps_its_callback() {
        let endpoint = configured_endpoint(1000, 16_000_000);
        let url = format!("quic://{}", endpoint.local_addr().unwrap());
        let (release_tx, mut release_rx) = tokio::sync::oneshot::channel::<()>();
        let (publish_tx, publish_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let (mut send, mut recv) = connection.accept_bi().await.unwrap();
            accept_connect(&mut recv, &mut send).await;
            let send = Arc::new(tokio::sync::Mutex::new(send));
            tokio::spawn(acknowledge_subscriptions(recv, Arc::clone(&send)));
            let mut held = Vec::new();
            loop {
                tokio::select! {
                    stream = connection.accept_uni() => match stream {
                        Ok(stream) => held.push(stream),
                        Err(_) => return,
                    },
                    _ = &mut release_rx => break,
                }
            }
            for stream in held {
                tokio::spawn(acknowledge_subscriptions(stream, Arc::clone(&send)));
            }
            let answering = Arc::clone(&send);
            tokio::spawn(async move {
                while let Ok(stream) = connection.accept_uni().await {
                    tokio::spawn(acknowledge_subscriptions(stream, Arc::clone(&answering)));
                }
            });
            let _ = publish_rx.await;
            send.lock()
                .await
                .write_all(&[0x30, 0x06, 0x00, 0x03, b't', b'/', b'a', 0x00])
                .await
                .unwrap();
            tokio::time::sleep(CANCEL_AFTER * 2).await;
        });
        let client =
            connected_quic_client("unsubscribe-overtakes", &url, StreamStrategy::DataPerTopic)
                .await;
        for index in 0..100 {
            let _ = tokio::time::timeout(
                Duration::from_millis(1),
                client.publish_qos0(format!("t/{index}"), vec![7u8; 256 * 1024]),
            )
            .await;
        }
        tokio::time::sleep(CANCEL_AFTER).await;
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counting = Arc::clone(&calls);
        let subscribing = client.clone();
        let subscribe = tokio::spawn(async move {
            subscribing
                .subscribe("t/a", move |_| {
                    counting.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                })
                .await
        });
        tokio::time::sleep(CANCEL_AFTER).await;
        let waiting = !subscribe.is_finished();
        let unsubscribed = tokio::time::timeout(SETTLE_WITHIN / 2, client.unsubscribe("t/a")).await;
        release_tx.send(()).unwrap();
        let subscribed = tokio::time::timeout(SETTLE_WITHIN, subscribe).await;
        publish_tx.send(()).unwrap();
        server.await.unwrap();
        let delivered = calls.load(std::sync::atomic::Ordering::SeqCst);

        assert!(
            waiting,
            "the subscribe must wait for a stream before its commit"
        );
        assert!(matches!(unsubscribed, Ok(Ok(()))), "{unsubscribed:?}");
        assert!(matches!(subscribed, Ok(Ok(Ok(_)))), "{subscribed:?}");
        assert_eq!(
            delivered, 1,
            "the subscribe was granted after an unsubscribe overtook it, so its callback must receive the PUBLISH"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_failed_ack_write_on_the_control_stream_ends_the_connection() {
        let endpoint = endpoint();
        let url = format!("quic://{}", endpoint.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let (mut send, mut recv) = connection.accept_bi().await.unwrap();
            accept_connect(&mut recv, &mut send).await;
            recv.stop(0u32.into()).unwrap();
            tokio::time::sleep(CANCEL_AFTER).await;
            send.write_all(&PUBLISH_T_ACK_QOS1).await.unwrap();
            tokio::time::sleep(SETTLE_WITHIN).await;
            drop(connection);
            drop(endpoint);
        });
        let (client, disconnects) =
            quic_client_counting_disconnects("quic-failed-ack-write", &url).await;

        wait_for_total(&[&disconnects], 1).await;
        let disconnected = disconnects.load(std::sync::atomic::Ordering::SeqCst);
        let connected = client.is_connected().await;
        server.abort();

        assert_eq!(
            disconnected, 1,
            "a PUBACK write that fails because the server stopped the control stream must end the connection with one Disconnected event"
        );
        assert!(
            !connected,
            "the client must not report connected after its PUBACK write failed"
        );
    }

    async fn quic_client_counting_disconnects(
        name: &str,
        url: &str,
    ) -> (MqttClient, Arc<std::sync::atomic::AtomicUsize>) {
        let options = ConnectOptions::new(name)
            .with_automatic_reconnect(false)
            .with_keep_alive(Duration::from_secs(30));
        let client = MqttClient::with_options(options.clone());
        client.set_insecure_tls(true).await;
        let disconnects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counting = Arc::clone(&disconnects);
        client
            .on_connection_event(move |event| {
                if matches!(event, ConnectionEvent::Disconnected { .. }) {
                    counting.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            })
            .await
            .unwrap();
        client.connect_with_options(url, options).await.unwrap();
        (client, disconnects)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_failed_ack_write_on_the_control_stream_closes_the_quic_connection() {
        let endpoint = endpoint();
        let url = format!("quic://{}", endpoint.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let (mut send, mut recv) = connection.accept_bi().await.unwrap();
            accept_connect(&mut recv, &mut send).await;
            recv.stop(0u32.into()).unwrap();
            tokio::time::sleep(CANCEL_AFTER).await;
            send.write_all(&PUBLISH_T_ACK_QOS1).await.unwrap();
            let closed = tokio::time::timeout(SETTLE_WITHIN / 2, connection.closed()).await;
            (closed.map(|reason| reason.to_string()), endpoint)
        });
        let (client, disconnects) =
            quic_client_counting_disconnects("quic-control-ack-write-closes", &url).await;

        wait_for_total(&[&disconnects], 1).await;
        let (closed, endpoint) = server.await.unwrap();
        let disconnected = disconnects.load(std::sync::atomic::Ordering::SeqCst);
        let connected = client.is_connected().await;
        drop(endpoint);

        assert_eq!(
            disconnected, 1,
            "a failed PUBACK write on the control stream must end the connection with one Disconnected event"
        );
        assert!(
            !connected,
            "the client must not report connected after its PUBACK write failed"
        );
        assert!(
            closed.is_ok(),
            "a failed PUBACK write on the control stream must close the QUIC connection: {closed:?}"
        );
    }

    const PUBLISH_T_AFTER_QOS0: [u8; 12] = [
        0x30, 0x0a, 0x00, 0x07, b't', b'/', b'a', b'f', b't', b'e', b'r', 0x00,
    ];

    async fn serve_a_data_flow_whose_ack_write_fails(
        endpoint: quinn::Endpoint,
        subscribed: tokio::sync::oneshot::Receiver<()>,
        wait_for_close: Duration,
    ) -> bool {
        let connection = endpoint.accept().await.unwrap().await.unwrap();
        let (mut send, mut recv) = connection.accept_bi().await.unwrap();
        accept_connect(&mut recv, &mut send).await;
        let control = Arc::new(tokio::sync::Mutex::new(send));
        tokio::spawn(acknowledge_subscriptions(recv, Arc::clone(&control)));
        subscribed.await.unwrap();
        let (mut flow_send, mut flow_recv) = connection.open_bi().await.unwrap();
        flow_recv.stop(0u32.into()).unwrap();
        tokio::time::sleep(CANCEL_AFTER).await;
        flow_send.write_all(&PUBLISH_T_ACK_QOS1).await.unwrap();
        tokio::time::sleep(CANCEL_AFTER).await;
        let _ = flow_send.write_all(&PUBLISH_T_AFTER_QOS0).await;
        tokio::time::timeout(wait_for_close, connection.closed())
            .await
            .is_ok()
    }

    async fn subscribe_recording_topics(client: &MqttClient) -> Arc<std::sync::Mutex<Vec<String>>> {
        let topics = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recording = Arc::clone(&topics);
        client
            .subscribe("t/#", move |message| {
                recording.lock().unwrap().push(message.topic.clone());
            })
            .await
            .unwrap();
        topics
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_server_data_flow_stops_reading_after_its_ack_write_fails() {
        let endpoint = endpoint();
        let url = format!("quic://{}", endpoint.local_addr().unwrap());
        let (subscribed_tx, subscribed_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve_a_data_flow_whose_ack_write_fails(
            endpoint,
            subscribed_rx,
            CANCEL_AFTER,
        ));
        let client =
            connected_quic_client("quic-flow-stops-reading", &url, StreamStrategy::ControlOnly)
                .await;
        let delivered = subscribe_recording_topics(&client).await;
        subscribed_tx.send(()).unwrap();

        server.await.unwrap();
        tokio::time::sleep(CANCEL_AFTER).await;
        let topics = delivered.lock().unwrap().clone();

        assert!(
            !topics.iter().any(|topic| topic == "t/after"),
            "a server data flow must stop reading once its PUBACK write fails, but it delivered {topics:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_failed_ack_write_on_a_server_data_flow_ends_the_connection() {
        let endpoint = endpoint();
        let url = format!("quic://{}", endpoint.local_addr().unwrap());
        let (subscribed_tx, subscribed_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve_a_data_flow_whose_ack_write_fails(
            endpoint,
            subscribed_rx,
            SETTLE_WITHIN / 2,
        ));
        let (client, disconnects) =
            quic_client_counting_disconnects("quic-flow-ack-write-ends", &url).await;
        let delivered = subscribe_recording_topics(&client).await;
        subscribed_tx.send(()).unwrap();

        wait_for_total(&[&disconnects], 1).await;
        let closed = server.await.unwrap();
        let disconnected = disconnects.load(std::sync::atomic::Ordering::SeqCst);
        let connected = client.is_connected().await;
        let topics = delivered.lock().unwrap().clone();

        assert_eq!(
            (disconnected, connected, closed),
            (1, false, true),
            "a PUBACK that can never be written on its data flow must end the connection with one Disconnected event, is_connected false and the QUIC connection closed; delivered {topics:?}"
        );
    }

    const RESERVED_TYPE_0_PACKET: [u8; 2] = [0x00, 0x00];

    async fn quic_closed_after_a_malformed_packet_ends_the_reader(
        name: &str,
        server_reads: bool,
    ) -> (usize, bool, bool) {
        let endpoint = endpoint();
        let url = format!("quic://{}", endpoint.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let (mut send, mut recv) = connection.accept_bi().await.unwrap();
            accept_connect(&mut recv, &mut send).await;
            let unread = if server_reads {
                tokio::spawn(async move {
                    let mut chunk = vec![0u8; 1 << 16];
                    while let Ok(Some(_)) = recv.read(&mut chunk).await {}
                });
                None
            } else {
                Some(recv)
            };
            tokio::time::sleep(SERVER_IDLE).await;
            send.write_all(&RESERVED_TYPE_0_PACKET).await.unwrap();
            let closed = tokio::time::timeout(SETTLE_WITHIN / 2, connection.closed())
                .await
                .is_ok();
            (closed, endpoint, send, unread)
        });
        let (client, disconnects) = quic_client_counting_disconnects(name, &url).await;
        let publisher = client.clone();
        let publishing = tokio::spawn(async move {
            publisher
                .publish_with_options(
                    "t/big",
                    large_payload(),
                    PublishOptions {
                        qos: QoS::AtMostOnce,
                        ..Default::default()
                    },
                )
                .await
        });

        wait_for_total(&[&disconnects], 1).await;
        let (closed, endpoint, send, unread) = server.await.unwrap();
        let disconnected = disconnects.load(std::sync::atomic::Ordering::SeqCst);
        let connected = client.is_connected().await;
        publishing.abort();
        drop((send, unread, endpoint));
        (disconnected, connected, closed)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_connection_is_closed_when_the_reader_ends_a_stalled_control_stream() {
        let (disconnected, connected, closed) =
            quic_closed_after_a_malformed_packet_ends_the_reader(
                "quic-reader-ends-stalled-control",
                false,
            )
            .await;

        assert_eq!(
            (disconnected, connected, closed),
            (1, false, true),
            "when the reader ends the connection on a malformed packet while the control stream write is stalled, the client must report one Disconnected event, is_connected false, and close the QUIC connection"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_connection_is_closed_when_the_reader_ends_a_reading_control_stream() {
        let (disconnected, connected, closed) =
            quic_closed_after_a_malformed_packet_ends_the_reader(
                "quic-reader-ends-reading-control",
                true,
            )
            .await;

        assert_eq!(
            (disconnected, connected, closed),
            (1, false, true),
            "when the reader ends the connection on a malformed packet while the server keeps reading the control stream, the client must report one Disconnected event, is_connected false, and close the QUIC connection"
        );
    }

    #[cfg(feature = "broker")]
    mod broker {
        use super::*;
        use mqtt5::broker::config::{
            BrokerConfig, QuicConfig as BrokerQuicConfig, StorageBackend, StorageConfig,
        };
        use mqtt5::broker::MqttBroker;
        use std::net::SocketAddr;
        use std::path::PathBuf;

        fn connect_frame(client_id: &str) -> Vec<u8> {
            let mut body = vec![
                0x00, 0x04, b'M', b'Q', b'T', b'T', 0x05, 0x02, 0x00, 0x3C, 0x00,
            ];
            body.extend_from_slice(&u16::try_from(client_id.len()).unwrap().to_be_bytes());
            body.extend_from_slice(client_id.as_bytes());
            let mut frame = vec![0x10, u8::try_from(body.len()).unwrap()];
            frame.extend_from_slice(&body);
            frame
        }

        fn qos0_publish_frame(topic: &str, payload: &[u8]) -> Vec<u8> {
            let mut body = u16::try_from(topic.len()).unwrap().to_be_bytes().to_vec();
            body.extend_from_slice(topic.as_bytes());
            body.push(0x00);
            body.extend_from_slice(payload);
            let mut frame = vec![0x30, u8::try_from(body.len()).unwrap()];
            frame.extend_from_slice(&body);
            frame
        }

        async fn publish_on_own_stream(connection: &quinn::Connection, bytes: &[u8]) {
            let mut stream = connection.open_uni().await.unwrap();
            stream.write_all(bytes).await.unwrap();
            stream.finish().unwrap();
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn broker_never_delivers_a_publish_cut_short_by_the_end_of_its_stream() {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let config = BrokerConfig::default()
                .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
                .with_bind_address(([127, 0, 0, 1], 0))
                .with_quic(
                    BrokerQuicConfig::new(
                        PathBuf::from("../../test_certs/server.pem"),
                        PathBuf::from("../../test_certs/server.key"),
                    )
                    .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap()),
                );
            let mut broker = MqttBroker::with_config(config).await.unwrap();
            let tcp_addr = broker.local_addr().unwrap();
            let quic_addr = broker.quic_local_addr().unwrap();
            let shutdown = broker.shutdown_handle();
            let run = tokio::spawn(async move { broker.run().await });

            let subscriber = MqttClient::new("cut-subscriber");
            subscriber
                .connect(&format!("mqtt://{tcp_addr}"))
                .await
                .unwrap();
            let (delivered_tx, mut delivered) = tokio::sync::mpsc::unbounded_channel();
            subscriber
                .subscribe("t/cut", move |message| {
                    let _ = delivered_tx.send(message.payload.clone());
                })
                .await
                .unwrap();

            let mut transport = QuicTransport::new(
                QuicConfig::new(quic_addr, "localhost").with_verify_server_cert(false),
            );
            transport.connect().await.unwrap();
            let mut split = transport.into_split().unwrap();
            split
                .send
                .write_all(&connect_frame("cut-publisher"))
                .await
                .unwrap();
            let mut connack = [0u8; 2];
            split.recv.read_exact(&mut connack).await.unwrap();
            assert_eq!(connack[0], 0x20);
            let mut connack_body = vec![0u8; usize::from(connack[1])];
            split.recv.read_exact(&mut connack_body).await.unwrap();

            let cut = qos0_publish_frame("t/cut", b"cut");
            publish_on_own_stream(&split.connection, &cut[..cut.len() - 1]).await;
            publish_on_own_stream(&split.connection, &cut[..1]).await;
            publish_on_own_stream(&split.connection, &qos0_publish_frame("t/cut", b"whole")).await;

            let first = tokio::time::timeout(SETTLE_WITHIN, delivered.recv())
                .await
                .unwrap()
                .unwrap();
            let later = tokio::time::timeout(SERVER_IDLE, delivered.recv()).await;

            assert_eq!(first, b"whole".to_vec());
            assert!(later.is_err(), "{later:?}");

            subscriber.disconnect().await.unwrap();
            shutdown.shutdown();
            tokio::time::timeout(SETTLE_WITHIN, run)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }

        struct Lcg(u64);

        impl Lcg {
            fn next(&mut self) -> u64 {
                self.0 = self
                    .0
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                self.0 >> 33
            }

            fn below(&mut self, bound: u64) -> u64 {
                self.next() % bound
            }
        }

        struct Load {
            rounds: usize,
            ops: usize,
            kinds: &'static [u64],
            topics: u64,
            quarter_mib_max: u64,
        }

        const PUBLISH_AND_SUBSCRIBE: Load = Load {
            rounds: 150,
            ops: 4,
            kinds: &[0, 2],
            topics: 20,
            quarter_mib_max: 6,
        };

        fn qos_of(choice: u64) -> QoS {
            match choice {
                0 => QoS::AtMostOnce,
                1 => QoS::AtLeastOnce,
                _ => QoS::ExactlyOnce,
            }
        }

        async fn run_cancelled_load(client: &MqttClient, seed: u64, load: &Load) {
            let mut rng = Lcg(seed);
            for _ in 0..load.rounds {
                let mut tasks = Vec::new();
                for _ in 0..load.ops {
                    let client = client.clone();
                    let picked = rng.below(load.kinds.len() as u64);
                    let kind = load.kinds[usize::try_from(picked).unwrap()];
                    let cancel_after = Duration::from_micros(rng.below(30_000));
                    let size = usize::try_from(1 + rng.below(load.quarter_mib_max)).unwrap() << 18;
                    let qos = qos_of(rng.below(3));
                    let topic = format!("load/{}", rng.below(load.topics));
                    tasks.push(tokio::spawn(async move {
                        let cancellable = async {
                            if kind == 2 {
                                let options = mqtt5::SubscribeOptions {
                                    qos,
                                    ..Default::default()
                                };
                                let subscribed =
                                    client.subscribe_with_options(&topic, options, |_| {}).await;
                                subscribed.is_ok()
                            } else {
                                let options = PublishOptions {
                                    qos,
                                    ..Default::default()
                                };
                                let published = client
                                    .publish_with_options(&topic, vec![5u8; size], options)
                                    .await;
                                published.is_ok()
                            }
                        };
                        tokio::time::timeout(cancel_after, cancellable).await
                    }));
                }
                for task in tasks {
                    let _ = task.await.unwrap();
                }
            }
        }

        async fn final_round_trip(client: &MqttClient, within: Duration) -> Result<(), String> {
            let (received, on_message) = counting_callback();
            let subscribed =
                tokio::time::timeout(within, client.subscribe("final/topic", on_message)).await;
            if !matches!(subscribed, Ok(Ok(_))) {
                return Err(format!("final subscribe: {subscribed:?}"));
            }
            let published = tokio::time::timeout(
                within,
                client.publish_qos1("final/topic", b"after".to_vec()),
            )
            .await;
            if !matches!(published, Ok(Ok(_))) {
                return Err(format!("final publish: {published:?}"));
            }
            for _ in 0..200 {
                if received.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err("final message not delivered".to_string())
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn quic_per_publish_flows_stay_usable_after_cancelled_publishes_to_subscribed_topics()
        {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let config = BrokerConfig::default()
                .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
                .with_bind_address(([127, 0, 0, 1], 0))
                .with_quic(
                    BrokerQuicConfig::new(
                        PathBuf::from("../../test_certs/server.pem"),
                        PathBuf::from("../../test_certs/server.key"),
                    )
                    .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap()),
                );
            let mut broker = MqttBroker::with_config(config).await.unwrap();
            let quic_addr = broker.quic_local_addr().unwrap();
            let shutdown = broker.shutdown_handle();
            let run = tokio::spawn(async move { broker.run().await });

            let options =
                ConnectOptions::new("quic-flows-cancelled").with_automatic_reconnect(false);
            let client = MqttClient::with_options(options.clone());
            client.set_insecure_tls(true).await;
            client
                .set_quic_stream_strategy(StreamStrategy::DataPerPublish)
                .await;
            client.set_quic_flow_headers(true).await;
            let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counted = Arc::clone(&drops);
            client
                .on_connection_event(move |event| {
                    if matches!(event, ConnectionEvent::Disconnected { .. }) {
                        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                })
                .await
                .unwrap();
            client
                .connect_with_options(&format!("quic://{quic_addr}"), options)
                .await
                .unwrap();

            run_cancelled_load(&client, 23, &PUBLISH_AND_SUBSCRIBE).await;
            let usable = final_round_trip(&client, Duration::from_secs(12)).await;
            let dropped = drops.load(std::sync::atomic::Ordering::SeqCst);

            shutdown.shutdown();
            let _ = tokio::time::timeout(SETTLE_WITHIN, run).await;
            assert_eq!(
                dropped,
                0,
                "the QUIC connection must not drop while publishes and subscribes on its flows are cancelled"
            );
            assert_eq!(
                usable,
                Ok(()),
                "after publishes to its own subscribed topics are cancelled, the client must still subscribe, publish and receive"
            );
        }
    }
}
