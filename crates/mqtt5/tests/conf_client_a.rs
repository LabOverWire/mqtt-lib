use mqtt5::{
    AuthHandler, AuthResponse, ConnectOptions, ConnectResult, MqttClient, MqttError,
    PublishOptions, PublishProperties, QoS,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

const T: Duration = Duration::from_secs(5);

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
const AUTH: u8 = 15;

const P_ASSIGNED_CLIENT_ID: u8 = 0x12;
const P_SERVER_KEEP_ALIVE: u8 = 0x13;
const P_AUTH_METHOD: u8 = 0x15;
const P_AUTH_DATA: u8 = 0x16;
const P_REQUEST_PROBLEM_INFO: u8 = 0x17;
const P_REASON_STRING: u8 = 0x1F;
const P_RECEIVE_MAXIMUM: u8 = 0x21;
const P_TOPIC_ALIAS_MAXIMUM: u8 = 0x22;
const P_TOPIC_ALIAS: u8 = 0x23;
const P_MAXIMUM_QOS: u8 = 0x24;
const P_RETAIN_AVAILABLE: u8 = 0x25;
const P_USER_PROPERTY: u8 = 0x26;
const P_MAXIMUM_PACKET_SIZE: u8 = 0x27;
const P_WILDCARD_AVAILABLE: u8 = 0x28;

#[derive(Debug, Clone)]
struct Raw {
    first: u8,
    body: Vec<u8>,
    wire_len: usize,
    remaining_len_bytes: Vec<u8>,
}

impl Raw {
    fn ptype(&self) -> u8 {
        self.first >> 4
    }
    fn flags(&self) -> u8 {
        self.first & 0x0F
    }
}

fn varint(mut n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut b = u8::try_from(n % 128).unwrap();
        n /= 128;
        if n > 0 {
            b |= 0x80;
        }
        out.push(b);
        if n == 0 {
            return out;
        }
    }
}

fn read_varint(buf: &[u8], pos: &mut usize) -> usize {
    let mut mult = 1usize;
    let mut val = 0usize;
    loop {
        let b = buf[*pos];
        *pos += 1;
        val += usize::from(b & 0x7F) * mult;
        if b & 0x80 == 0 {
            return val;
        }
        mult *= 128;
    }
}

fn read_u16(buf: &[u8], pos: &mut usize) -> u16 {
    let v = u16::from_be_bytes([buf[*pos], buf[*pos + 1]]);
    *pos += 2;
    v
}

fn read_bin(buf: &[u8], pos: &mut usize) -> Vec<u8> {
    let len = usize::from(read_u16(buf, pos));
    let v = buf[*pos..*pos + len].to_vec();
    *pos += len;
    v
}

fn packet(first: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![first];
    out.extend(varint(body.len()));
    out.extend_from_slice(body);
    out
}

fn p_u8(id: u8, v: u8) -> Vec<u8> {
    vec![id, v]
}
fn p_u16(id: u8, v: u16) -> Vec<u8> {
    let mut o = vec![id];
    o.extend(v.to_be_bytes());
    o
}
fn p_u32(id: u8, v: u32) -> Vec<u8> {
    let mut o = vec![id];
    o.extend(v.to_be_bytes());
    o
}
fn mqtt_str(s: &[u8]) -> Vec<u8> {
    let mut o = u16::try_from(s.len()).unwrap().to_be_bytes().to_vec();
    o.extend_from_slice(s);
    o
}
fn p_str(id: u8, s: &[u8]) -> Vec<u8> {
    let mut o = vec![id];
    o.extend(mqtt_str(s));
    o
}
fn p_pair(k: &[u8], v: &[u8]) -> Vec<u8> {
    let mut o = vec![P_USER_PROPERTY];
    o.extend(mqtt_str(k));
    o.extend(mqtt_str(v));
    o
}

fn with_props(props: &[u8]) -> Vec<u8> {
    let mut o = varint(props.len());
    o.extend_from_slice(props);
    o
}

fn connack(session_present: bool, props: &[u8]) -> Vec<u8> {
    let mut body = vec![u8::from(session_present), 0x00];
    body.extend(with_props(props));
    packet(0x20, &body)
}

fn publish_bytes(
    first: u8,
    topic: &[u8],
    packet_id: Option<u16>,
    props: &[u8],
    payload: &[u8],
) -> Vec<u8> {
    let mut body = mqtt_str(topic);
    if let Some(id) = packet_id {
        body.extend(id.to_be_bytes());
    }
    body.extend(with_props(props));
    body.extend_from_slice(payload);
    packet(first, &body)
}

fn ack_with_props(first: u8, packet_id: u16, rc: u8, props: &[u8]) -> Vec<u8> {
    let mut body = packet_id.to_be_bytes().to_vec();
    body.push(rc);
    body.extend(with_props(props));
    packet(first, &body)
}

fn prop_value_len(id: u8, buf: &[u8], pos: usize) -> usize {
    match id {
        0x01 | 0x17 | 0x19 | 0x24 | 0x25 | 0x28 | 0x29 | 0x2A => 1,
        0x13 | 0x21 | 0x22 | 0x23 => 2,
        0x02 | 0x11 | 0x18 | 0x27 => 4,
        0x0B => {
            let mut p = pos;
            let start = p;
            read_varint(buf, &mut p);
            p - start
        }
        0x26 => {
            let l1 = usize::from(u16::from_be_bytes([buf[pos], buf[pos + 1]]));
            let l2 = usize::from(u16::from_be_bytes([buf[pos + 2 + l1], buf[pos + 3 + l1]]));
            4 + l1 + l2
        }
        _ => 2 + usize::from(u16::from_be_bytes([buf[pos], buf[pos + 1]])),
    }
}

fn parse_props(buf: &[u8], pos: &mut usize) -> Vec<(u8, Vec<u8>)> {
    let len = read_varint(buf, pos);
    let end = *pos + len;
    let mut out = Vec::new();
    while *pos < end {
        let id = u8::try_from(read_varint(buf, pos)).unwrap();
        let vlen = prop_value_len(id, buf, *pos);
        out.push((id, buf[*pos..*pos + vlen].to_vec()));
        *pos += vlen;
    }
    out
}

fn has_prop(props: &[(u8, Vec<u8>)], id: u8) -> bool {
    props.iter().any(|(i, _)| *i == id)
}

#[derive(Debug)]
struct ParsedConnect {
    flags: u8,
    keep_alive: u16,
    props: Vec<(u8, Vec<u8>)>,
    client_id: Vec<u8>,
    will_props: Option<Vec<(u8, Vec<u8>)>>,
    will_topic: Option<Vec<u8>>,
    will_payload: Option<Vec<u8>>,
    username: Option<Vec<u8>>,
    password: Option<Vec<u8>>,
    trailing: usize,
}

fn parse_connect(raw: &Raw) -> ParsedConnect {
    let b = &raw.body;
    let mut pos = 0;
    let name = read_bin(b, &mut pos);
    assert_eq!(name, b"MQTT");
    assert_eq!(b[pos], 5);
    pos += 1;
    let flags = b[pos];
    pos += 1;
    let keep_alive = read_u16(b, &mut pos);
    let props = parse_props(b, &mut pos);
    let client_id = read_bin(b, &mut pos);
    let (will_props, will_topic, will_payload) = if flags & 0x04 != 0 {
        let wp = parse_props(b, &mut pos);
        let wt = read_bin(b, &mut pos);
        let wpl = read_bin(b, &mut pos);
        (Some(wp), Some(wt), Some(wpl))
    } else {
        (None, None, None)
    };
    let username = (flags & 0x80 != 0).then(|| read_bin(b, &mut pos));
    let password = (flags & 0x40 != 0).then(|| read_bin(b, &mut pos));
    ParsedConnect {
        flags,
        keep_alive,
        props,
        client_id,
        will_props,
        will_topic,
        will_payload,
        username,
        password,
        trailing: b.len() - pos,
    }
}

#[derive(Debug)]
struct ParsedPublish {
    qos: u8,
    retain: bool,
    dup: bool,
    topic: Vec<u8>,
    packet_id: Option<u16>,
    props: Vec<(u8, Vec<u8>)>,
    payload: Vec<u8>,
}

fn parse_publish(raw: &Raw) -> ParsedPublish {
    let b = &raw.body;
    let mut pos = 0;
    let qos = (raw.flags() >> 1) & 0x03;
    let topic = read_bin(b, &mut pos);
    let packet_id = (qos > 0).then(|| read_u16(b, &mut pos));
    let props = parse_props(b, &mut pos);
    ParsedPublish {
        qos,
        retain: raw.flags() & 0x01 != 0,
        dup: raw.flags() & 0x08 != 0,
        topic,
        packet_id,
        props,
        payload: b[pos..].to_vec(),
    }
}

async fn read_raw(s: &mut TcpStream) -> Option<Raw> {
    let mut first = [0u8; 1];
    s.read_exact(&mut first).await.ok()?;
    let mut remaining_len_bytes = Vec::new();
    let mut mult = 1usize;
    let mut len = 0usize;
    loop {
        let mut b = [0u8; 1];
        s.read_exact(&mut b).await.ok()?;
        remaining_len_bytes.push(b[0]);
        len += usize::from(b[0] & 0x7F) * mult;
        if b[0] & 0x80 == 0 {
            break;
        }
        mult *= 128;
    }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).await.ok()?;
    Some(Raw {
        first: first[0],
        wire_len: 1 + remaining_len_bytes.len() + len,
        remaining_len_bytes,
        body,
    })
}

enum Next {
    Packet(Raw),
    Closed,
    Silent,
}

async fn next_packet(s: &mut TcpStream, d: Duration) -> Next {
    match timeout(d, read_raw(s)).await {
        Ok(Some(r)) => Next::Packet(r),
        Ok(None) => Next::Closed,
        Err(_) => Next::Silent,
    }
}

async fn next_non_ping(s: &mut TcpStream, d: Duration) -> Next {
    let deadline = tokio::time::Instant::now() + d;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match next_packet(s, left).await {
            Next::Packet(r) if r.ptype() == PINGREQ => {
                let _ = s.write_all(&[0xD0, 0x00]).await;
            }
            other => return other,
        }
    }
}

async fn next_of_type(s: &mut TcpStream, ptype: u8, d: Duration) -> Option<Raw> {
    let deadline = tokio::time::Instant::now() + d;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match next_packet(s, left).await {
            Next::Packet(r) if r.ptype() == ptype => return Some(r),
            Next::Packet(r) if r.ptype() == PINGREQ => {
                let _ = s.write_all(&[0xD0, 0x00]).await;
            }
            Next::Packet(_) => {}
            Next::Closed | Next::Silent => return None,
        }
    }
}

struct CloseObservation {
    closed: bool,
    disconnect_rc: Option<u8>,
}

async fn observe_close(s: &mut TcpStream, d: Duration) -> CloseObservation {
    let deadline = tokio::time::Instant::now() + d;
    let mut disconnect_rc = None;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match next_non_ping(s, left).await {
            Next::Packet(r) if r.ptype() == DISCONNECT => {
                disconnect_rc = Some(r.body.first().copied().unwrap_or(0));
            }
            Next::Packet(_) => {}
            Next::Closed => {
                return CloseObservation {
                    closed: true,
                    disconnect_rc,
                }
            }
            Next::Silent => {
                return CloseObservation {
                    closed: false,
                    disconnect_rc,
                }
            }
        }
    }
}

fn opts(id: &str) -> ConnectOptions {
    ConnectOptions::new(id)
        .with_automatic_reconnect(false)
        .with_keep_alive(Duration::from_secs(60))
}

fn reconnecting_opts(id: &str) -> ConnectOptions {
    ConnectOptions::new(id)
        .with_automatic_reconnect(true)
        .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(300))
        .with_keep_alive(Duration::from_secs(60))
}

struct Setup {
    client: MqttClient,
    listener: TcpListener,
    stream: TcpStream,
    connect: Raw,
    result: mqtt5::Result<ConnectResult>,
}

async fn start(options: ConnectOptions, session_present: bool, props: &[u8]) -> Setup {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let client = MqttClient::with_options(options.clone());
    let c = client.clone();
    let handle = tokio::spawn(async move { Box::pin(c.connect_with_options(&url, options)).await });
    let (mut stream, _) = timeout(T, listener.accept()).await.unwrap().unwrap();
    let connect = timeout(T, read_raw(&mut stream)).await.unwrap().unwrap();
    assert_eq!(connect.ptype(), CONNECT);
    stream
        .write_all(&connack(session_present, props))
        .await
        .unwrap();
    let result = timeout(T, handle).await.unwrap().unwrap();
    Setup {
        client,
        listener,
        stream,
        connect,
        result,
    }
}

async fn accept_next(
    listener: &TcpListener,
    session_present: bool,
    props: &[u8],
) -> (TcpStream, Raw) {
    let (mut stream, _) = timeout(Duration::from_secs(8), listener.accept())
        .await
        .expect("client did not reconnect")
        .unwrap();
    let connect = timeout(T, read_raw(&mut stream)).await.unwrap().unwrap();
    assert_eq!(connect.ptype(), CONNECT);
    stream
        .write_all(&connack(session_present, props))
        .await
        .unwrap();
    (stream, connect)
}

async fn wait_connected(client: &MqttClient, want: bool) {
    for _ in 0..100 {
        if client.is_connected().await == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("client connected state never became {want}");
}

async fn reconnect_with(
    conn1_props: &[u8],
    conn2_props: &[u8],
    id: &str,
) -> (MqttClient, TcpListener, TcpStream) {
    let setup = start(reconnecting_opts(id), false, conn1_props).await;
    setup.result.expect("first connect");
    let Setup {
        client,
        listener,
        stream,
        ..
    } = setup;
    drop(stream);
    wait_connected(&client, false).await;
    let (stream2, _) = accept_next(&listener, false, conn2_props).await;
    wait_connected(&client, true).await;
    (client, listener, stream2)
}

fn qos(q: QoS) -> PublishOptions {
    PublishOptions {
        qos: q,
        ..Default::default()
    }
}

fn with_alias(alias: u16) -> PublishOptions {
    PublishOptions {
        properties: PublishProperties {
            topic_alias: Some(alias),
            ..Default::default()
        },
        ..Default::default()
    }
}

async fn assert_no_alias_sent(client: &MqttClient, stream: &mut TcpStream, alias: u16, id: &str) {
    let result = client
        .publish_with_options("alias/topic", b"x".to_vec(), with_alias(alias))
        .await;
    if let Some(raw) = next_of_type(stream, PUBLISH, Duration::from_secs(1)).await {
        let p = parse_publish(&raw);
        assert!(
            !has_prop(&p.props, P_TOPIC_ALIAS),
            "{id} VIOLATION: client sent Topic Alias {alias} in PUBLISH (props {:?}); publish() returned {result:?}",
            p.props
        );
    }
}

#[tokio::test]
async fn mqtt_3_2_2_4_fresh_client_clean_start_1_session_present_1_must_close() {
    let mut s = start(opts("sp-clean"), true, &[]).await;
    let obs = observe_close(&mut s.stream, Duration::from_secs(2)).await;
    assert!(
        s.result.is_err() && obs.closed,
        "MQTT-3.2.2-4 VIOLATION: fresh client (Clean Start=1, no session state) accepted CONNACK Session Present=1: connect result {:?}, network closed={}",
        s.result,
        obs.closed
    );
}

#[tokio::test]
async fn mqtt_3_2_2_4_fresh_client_clean_start_0_session_present_1_must_close() {
    let o = opts("sp-fresh")
        .with_clean_start(false)
        .with_session_expiry_interval(300);
    let mut s = start(o, true, &[]).await;
    let obs = observe_close(&mut s.stream, Duration::from_secs(2)).await;
    assert!(
        s.result.is_err() && obs.closed,
        "MQTT-3.2.2-4 VIOLATION: brand-new client with no session state accepted CONNACK Session Present=1: connect result {:?}, network closed={}",
        s.result,
        obs.closed
    );
}

#[tokio::test]
async fn mqtt_3_2_2_18_topic_alias_absent_client_must_not_send_alias() {
    let mut s = start(opts("ta-absent"), false, &[]).await;
    s.result.as_ref().unwrap();
    assert_no_alias_sent(&s.client, &mut s.stream, 1, "MQTT-3.2.2-18").await;
}

#[tokio::test]
async fn mqtt_3_2_2_17_topic_alias_above_server_maximum() {
    let mut s = start(opts("ta-max"), false, &p_u16(P_TOPIC_ALIAS_MAXIMUM, 2)).await;
    s.result.as_ref().unwrap();
    assert_no_alias_sent(&s.client, &mut s.stream, 3, "MQTT-3.2.2-17").await;
}

#[tokio::test]
async fn mqtt_3_2_2_14_retain_available_0_client_must_not_send_retain() {
    let mut s = start(opts("ra0"), false, &p_u8(P_RETAIN_AVAILABLE, 0)).await;
    s.result.as_ref().unwrap();
    let r = s.client.publish_retain("r/t", b"x".to_vec()).await;
    if let Some(raw) = next_of_type(&mut s.stream, PUBLISH, Duration::from_secs(1)).await {
        assert!(
            !parse_publish(&raw).retain,
            "MQTT-3.2.2-14 VIOLATION: client sent PUBLISH with RETAIN=1 after CONNACK Retain Available=0 (publish returned {r:?})"
        );
    }
}

#[tokio::test]
async fn mqtt_3_2_2_11_maximum_qos_live_publish() {
    let mut s = start(opts("mq0"), false, &p_u8(P_MAXIMUM_QOS, 0)).await;
    s.result.as_ref().unwrap();
    let c = s.client.clone();
    let h =
        tokio::spawn(async move { c.publish_qos("q/t", b"x".to_vec(), QoS::ExactlyOnce).await });
    if let Some(raw) = next_of_type(&mut s.stream, PUBLISH, Duration::from_secs(1)).await {
        assert_eq!(
            parse_publish(&raw).qos,
            0,
            "MQTT-3.2.2-11 VIOLATION: PUBLISH QoS exceeds server Maximum QoS 0"
        );
    }
    h.abort();
}

async fn queued_publish_then_reconnect(
    conn2_props: &[u8],
    options: PublishOptions,
    id: &str,
) -> Option<ParsedPublish> {
    let o = reconnecting_opts(id)
        .with_clean_start(false)
        .with_session_expiry_interval(300);
    let s = start(o, false, &[]).await;
    s.result.as_ref().unwrap();
    let Setup {
        client,
        listener,
        stream,
        ..
    } = s;
    drop(stream);
    wait_connected(&client, false).await;
    let queued = client
        .publish_with_options("queued/t", b"offline".to_vec(), options)
        .await;
    assert!(
        queued.is_ok(),
        "publish while offline should queue: {queued:?}"
    );
    let (mut stream2, _) = accept_next(&listener, true, conn2_props).await;
    let raw = next_of_type(&mut stream2, PUBLISH, Duration::from_secs(3)).await;
    raw.as_ref().map(parse_publish).inspect(|p| {
        assert_eq!(p.topic, b"queued/t");
        assert_eq!(p.payload, b"offline");
    })
}

#[tokio::test]
async fn mqtt_3_2_2_11_maximum_qos_applies_to_messages_queued_while_offline() {
    let p =
        queued_publish_then_reconnect(&p_u8(P_MAXIMUM_QOS, 1), qos(QoS::ExactlyOnce), "mq-queued")
            .await;
    let p = p.expect("queued message was never sent");
    assert!(
        p.qos <= 1,
        "MQTT-3.2.2-11 VIOLATION: message queued offline was flushed as QoS {} after CONNACK Maximum QoS=1",
        p.qos
    );
}

#[tokio::test]
async fn mqtt_3_2_2_14_retain_available_applies_to_messages_queued_while_offline() {
    let options = PublishOptions {
        qos: QoS::AtLeastOnce,
        retain: true,
        ..Default::default()
    };
    let p = queued_publish_then_reconnect(&p_u8(P_RETAIN_AVAILABLE, 0), options, "ra-queued").await;
    if let Some(p) = p {
        assert!(
            !p.retain,
            "MQTT-3.2.2-14 VIOLATION: message queued offline was flushed with RETAIN=1 after CONNACK Retain Available=0"
        );
    }
}

#[tokio::test]
async fn crosscheck1_mqtt_3_2_2_18_topic_alias_maximum_not_stale_after_reconnect() {
    let (client, _l, mut stream) =
        reconnect_with(&p_u16(P_TOPIC_ALIAS_MAXIMUM, 10), &[], "stale-ta").await;
    assert_no_alias_sent(
        &client,
        &mut stream,
        1,
        "MQTT-3.2.2-18 (stale Topic Alias Maximum)",
    )
    .await;
}

#[tokio::test]
async fn crosscheck1_mqtt_3_2_2_11_maximum_qos_not_stale_after_reconnect() {
    let (client, _l, mut stream) = reconnect_with(&p_u8(P_MAXIMUM_QOS, 1), &[], "stale-mq").await;
    let c = client.clone();
    let h =
        tokio::spawn(async move { c.publish_qos("q/t", b"x".to_vec(), QoS::ExactlyOnce).await });
    let raw = next_of_type(&mut stream, PUBLISH, Duration::from_secs(2))
        .await
        .expect("no PUBLISH");
    assert_eq!(
        parse_publish(&raw).qos,
        2,
        "stale capability: conn 2 CONNACK omitted Maximum QoS (=2) but client still downgraded using conn 1 Maximum QoS=1"
    );
    h.abort();
}

#[tokio::test]
async fn crosscheck1_mqtt_3_2_2_15_maximum_packet_size_not_stale_after_reconnect() {
    let (client, _l, mut stream) =
        reconnect_with(&p_u32(P_MAXIMUM_PACKET_SIZE, 64), &[], "stale-mps").await;
    let r = client.publish("big/t", vec![b'x'; 500]).await;
    assert!(
        r.is_ok(),
        "stale capability: conn 2 CONNACK omitted Maximum Packet Size but client still enforced conn 1 limit 64: {r:?}"
    );
    assert!(next_of_type(&mut stream, PUBLISH, Duration::from_secs(1))
        .await
        .is_some());
}

#[tokio::test]
async fn crosscheck1_mqtt_3_2_2_15_maximum_packet_size_newly_imposed_on_reconnect() {
    let (client, _l, mut stream) =
        reconnect_with(&[], &p_u32(P_MAXIMUM_PACKET_SIZE, 64), "new-mps").await;
    let r = client.publish("big/t", vec![b'x'; 500]).await;
    let sent = next_of_type(&mut stream, PUBLISH, Duration::from_secs(1)).await;
    assert!(
        sent.is_none() || sent.as_ref().is_some_and(|p| p.wire_len <= 64),
        "MQTT-3.2.2-15 VIOLATION: client sent {}-byte PUBLISH after conn 2 CONNACK Maximum Packet Size=64 (publish returned {r:?})",
        sent.map_or(0, |p| p.wire_len)
    );
}

#[tokio::test]
async fn crosscheck1_retain_available_not_stale_after_reconnect() {
    let (client, _l, mut stream) =
        reconnect_with(&p_u8(P_RETAIN_AVAILABLE, 0), &[], "stale-ra").await;
    let r = client.publish_retain("r/t", b"x".to_vec()).await;
    let raw = next_of_type(&mut stream, PUBLISH, Duration::from_secs(1)).await;
    assert!(
        raw.as_ref().is_some_and(|p| parse_publish(p).retain),
        "stale capability: conn 2 CONNACK omitted Retain Available (=1) but retained publish was not sent with RETAIN=1 ({r:?})"
    );
}

#[tokio::test]
async fn crosscheck1_receive_maximum_not_stale_after_reconnect() {
    let (client, _l, mut stream) =
        reconnect_with(&p_u16(P_RECEIVE_MAXIMUM, 1), &[], "stale-rm").await;
    let mut handles = Vec::new();
    for i in 0..3u8 {
        let c = client.clone();
        handles.push(tokio::spawn(async move {
            c.publish_qos("rm/t", vec![i], QoS::AtLeastOnce).await
        }));
    }
    let mut seen = 0;
    while next_of_type(&mut stream, PUBLISH, Duration::from_millis(700))
        .await
        .is_some()
    {
        seen += 1;
    }
    for h in handles {
        h.abort();
    }
    assert_eq!(
        seen, 3,
        "stale capability: conn 2 CONNACK omitted Receive Maximum (=65535) but client only sent {seen} of 3 unacked QoS1 PUBLISHes"
    );
}

#[tokio::test]
async fn crosscheck1_mqtt_3_2_2_11_maximum_qos_newly_imposed_on_reconnect() {
    let (client, _l, mut stream) = reconnect_with(&[], &p_u8(P_MAXIMUM_QOS, 0), "new-mq").await;
    let c = client.clone();
    let h =
        tokio::spawn(async move { c.publish_qos("q/t", b"x".to_vec(), QoS::AtLeastOnce).await });
    if let Some(raw) = next_of_type(&mut stream, PUBLISH, Duration::from_secs(1)).await {
        assert_eq!(
            parse_publish(&raw).qos,
            0,
            "MQTT-3.2.2-11 VIOLATION: after conn 2 CONNACK Maximum QoS=0 the client still sent QoS>0"
        );
    }
    h.abort();
}

fn rpi0_opts(id: &str) -> ConnectOptions {
    let mut o = opts(id);
    o.properties.request_problem_information = Some(false);
    o
}

#[tokio::test]
async fn crosscheck3_mqtt_3_1_2_29_request_problem_information_0_is_sent() {
    let s = start(rpi0_opts("rpi-wire"), false, &[]).await;
    let c = parse_connect(&s.connect);
    assert!(
        c.props.iter().any(|(id, v)| *id == P_REQUEST_PROBLEM_INFO && v == &[0]),
        "MQTT-3.1.2-29 precondition BUG: ConnectProperties.request_problem_information=Some(false) was not put on the wire; CONNECT props {:?}",
        c.props
    );
}

#[tokio::test]
async fn crosscheck3_mqtt_3_1_2_29_reason_string_on_puback_with_rpi0_disconnects_0x82() {
    let mut s = start(rpi0_opts("rpi-puback"), false, &[]).await;
    s.result.as_ref().unwrap();
    let c = s.client.clone();
    let h =
        tokio::spawn(async move { c.publish_qos("p/t", b"x".to_vec(), QoS::AtLeastOnce).await });
    let raw = next_of_type(&mut s.stream, PUBLISH, T).await.unwrap();
    let pid = parse_publish(&raw).packet_id.unwrap();
    s.stream
        .write_all(&ack_with_props(
            0x40,
            pid,
            0x00,
            &p_str(P_REASON_STRING, b"why"),
        ))
        .await
        .unwrap();
    let obs = observe_close(&mut s.stream, Duration::from_secs(2)).await;
    let r = timeout(Duration::from_secs(1), h).await;
    assert!(
        obs.disconnect_rc == Some(0x82) && obs.closed,
        "MQTT-3.1.2-29 (§3.1.2.11.7) VIOLATION: with Request Problem Information=0 configured, PUBACK carrying a Reason String did not trigger DISCONNECT 0x82 (disconnect rc {:?}, closed {}, publish result {r:?})",
        obs.disconnect_rc,
        obs.closed
    );
}

#[tokio::test]
async fn crosscheck3_mqtt_3_1_2_29_user_property_on_suback_with_rpi0_disconnects_0x82() {
    let mut s = start(rpi0_opts("rpi-suback"), false, &[]).await;
    s.result.as_ref().unwrap();
    let c = s.client.clone();
    let h = tokio::spawn(async move { c.subscribe("s/t", |_| {}).await });
    let raw = next_of_type(&mut s.stream, SUBSCRIBE, T).await.unwrap();
    let pid = u16::from_be_bytes([raw.body[0], raw.body[1]]);
    let mut body = pid.to_be_bytes().to_vec();
    body.extend(with_props(&p_pair(b"k", b"v")));
    body.push(0x00);
    s.stream.write_all(&packet(0x90, &body)).await.unwrap();
    let obs = observe_close(&mut s.stream, Duration::from_secs(2)).await;
    let r = timeout(Duration::from_secs(1), h).await;
    assert!(
        obs.disconnect_rc == Some(0x82) && obs.closed,
        "MQTT-3.1.2-29 (§3.1.2.11.7) VIOLATION: with Request Problem Information=0 configured, SUBACK carrying a User Property did not trigger DISCONNECT 0x82 (disconnect rc {:?}, closed {}, subscribe result {r:?})",
        obs.disconnect_rc,
        obs.closed
    );
}

type AuthFut<'a, R> =
    std::pin::Pin<Box<dyn std::future::Future<Output = mqtt5::Result<R>> + Send + 'a>>;

struct StaticAuth;

impl AuthHandler for StaticAuth {
    fn handle_challenge<'a>(
        &'a self,
        _auth_method: &'a str,
        _challenge_data: Option<&'a [u8]>,
    ) -> AuthFut<'a, AuthResponse> {
        Box::pin(async move { Ok(AuthResponse::Continue(b"resp".to_vec())) })
    }

    fn initial_response<'a>(&'a self, _auth_method: &'a str) -> AuthFut<'a, Option<Vec<u8>>> {
        Box::pin(async move { Ok(Some(b"init".to_vec())) })
    }
}

fn auth_packet(rc: u8, extra_props: &[u8]) -> Vec<u8> {
    let mut props = p_str(P_AUTH_METHOD, b"TEST");
    props.extend_from_slice(extra_props);
    let mut body = vec![rc];
    body.extend(with_props(&props));
    packet(0xF0, &body)
}

#[tokio::test]
async fn crosscheck3_mqtt_3_1_2_29_reason_string_on_reauth_auth_with_rpi0_disconnects_0x82() {
    let mut o = rpi0_opts("rpi-auth");
    o.properties.authentication_method = Some("TEST".to_string());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let client = MqttClient::with_options(o.clone());
    client.set_auth_handler(StaticAuth).await;
    let c = client.clone();
    let h = tokio::spawn(async move { Box::pin(c.connect_with_options(&url, o)).await });
    let (mut stream, _) = timeout(T, listener.accept()).await.unwrap().unwrap();
    read_raw(&mut stream).await.unwrap();
    stream
        .write_all(&connack(false, &p_str(P_AUTH_METHOD, b"TEST")))
        .await
        .unwrap();
    timeout(T, h).await.unwrap().unwrap().unwrap();
    client.reauthenticate().await.unwrap();
    let auth = next_of_type(&mut stream, AUTH, T)
        .await
        .expect("no re-auth AUTH");
    assert_eq!(auth.body[0], 0x19);
    stream
        .write_all(&auth_packet(0x00, &p_str(P_REASON_STRING, b"why")))
        .await
        .unwrap();
    let obs = observe_close(&mut stream, Duration::from_secs(2)).await;
    assert!(
        obs.disconnect_rc == Some(0x82) && obs.closed,
        "MQTT-3.1.2-29 (§3.1.2.11.7) VIOLATION: with Request Problem Information=0 configured, re-auth AUTH carrying a Reason String did not trigger DISCONNECT 0x82 (disconnect rc {:?}, closed {})",
        obs.disconnect_rc,
        obs.closed
    );
}

#[tokio::test]
async fn crosscheck4_mqtt_3_2_2_15_automatic_puback_respects_tiny_maximum_packet_size() {
    let mut s = start(opts("tiny-mps"), false, &p_u32(P_MAXIMUM_PACKET_SIZE, 3)).await;
    s.result.as_ref().unwrap();
    s.stream
        .write_all(&publish_bytes(0x32, b"a", Some(7), &[], b"x"))
        .await
        .unwrap();
    let ack = next_of_type(&mut s.stream, PUBACK, Duration::from_secs(1)).await;
    assert!(
        ack.as_ref().is_none_or(|a| a.wire_len <= 3),
        "MQTT-3.2.2-15 VIOLATION: automatic PUBACK of {} bytes sent despite server Maximum Packet Size=3",
        ack.map_or(0, |a| a.wire_len)
    );
}

#[tokio::test]
async fn crosscheck4_mqtt_3_2_2_15_automatic_pubrec_respects_tiny_maximum_packet_size() {
    let mut s = start(opts("tiny-mps2"), false, &p_u32(P_MAXIMUM_PACKET_SIZE, 3)).await;
    s.result.as_ref().unwrap();
    s.stream
        .write_all(&publish_bytes(0x34, b"a", Some(8), &[], b"x"))
        .await
        .unwrap();
    let ack = next_of_type(&mut s.stream, PUBREC, Duration::from_secs(1)).await;
    assert!(
        ack.as_ref().is_none_or(|a| a.wire_len <= 3),
        "MQTT-3.2.2-15 VIOLATION: automatic PUBREC of {} bytes sent despite server Maximum Packet Size=3",
        ack.map_or(0, |a| a.wire_len)
    );
}

#[tokio::test]
async fn crosscheck5_auth_data_without_auth_method_never_sent() {
    let s = start(
        opts("authdata").with_authentication_data(b"secret"),
        false,
        &[],
    )
    .await;
    let c = parse_connect(&s.connect);
    assert!(
        !has_prop(&c.props, P_AUTH_DATA) || has_prop(&c.props, P_AUTH_METHOD),
        "§3.1.2.11.10 VIOLATION: CONNECT carries Authentication Data without Authentication Method: {:?}",
        c.props
    );
}

#[tokio::test]
async fn crosscheck6_mqtt_3_2_2_15_oversized_publish_rejected_and_packet_id_not_leaked() {
    let mut s = start(opts("mps-pub"), false, &p_u32(P_MAXIMUM_PACKET_SIZE, 64)).await;
    s.result.as_ref().unwrap();
    let big = s
        .client
        .publish_qos("p/t", vec![b'x'; 200], QoS::AtLeastOnce)
        .await;
    assert!(
        matches!(big, Err(MqttError::PacketTooLarge { .. })),
        "oversized publish not rejected: {big:?}"
    );
    let c = s.client.clone();
    let h =
        tokio::spawn(async move { c.publish_qos("p/t", b"x".to_vec(), QoS::AtLeastOnce).await });
    let raw = next_of_type(&mut s.stream, PUBLISH, T).await.unwrap();
    let p = parse_publish(&raw);
    assert_eq!(
        p.packet_id,
        Some(1),
        "packet id leaked by rejected oversized publish"
    );
    s.stream
        .write_all(&packet(0x40, &p.packet_id.unwrap().to_be_bytes()))
        .await
        .unwrap();
    timeout(T, h).await.unwrap().unwrap().unwrap();
}

#[tokio::test]
async fn crosscheck6_mqtt_3_2_2_15_oversized_subscribe_not_sent() {
    let mut s = start(opts("mps-sub"), false, &p_u32(P_MAXIMUM_PACKET_SIZE, 64)).await;
    s.result.as_ref().unwrap();
    let c = s.client.clone();
    let filter = format!("f/{}", "x".repeat(100));
    let h = tokio::spawn(async move { c.subscribe(filter, |_| {}).await });
    let sent = next_of_type(&mut s.stream, SUBSCRIBE, Duration::from_secs(1)).await;
    h.abort();
    assert!(
        sent.as_ref().is_none_or(|p| p.wire_len <= 64),
        "MQTT-3.2.2-15 VIOLATION: client sent {}-byte SUBSCRIBE despite server Maximum Packet Size=64",
        sent.map_or(0, |p| p.wire_len)
    );
}

#[tokio::test]
async fn crosscheck6_mqtt_3_2_2_15_oversized_unsubscribe_not_sent() {
    let mut s = start(opts("mps-unsub"), false, &p_u32(P_MAXIMUM_PACKET_SIZE, 64)).await;
    s.result.as_ref().unwrap();
    let c = s.client.clone();
    let filter = format!("f/{}", "x".repeat(100));
    let h = tokio::spawn(async move { c.unsubscribe(filter).await });
    let sent = next_of_type(&mut s.stream, UNSUBSCRIBE, Duration::from_secs(1)).await;
    h.abort();
    assert!(
        sent.as_ref().is_none_or(|p| p.wire_len <= 64),
        "MQTT-3.2.2-15 VIOLATION: client sent {}-byte UNSUBSCRIBE despite server Maximum Packet Size=64",
        sent.map_or(0, |p| p.wire_len)
    );
}

#[tokio::test]
async fn mqtt_2_2_1_3_packet_id_not_reused_while_in_flight_after_wrap() {
    let mut s = start(opts("pid-wrap"), false, &[]).await;
    s.result.as_ref().unwrap();
    let held = s.client.clone();
    let _held = tokio::spawn(async move {
        held.publish_qos("w/held", b"h".to_vec(), QoS::AtLeastOnce)
            .await
    });
    let first = parse_publish(&next_of_type(&mut s.stream, PUBLISH, T).await.unwrap());
    let held_id = first.packet_id.unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<u16>();
    let mut stream = s.stream;
    let acker = tokio::spawn(async move {
        while let Some(raw) = read_raw(&mut stream).await {
            if raw.ptype() == PUBLISH {
                let id = parse_publish(&raw).packet_id.unwrap();
                let _ = tx.send(id);
                if id != held_id {
                    let _ = stream.write_all(&packet(0x40, &id.to_be_bytes())).await;
                }
            } else if raw.ptype() == PINGREQ {
                let _ = stream.write_all(&[0xD0, 0x00]).await;
            }
        }
    });

    let mut reused = None;
    for _ in 0..65535u32 {
        let c = s.client.clone();
        let _ = timeout(T, c.publish_qos("w/t", b"x".to_vec(), QoS::AtLeastOnce)).await;
        if let Some(id) = rx.recv().await {
            if id == held_id {
                reused = Some(id);
                break;
            }
        }
    }
    acker.abort();
    assert!(
        reused.is_none(),
        "MQTT-2.2.1-3 VIOLATION: packet id {held_id} reassigned to a new PUBLISH while the original QoS1 PUBLISH with that id was still unacknowledged"
    );
}

#[tokio::test]
async fn mqtt_3_1_2_23_unacked_qos1_publish_resent_on_session_resume() {
    let o = reconnecting_opts("resend1")
        .with_clean_start(false)
        .with_session_expiry_interval(300);
    let mut s = start(o, false, &[]).await;
    s.result.as_ref().unwrap();
    let c = s.client.clone();
    let _h =
        tokio::spawn(async move { c.publish_qos("r/t", b"x".to_vec(), QoS::AtLeastOnce).await });
    let orig = parse_publish(&next_of_type(&mut s.stream, PUBLISH, T).await.unwrap());
    let Setup {
        client,
        listener,
        stream,
        ..
    } = s;
    drop(stream);
    wait_connected(&client, false).await;
    let (mut stream2, _) = accept_next(&listener, true, &[]).await;
    let resent = next_of_type(&mut stream2, PUBLISH, Duration::from_secs(3)).await;
    let resent = resent.map(|r| parse_publish(&r));
    assert!(
        resent
            .as_ref()
            .is_some_and(|p| p.packet_id == orig.packet_id && p.dup),
        "MQTT-3.1.2-23 / MQTT-4.4.0-1 VIOLATION: unacknowledged QoS1 PUBLISH id {:?} not resent (DUP=1, same id) after reconnect with Clean Start=0 and Session Present=1; got {resent:?}",
        orig.packet_id
    );
}

#[tokio::test]
async fn mqtt_3_1_2_23_unacked_pubrel_resent_on_session_resume() {
    let o = reconnecting_opts("resend2")
        .with_clean_start(false)
        .with_session_expiry_interval(300);
    let mut s = start(o, false, &[]).await;
    s.result.as_ref().unwrap();
    let c = s.client.clone();
    let _h =
        tokio::spawn(async move { c.publish_qos("r/t", b"x".to_vec(), QoS::ExactlyOnce).await });
    let orig = parse_publish(&next_of_type(&mut s.stream, PUBLISH, T).await.unwrap());
    let pid = orig.packet_id.unwrap();
    s.stream
        .write_all(&packet(0x50, &pid.to_be_bytes()))
        .await
        .unwrap();
    next_of_type(&mut s.stream, PUBREL, T)
        .await
        .expect("no PUBREL");
    let Setup {
        client,
        listener,
        stream,
        ..
    } = s;
    drop(stream);
    wait_connected(&client, false).await;
    let (mut stream2, _) = accept_next(&listener, true, &[]).await;
    let resent = next_of_type(&mut stream2, PUBREL, Duration::from_secs(3)).await;
    assert!(
        resent
            .as_ref()
            .is_some_and(|r| u16::from_be_bytes([r.body[0], r.body[1]]) == pid),
        "MQTT-3.1.2-23 / MQTT-4.4.0-1 VIOLATION: PUBREL for id {pid} not resent after reconnect with Clean Start=0 and Session Present=1"
    );
}

#[tokio::test]
async fn mqtt_3_1_3_2_assigned_client_identifier_used_to_resume_session() {
    let o = reconnecting_opts("")
        .with_clean_start(false)
        .with_session_expiry_interval(300);
    let s = start(o, false, &p_str(P_ASSIGNED_CLIENT_ID, b"srv-assigned-1")).await;
    s.result.as_ref().unwrap();
    let first = parse_connect(&s.connect);
    let Setup {
        client,
        listener,
        stream,
        ..
    } = s;
    drop(stream);
    wait_connected(&client, false).await;
    let (_stream2, raw2) = accept_next(&listener, true, &[]).await;
    let second = parse_connect(&raw2);
    assert_eq!(
        String::from_utf8_lossy(&second.client_id),
        "srv-assigned-1",
        "MQTT-3.1.3-2 VIOLATION: client holding Session State (Clean Start=0, SEI=300) under server-assigned id reconnected with ClientID {:?} (first CONNECT used {:?}), so the state it holds cannot be identified",
        String::from_utf8_lossy(&second.client_id),
        String::from_utf8_lossy(&first.client_id)
    );
}

#[tokio::test]
async fn mqtt_3_1_2_21_server_keep_alive_overrides_client_value() {
    let mut s = start(opts("ska"), false, &p_u16(P_SERVER_KEEP_ALIVE, 1)).await;
    s.result.as_ref().unwrap();
    let ping = next_of_type(&mut s.stream, PINGREQ, Duration::from_millis(1600)).await;
    assert!(
        ping.is_some(),
        "MQTT-3.1.2-21 VIOLATION: Server Keep Alive=1 but no PINGREQ within 1.6s (client used its own 60s)"
    );
}

#[tokio::test]
async fn mqtt_3_1_2_20_pingreq_sent_when_idle() {
    let o = opts("ka1").with_keep_alive(Duration::from_secs(1));
    let mut s = start(o, false, &[]).await;
    s.result.as_ref().unwrap();
    let mut pings = 0;
    for _ in 0..3 {
        match next_packet(&mut s.stream, Duration::from_millis(1100)).await {
            Next::Packet(r) if r.ptype() == PINGREQ => {
                pings += 1;
                s.stream.write_all(&[0xD0, 0x00]).await.unwrap();
            }
            _ => break,
        }
    }
    assert_eq!(
        pings, 3,
        "MQTT-3.1.2-20 VIOLATION: Keep Alive=1s idle client did not send a PINGREQ within each 1.1s window"
    );
}

struct MalformedOutcome {
    delivered: usize,
    still_connected: bool,
    obs: CloseObservation,
}

async fn inject_after_subscribe(bytes: &[u8], id: &str) -> MalformedOutcome {
    let mut s = start(opts(id), false, &[]).await;
    s.result.as_ref().unwrap();
    let delivered = Arc::new(AtomicUsize::new(0));
    let d = delivered.clone();
    let c = s.client.clone();
    let sub = tokio::spawn(async move {
        c.subscribe("#", move |_| {
            d.fetch_add(1, Ordering::SeqCst);
        })
        .await
    });
    let raw = next_of_type(&mut s.stream, SUBSCRIBE, T).await.unwrap();
    let mut body = raw.body[0..2].to_vec();
    body.extend([0x00, 0x00]);
    s.stream.write_all(&packet(0x90, &body)).await.unwrap();
    timeout(T, sub).await.unwrap().unwrap().unwrap();
    s.stream.write_all(bytes).await.unwrap();
    let obs = observe_close(&mut s.stream, Duration::from_secs(3)).await;
    MalformedOutcome {
        delivered: delivered.load(Ordering::SeqCst),
        still_connected: s.client.is_connected().await,
        obs,
    }
}

async fn assert_malformed_rejected(bytes: &[u8], id: &str, what: &str) {
    let o = inject_after_subscribe(bytes, id).await;
    assert!(
        o.delivered == 0 && !o.still_connected,
        "{id} VIOLATION: malformed {what} was accepted (delivered {} times, client still connected={})",
        o.delivered,
        o.still_connected
    );
}

fn surrogate_topic_publish() -> Vec<u8> {
    publish_bytes(0x30, &[b'a', 0xED, 0xA0, 0x80], None, &[], b"x")
}

#[tokio::test]
async fn mqtt_1_5_4_1_surrogate_in_inbound_topic_is_malformed() {
    assert_malformed_rejected(
        &surrogate_topic_publish(),
        "MQTT-1.5.4-1",
        "PUBLISH topic with UTF-16 surrogate",
    )
    .await;
}

#[tokio::test]
async fn mqtt_1_5_4_2_null_in_inbound_topic_is_malformed() {
    assert_malformed_rejected(
        &publish_bytes(0x30, b"a\0b", None, &[], b"x"),
        "MQTT-1.5.4-2",
        "PUBLISH topic containing U+0000",
    )
    .await;
}

#[tokio::test]
async fn mqtt_1_5_7_1_invalid_utf8_user_property_pair_is_malformed() {
    let mut prop = vec![P_USER_PROPERTY];
    prop.extend(mqtt_str(b"k"));
    prop.extend(mqtt_str(&[0xFF, 0xFE]));
    assert_malformed_rejected(
        &publish_bytes(0x30, b"a", None, &prop, b"x"),
        "MQTT-1.5.7-1",
        "PUBLISH with invalid UTF-8 User Property value",
    )
    .await;
}

#[tokio::test]
async fn mqtt_2_1_3_1_reserved_flags_on_inbound_puback_is_malformed() {
    let mut s = start(opts("flags-puback"), false, &[]).await;
    s.result.as_ref().unwrap();
    let c = s.client.clone();
    let h =
        tokio::spawn(async move { c.publish_qos("p/t", b"x".to_vec(), QoS::AtLeastOnce).await });
    let pid = parse_publish(&next_of_type(&mut s.stream, PUBLISH, T).await.unwrap())
        .packet_id
        .unwrap();
    s.stream
        .write_all(&packet(0x42, &pid.to_be_bytes()))
        .await
        .unwrap();
    let r = timeout(Duration::from_secs(2), h).await.unwrap().unwrap();
    assert!(
        !matches!(r, Ok(mqtt5::PublishResult::Sent(_))) && !s.client.is_connected().await,
        "MQTT-2.1.3-1 VIOLATION: PUBACK with reserved flag bits 0x2 accepted as a valid acknowledgement ({r:?})"
    );
}

#[tokio::test]
async fn mqtt_2_1_3_1_reserved_flags_on_inbound_suback_is_malformed() {
    let mut s = start(opts("flags-suback"), false, &[]).await;
    s.result.as_ref().unwrap();
    let c = s.client.clone();
    let h = tokio::spawn(async move { c.subscribe("s/t", |_| {}).await });
    let raw = next_of_type(&mut s.stream, SUBSCRIBE, T).await.unwrap();
    let mut body = raw.body[0..2].to_vec();
    body.extend([0x00, 0x00]);
    s.stream.write_all(&packet(0x92, &body)).await.unwrap();
    let r = timeout(Duration::from_secs(2), h).await.unwrap().unwrap();
    assert!(
        r.is_err() && !s.client.is_connected().await,
        "MQTT-2.1.3-1 VIOLATION: SUBACK with reserved flag bits 0x2 accepted as a valid acknowledgement ({r:?}, still connected={})",
        s.client.is_connected().await
    );
}

#[tokio::test]
async fn mqtt_4_13_2_1_network_connection_closed_after_malformed_packet() {
    let o = inject_after_subscribe(&surrogate_topic_publish(), "close-after-malformed").await;
    assert!(
        o.obs.closed,
        "MQTT-4.13.2-1 / §4.13.1 VIOLATION: after detecting a Malformed Packet (reason 0x81) the client marked itself disconnected (is_connected={}) but left the TCP connection open for 3s without sending DISCONNECT (rc {:?}); socket stays half-open until a reconnect or disconnect() runs",
        o.still_connected,
        o.obs.disconnect_rc
    );
}

#[tokio::test]
async fn mqtt_3_2_2_1_connack_reserved_bits_rejected() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let o = opts("connack-flags");
    let client = MqttClient::with_options(o.clone());
    let c = client.clone();
    let h = tokio::spawn(async move { Box::pin(c.connect_with_options(&url, o)).await });
    let (mut stream, _) = timeout(T, listener.accept()).await.unwrap().unwrap();
    read_raw(&mut stream).await.unwrap();
    stream
        .write_all(&[0x20, 0x03, 0x02, 0x00, 0x00])
        .await
        .unwrap();
    let r = timeout(T, h).await.unwrap().unwrap();
    let obs = observe_close(&mut stream, Duration::from_secs(2)).await;
    assert!(
        r.is_err() && obs.closed,
        "MQTT-3.2.2-1 VIOLATION: CONNACK with reserved flag bit 1 set accepted: {r:?}, closed {}",
        obs.closed
    );
}

#[tokio::test]
async fn mqtt_2_2_1_2_and_2_2_2_1_qos0_publish_has_no_packet_id_and_zero_property_length() {
    let mut s = start(opts("qos0"), false, &[]).await;
    s.result.as_ref().unwrap();
    s.client.publish("t", b"p".to_vec()).await.unwrap();
    let raw = next_of_type(&mut s.stream, PUBLISH, T).await.unwrap();
    assert_eq!(
        raw.body,
        vec![0x00, 0x01, b't', 0x00, b'p'],
        "MQTT-2.2.1-2 / MQTT-2.2.2-1 VIOLATION: QoS0 PUBLISH body must be topic, property length 0, payload (no packet id)"
    );
    assert_eq!(raw.flags(), 0, "MQTT-2.1.3-1: QoS0 PUBLISH flags");
}

#[tokio::test]
async fn mqtt_1_5_5_1_remaining_length_minimal_encoding() {
    let mut s = start(opts("varint"), false, &[]).await;
    s.result.as_ref().unwrap();
    for size in [100usize, 125, 126, 200, 16_380, 20_000] {
        s.client.publish("v", vec![0u8; size]).await.unwrap();
        let raw = next_of_type(&mut s.stream, PUBLISH, T).await.unwrap();
        assert_eq!(
            raw.remaining_len_bytes,
            varint(raw.body.len()),
            "MQTT-1.5.5-1 VIOLATION: non-minimal Remaining Length encoding"
        );
    }
}

#[tokio::test]
async fn mqtt_2_2_1_5_acks_echo_inbound_packet_id() {
    let mut s = start(opts("ack-ids"), false, &[]).await;
    s.result.as_ref().unwrap();
    s.stream
        .write_all(&publish_bytes(0x32, b"a", Some(0x1234), &[], b"x"))
        .await
        .unwrap();
    let puback = next_of_type(&mut s.stream, PUBACK, T).await.unwrap();
    assert_eq!(puback.flags(), 0, "MQTT-2.1.3-1 PUBACK flags");
    assert_eq!(&puback.body[0..2], &[0x12, 0x34], "MQTT-2.2.1-5 PUBACK id");
    s.stream
        .write_all(&publish_bytes(0x34, b"a", Some(0x4321), &[], b"x"))
        .await
        .unwrap();
    let pubrec = next_of_type(&mut s.stream, PUBREC, T).await.unwrap();
    assert_eq!(pubrec.flags(), 0, "MQTT-2.1.3-1 PUBREC flags");
    assert_eq!(&pubrec.body[0..2], &[0x43, 0x21], "MQTT-2.2.1-5 PUBREC id");
    s.stream
        .write_all(&packet(0x62, &0x4321u16.to_be_bytes()))
        .await
        .unwrap();
    let pubcomp = next_of_type(&mut s.stream, PUBCOMP, T).await.unwrap();
    assert_eq!(pubcomp.flags(), 0, "MQTT-2.1.3-1 PUBCOMP flags");
    assert_eq!(
        &pubcomp.body[0..2],
        &[0x43, 0x21],
        "MQTT-2.2.1-5 PUBCOMP id"
    );
}

#[tokio::test]
async fn mqtt_2_1_3_1_outbound_reserved_flags_and_2_2_1_3_nonzero_ids() {
    let mut s = start(opts("out-flags"), false, &[]).await;
    s.result.as_ref().unwrap();
    let cl = s.client.clone();
    let task = tokio::spawn(async move { cl.subscribe("s/t", |_| {}).await });
    let sub = next_of_type(&mut s.stream, SUBSCRIBE, T).await.unwrap();
    assert_eq!(sub.flags(), 0x02, "MQTT-2.1.3-1 SUBSCRIBE flags");
    assert_ne!(&sub.body[0..2], &[0, 0], "MQTT-2.2.1-3 zero SUBSCRIBE id");
    let mut body = sub.body[0..2].to_vec();
    body.extend([0x00, 0x00]);
    s.stream.write_all(&packet(0x90, &body)).await.unwrap();
    timeout(T, task).await.unwrap().unwrap().unwrap();

    let cl = s.client.clone();
    let task = tokio::spawn(async move { cl.unsubscribe("s/t").await });
    let unsub = next_of_type(&mut s.stream, UNSUBSCRIBE, T).await.unwrap();
    assert_eq!(unsub.flags(), 0x02, "MQTT-2.1.3-1 UNSUBSCRIBE flags");
    assert_ne!(
        &unsub.body[0..2],
        &[0, 0],
        "MQTT-2.2.1-3 zero UNSUBSCRIBE id"
    );
    let mut body = unsub.body[0..2].to_vec();
    body.extend([0x00, 0x00]);
    s.stream.write_all(&packet(0xB0, &body)).await.unwrap();
    timeout(T, task).await.unwrap().unwrap().unwrap();

    let cl = s.client.clone();
    let task =
        tokio::spawn(async move { cl.publish_qos("q2", b"x".to_vec(), QoS::ExactlyOnce).await });
    let publish = next_of_type(&mut s.stream, PUBLISH, T).await.unwrap();
    let pid = parse_publish(&publish).packet_id.unwrap();
    assert_ne!(pid, 0, "MQTT-2.2.1-3 zero PUBLISH id");
    s.stream
        .write_all(&packet(0x50, &pid.to_be_bytes()))
        .await
        .unwrap();
    let rel = next_of_type(&mut s.stream, PUBREL, T).await.unwrap();
    assert_eq!(rel.flags(), 0x02, "MQTT-2.1.3-1 PUBREL flags");
    assert_eq!(
        &rel.body[0..2],
        &pid.to_be_bytes(),
        "MQTT-2.2.1-5 PUBREL id"
    );
    s.stream
        .write_all(&packet(0x70, &pid.to_be_bytes()))
        .await
        .unwrap();
    timeout(T, task).await.unwrap().unwrap().unwrap();

    s.client.disconnect().await.unwrap();
    let disc = next_of_type(&mut s.stream, DISCONNECT, T).await.unwrap();
    assert_eq!(disc.flags(), 0, "MQTT-2.1.3-1 DISCONNECT flags");
}

#[tokio::test]
async fn mqtt_3_1_connect_flags_payload_order_and_will() {
    let will = mqtt5::WillMessage::new("will/t", b"bye".to_vec()).with_qos(QoS::AtLeastOnce);
    let o = opts("cid-1")
        .with_will(will)
        .with_credentials("user", b"pass");
    let s = start(o, false, &[]).await;
    let c = parse_connect(&s.connect);
    assert_eq!(c.flags & 0x01, 0, "MQTT-3.1.2-3 reserved CONNECT flag");
    assert_eq!(c.flags & 0x04, 0x04, "will flag");
    assert!(
        c.will_props.is_some(),
        "MQTT-3.1.2-9 will properties present"
    );
    assert_eq!(
        c.will_topic.as_deref(),
        Some(&b"will/t"[..]),
        "MQTT-3.1.2-9 will topic"
    );
    assert_eq!(
        c.will_payload.as_deref(),
        Some(&b"bye"[..]),
        "MQTT-3.1.2-9 will payload"
    );
    assert_eq!(
        c.username.as_deref(),
        Some(&b"user"[..]),
        "MQTT-3.1.2-17 / 3.1.3-1 username"
    );
    assert_eq!(
        c.password.as_deref(),
        Some(&b"pass"[..]),
        "MQTT-3.1.2-19 / 3.1.3-1 password"
    );
    assert_eq!(c.client_id, b"cid-1", "MQTT-3.1.3-1 client id first");
    assert_eq!(
        c.trailing, 0,
        "MQTT-3.1.3-1 no trailing bytes after declared fields"
    );
    assert_eq!(c.keep_alive, 60);

    let s2 = start(opts("cid-2"), false, &[]).await;
    let c2 = parse_connect(&s2.connect);
    assert_eq!(
        c2.flags & 0x3C,
        0,
        "MQTT-3.1.2-13 will QoS/retain must be 0 without will"
    );
    assert_eq!(c2.flags & 0xC0, 0, "MQTT-3.1.2-16/18 no user/pass flags");
    assert_eq!(c2.trailing, 0, "MQTT-3.1.2-16/18 no user/pass present");
    assert!(
        c2.props.is_empty(),
        "MQTT-2.2.2-1: CONNECT without properties"
    );
    assert_eq!(s2.connect.body[10], 0x00, "MQTT-2.2.2-1: Property Length 0");
}

#[tokio::test]
async fn mqtt_1_5_4_2_null_character_never_encoded() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let options = opts("nul").with_credentials("us\0er", b"p");
    let client = MqttClient::with_options(options.clone());
    let cl = client.clone();
    let task = tokio::spawn(async move { Box::pin(cl.connect_with_options(&url, options)).await });
    let (mut stream, _) = timeout(T, listener.accept()).await.unwrap().unwrap();
    let got = timeout(Duration::from_secs(2), read_raw(&mut stream)).await;
    let outcome = timeout(T, task).await;
    assert!(
        !matches!(got, Ok(Some(ref raw)) if raw.body.windows(5).any(|w| w == b"us\0er")),
        "MQTT-1.5.4-2 VIOLATION: CONNECT User Name carries U+0000 (connect {outcome:?})"
    );

    let mut s = start(opts("nul-topic"), false, &[]).await;
    s.result.as_ref().unwrap();
    let outcome = s.client.publish("a\0b", b"x".to_vec()).await;
    let sent = next_of_type(&mut s.stream, PUBLISH, Duration::from_millis(500)).await;
    assert!(
        sent.is_none(),
        "MQTT-1.5.4-2 VIOLATION: PUBLISH topic with U+0000 sent ({outcome:?})"
    );
}

#[tokio::test]
async fn mqtt_3_1_2_30_only_auth_before_connack_when_auth_method_set() {
    let mut o = opts("auth-seq");
    o.properties.authentication_method = Some("TEST".to_string());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let client = MqttClient::with_options(o.clone());
    client.set_auth_handler(StaticAuth).await;
    let c = client.clone();
    let h = tokio::spawn(async move { Box::pin(c.connect_with_options(&url, o)).await });
    let (mut stream, _) = timeout(T, listener.accept()).await.unwrap().unwrap();
    let connect = read_raw(&mut stream).await.unwrap();
    let cp = parse_connect(&connect);
    assert!(has_prop(&cp.props, P_AUTH_METHOD));
    let spam = client.clone();
    let spammer = tokio::spawn(async move {
        for _ in 0..50 {
            let _ = spam.publish("x", b"y".to_vec()).await;
            let _ = spam.subscribe("z", |_| {}).await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    stream.write_all(&auth_packet(0x18, &[])).await.unwrap();
    let mut before_connack = Vec::new();
    while let Next::Packet(r) = next_packet(&mut stream, Duration::from_millis(500)).await {
        before_connack.push(r.ptype());
    }
    spammer.abort();
    stream
        .write_all(&connack(false, &p_str(P_AUTH_METHOD, b"TEST")))
        .await
        .unwrap();
    let _ = timeout(T, h).await;
    assert!(
        before_connack.iter().all(|t| *t == AUTH || *t == DISCONNECT),
        "MQTT-3.1.2-30 VIOLATION: packets other than AUTH/DISCONNECT sent before CONNACK: {before_connack:?}"
    );
    assert!(
        before_connack.contains(&AUTH),
        "client did not answer AUTH challenge"
    );
}

#[tokio::test]
async fn json_mqtt_3_2_2_21_wildcard_subscription_available_0() {
    let mut s = start(opts("wsa0"), false, &p_u8(P_WILDCARD_AVAILABLE, 0)).await;
    s.result.as_ref().unwrap();
    let c = s.client.clone();
    let h = tokio::spawn(async move { c.subscribe("a/#", |_| {}).await });
    let sent = next_of_type(&mut s.stream, SUBSCRIBE, Duration::from_secs(1)).await;
    h.abort();
    assert!(
        sent.is_none(),
        "manifest MQTT-3.2.2-21 text (Wildcard Subscription Available=0) VIOLATION: client sent SUBSCRIBE with wildcard filter"
    );
}

#[tokio::test]
async fn resume_existing_session_opt_in_accepts_broker_held_session() {
    let o = opts("sp-opt-in")
        .with_clean_start(false)
        .with_session_expiry_interval(300)
        .with_resume_existing_session(true);
    let mut s = start(o, true, &[]).await;
    let obs = observe_close(&mut s.stream, Duration::from_millis(500)).await;
    assert!(
        s.result.as_ref().is_ok_and(|r| r.session_present) && !obs.closed,
        "resume_existing_session(true) must accept Session Present=1 for a fresh Clean Start=0 client: connect result {:?}, network closed={}",
        s.result,
        obs.closed
    );
}

#[tokio::test]
async fn resume_existing_session_opt_in_does_not_cover_clean_start_1() {
    let mut s = start(
        opts("sp-opt-in-clean").with_resume_existing_session(true),
        true,
        &[],
    )
    .await;
    let obs = observe_close(&mut s.stream, Duration::from_secs(2)).await;
    assert!(
        s.result.is_err() && obs.closed,
        "MQTT-3.2.2-4 VIOLATION: Clean Start=1 accepted Session Present=1 despite the opt-in: connect result {:?}, network closed={}",
        s.result,
        obs.closed
    );
}

#[tokio::test]
async fn topic_alias_not_carried_into_resumed_session_replay() {
    let o = reconnecting_opts("alias-replay")
        .with_clean_start(false)
        .with_session_expiry_interval(300);
    let mut s = start(o, false, &p_u16(P_TOPIC_ALIAS_MAXIMUM, 5)).await;
    s.result.as_ref().unwrap();
    s.client
        .publish_with_options("alias/t", b"map".to_vec(), with_alias(1))
        .await
        .unwrap();
    let mapped = parse_publish(&next_of_type(&mut s.stream, PUBLISH, T).await.unwrap());
    assert_eq!(mapped.topic, b"alias/t");
    let c = s.client.clone();
    let _h = tokio::spawn(async move {
        let options = PublishOptions {
            qos: QoS::AtLeastOnce,
            ..with_alias(1)
        };
        c.publish_with_options("", b"aliased".to_vec(), options)
            .await
    });
    let orig = parse_publish(&next_of_type(&mut s.stream, PUBLISH, T).await.unwrap());
    assert!(orig.topic.is_empty(), "setup: live PUBLISH uses the alias");
    let Setup {
        client,
        listener,
        stream,
        ..
    } = s;
    drop(stream);
    wait_connected(&client, false).await;
    let (mut stream2, _) = accept_next(&listener, true, &p_u16(P_TOPIC_ALIAS_MAXIMUM, 5)).await;
    let resent = parse_publish(
        &next_of_type(&mut stream2, PUBLISH, Duration::from_secs(3))
            .await
            .expect("unacked PUBLISH not resent"),
    );
    assert_eq!(resent.packet_id, orig.packet_id);
    assert_eq!(
        resent.topic, b"alias/t",
        "replayed PUBLISH must carry the full Topic Name; the alias mapping belongs to the previous connection"
    );
    assert!(
        !has_prop(&resent.props, P_TOPIC_ALIAS),
        "MQTT-3.3.2-7 VIOLATION: replayed PUBLISH reused a Topic Alias never mapped on the new connection"
    );
}

#[tokio::test]
async fn topic_alias_not_carried_into_offline_queue_flush() {
    let options = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..with_alias(1)
    };
    let p =
        queued_publish_then_reconnect(&p_u16(P_TOPIC_ALIAS_MAXIMUM, 5), options, "alias-queued")
            .await
            .expect("queued message was never sent");
    assert!(
        !has_prop(&p.props, P_TOPIC_ALIAS),
        "MQTT-3.3.2-7 VIOLATION: queued PUBLISH carried a Topic Alias created before the connection it was flushed on"
    );
}
