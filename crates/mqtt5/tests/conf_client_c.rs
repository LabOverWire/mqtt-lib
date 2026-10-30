use mqtt5::time::Duration;
use mqtt5::{ConnectOptions, MqttClient, QoS, SubscribeOptions};
use std::future::Future;
use std::pin::Pin;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const TEST_TIMEOUT: Duration = Duration::from_secs(40);

#[derive(Debug, Clone, PartialEq, Eq)]
struct Raw {
    header: u8,
    body: Vec<u8>,
}

impl Raw {
    fn kind(&self) -> u8 {
        self.header >> 4
    }

    fn ack_id(&self) -> u16 {
        u16::from_be_bytes([self.body[0], self.body[1]])
    }

    fn reason(&self) -> u8 {
        match self.kind() {
            14 | 15 => self.body.first().copied().unwrap_or(0),
            _ => self.body.get(2).copied().unwrap_or(0),
        }
    }

    fn publish_qos(&self) -> u8 {
        (self.header >> 1) & 0x03
    }

    fn publish_dup(&self) -> bool {
        self.header & 0x08 != 0
    }

    fn publish_id(&self) -> Option<u16> {
        let topic_len = usize::from(u16::from_be_bytes([self.body[0], self.body[1]]));
        (self.publish_qos() > 0)
            .then(|| u16::from_be_bytes([self.body[2 + topic_len], self.body[3 + topic_len]]))
    }

    fn describe(&self) -> String {
        match self.kind() {
            3 => format!(
                "PUBLISH(qos={},dup={},id={:?})",
                self.publish_qos(),
                self.publish_dup(),
                self.publish_id()
            ),
            4 => format!("PUBACK({})", self.ack_id()),
            5 => format!("PUBREC({})", self.ack_id()),
            6 => format!("PUBREL({})", self.ack_id()),
            7 => format!("PUBCOMP({})", self.ack_id()),
            8 => "SUBSCRIBE".to_string(),
            12 => "PINGREQ".to_string(),
            14 => format!("DISCONNECT(0x{:02X})", self.reason()),
            15 => format!("AUTH(0x{:02X})", self.reason()),
            other => format!("type{other}"),
        }
    }
}

enum Next {
    Packet(Raw),
    Eof,
    Timeout,
}

fn encode(header: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![header];
    let mut len = body.len();
    loop {
        let mut byte = u8::try_from(len % 128).expect("fits");
        len /= 128;
        if len > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if len == 0 {
            break;
        }
    }
    out.extend_from_slice(body);
    out
}

fn connack() -> Vec<u8> {
    encode(0x20, &[0x00, 0x00, 0x00])
}

fn server_publish(qos: u8, dup: bool, packet_id: u16, topic: &str, payload: &[u8]) -> Vec<u8> {
    let topic_len = u16::try_from(topic.len()).expect("topic fits");
    let mut body = topic_len.to_be_bytes().to_vec();
    body.extend_from_slice(topic.as_bytes());
    if qos > 0 {
        body.extend_from_slice(&packet_id.to_be_bytes());
    }
    body.push(0x00);
    body.extend_from_slice(payload);
    encode(0x30 | (u8::from(dup) << 3) | (qos << 1), &body)
}

struct Wire {
    stream: TcpStream,
}

impl Wire {
    async fn raw_read(&mut self) -> Option<Raw> {
        let mut first = [0u8; 1];
        if self.stream.read_exact(&mut first).await.is_err() {
            return None;
        }
        let mut len = 0usize;
        let mut shift = 0;
        loop {
            let mut b = [0u8; 1];
            if self.stream.read_exact(&mut b).await.is_err() {
                return None;
            }
            len |= usize::from(b[0] & 0x7F) << shift;
            shift += 7;
            if b[0] & 0x80 == 0 {
                break;
            }
        }
        let mut body = vec![0u8; len];
        if self.stream.read_exact(&mut body).await.is_err() {
            return None;
        }
        Some(Raw {
            header: first[0],
            body,
        })
    }

    async fn next(&mut self, wait: Duration) -> Next {
        loop {
            match tokio::time::timeout(wait, self.raw_read()).await {
                Err(_) => return Next::Timeout,
                Ok(None) => return Next::Eof,
                Ok(Some(raw)) if raw.kind() == 12 => {
                    self.send(&[0xD0, 0x00]).await;
                }
                Ok(Some(raw)) if raw.kind() == 8 => {
                    let mut suback = raw.body[0..2].to_vec();
                    suback.extend_from_slice(&[0x00, 0x02]);
                    self.send(&encode(0x90, &suback)).await;
                }
                Ok(Some(raw)) => return Next::Packet(raw),
            }
        }
    }

    async fn expect(&mut self, what: &str) -> Raw {
        match self.next(Duration::from_secs(5)).await {
            Next::Packet(raw) => raw,
            Next::Eof => panic!("connection closed while waiting for {what}"),
            Next::Timeout => panic!("timed out waiting for {what}"),
        }
    }

    async fn collect(&mut self, wait: Duration) -> (Vec<Raw>, bool) {
        let mut out = Vec::new();
        loop {
            match self.next(wait).await {
                Next::Packet(raw) => out.push(raw),
                Next::Eof => return (out, true),
                Next::Timeout => return (out, false),
            }
        }
    }

    async fn send(&mut self, bytes: &[u8]) {
        self.stream
            .write_all(bytes)
            .await
            .expect("fake broker write");
        self.stream.flush().await.expect("fake broker flush");
    }
}

async fn accept(listener: &TcpListener) -> Wire {
    let (stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .expect("client did not connect in time")
        .expect("accept");
    let mut wire = Wire { stream };
    let connect = wire.raw_read().await.expect("CONNECT");
    assert_eq!(connect.kind(), 1, "first packet must be CONNECT");
    wire.send(&connack()).await;
    wire
}

fn url(listener: &TcpListener) -> String {
    format!("mqtt://{}", listener.local_addr().expect("addr"))
}

fn base_options(name: &str) -> ConnectOptions {
    ConnectOptions::new(name)
        .with_keep_alive(Duration::from_secs(60))
        .with_automatic_reconnect(false)
}

fn persistent_options(name: &str) -> ConnectOptions {
    ConnectOptions::new(name)
        .with_keep_alive(Duration::from_secs(60))
        .with_clean_start(false)
        .with_session_expiry_interval(3600)
        .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(500))
}

fn deferred_options(name: &str) -> ConnectOptions {
    base_options(name)
        .with_clean_start(false)
        .with_session_expiry_interval(3600)
        .with_receive_maximum(16)
        .with_deferred_ack(true)
}

fn connected(
    listener: &TcpListener,
    options: ConnectOptions,
) -> Pin<Box<dyn Future<Output = (MqttClient, Wire)> + '_>> {
    Box::pin(async move {
        let client = MqttClient::with_options(options.clone());
        let address = url(listener);
        let (result, wire) = tokio::join!(
            client.connect_with_options(&address, options),
            accept(listener)
        );
        result.expect("client connect");
        (client, wire)
    })
}

fn qos_options(qos: QoS) -> SubscribeOptions {
    SubscribeOptions {
        qos,
        ..Default::default()
    }
}

fn descriptions(packets: &[Raw]) -> Vec<String> {
    packets.iter().map(Raw::describe).collect()
}

async fn with_timeout(fut: Pin<Box<dyn Future<Output = ()>>>) {
    tokio::time::timeout(TEST_TIMEOUT, fut)
        .await
        .expect("test exceeded its overall timeout");
}

#[tokio::test]
async fn crosscheck4_protocol_error_disconnect_flushed_before_close_with_auto_reconnect() {
    with_timeout(Box::pin(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (_client, mut wire) = connected(&listener, persistent_options("c-proto-err")).await;
        wire.send(&[0x36, 0x00]).await;
        let (packets, eof) = wire.collect(Duration::from_secs(6)).await;
        let seen = descriptions(&packets);
        assert!(
            eof,
            "protocol error: old socket never closed within 6s; saw {seen:?}"
        );
        assert!(
            packets.iter().any(|p| p.kind() == 14 && p.reason() >= 0x80),
            "cross-check 4: socket closed after malformed PUBLISH (QoS 3) without any DISCONNECT on the wire; saw {seen:?}"
        );
    }))
    .await;
}

async fn collect_acks(wire: &mut Wire, kind: u8, count: usize) -> Vec<u16> {
    let mut ids = Vec::new();
    while ids.len() < count {
        let raw = wire.expect("ack").await;
        if raw.kind() == kind {
            ids.push(raw.ack_id());
        }
    }
    ids
}

#[tokio::test]
async fn mqtt_4_6_0_2_and_4_6_0_3_automatic_acks_follow_publish_order_with_slow_and_panicking_callbacks(
) {
    with_timeout(Box::pin(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (client, mut wire) = connected(&listener, base_options("c-auto-order")).await;
        let (sub, ()) = tokio::join!(
            client.subscribe_with_options("t/#", qos_options(QoS::ExactlyOnce), |m| {
                assert!(m.payload != b"panic", "callback bug");
                std::thread::sleep(std::time::Duration::from_millis(30));
            }),
            async {
                let _ = wire.next(Duration::from_millis(300)).await;
            }
        );
        sub.expect("subscribe");
        let mut burst = Vec::new();
        for id in 1..=6u16 {
            let payload: &[u8] = if id == 2 { b"panic" } else { b"x" };
            burst.extend(server_publish(1, false, id, "t/a", payload));
        }
        wire.send(&burst).await;
        assert_eq!(
            collect_acks(&mut wire, 4, 6).await,
            vec![1, 2, 3, 4, 5, 6],
            "MQTT-4.6.0-2 violated on automatic path"
        );
        let mut burst = Vec::new();
        for id in 20..=25u16 {
            let payload: &[u8] = if id == 21 { b"panic" } else { b"x" };
            burst.extend(server_publish(2, false, id, "t/b", payload));
        }
        wire.send(&burst).await;
        assert_eq!(
            collect_acks(&mut wire, 5, 6).await,
            vec![20, 21, 22, 23, 24, 25],
            "MQTT-4.6.0-3 violated on automatic path"
        );
    }))
    .await;
}

#[tokio::test]
async fn mqtt_4_6_0_2_deferred_acked_immediately_mixed_with_automatic() {
    with_timeout(Box::pin(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (client, mut wire) = connected(&listener, deferred_options("c-mixed")).await;
        let (sub, ()) = tokio::join!(
            client.subscribe_with_ack("a", qos_options(QoS::AtLeastOnce), |_p, t| t.ack()),
            async {
                let _ = wire.next(Duration::from_millis(300)).await;
            }
        );
        sub.expect("subscribe_with_ack");
        let (sub, ()) = tokio::join!(
            client.subscribe_with_options("b", qos_options(QoS::AtLeastOnce), |_m| {}),
            async {
                let _ = wire.next(Duration::from_millis(300)).await;
            }
        );
        sub.expect("subscribe");
        let mut burst = server_publish(1, false, 1, "a", b"deferred");
        burst.extend(server_publish(1, false, 2, "b", b"automatic"));
        wire.send(&burst).await;
        assert_eq!(
            collect_acks(&mut wire, 4, 2).await,
            vec![1, 2],
            "MQTT-4.6.0-2 violated: deferred message acked immediately inside its callback still reaches the wire after a later automatic PUBACK"
        );
    }))
    .await;
}

async fn published_id(wire: &mut Wire, what: &str) -> u16 {
    let raw = wire.expect(what).await;
    assert_eq!(raw.kind(), 3, "expected {what}, got {}", raw.describe());
    raw.publish_id().expect("QoS > 0 PUBLISH has a packet id")
}

async fn replay_after_resume_with_receive_maximum_one(name: &str, qos1_first: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (client, mut wire) = connected(&listener, persistent_options(name)).await;
    let mut publishing = Vec::new();
    let mut ids = [0u16; 2];
    for (slot, qos) in if qos1_first {
        [(1, 1), (0, 2)]
    } else {
        [(0, 2), (1, 1)]
    } {
        let c = client.clone();
        let qos = if qos == 2 {
            QoS::ExactlyOnce
        } else {
            QoS::AtLeastOnce
        };
        publishing.push(tokio::spawn(async move {
            c.publish_qos("t/x", b"x".to_vec(), qos).await
        }));
        ids[slot] = published_id(&mut wire, "PUBLISH").await;
    }
    let [released, unacked] = ids;
    wire.send(&encode(0x50, &released.to_be_bytes())).await;
    let pubrel = wire.expect("PUBREL").await;
    assert_eq!((pubrel.kind(), pubrel.ack_id()), (6, released));
    drop(wire);
    let (stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .expect("client did not reconnect in time")
        .expect("accept");
    let mut wire = Wire { stream };
    let connect = wire.raw_read().await.expect("CONNECT");
    assert_eq!(connect.kind(), 1, "first packet must be CONNECT");
    wire.send(&encode(0x20, &[0x01, 0x00, 0x03, 0x21, 0x00, 0x01]))
        .await;
    let mut seen = Vec::new();
    while let Next::Packet(raw) = wire.next(Duration::from_secs(3)).await {
        match raw.kind() {
            6 => wire.send(&encode(0x70, &raw.ack_id().to_be_bytes())).await,
            3 => {
                let id = raw.publish_id().expect("QoS 1 PUBLISH has a packet id");
                wire.send(&encode(0x40, &id.to_be_bytes())).await;
            }
            _ => {}
        }
        seen.push(raw);
    }
    publishing.iter().for_each(tokio::task::JoinHandle::abort);
    let resent: Vec<(u8, u16)> = seen
        .iter()
        .filter_map(|p| match p.kind() {
            6 => Some((6, p.ack_id())),
            3 => p.publish_id().map(|id| (3, id)),
            _ => None,
        })
        .collect();
    assert_eq!(
        resent,
        [(6, released), (3, unacked)],
        "after resuming with Receive Maximum 1 and PUBREL {released} outstanding, the replay must send the PUBREL before waiting for quota for PUBLISH {unacked}; saw {:?}",
        descriptions(&seen)
    );
}

#[tokio::test]
async fn replay_sends_pubrel_before_publish_waiting_for_quota_sent_after_it() {
    with_timeout(Box::pin(replay_after_resume_with_receive_maximum_one(
        "c-replay-rel-first",
        false,
    )))
    .await;
}

#[tokio::test]
async fn replay_sends_pubrel_before_publish_waiting_for_quota_sent_before_it() {
    with_timeout(Box::pin(replay_after_resume_with_receive_maximum_one(
        "c-replay-pub-first",
        true,
    )))
    .await;
}
