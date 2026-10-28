use mqtt5::{
    ConnectOptions, ConnectResult, MqttClient, MqttError, PublishOptions, PublishProperties, QoS,
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

const T: Duration = Duration::from_secs(5);

const CONNECT: u8 = 1;
const PUBLISH: u8 = 3;
const SUBSCRIBE: u8 = 8;
const PINGREQ: u8 = 12;

const P_AUTH_METHOD: u8 = 0x15;
const P_AUTH_DATA: u8 = 0x16;
const P_REQUEST_PROBLEM_INFO: u8 = 0x17;
const P_RECEIVE_MAXIMUM: u8 = 0x21;
const P_TOPIC_ALIAS_MAXIMUM: u8 = 0x22;
const P_TOPIC_ALIAS: u8 = 0x23;
const P_MAXIMUM_QOS: u8 = 0x24;
const P_RETAIN_AVAILABLE: u8 = 0x25;
const P_MAXIMUM_PACKET_SIZE: u8 = 0x27;
const P_WILDCARD_AVAILABLE: u8 = 0x28;

#[derive(Debug, Clone)]
struct Raw {
    first: u8,
    body: Vec<u8>,
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

fn connect_properties(raw: &Raw) -> Vec<(u8, Vec<u8>)> {
    let b = &raw.body;
    let mut pos = 0;
    let name = read_bin(b, &mut pos);
    assert_eq!(name, b"MQTT");
    assert_eq!(b[pos], 5);
    pos += 4;
    parse_props(b, &mut pos)
}

#[derive(Debug)]
struct ParsedPublish {
    qos: u8,
    retain: bool,
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
        topic,
        packet_id,
        props,
        payload: b[pos..].to_vec(),
    }
}

async fn read_raw(s: &mut TcpStream) -> Option<Raw> {
    let mut first = [0u8; 1];
    s.read_exact(&mut first).await.ok()?;
    let mut mult = 1usize;
    let mut len = 0usize;
    loop {
        let mut b = [0u8; 1];
        s.read_exact(&mut b).await.ok()?;
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

async fn closes_within(s: &mut TcpStream, d: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + d;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match next_non_ping(s, left).await {
            Next::Packet(_) => {}
            Next::Closed => return true,
            Next::Silent => return false,
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

fn with_alias(alias: u16) -> PublishOptions {
    PublishOptions {
        properties: PublishProperties {
            topic_alias: Some(alias),
            ..Default::default()
        },
        ..Default::default()
    }
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
async fn maximum_qos_not_stale_after_reconnect() {
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
async fn maximum_packet_size_not_stale_after_reconnect() {
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
async fn retain_available_not_stale_after_reconnect() {
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
async fn receive_maximum_not_stale_after_reconnect() {
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

fn rpi0_opts(id: &str) -> ConnectOptions {
    let mut o = opts(id);
    o.properties.request_problem_information = Some(false);
    o
}

#[tokio::test]
async fn request_problem_information_0_is_sent() {
    let s = start(rpi0_opts("rpi-wire"), false, &[]).await;
    let props = connect_properties(&s.connect);
    assert!(
        props.iter().any(|(id, v)| *id == P_REQUEST_PROBLEM_INFO && v == &[0]),
        "ConnectProperties.request_problem_information=Some(false) was not put on the wire; CONNECT props {props:?}"
    );
}

#[tokio::test]
async fn auth_data_without_auth_method_never_sent() {
    let s = start(
        opts("authdata").with_authentication_data(b"secret"),
        false,
        &[],
    )
    .await;
    let props = connect_properties(&s.connect);
    assert!(
        !has_prop(&props, P_AUTH_DATA) || has_prop(&props, P_AUTH_METHOD),
        "§3.1.2.11.10: CONNECT carries Authentication Data without Authentication Method: {props:?}"
    );
}

#[tokio::test]
async fn oversized_publish_rejected_and_packet_id_not_leaked() {
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
async fn wildcard_subscription_available_0_blocks_wildcard_subscribe() {
    let mut s = start(opts("wsa0"), false, &p_u8(P_WILDCARD_AVAILABLE, 0)).await;
    s.result.as_ref().unwrap();
    let c = s.client.clone();
    let h = tokio::spawn(async move { c.subscribe("a/#", |_| {}).await });
    let sent = next_of_type(&mut s.stream, SUBSCRIBE, Duration::from_secs(1)).await;
    h.abort();
    assert!(
        sent.is_none(),
        "client sent SUBSCRIBE with a wildcard filter after CONNACK Wildcard Subscription Available=0"
    );
}

#[tokio::test]
async fn resume_existing_session_opt_in_accepts_broker_held_session() {
    let o = opts("sp-opt-in")
        .with_clean_start(false)
        .with_session_expiry_interval(300)
        .with_resume_existing_session(true);
    let mut s = start(o, true, &[]).await;
    let closed = closes_within(&mut s.stream, Duration::from_millis(500)).await;
    assert!(
        s.result.as_ref().is_ok_and(|r| r.session_present) && !closed,
        "resume_existing_session(true) must accept Session Present=1 for a fresh Clean Start=0 client: connect result {:?}, network closed={closed}",
        s.result
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
    let closed = closes_within(&mut s.stream, Duration::from_secs(2)).await;
    assert!(
        s.result.is_err() && closed,
        "MQTT-3.2.2-4 VIOLATION: Clean Start=1 accepted Session Present=1 despite the opt-in: connect result {:?}, network closed={closed}",
        s.result
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
        "replayed PUBLISH reused a Topic Alias never mapped on the new connection"
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
        "queued PUBLISH carried a Topic Alias created before the connection it was flushed on"
    );
}
