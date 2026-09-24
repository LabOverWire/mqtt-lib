use mqtt5::time::Duration;
use mqtt5::{
    AckToken, AuthHandler, AuthResponse, ConnectOptions, MqttClient, QoS, SubscribeOptions,
};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const TEST_TIMEOUT: Duration = Duration::from_secs(40);
const DISCONNECT_REASON_TABLE: [u8; 29] = [
    0x00, 0x04, 0x80, 0x81, 0x82, 0x83, 0x87, 0x89, 0x8B, 0x8D, 0x8E, 0x8F, 0x90, 0x93, 0x94, 0x95,
    0x96, 0x97, 0x98, 0x99, 0x9A, 0x9B, 0x9C, 0x9D, 0x9E, 0x9F, 0xA0, 0xA1, 0xA2,
];
const AUTH_REASON_TABLE: [u8; 3] = [0x00, 0x18, 0x19];

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

fn connack(session_present: bool) -> Vec<u8> {
    encode(0x20, &[u8::from(session_present), 0x00, 0x00])
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

fn ack(header: u8, packet_id: u16) -> Vec<u8> {
    encode(header, &packet_id.to_be_bytes())
}

fn server_auth(reason: u8, method: &str) -> Vec<u8> {
    let method_len = u16::try_from(method.len()).expect("method fits");
    let mut props = vec![0x15];
    props.extend_from_slice(&method_len.to_be_bytes());
    props.extend_from_slice(method.as_bytes());
    let mut body = vec![reason, u8::try_from(props.len()).expect("props fit")];
    body.extend_from_slice(&props);
    encode(0xF0, &body)
}

struct Wire {
    stream: TcpStream,
    clean_start: bool,
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

async fn accept(listener: &TcpListener, session_present: bool) -> Wire {
    let (stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .expect("client did not connect in time")
        .expect("accept");
    let mut wire = Wire {
        stream,
        clean_start: false,
    };
    let connect = wire.raw_read().await.expect("CONNECT");
    assert_eq!(connect.kind(), 1, "first packet must be CONNECT");
    wire.clean_start = connect.body[7] & 0x02 != 0;
    wire.send(&connack(session_present)).await;
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
            accept(listener, false)
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

async fn wait_until(cond: impl Fn() -> bool) -> bool {
    for _ in 0..200 {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
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
async fn mqtt_3_14_2_1_and_3_14_4_2_user_disconnect_uses_table_code_and_closes() {
    with_timeout(Box::pin(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (client, mut wire) = connected(&listener, base_options("c-disc")).await;
        client.disconnect().await.expect("disconnect");
        let (packets, eof) = wire.collect(Duration::from_secs(3)).await;
        let disconnect = packets
            .iter()
            .find(|p| p.kind() == 14)
            .expect("MQTT-3.14.2-1: client disconnect() must put a DISCONNECT on the wire");
        assert!(
            DISCONNECT_REASON_TABLE.contains(&disconnect.reason()),
            "MQTT-3.14.2-1: DISCONNECT reason 0x{:02X} outside table",
            disconnect.reason()
        );
        assert!(
            eof,
            "MQTT-3.14.4-2: network connection not closed after DISCONNECT"
        );
    }))
    .await;
}

#[tokio::test]
async fn mqtt_3_14_4_1_no_packet_after_disconnect_when_deferred_ack_resolves_concurrently() {
    with_timeout(Box::pin(async {
        for round in 0..10 {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let (client, mut wire) =
                connected(&listener, deferred_options(&format!("c-race-{round}"))).await;
            let tokens: Arc<Mutex<Vec<AckToken>>> = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::clone(&tokens);
            let (sub, ()) = tokio::join!(
                client.subscribe_with_ack("a", qos_options(QoS::AtLeastOnce), move |_p, t| {
                    sink.lock().unwrap().push(t);
                }),
                async {
                    let _ = wire.next(Duration::from_millis(300)).await;
                }
            );
            sub.expect("subscribe_with_ack");
            wire.send(&server_publish(1, false, 11, "a", b"x")).await;
            assert!(wait_until(|| tokens.lock().unwrap().len() == 1).await);
            let token = tokens.lock().unwrap().pop().unwrap();
            token.ack();
            client.disconnect().await.expect("disconnect");
            let (packets, _) = wire.collect(Duration::from_secs(2)).await;
            let seen = descriptions(&packets);
            if let Some(pos) = packets.iter().position(|p| p.kind() == 14) {
                assert!(
                    pos == packets.len() - 1,
                    "MQTT-3.14.4-1 violated (round {round}): packets after DISCONNECT: {seen:?}"
                );
            } else {
                panic!("no DISCONNECT seen (round {round}): {seen:?}");
            }
        }
    }))
    .await;
}

#[tokio::test]
async fn mqtt_3_14_1_1_disconnect_reserved_bits_client_sends_0x81_then_closes() {
    with_timeout(Box::pin(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (_client, mut wire) = connected(&listener, base_options("c-rsv-disc")).await;
        wire.send(&[0xE1, 0x00]).await;
        let (packets, eof) = wire.collect(Duration::from_secs(5)).await;
        let seen = descriptions(&packets);
        assert!(
            packets.iter().any(|p| p.kind() == 14 && p.reason() == 0x81),
            "MQTT-3.14.1-1 violated: no DISCONNECT 0x81 after DISCONNECT with reserved flags 0x1; saw {seen:?}, socket closed={eof}"
        );
        assert!(
            eof,
            "MQTT-3.14.4-2 violated: network connection not closed after DISCONNECT 0x81; saw {seen:?}"
        );
    }))
    .await;
}

#[tokio::test]
async fn mqtt_3_15_1_1_auth_reserved_bits_treated_malformed_and_connection_closed() {
    with_timeout(Box::pin(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (client, mut wire) = connected(&listener, base_options("c-rsv-auth")).await;
        wire.send(&[0xF1, 0x00]).await;
        let (packets, eof) = wire.collect(Duration::from_secs(5)).await;
        let still_connected = client.is_connected().await;
        assert!(
            eof,
            "MQTT-3.15.1-1 violated: AUTH with reserved flags 0x1 did not close the network connection within 5s (client is_connected={still_connected}, saw {:?})",
            descriptions(&packets)
        );
    }))
    .await;
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
            "MQTT-4.13 / cross-check 4: socket closed after malformed PUBLISH (QoS 3) without any DISCONNECT on the wire; saw {seen:?}"
        );
    }))
    .await;
}

struct ScriptedAuth {
    calls: AtomicU32,
}

impl AuthHandler for ScriptedAuth {
    fn handle_challenge<'a>(
        &'a self,
        _auth_method: &'a str,
        _challenge_data: Option<&'a [u8]>,
    ) -> Pin<Box<dyn Future<Output = mqtt5::Result<AuthResponse>> + Send + 'a>> {
        Box::pin(async move {
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => Ok(AuthResponse::Continue(b"resp".to_vec())),
                1 => Ok(AuthResponse::Abort("handler refuses".to_string())),
                _ => Err(mqtt5::MqttError::AuthenticationFailed),
            }
        })
    }

    fn initial_response<'a>(
        &'a self,
        _auth_method: &'a str,
    ) -> Pin<Box<dyn Future<Output = mqtt5::Result<Option<Vec<u8>>>> + Send + 'a>> {
        Box::pin(async move { Ok(Some(b"init".to_vec())) })
    }
}

#[tokio::test]
async fn mqtt_3_14_2_1_and_3_15_2_1_auth_paths_only_emit_table_reason_codes() {
    with_timeout(Box::pin(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let options = base_options("c-auth").with_authentication_method("SCRIPT");
        let client = MqttClient::with_options(options.clone());
        client
            .set_auth_handler(ScriptedAuth {
                calls: AtomicU32::new(0),
            })
            .await;
        let address = url(&listener);
        let (result, mut wire) = tokio::join!(
            client.connect_with_options(&address, options),
            accept(&listener, false)
        );
        result.expect("connect");

        client.reauthenticate().await.expect("reauthenticate");
        let reauth = wire.expect("re-auth AUTH").await;
        let mut all = vec![reauth];
        wire.send(&server_auth(0x18, "SCRIPT")).await;
        all.push(wire.expect("continue AUTH").await);
        wire.send(&server_auth(0x18, "SCRIPT")).await;
        let (rest, _) = wire.collect(Duration::from_secs(3)).await;
        all.extend(rest);
        let seen = descriptions(&all);
        for p in &all {
            match p.kind() {
                14 => assert!(
                    DISCONNECT_REASON_TABLE.contains(&p.reason()),
                    "MQTT-3.14.2-1 violated: {seen:?}"
                ),
                15 => assert!(
                    AUTH_REASON_TABLE.contains(&p.reason()),
                    "MQTT-3.15.2-1 violated: {seen:?}"
                ),
                _ => {}
            }
        }
        assert_eq!(all[0].kind(), 15, "reauthenticate must send AUTH: {seen:?}");
        assert_eq!(all[0].reason(), 0x19, "re-auth AUTH reason: {seen:?}");
        assert_eq!(all[1].reason(), 0x18, "continue AUTH reason: {seen:?}");
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

async fn deferred_reverse_order(qos: u8, ack_kind: u8) -> Vec<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (client, mut wire) = connected(&listener, deferred_options("c-def-rev")).await;
    let tokens: Arc<Mutex<Vec<AckToken>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&tokens);
    let sub_qos = if qos == 2 {
        QoS::ExactlyOnce
    } else {
        QoS::AtLeastOnce
    };
    let (sub, ()) = tokio::join!(
        client.subscribe_with_ack("a", qos_options(sub_qos), move |_p, t| {
            sink.lock().unwrap().push(t);
        }),
        async {
            let _ = wire.next(Duration::from_millis(300)).await;
        }
    );
    sub.expect("subscribe_with_ack");
    let mut burst = server_publish(qos, false, 1, "a", b"first");
    burst.extend(server_publish(qos, false, 2, "a", b"second"));
    wire.send(&burst).await;
    assert!(wait_until(|| tokens.lock().unwrap().len() == 2).await);
    let second = tokens.lock().unwrap().pop().unwrap();
    let first = tokens.lock().unwrap().pop().unwrap();
    second.ack();
    tokio::time::sleep(Duration::from_millis(100)).await;
    first.ack();
    collect_acks(&mut wire, ack_kind, 2).await
}

#[tokio::test]
async fn mqtt_4_6_0_2_deferred_tokens_acked_in_reverse_order() {
    with_timeout(Box::pin(async {
        let order = Box::pin(deferred_reverse_order(1, 4)).await;
        assert_eq!(
            order,
            vec![1, 2],
            "MQTT-4.6.0-2 violated: AckToken API lets PUBACKs reach the wire in resolution order, not PUBLISH arrival order"
        );
    }))
    .await;
}

#[tokio::test]
async fn mqtt_4_6_0_3_deferred_tokens_acked_in_reverse_order() {
    with_timeout(Box::pin(async {
        let order = Box::pin(deferred_reverse_order(2, 5)).await;
        assert_eq!(
            order,
            vec![1, 2],
            "MQTT-4.6.0-3 violated: AckToken API lets PUBRECs reach the wire in resolution order, not PUBLISH arrival order"
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

#[tokio::test]
async fn mqtt_4_3_1_1_4_3_2_2_4_3_3_2_first_send_dup_zero() {
    with_timeout(Box::pin(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (client, mut wire) = connected(&listener, base_options("c-dup0")).await;
        let pub_client = client.clone();
        let publisher = tokio::spawn(async move {
            pub_client
                .publish_qos("t", b"0".to_vec(), QoS::AtMostOnce)
                .await?;
            pub_client
                .publish_qos("t", b"1".to_vec(), QoS::AtLeastOnce)
                .await?;
            pub_client
                .publish_qos("t", b"2".to_vec(), QoS::ExactlyOnce)
                .await
        });
        let q0 = wire.expect("QoS0 PUBLISH").await;
        assert!(
            q0.kind() == 3 && q0.publish_qos() == 0 && !q0.publish_dup(),
            "MQTT-4.3.1-1: {}",
            q0.describe()
        );
        let q1 = wire.expect("QoS1 PUBLISH").await;
        assert!(
            q1.kind() == 3 && q1.publish_qos() == 1 && !q1.publish_dup(),
            "MQTT-4.3.2-2: {}",
            q1.describe()
        );
        wire.send(&ack(0x40, q1.publish_id().unwrap())).await;
        let q2 = wire.expect("QoS2 PUBLISH").await;
        assert!(
            q2.kind() == 3 && q2.publish_qos() == 2 && !q2.publish_dup(),
            "MQTT-4.3.3-2: {}",
            q2.describe()
        );
        let id = q2.publish_id().unwrap();
        wire.send(&ack(0x50, id)).await;
        let rel = wire.expect("PUBREL").await;
        assert_eq!(
            rel.kind(),
            6,
            "MQTT-4.3.3-4: expected PUBREL, got {}",
            rel.describe()
        );
        assert_eq!(rel.header, 0x62, "PUBREL fixed header flags");
        assert_eq!(rel.ack_id(), id, "MQTT-4.3.3-4: PUBREL id mismatch");
        wire.send(&ack(0x70, id)).await;
        publisher.await.unwrap().expect("publishes complete");
        let (extra, _) = wire.collect(Duration::from_millis(500)).await;
        assert!(
            extra.iter().all(|p| p.kind() != 3),
            "MQTT-4.3.3-6: PUBLISH re-sent after PUBREL: {:?}",
            descriptions(&extra)
        );
    }))
    .await;
}

#[tokio::test]
async fn mqtt_4_4_0_2_error_pubrec_gets_no_pubrel() {
    with_timeout(Box::pin(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (client, mut wire) = connected(&listener, base_options("c-pubrec-err")).await;
        let pub_client = client.clone();
        let publisher = tokio::spawn(async move {
            pub_client
                .publish_qos("t", b"2".to_vec(), QoS::ExactlyOnce)
                .await
        });
        let p = wire.expect("QoS2 PUBLISH").await;
        let id = p.publish_id().unwrap();
        let mut rec = id.to_be_bytes().to_vec();
        rec.extend_from_slice(&[0x80, 0x00]);
        wire.send(&encode(0x50, &rec)).await;
        let (after, _) = wire.collect(Duration::from_secs(1)).await;
        assert!(
            after.is_empty(),
            "MQTT-4.4.0-2 / 4.3.3-4: client answered an error PUBREC: {:?}",
            descriptions(&after)
        );
        assert!(
            publisher.await.unwrap().is_err(),
            "error PUBREC must fail the publish"
        );
    }))
    .await;
}

#[tokio::test]
async fn mqtt_4_3_2_4_4_3_3_8_4_3_3_10_4_3_3_11_receiver_acks_and_dedup() {
    with_timeout(Box::pin(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (client, mut wire) = connected(&listener, base_options("c-recv")).await;
        let hits = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&hits);
        let (sub, ()) = tokio::join!(
            client.subscribe_with_options("r", qos_options(QoS::ExactlyOnce), move |_m| {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
            async {
                let _ = wire.next(Duration::from_millis(300)).await;
            }
        );
        sub.expect("subscribe");
        wire.send(&server_publish(1, false, 300, "r", b"q1")).await;
        let puback = wire.expect("PUBACK").await;
        assert_eq!((puback.kind(), puback.ack_id()), (4, 300), "MQTT-4.3.2-4");
        wire.send(&server_publish(2, false, 301, "r", b"q2")).await;
        let pubrec = wire.expect("PUBREC").await;
        assert_eq!((pubrec.kind(), pubrec.ack_id()), (5, 301), "MQTT-4.3.3-8");
        wire.send(&server_publish(2, true, 301, "r", b"q2")).await;
        let pubrec = wire.expect("PUBREC for duplicate").await;
        assert_eq!(
            (pubrec.kind(), pubrec.ack_id()),
            (5, 301),
            "MQTT-4.3.3-10: duplicate must be PUBREC'd"
        );
        wire.send(&ack(0x62, 301)).await;
        let pubcomp = wire.expect("PUBCOMP").await;
        assert_eq!(
            (pubcomp.kind(), pubcomp.ack_id()),
            (7, 301),
            "MQTT-4.3.3-11"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "MQTT-4.3.3-10: QoS2 duplicate delivered to the application"
        );
    }))
    .await;
}

#[tokio::test]
async fn mqtt_4_5_0_2_publish_without_matching_callback_is_acked() {
    with_timeout(Box::pin(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (_client, mut wire) = connected(&listener, base_options("c-nocb")).await;
        wire.send(&server_publish(1, false, 5, "unsubscribed", b"x"))
            .await;
        let a = wire.expect("PUBACK").await;
        assert_eq!((a.kind(), a.ack_id()), (4, 5), "MQTT-4.5.0-2");
        wire.send(&server_publish(2, false, 6, "unsubscribed", b"x"))
            .await;
        let r = wire.expect("PUBREC").await;
        assert_eq!((r.kind(), r.ack_id()), (5, 6), "MQTT-4.5.0-2");
    }))
    .await;
}

#[tokio::test]
async fn mqtt_4_4_0_1_and_4_6_0_1_publish_resent_in_order_with_dup_after_session_resume() {
    with_timeout(Box::pin(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (client, mut wire) = connected(&listener, persistent_options("c-resend")).await;
        let mut sent = Vec::new();
        for (i, qos) in [QoS::AtLeastOnce, QoS::ExactlyOnce, QoS::AtLeastOnce]
            .into_iter()
            .enumerate()
        {
            let c = client.clone();
            tokio::spawn(async move { c.publish_qos("t", vec![u8::try_from(i).unwrap()], qos).await });
            let p = wire.expect("PUBLISH").await;
            sent.push(p.publish_id().unwrap());
        }
        drop(wire);
        let mut wire = accept(&listener, true).await;
        assert!(!wire.clean_start, "reconnect must use Clean Start 0");
        let (packets, _) = wire.collect(Duration::from_secs(3)).await;
        let resent: Vec<(u16, bool)> = packets
            .iter()
            .filter(|p| p.kind() == 3)
            .map(|p| (p.publish_id().unwrap_or(0), p.publish_dup()))
            .collect();
        let expected: Vec<(u16, bool)> = sent.iter().map(|id| (*id, true)).collect();
        assert_eq!(
            resent, expected,
            "MQTT-4.4.0-1 / 4.6.0-1: unacknowledged PUBLISH {sent:?} must be re-sent in original order with DUP=1 after Session Present=1; wire: {:?}",
            descriptions(&packets)
        );
    }))
    .await;
}

#[tokio::test]
async fn mqtt_4_6_0_4_pubrel_order_and_resend_after_session_resume() {
    with_timeout(Box::pin(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (client, mut wire) = connected(&listener, persistent_options("c-pubrel")).await;
        let mut ids = Vec::new();
        for i in 0..3u8 {
            let c = client.clone();
            tokio::spawn(async move { c.publish_qos("t", vec![i], QoS::ExactlyOnce).await });
            ids.push(wire.expect("PUBLISH").await.publish_id().unwrap());
        }
        let mut burst = Vec::new();
        for id in &ids {
            burst.extend(ack(0x50, *id));
        }
        wire.send(&burst).await;
        let first_rels = collect_acks(&mut wire, 6, 3).await;
        assert_eq!(first_rels, ids, "MQTT-4.6.0-4: PUBREL order differs from PUBREC order");
        wire.send(&ack(0x70, ids[1])).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(wire);
        let mut wire = accept(&listener, true).await;
        let (packets, _) = wire.collect(Duration::from_secs(3)).await;
        let rels: Vec<u16> = packets.iter().filter(|p| p.kind() == 6).map(Raw::ack_id).collect();
        assert!(
            packets.iter().all(|p| p.kind() != 3),
            "MQTT-4.3.3-6: PUBLISH re-sent after PUBREL: {:?}",
            descriptions(&packets)
        );
        assert_eq!(
            rels,
            vec![ids[0], ids[2]],
            "MQTT-4.4.0-1 / 4.6.0-4: outstanding PUBRELs must be re-sent in PUBREC order after Session Present=1; wire: {:?}",
            descriptions(&packets)
        );
    }))
    .await;
}

#[tokio::test]
async fn mqtt_3_2_2_5_session_present_zero_discards_session_state() {
    with_timeout(Box::pin(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (client, mut wire) = connected(&listener, persistent_options("c-sp0")).await;
        let hits = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&hits);
        let (sub, ()) = tokio::join!(
            client.subscribe_with_options("in", qos_options(QoS::ExactlyOnce), move |_m| {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
            async {
                let _ = wire.next(Duration::from_millis(300)).await;
            }
        );
        sub.expect("subscribe");
        let c = client.clone();
        tokio::spawn(async move { c.publish_qos("t", b"q2".to_vec(), QoS::ExactlyOnce).await });
        let out_id = wire.expect("PUBLISH").await.publish_id().unwrap();
        wire.send(&ack(0x50, out_id)).await;
        assert_eq!(wire.expect("PUBREL").await.kind(), 6);
        wire.send(&server_publish(2, false, 9, "in", b"old")).await;
        assert_eq!(wire.expect("PUBREC").await.kind(), 5);
        assert!(wait_until(|| hits.load(Ordering::SeqCst) == 1).await);
        drop(wire);

        let mut wire = accept(&listener, false).await;
        let (packets, _) = wire.collect(Duration::from_secs(2)).await;
        assert!(
            packets.iter().all(|p| p.kind() != 3 && p.kind() != 6),
            "MQTT-3.2.2-5: old session packets re-sent after Session Present=0: {:?}",
            descriptions(&packets)
        );
        wire.send(&server_publish(2, false, 9, "in", b"new")).await;
        assert_eq!(wire.expect("PUBREC").await.kind(), 5);
        assert!(
            wait_until(|| hits.load(Ordering::SeqCst) == 2).await,
            "MQTT-3.2.2-5: inbound QoS2 state not discarded; new PUBLISH id 9 suppressed as duplicate"
        );
    }))
    .await;
}

#[tokio::test]
async fn mqtt_4_3_2_2_queued_while_offline_first_send_has_dup_zero() {
    with_timeout(Box::pin(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (client, wire) = connected(&listener, persistent_options("c-queue")).await;
        client.set_queue_on_disconnect(true).await;
        drop(wire);
        let probe = client.clone();
        let mut offline = false;
        for _ in 0..100 {
            if !probe.is_connected().await {
                offline = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(offline, "client did not notice the dropped connection");
        client
            .publish_qos("t", b"queued".to_vec(), QoS::AtLeastOnce)
            .await
            .expect("queued publish accepted");
        let mut wire = accept(&listener, true).await;
        let p = wire.expect("queued PUBLISH").await;
        assert_eq!(p.kind(), 3, "expected PUBLISH, got {}", p.describe());
        assert!(
            !p.publish_dup(),
            "MQTT-4.3.2-2 violated: first transmission of a queued QoS1 message carries DUP=1: {}",
            p.describe()
        );
    }))
    .await;
}

#[tokio::test]
async fn mqtt_4_3_2_1_packet_id_not_reused_while_pubrel_outstanding() {
    with_timeout(Box::pin(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (client, mut wire) = connected(&listener, base_options("c-pid-wrap")).await;
        let c = client.clone();
        tokio::spawn(async move { c.publish_qos("t", b"held".to_vec(), QoS::ExactlyOnce).await });
        let held = wire.expect("QoS2 PUBLISH").await.publish_id().unwrap();
        wire.send(&ack(0x50, held)).await;
        assert_eq!(wire.expect("PUBREL").await.kind(), 6);
        let c = client.clone();
        let publisher = tokio::spawn(async move {
            for _ in 0..65_535u32 {
                if c.publish_qos("t", b"x".to_vec(), QoS::AtLeastOnce).await.is_err() {
                    break;
                }
            }
        });
        let mut reused = None;
        for _ in 0..65_535u32 {
            let p = wire.expect("QoS1 PUBLISH").await;
            let id = p.publish_id().unwrap();
            if id == held {
                reused = Some(id);
                break;
            }
            wire.send(&ack(0x40, id)).await;
        }
        publisher.abort();
        assert!(
            reused.is_none(),
            "MQTT-4.3.2-1 violated: packet id {held} reassigned to a new QoS1 message while its QoS2 PUBREL is still unacknowledged"
        );
    }))
    .await;
}
