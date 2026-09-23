use mqtt5::{AuthHandler, AuthResponse, ConnectOptions, MqttClient};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{timeout, Instant};

const CONNECT: u8 = 1;
const PUBLISH: u8 = 3;
const PUBACK: u8 = 4;
const PUBREC: u8 = 5;
const PUBCOMP: u8 = 7;
const SUBSCRIBE: u8 = 8;
const UNSUBSCRIBE: u8 = 10;
const PINGREQ: u8 = 12;
const AUTH: u8 = 15;

const PROP_SERVER_KEEP_ALIVE: u8 = 0x13;
const PROP_AUTH_METHOD: u8 = 0x15;
const PROP_AUTH_DATA: u8 = 0x16;
const PROP_RECEIVE_MAXIMUM: u8 = 0x21;

#[derive(Debug, Clone)]
struct Pkt {
    kind: u8,
    flags: u8,
    body: Vec<u8>,
}

enum Read {
    Packet(Pkt),
    Closed,
    TimedOut,
}

fn varint(mut n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut byte = u8::try_from(n % 128).unwrap_or(0);
        n /= 128;
        if n > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if n == 0 {
            return out;
        }
    }
}

fn frame(first: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![first];
    out.extend(varint(body.len()));
    out.extend_from_slice(body);
    out
}

fn mqtt_str(s: &[u8]) -> Vec<u8> {
    let len = u16::try_from(s.len()).unwrap_or(u16::MAX);
    let mut out = len.to_be_bytes().to_vec();
    out.extend_from_slice(s);
    out
}

fn prop_u16(id: u8, v: u16) -> Vec<u8> {
    let mut out = vec![id];
    out.extend(v.to_be_bytes());
    out
}

fn prop_str(id: u8, s: &str) -> Vec<u8> {
    let mut out = vec![id];
    out.extend(mqtt_str(s.as_bytes()));
    out
}

fn with_props(fixed: &[u8], props: &[u8]) -> Vec<u8> {
    let mut body = fixed.to_vec();
    body.extend(varint(props.len()));
    body.extend_from_slice(props);
    body
}

fn connack(session_present: bool, reason: u8, props: &[u8]) -> Vec<u8> {
    frame(
        0x20,
        &with_props(&[u8::from(session_present), reason], props),
    )
}

fn auth(reason: u8, props: &[u8]) -> Vec<u8> {
    frame(0xF0, &with_props(&[reason], props))
}

fn disconnect(reason: u8) -> Vec<u8> {
    frame(0xE0, &[reason, 0])
}

fn puback(pid: u16) -> Vec<u8> {
    frame(0x40, &pid.to_be_bytes())
}

fn pubrel(pid: u16) -> Vec<u8> {
    frame(0x62, &pid.to_be_bytes())
}

fn publish(topic: &str, qos: u8, pid: u16, payload: &[u8]) -> Vec<u8> {
    let mut body = mqtt_str(topic.as_bytes());
    if qos > 0 {
        body.extend(pid.to_be_bytes());
    }
    body.push(0);
    body.extend_from_slice(payload);
    frame(0x30 | (qos << 1), &body)
}

fn suback(pid: u16, count: usize) -> Vec<u8> {
    let mut body = pid.to_be_bytes().to_vec();
    body.push(0);
    body.extend(std::iter::repeat_n(0u8, count));
    frame(0x90, &body)
}

fn unsuback(pid: u16, count: usize) -> Vec<u8> {
    let mut body = pid.to_be_bytes().to_vec();
    body.push(0);
    body.extend(std::iter::repeat_n(0u8, count));
    frame(0xB0, &body)
}

fn take_varint(b: &[u8], pos: &mut usize) -> Option<usize> {
    let mut value = 0usize;
    let mut mult = 1usize;
    loop {
        let byte = *b.get(*pos)?;
        *pos += 1;
        value += usize::from(byte & 0x7f) * mult;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        mult *= 128;
        if mult > 128 * 128 * 128 {
            return None;
        }
    }
}

fn take_str(b: &[u8], pos: &mut usize) -> Option<Vec<u8>> {
    let len = usize::from(u16::from_be_bytes([*b.get(*pos)?, *b.get(*pos + 1)?]));
    let start = *pos + 2;
    let out = b.get(start..start + len)?.to_vec();
    *pos = start + len;
    Some(out)
}

fn take_u16(b: &[u8], pos: &mut usize) -> Option<u16> {
    let v = u16::from_be_bytes([*b.get(*pos)?, *b.get(*pos + 1)?]);
    *pos += 2;
    Some(v)
}

fn parse_props(b: &[u8], pos: &mut usize) -> Option<Vec<(u8, Vec<u8>)>> {
    let len = take_varint(b, pos)?;
    let end = *pos + len;
    let mut out = Vec::new();
    while *pos < end {
        let id = *b.get(*pos)?;
        *pos += 1;
        let value = match id {
            0x01 | 0x17 | 0x19 | 0x24 | 0x25 | 0x28 | 0x29 | 0x2A => {
                let v = vec![*b.get(*pos)?];
                *pos += 1;
                v
            }
            0x13 | 0x21 | 0x22 | 0x23 => {
                let v = b.get(*pos..*pos + 2)?.to_vec();
                *pos += 2;
                v
            }
            0x02 | 0x11 | 0x18 | 0x27 => {
                let v = b.get(*pos..*pos + 4)?.to_vec();
                *pos += 4;
                v
            }
            0x0B => {
                let v = take_varint(b, pos)?;
                v.to_be_bytes().to_vec()
            }
            0x03 | 0x08 | 0x12 | 0x15 | 0x1A | 0x1C | 0x1F | 0x09 | 0x16 => take_str(b, pos)?,
            0x26 => {
                let mut k = take_str(b, pos)?;
                k.push(b'=');
                k.extend(take_str(b, pos)?);
                k
            }
            _ => return None,
        };
        out.push((id, value));
    }
    Some(out)
}

fn prop_string(props: &[(u8, Vec<u8>)], id: u8) -> Option<String> {
    props
        .iter()
        .find(|(pid, _)| *pid == id)
        .map(|(_, v)| String::from_utf8_lossy(v).into_owned())
}

struct ConnectInfo {
    clean_start: bool,
    auth_method: Option<String>,
}

fn parse_connect(p: &Pkt) -> Option<ConnectInfo> {
    let b = &p.body;
    let mut pos = 0;
    take_str(b, &mut pos)?;
    pos += 1;
    let flags = *b.get(pos)?;
    pos += 1;
    take_u16(b, &mut pos)?;
    let props = parse_props(b, &mut pos)?;
    Some(ConnectInfo {
        clean_start: flags & 0x02 != 0,
        auth_method: prop_string(&props, PROP_AUTH_METHOD),
    })
}

struct PublishInfo {
    topic: String,
    qos: u8,
    dup: bool,
    pid: Option<u16>,
}

fn parse_publish(p: &Pkt) -> Option<PublishInfo> {
    let b = &p.body;
    let mut pos = 0;
    let topic = String::from_utf8_lossy(&take_str(b, &mut pos)?).into_owned();
    let qos = (p.flags >> 1) & 0x03;
    let pid = if qos > 0 {
        Some(take_u16(b, &mut pos)?)
    } else {
        None
    };
    Some(PublishInfo {
        topic,
        qos,
        dup: p.flags & 0x08 != 0,
        pid,
    })
}

struct AuthInfo {
    reason: u8,
    method: Option<String>,
}

fn parse_auth(p: &Pkt) -> AuthInfo {
    if p.body.is_empty() {
        return AuthInfo {
            reason: 0,
            method: None,
        };
    }
    let mut pos = 1;
    let method =
        parse_props(&p.body, &mut pos).and_then(|props| prop_string(&props, PROP_AUTH_METHOD));
    AuthInfo {
        reason: p.body[0],
        method,
    }
}

fn parse_filters(p: &Pkt, with_options: bool) -> Option<(u16, Vec<String>)> {
    let b = &p.body;
    let mut pos = 0;
    let pid = take_u16(b, &mut pos)?;
    parse_props(b, &mut pos)?;
    let mut filters = Vec::new();
    while pos < b.len() {
        filters.push(String::from_utf8_lossy(&take_str(b, &mut pos)?).into_owned());
        if with_options {
            pos += 1;
        }
    }
    Some((pid, filters))
}

fn packet_id(p: &Pkt) -> Option<u16> {
    let mut pos = 0;
    take_u16(&p.body, &mut pos)
}

fn try_split_packet(buf: &mut Vec<u8>) -> Option<Pkt> {
    let first = *buf.first()?;
    let mut pos = 1;
    let len = take_varint(buf, &mut pos)?;
    if buf.len() < pos + len {
        return None;
    }
    let body = buf[pos..pos + len].to_vec();
    buf.drain(..pos + len);
    Some(Pkt {
        kind: first >> 4,
        flags: first & 0x0f,
        body,
    })
}

struct Peer {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl Peer {
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            buf: Vec::new(),
        }
    }

    async fn read(&mut self, wait: Duration) -> Read {
        let deadline = Instant::now() + wait;
        loop {
            if let Some(p) = try_split_packet(&mut self.buf) {
                return Read::Packet(p);
            }
            let mut chunk = [0u8; 4096];
            match tokio::time::timeout_at(deadline, self.stream.read(&mut chunk)).await {
                Err(_) => return Read::TimedOut,
                Ok(Ok(0) | Err(_)) => return Read::Closed,
                Ok(Ok(n)) => self.buf.extend_from_slice(&chunk[..n]),
            }
        }
    }

    async fn expect(&mut self, wait: Duration) -> Option<Pkt> {
        match self.read(wait).await {
            Read::Packet(p) => Some(p),
            Read::Closed | Read::TimedOut => None,
        }
    }

    async fn send(&mut self, bytes: &[u8]) {
        let _ = self.stream.write_all(bytes).await;
        let _ = self.stream.flush().await;
    }

    async fn closed_within(&mut self, wait: Duration) -> (bool, Vec<u8>) {
        let deadline = Instant::now() + wait;
        let mut seen = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.read(remaining).await {
                Read::Packet(p) => seen.push(p.kind),
                Read::Closed => return (true, seen),
                Read::TimedOut => return (false, seen),
            }
        }
    }
}

async fn listener() -> (TcpListener, SocketAddr) {
    let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let a = l.local_addr().expect("addr");
    (l, a)
}

async fn accept_connect(l: &TcpListener, wait: Duration) -> Option<(Peer, ConnectInfo)> {
    let (stream, _) = timeout(wait, l.accept()).await.ok()?.ok()?;
    let mut peer = Peer::new(stream);
    let p = peer.expect(wait).await?;
    if p.kind != CONNECT {
        return None;
    }
    let info = parse_connect(&p)?;
    Some((peer, info))
}

fn opts(id: &str) -> ConnectOptions {
    ConnectOptions::new(id).with_automatic_reconnect(false)
}

fn persistent_opts(id: &str) -> ConnectOptions {
    ConnectOptions::new(id)
        .with_clean_start(false)
        .with_session_expiry_interval(3600)
        .with_automatic_reconnect(true)
        .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(500))
}

const W: Duration = Duration::from_secs(3);

#[derive(Default)]
struct Recorded {
    publishes: Vec<String>,
    subscribes: Vec<String>,
    unsubscribes: Vec<String>,
}

async fn recorder_server() -> (SocketAddr, Arc<Mutex<Recorded>>) {
    let (l, addr) = listener().await;
    let rec = Arc::new(Mutex::new(Recorded::default()));
    let rec2 = Arc::clone(&rec);
    tokio::spawn(async move {
        let Some((mut peer, _)) = accept_connect(&l, Duration::from_secs(10)).await else {
            return;
        };
        peer.send(&connack(false, 0, &[])).await;
        while let Read::Packet(p) = peer.read(Duration::from_secs(60)).await {
            match p.kind {
                PUBLISH => {
                    if let Some(info) = parse_publish(&p) {
                        rec2.lock().expect("lock").publishes.push(info.topic);
                        if let (1, Some(pid)) = (info.qos, info.pid) {
                            peer.send(&puback(pid)).await;
                        }
                    }
                }
                SUBSCRIBE => {
                    if let Some((pid, filters)) = parse_filters(&p, true) {
                        let n = filters.len();
                        rec2.lock().expect("lock").subscribes.extend(filters);
                        peer.send(&suback(pid, n)).await;
                    }
                }
                UNSUBSCRIBE => {
                    if let Some((pid, filters)) = parse_filters(&p, false) {
                        let n = filters.len();
                        rec2.lock().expect("lock").unsubscribes.extend(filters);
                        peer.send(&unsuback(pid, n)).await;
                    }
                }
                PINGREQ => peer.send(&[0xD0, 0x00]).await,
                _ => {}
            }
        }
    });
    (addr, rec)
}

async fn recorder_client(id: &str) -> (MqttClient, Arc<Mutex<Recorded>>) {
    let (addr, rec) = recorder_server().await;
    let client = MqttClient::with_options(opts(id));
    client
        .connect(&format!("mqtt://{addr}"))
        .await
        .expect("connect to recorder");
    (client, rec)
}

async fn publish_cases(
    client: &MqttClient,
    rec: &Arc<Mutex<Recorded>>,
    topics: &[String],
) -> Vec<String> {
    for t in topics {
        let _ = timeout(W, client.publish(t.clone(), b"x".to_vec())).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let r = rec.lock().expect("lock");
    topics
        .iter()
        .filter(|t| r.publishes.contains(t))
        .map(|t| format!("{:?}", preview(t)))
        .collect()
}

async fn subscribe_cases(
    client: &MqttClient,
    rec: &Arc<Mutex<Recorded>>,
    filters: &[String],
) -> Vec<String> {
    for f in filters {
        let _ = timeout(W, client.subscribe(f.clone(), |_| {})).await;
        let _ = timeout(W, client.unsubscribe(f.clone())).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let r = rec.lock().expect("lock");
    filters
        .iter()
        .filter(|f| r.subscribes.contains(f) || r.unsubscribes.contains(f))
        .map(|f| {
            let which = match (r.subscribes.contains(f), r.unsubscribes.contains(f)) {
                (true, true) => "SUBSCRIBE+UNSUBSCRIBE",
                (true, false) => "SUBSCRIBE",
                _ => "UNSUBSCRIBE",
            };
            format!("{:?} via {which}", preview(f))
        })
        .collect()
}

fn preview(s: &str) -> String {
    if s.len() > 40 {
        format!("<{} bytes>", s.len())
    } else {
        s.to_string()
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_7_0_1_client_must_not_send_wildcards_in_topic_name() {
    let (client, rec) = recorder_client("d-4701").await;
    let sent = publish_cases(
        &client,
        &rec,
        &strings(&["a/+", "a/#", "+", "#", "sport+", "a/b#"]),
    )
    .await;
    assert!(
        sent.is_empty(),
        "MQTT-4.7.0-1 VIOLATION: client put wildcard Topic Names on the wire in PUBLISH: {sent:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_7_1_1_client_must_not_send_misplaced_multilevel_wildcard() {
    let (client, rec) = recorder_client("d-4711").await;
    let sent = subscribe_cases(
        &client,
        &rec,
        &strings(&["a/#/b", "a#", "#/a", "a/b#", "##"]),
    )
    .await;
    assert!(
        sent.is_empty(),
        "MQTT-4.7.1-1 VIOLATION: client sent Topic Filters with misplaced '#': {sent:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_7_1_2_client_must_not_send_partial_level_single_wildcard() {
    let (client, rec) = recorder_client("d-4712").await;
    let sent = subscribe_cases(
        &client,
        &rec,
        &strings(&["a+", "a/+b", "+a/b", "a/b+/c", "++"]),
    )
    .await;
    assert!(
        sent.is_empty(),
        "MQTT-4.7.1-2 VIOLATION: client sent Topic Filters where '+' does not occupy a whole level: {sent:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_7_3_1_client_must_not_send_empty_topic_or_filter() {
    let (client, rec) = recorder_client("d-4731").await;
    let empty = vec![String::new()];
    let published = publish_cases(&client, &rec, &empty).await;
    let subscribed = subscribe_cases(&client, &rec, &empty).await;
    assert!(
        published.is_empty() && subscribed.is_empty(),
        "MQTT-4.7.3-1 VIOLATION: client sent zero-length Topic Name (PUBLISH, no Topic Alias) {published:?} / Topic Filter {subscribed:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_7_3_2_client_must_not_send_null_character() {
    let (client, rec) = recorder_client("d-4732").await;
    let cases = strings(&["a\0b", "\0"]);
    let published = publish_cases(&client, &rec, &cases).await;
    let subscribed = subscribe_cases(&client, &rec, &cases).await;
    assert!(
        published.is_empty() && subscribed.is_empty(),
        "MQTT-4.7.3-2 VIOLATION: client sent U+0000 in Topic Name {published:?} / Topic Filter {subscribed:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_7_3_3_client_must_not_send_topic_over_65535_bytes() {
    let (client, rec) = recorder_client("d-4733").await;
    let cases = vec!["a".repeat(65_536), "é".repeat(32_768)];
    let published = publish_cases(&client, &rec, &cases).await;
    let subscribed = subscribe_cases(&client, &rec, &cases).await;
    assert!(
        published.is_empty() && subscribed.is_empty(),
        "MQTT-4.7.3-3 VIOLATION: client sent >65535-byte Topic Name {published:?} / Topic Filter {subscribed:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_8_2_1_client_must_not_send_share_without_sharename_or_filter() {
    let (client, rec) = recorder_client("d-4821").await;
    let sent = subscribe_cases(
        &client,
        &rec,
        &strings(&[
            "$share//x",
            "$share/g",
            "$share/g/",
            "$share/",
            "$share/g/a/#/b",
        ]),
    )
    .await;
    assert!(
        sent.is_empty(),
        "MQTT-4.8.2-1 VIOLATION: client sent malformed Shared Subscription filters (empty ShareName / missing or invalid filter): {sent:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_8_2_2_client_must_not_send_wildcard_in_sharename() {
    let (client, rec) = recorder_client("d-4822").await;
    let sent = subscribe_cases(
        &client,
        &rec,
        &strings(&["$share/g+/x", "$share/g#/x", "$share/+/x", "$share/#/x"]),
    )
    .await;
    assert!(
        sent.is_empty(),
        "MQTT-4.8.2-2 VIOLATION: client sent Shared Subscription ShareName containing '+' or '#': {sent:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_7_and_4_8_client_must_not_reject_valid_filters() {
    let (client, rec) = recorder_client("d-47valid").await;
    let valid = strings(&[
        "$shared/x",
        "$sharex",
        "$share/g/a/+",
        "$share/g/#",
        "/",
        "+/+",
        "a/+/b/#",
        "+",
        "#",
        "$SYS/#",
        "a//b",
    ]);
    let mut outcomes = Vec::new();
    for f in &valid {
        let r = timeout(W, client.subscribe(f.clone(), |_| {})).await;
        outcomes.push((f.clone(), matches!(r, Ok(Ok(_)))));
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let r = rec.lock().expect("lock");
    let rejected: Vec<_> = outcomes
        .iter()
        .filter(|(f, ok)| !ok || !r.subscribes.contains(f))
        .map(|(f, _)| f.clone())
        .collect();
    assert!(
        rejected.is_empty(),
        "MQTT-4.7.1/4.8.2 over-rejection: client refused valid Topic Filters: {rejected:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_9_0_1_initial_send_quota_is_receive_maximum() {
    let (l, addr) = listener().await;
    let seen = Arc::new(Mutex::new(0usize));
    let seen2 = Arc::clone(&seen);
    tokio::spawn(async move {
        let Some((mut peer, _)) = accept_connect(&l, W).await else {
            return;
        };
        peer.send(&connack(false, 0, &prop_u16(PROP_RECEIVE_MAXIMUM, 2)))
            .await;
        while let Read::Packet(p) = peer.read(Duration::from_secs(30)).await {
            if p.kind == PUBLISH {
                *seen2.lock().expect("lock") += 1;
            }
        }
    });
    let client = MqttClient::with_options(opts("d-4901"));
    client
        .connect(&format!("mqtt://{addr}"))
        .await
        .expect("connect");
    let mut handles = Vec::new();
    for i in 0..4u8 {
        let c = client.clone();
        handles.push(tokio::spawn(
            async move { c.publish_qos1("q/t", vec![i]).await },
        ));
    }
    tokio::time::sleep(Duration::from_millis(700)).await;
    let n = *seen.lock().expect("lock");
    for h in handles {
        h.abort();
    }
    assert_eq!(n, 2, "MQTT-4.9.0-1: initial send quota must equal Receive Maximum 2, saw {n} unacked QoS1 PUBLISH");
}

struct QuotaTrace {
    distinct_unacked: usize,
    dup_resends: usize,
}

async fn count_unacked_publishes(peer: &mut Peer, window: Duration) -> (QuotaTrace, Vec<u16>) {
    let deadline = Instant::now() + window;
    let mut pids = Vec::new();
    let mut dup = 0;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match peer.read(remaining).await {
            Read::Packet(p) if p.kind == PUBLISH => {
                if let Some(info) = parse_publish(&p) {
                    if info.dup {
                        dup += 1;
                    }
                    if let (true, Some(pid)) = (info.qos > 0, info.pid) {
                        if !pids.contains(&pid) {
                            pids.push(pid);
                        }
                    }
                }
            }
            Read::Packet(p) if p.kind == PINGREQ => peer.send(&[0xD0, 0x00]).await,
            Read::Packet(_) => {}
            Read::Closed | Read::TimedOut => break,
        }
    }
    (
        QuotaTrace {
            distinct_unacked: pids.len(),
            dup_resends: dup,
        },
        pids,
    )
}

async fn wait_disconnected(client: &MqttClient) -> bool {
    let deadline = Instant::now() + W;
    while Instant::now() < deadline {
        if let Ok(false) = timeout(Duration::from_millis(200), client.is_connected()).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

async fn wait_connected(client: &MqttClient) -> bool {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if let Ok(true) = timeout(Duration::from_millis(200), client.is_connected()).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

async fn first_connection_with_two_unacked(
    l: &TcpListener,
    addr: SocketAddr,
    id: &str,
) -> (
    MqttClient,
    Vec<tokio::task::JoinHandle<mqtt5::Result<mqtt5::PublishResult>>>,
) {
    let client = MqttClient::with_options(persistent_opts(id));
    let connect = {
        let c = client.clone();
        tokio::spawn(async move { c.connect(&format!("mqtt://{addr}")).await })
    };
    let (mut peer, _) = accept_connect(l, W).await.expect("conn1 CONNECT");
    peer.send(&connack(false, 0, &[])).await;
    connect.await.expect("join").expect("conn1 connect");
    let mut handles = Vec::new();
    for i in 0..2u8 {
        let c = client.clone();
        handles.push(tokio::spawn(async move {
            c.publish_qos1("q/resume", vec![i]).await
        }));
    }
    let (trace, _) = count_unacked_publishes(&mut peer, Duration::from_millis(500)).await;
    assert_eq!(
        trace.distinct_unacked, 2,
        "setup: conn1 must carry 2 unacked QoS1 PUBLISH"
    );
    drop(peer);
    assert!(
        wait_disconnected(&client).await,
        "setup: client must notice conn1 loss"
    );
    (client, handles)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_9_0_2_send_quota_after_resume_with_smaller_receive_maximum() {
    let (l, addr) = listener().await;
    let (client, first_handles) =
        first_connection_with_two_unacked(&l, addr, "d-4902-resume").await;

    let (mut peer, info) = accept_connect(&l, Duration::from_secs(8))
        .await
        .expect("conn2 CONNECT");
    assert!(
        !info.clean_start,
        "setup: reconnect must resume the session (Clean Start 0)"
    );
    peer.send(&connack(true, 0, &prop_u16(PROP_RECEIVE_MAXIMUM, 1)))
        .await;
    assert!(
        wait_connected(&client).await,
        "setup: client must be connected on conn2"
    );

    let mut later = Vec::new();
    for i in 0..2u8 {
        let c = client.clone();
        later.push(tokio::spawn(async move {
            c.publish_qos1("q/resume", vec![10 + i]).await
        }));
    }
    let (trace, pids) = count_unacked_publishes(&mut peer, Duration::from_millis(1500)).await;
    eprintln!(
        "MQTT-4.9.0-2 resume trace: unacked QoS1 on conn2 = {} (pids {pids:?}), DUP resends = {}",
        trace.distinct_unacked, trace.dup_resends
    );

    for pid in &pids {
        peer.send(&puback(*pid)).await;
    }
    let server = tokio::spawn(async move {
        while let Read::Packet(p) = peer.read(Duration::from_secs(20)).await {
            match p.kind {
                PUBLISH => {
                    if let Some(pid) = parse_publish(&p).and_then(|i| i.pid) {
                        peer.send(&puback(pid)).await;
                    }
                }
                PINGREQ => peer.send(&[0xD0, 0x00]).await,
                _ => {}
            }
        }
    });

    let mut panicked = 0;
    let mut hung = 0;
    for h in first_handles.into_iter().chain(later) {
        match timeout(Duration::from_secs(15), h).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => panicked += 1,
            Err(_) => hung += 1,
        }
    }
    let fresh = timeout(
        Duration::from_secs(5),
        client.publish_qos1("q/resume", b"fresh".to_vec()),
    )
    .await;
    server.abort();

    assert_eq!(
        panicked, 0,
        "MQTT-4.9.0-2: publish task panicked after resume"
    );
    assert_eq!(
        hung, 0,
        "MQTT-4.9.0-2: {hung} publish calls deadlocked after resume with smaller Receive Maximum"
    );
    assert!(
        trace.distinct_unacked <= 1,
        "MQTT-4.9.0-2 VIOLATION: after resume with Receive Maximum 1 the client had {} unacked QoS>0 PUBLISH on the wire ({} DUP)",
        trace.distinct_unacked,
        trace.dup_resends
    );
    assert!(
        matches!(fresh, Ok(Ok(_))),
        "MQTT-4.9.0-2: send quota leaked after resume; a fresh acked QoS1 publish failed: {fresh:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_9_0_2_offline_queue_flush_respects_receive_maximum() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(
        persistent_opts("d-4902-queue")
            .with_reconnect_delay(Duration::from_millis(600), Duration::from_secs(1)),
    );
    let connect = {
        let c = client.clone();
        tokio::spawn(async move { c.connect(&format!("mqtt://{addr}")).await })
    };
    let (peer, _) = accept_connect(&l, W).await.expect("conn1 CONNECT");
    let mut peer = peer;
    peer.send(&connack(false, 0, &[])).await;
    connect.await.expect("join").expect("conn1 connect");
    drop(peer);
    assert!(
        wait_disconnected(&client).await,
        "setup: client must notice conn1 loss"
    );

    let mut queued = 0;
    for i in 0..3u8 {
        if let Ok(Ok(_)) = timeout(
            Duration::from_millis(300),
            client.publish_qos1("q/offline", vec![i]),
        )
        .await
        {
            queued += 1;
        }
    }
    assert_eq!(
        queued, 3,
        "setup: 3 QoS1 publishes must be accepted into the offline queue"
    );

    let (mut peer, _) = accept_connect(&l, Duration::from_secs(8))
        .await
        .expect("conn2 CONNECT");
    peer.send(&connack(true, 0, &prop_u16(PROP_RECEIVE_MAXIMUM, 1)))
        .await;
    let (trace, _) = count_unacked_publishes(&mut peer, Duration::from_millis(1500)).await;
    assert!(
        trace.distinct_unacked <= 1,
        "MQTT-4.9.0-2 VIOLATION: offline-queue flush after reconnect sent {} unacked QoS1 PUBLISH with server Receive Maximum 1 (send quota bypassed)",
        trace.distinct_unacked
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_9_0_1_send_quota_reinitialized_after_ack_timeout_and_reconnect() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(persistent_opts("d-4901-reinit"));
    let connect = start_connect(&client, addr);
    let (mut peer, _) = accept_connect(&l, W).await.expect("conn1 CONNECT");
    peer.send(&connack(false, 0, &prop_u16(PROP_RECEIVE_MAXIMUM, 1)))
        .await;
    connect.await.expect("join").expect("conn1 connect");

    let timed_out = timeout(
        Duration::from_secs(15),
        client.publish_qos1("q/reinit", b"unacked".to_vec()),
    )
    .await;
    assert!(
        matches!(timed_out, Ok(Err(mqtt5::MqttError::Timeout))),
        "setup: unacked QoS1 publish must hit the client ack timeout: {timed_out:?}"
    );
    drop(peer);
    assert!(
        wait_disconnected(&client).await,
        "setup: client must notice conn1 loss"
    );

    let (mut peer, _) = accept_connect(&l, Duration::from_secs(8))
        .await
        .expect("conn2 CONNECT");
    peer.send(&connack(false, 0, &prop_u16(PROP_RECEIVE_MAXIMUM, 1)))
        .await;
    assert!(wait_connected(&client).await, "setup: reconnect");

    let fresh = {
        let c = client.clone();
        tokio::spawn(async move { c.publish_qos1("q/reinit", b"fresh".to_vec()).await })
    };
    let mut delivered = false;
    let deadline = Instant::now() + W;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match peer.read(remaining).await {
            Read::Packet(p) if p.kind == PUBLISH => {
                if let Some(pid) = parse_publish(&p).and_then(|i| i.pid) {
                    peer.send(&puback(pid)).await;
                }
                delivered = true;
                break;
            }
            Read::Packet(p) if p.kind == PINGREQ => peer.send(&[0xD0, 0x00]).await,
            Read::Packet(_) => {}
            Read::Closed | Read::TimedOut => break,
        }
    }
    fresh.abort();
    assert!(
        delivered,
        "MQTT-4.9.0-1 VIOLATION: on a new Network Connection (Session Present 0, Receive Maximum 1) the send quota was not re-initialized; the permit held by a pre-disconnect ack-timeout leaked, initial quota is 0 and a fresh QoS1 PUBLISH never reached the wire within 3s"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn out_of_group_mqtt_4_4_0_1_resend_unacked_publish_on_resume() {
    let (l, addr) = listener().await;
    let (client, handles) = first_connection_with_two_unacked(&l, addr, "d-4401").await;
    let (mut peer, _) = accept_connect(&l, Duration::from_secs(8))
        .await
        .expect("conn2 CONNECT");
    peer.send(&connack(true, 0, &[])).await;
    assert!(wait_connected(&client).await, "setup: reconnect");
    let (trace, _) = count_unacked_publishes(&mut peer, Duration::from_millis(1500)).await;
    for h in handles {
        h.abort();
    }
    assert_eq!(
        trace.dup_resends, 2,
        "MQTT-4.4.0-1 VIOLATION (outside group D): on Session resume the client resent {} of 2 unacknowledged QoS1 PUBLISH packets",
        trace.dup_resends
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_9_0_3_client_still_acks_and_pings_at_zero_quota() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(opts("d-4903").with_keep_alive(Duration::from_secs(1)));
    let connect = {
        let c = client.clone();
        tokio::spawn(async move { c.connect(&format!("mqtt://{addr}")).await })
    };
    let (mut peer, _) = accept_connect(&l, W).await.expect("CONNECT");
    let mut props = prop_u16(PROP_RECEIVE_MAXIMUM, 1);
    props.extend(prop_u16(PROP_SERVER_KEEP_ALIVE, 1));
    peer.send(&connack(false, 0, &props)).await;
    connect.await.expect("join").expect("connect");

    let blocked = {
        let c = client.clone();
        tokio::spawn(async move { c.publish_qos1("q/zero", b"hold".to_vec()).await })
    };
    let first = peer.expect(W).await.expect("first QoS1 PUBLISH");
    assert_eq!(first.kind, PUBLISH);
    let queued_publish = {
        let c = client.clone();
        tokio::spawn(async move { c.publish_qos1("q/zero", b"blocked".to_vec()).await })
    };

    peer.send(&publish("in/q1", 1, 700, b"a")).await;
    peer.send(&publish("in/q2", 2, 701, b"b")).await;
    let subscribe = {
        let c = client.clone();
        tokio::spawn(async move { c.subscribe("in/#", |_| {}).await })
    };

    let mut got_puback = false;
    let mut got_pubrec = false;
    let mut got_pubcomp = false;
    let mut got_ping = false;
    let mut got_subscribe = false;
    let mut extra_publish = 0;
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match peer.read(remaining).await {
            Read::Packet(p) => match p.kind {
                PUBACK if packet_id(&p) == Some(700) => got_puback = true,
                PUBREC if packet_id(&p) == Some(701) => {
                    got_pubrec = true;
                    peer.send(&pubrel(701)).await;
                }
                PUBCOMP if packet_id(&p) == Some(701) => got_pubcomp = true,
                PINGREQ => {
                    got_ping = true;
                    peer.send(&[0xD0, 0x00]).await;
                }
                SUBSCRIBE => {
                    got_subscribe = true;
                    if let Some((pid, f)) = parse_filters(&p, true) {
                        peer.send(&suback(pid, f.len())).await;
                    }
                }
                PUBLISH => extra_publish += 1,
                _ => {}
            },
            Read::Closed | Read::TimedOut => break,
        }
        if got_puback && got_pubrec && got_pubcomp && got_ping && got_subscribe {
            break;
        }
    }
    let sub_ok = matches!(timeout(W, subscribe).await, Ok(Ok(Ok(_))));
    blocked.abort();
    queued_publish.abort();
    assert_eq!(
        extra_publish, 0,
        "MQTT-4.9.0-2: client sent a QoS1 PUBLISH while quota was 0"
    );
    assert!(
        got_puback && got_pubrec && got_pubcomp && got_ping && got_subscribe && sub_ok,
        "MQTT-4.9.0-3 VIOLATION at quota 0: PUBACK={got_puback} PUBREC={got_pubrec} PUBCOMP={got_pubcomp} PINGREQ={got_ping} SUBSCRIBE={got_subscribe} SUBACK-processed={sub_ok}"
    );
}

struct ScriptedAuth {
    calls: Arc<Mutex<Vec<String>>>,
}

impl AuthHandler for ScriptedAuth {
    fn handle_challenge<'a>(
        &'a self,
        auth_method: &'a str,
        _challenge_data: Option<&'a [u8]>,
    ) -> Pin<Box<dyn Future<Output = mqtt5::Result<AuthResponse>> + Send + 'a>> {
        self.calls
            .lock()
            .expect("lock")
            .push(auth_method.to_string());
        Box::pin(async move { Ok(AuthResponse::Continue(b"resp".to_vec())) })
    }

    fn initial_response<'a>(
        &'a self,
        _auth_method: &'a str,
    ) -> Pin<Box<dyn Future<Output = mqtt5::Result<Option<Vec<u8>>>> + Send + 'a>> {
        Box::pin(async move { Ok(Some(b"init".to_vec())) })
    }
}

fn scripted() -> (ScriptedAuth, Arc<Mutex<Vec<String>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    (
        ScriptedAuth {
            calls: Arc::clone(&calls),
        },
        calls,
    )
}

fn start_connect(
    client: &MqttClient,
    addr: SocketAddr,
) -> tokio::task::JoinHandle<mqtt5::Result<()>> {
    let c = client.clone();
    tokio::spawn(async move { c.connect(&format!("mqtt://{addr}")).await })
}

async fn auth_packets_until_close(peer: &mut Peer, wait: Duration) -> (Vec<AuthInfo>, bool) {
    let deadline = Instant::now() + wait;
    let mut auths = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match peer.read(remaining).await {
            Read::Packet(p) if p.kind == AUTH => auths.push(parse_auth(&p)),
            Read::Packet(_) => {}
            Read::Closed => return (auths, true),
            Read::TimedOut => return (auths, false),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_12_0_7_no_method_no_handler_auth_during_connect() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(opts("d-41207a"));
    let connect = start_connect(&client, addr);
    let (mut peer, info) = accept_connect(&l, W).await.expect("CONNECT");
    assert!(info.auth_method.is_none());
    peer.send(&auth(0x18, &prop_str(PROP_AUTH_METHOD, "X")))
        .await;
    let (auths, closed) = auth_packets_until_close(&mut peer, W).await;
    let result = timeout(W, connect).await;
    assert!(
        auths.is_empty(),
        "MQTT-4.12.0-7 VIOLATION: client without Authentication Method sent AUTH"
    );
    assert!(
        closed && matches!(result, Ok(Ok(Err(_)))),
        "MQTT-4.12.0-6 (client side): unexpected AUTH must fail the connect and close the connection; closed={closed} result={result:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_12_0_7_no_method_with_handler_auth_during_connect() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(opts("d-41207b"));
    let (handler, _) = scripted();
    client.set_auth_handler(handler).await;
    let connect = start_connect(&client, addr);
    let (mut peer, info) = accept_connect(&l, W).await.expect("CONNECT");
    assert!(
        info.auth_method.is_none(),
        "setup: CONNECT carries no Authentication Method"
    );
    peer.send(&auth(0x18, &prop_str(PROP_AUTH_METHOD, "X")))
        .await;
    let got = peer.expect(Duration::from_secs(1)).await;
    connect.abort();
    let sent_auth = got.as_ref().filter(|p| p.kind == AUTH).map(parse_auth);
    assert!(
        sent_auth.is_none(),
        "MQTT-4.12.0-7 VIOLATION: CONNECT had no Authentication Method but client answered server AUTH with AUTH reason=0x{:02X} method={:?}",
        sent_auth.as_ref().map_or(0, |a| a.reason),
        sent_auth.as_ref().and_then(|a| a.method.clone())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_12_0_7_no_method_with_handler_auth_after_connack() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(opts("d-41207c"));
    let (handler, _) = scripted();
    client.set_auth_handler(handler).await;
    let connect = start_connect(&client, addr);
    let (mut peer, info) = accept_connect(&l, W).await.expect("CONNECT");
    assert!(info.auth_method.is_none());
    peer.send(&connack(false, 0, &[])).await;
    connect.await.expect("join").expect("connect");
    peer.send(&auth(0x18, &prop_str(PROP_AUTH_METHOD, "X")))
        .await;
    let got = peer.expect(Duration::from_secs(1)).await;
    let sent_auth = got.as_ref().filter(|p| p.kind == AUTH).map(parse_auth);
    assert!(
        sent_auth.is_none(),
        "MQTT-4.12.0-7 VIOLATION: after CONNACK, client without Authentication Method answered server AUTH with AUTH reason=0x{:02X} method={:?}",
        sent_auth.as_ref().map_or(0, |a| a.reason),
        sent_auth.as_ref().and_then(|a| a.method.clone())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_12_0_3_and_4_12_0_5_continue_auth_uses_0x18_and_connect_method() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(opts("d-41203").with_authentication_method("M-1"));
    let (handler, _) = scripted();
    client.set_auth_handler(handler).await;
    let connect = start_connect(&client, addr);
    let (mut peer, info) = accept_connect(&l, W).await.expect("CONNECT");
    assert_eq!(info.auth_method.as_deref(), Some("M-1"));
    peer.send(&auth(0x18, &prop_str(PROP_AUTH_METHOD, "M-1")))
        .await;
    let reply = peer.expect(W).await.expect("client AUTH");
    let a = parse_auth(&reply);
    peer.send(&connack(false, 0, &prop_str(PROP_AUTH_METHOD, "M-1")))
        .await;
    let result = timeout(W, connect).await;
    assert_eq!(reply.kind, AUTH, "client must answer with AUTH");
    assert_eq!(
        a.reason, 0x18,
        "MQTT-4.12.0-3 VIOLATION: client continuation AUTH reason 0x{:02X}",
        a.reason
    );
    assert_eq!(
        a.method.as_deref(),
        Some("M-1"),
        "MQTT-4.12.0-5 VIOLATION: client AUTH method differs from CONNECT"
    );
    assert!(
        matches!(result, Ok(Ok(Ok(())))),
        "enhanced auth connect must succeed: {result:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_12_0_5_server_auth_with_different_method() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(opts("d-41205").with_authentication_method("M-1"));
    let (handler, calls) = scripted();
    client.set_auth_handler(handler).await;
    let connect = start_connect(&client, addr);
    let (mut peer, _) = accept_connect(&l, W).await.expect("CONNECT");
    peer.send(&auth(0x18, &prop_str(PROP_AUTH_METHOD, "OTHER")))
        .await;
    let (auths, closed) = auth_packets_until_close(&mut peer, Duration::from_millis(1500)).await;
    connect.abort();
    let wrong: Vec<_> = auths
        .iter()
        .filter(|a| a.method.as_deref() != Some("M-1"))
        .collect();
    assert!(
        wrong.is_empty(),
        "MQTT-4.12.0-5 VIOLATION: client sent AUTH with a method other than its CONNECT method"
    );
    eprintln!(
        "MQTT-4.12.0-5 observation: server AUTH method mismatch -> client replied with {} AUTH(s) using CONNECT method, handler saw {:?}, connection closed by client={closed}",
        auths.len(),
        calls.lock().expect("lock")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_12_0_2_non_continue_reason_during_connect_is_rejected() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(opts("d-41202").with_authentication_method("M-1"));
    let (handler, _) = scripted();
    client.set_auth_handler(handler).await;
    let connect = start_connect(&client, addr);
    let (mut peer, _) = accept_connect(&l, W).await.expect("CONNECT");
    peer.send(&auth(0x19, &prop_str(PROP_AUTH_METHOD, "M-1")))
        .await;
    let (auths, closed) = auth_packets_until_close(&mut peer, W).await;
    let result = timeout(W, connect).await;
    assert!(
        auths.is_empty() && closed && matches!(result, Ok(Ok(Err(_)))),
        "MQTT-4.12.0-2 (client side): AUTH 0x19 during CONNECT must be rejected and the connection closed; client AUTHs={} closed={closed} result={result:?}",
        auths.len()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_12_0_1_and_4_12_0_4_connack_failure_closes_connection() {
    for reason in [0x8Cu8, 0x87] {
        let (l, addr) = listener().await;
        let client = MqttClient::with_options(opts("d-41204").with_authentication_method("M-1"));
        let (handler, _) = scripted();
        client.set_auth_handler(handler).await;
        let connect = start_connect(&client, addr);
        let (mut peer, _) = accept_connect(&l, W).await.expect("CONNECT");
        peer.send(&auth(0x18, &prop_str(PROP_AUTH_METHOD, "M-1")))
            .await;
        let _ = peer.expect(W).await;
        peer.send(&connack(false, reason, &[])).await;
        let (closed, _) = peer.closed_within(W).await;
        let result = timeout(W, connect).await;
        assert!(
            closed && matches!(result, Ok(Ok(Err(_)))),
            "MQTT-4.12.0-4/4.12.0-1: CONNACK 0x{reason:02X} mid-auth must fail connect and close; closed={closed} result={result:?}"
        );
    }
}

async fn authenticated_session(id: &str, keep_alive: Duration) -> (MqttClient, Peer) {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(
        opts(id)
            .with_authentication_method("M-1")
            .with_keep_alive(keep_alive),
    );
    let (handler, _) = scripted();
    client.set_auth_handler(handler).await;
    let connect = start_connect(&client, addr);
    let (mut peer, _) = accept_connect(&l, W).await.expect("CONNECT");
    peer.send(&connack(false, 0, &prop_str(PROP_AUTH_METHOD, "M-1")))
        .await;
    connect.await.expect("join").expect("connect");
    (client, peer)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_12_1_1_reauth_uses_0x19_and_original_method() {
    let (client, mut peer) = authenticated_session("d-41211", Duration::from_secs(60)).await;
    client.reauthenticate().await.expect("reauthenticate");
    let p = peer.expect(W).await.expect("re-auth AUTH");
    let a = parse_auth(&p);
    assert_eq!(p.kind, AUTH);
    assert_eq!(
        a.reason, 0x19,
        "MQTT-4.12.1-1: re-auth must use reason 0x19"
    );
    assert_eq!(
        a.method.as_deref(),
        Some("M-1"),
        "MQTT-4.12.1-1 VIOLATION: re-auth method differs from original"
    );
    peer.send(&auth(0x18, &{
        let mut pr = prop_str(PROP_AUTH_METHOD, "M-1");
        pr.push(PROP_AUTH_DATA);
        pr.extend(mqtt_str(b"chal"));
        pr
    }))
    .await;
    let p2 = peer.expect(W).await.expect("re-auth continuation");
    let a2 = parse_auth(&p2);
    assert!(
        p2.kind == AUTH && a2.reason == 0x18 && a2.method.as_deref() == Some("M-1"),
        "MQTT-4.12.0-3/4.12.0-5 during re-auth: continuation reason=0x{:02X} method={:?}",
        a2.reason,
        a2.method
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_12_1_2_server_disconnect_0x87_during_reauth_closes_connection() {
    let (client, mut peer) = authenticated_session("d-41212", Duration::from_secs(1)).await;
    client.reauthenticate().await.expect("reauthenticate");
    let p = peer.expect(W).await.expect("re-auth AUTH");
    assert_eq!(p.kind, AUTH);
    peer.send(&disconnect(0x87)).await;
    let (closed, after) = peer.closed_within(Duration::from_secs(4)).await;
    let pinged = after.contains(&PINGREQ);
    assert!(
        closed,
        "MQTT-4.12.1-2 / MQTT-4.13.2-1 VIOLATION: after server DISCONNECT 0x87 during re-authentication the client kept the TCP connection open for 4s (reconnect disabled); PINGREQ sent afterwards={pinged}, packet types seen={after:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_12_1_2_reauth_failure_auth_reason_closes_connection() {
    let (client, mut peer) = authenticated_session("d-41212b", Duration::from_secs(60)).await;
    client.reauthenticate().await.expect("reauthenticate");
    let _ = peer.expect(W).await;
    peer.send(&auth(0x87, &[])).await;
    let (closed, after) = peer.closed_within(Duration::from_secs(4)).await;
    assert!(
        closed,
        "MQTT-4.12.1-2 VIOLATION: client treated failed re-authentication (AUTH 0x87) as fatal but never closed the Network Connection; packets seen={after:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_13_2_1_connack_error_closes_connection() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(opts("d-41321a"));
    let connect = start_connect(&client, addr);
    let (mut peer, _) = accept_connect(&l, W).await.expect("CONNECT");
    peer.send(&connack(false, 0x87, &[])).await;
    let (closed, _) = peer.closed_within(W).await;
    let result = timeout(W, connect).await;
    assert!(
        closed && matches!(result, Ok(Ok(Err(_)))),
        "MQTT-4.13.2-1: CONNACK 0x87 must fail connect and close; closed={closed} result={result:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_13_2_1_server_disconnect_error_closes_connection() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(opts("d-41321b").with_keep_alive(Duration::from_secs(1)));
    let connect = start_connect(&client, addr);
    let (mut peer, _) = accept_connect(&l, W).await.expect("CONNECT");
    peer.send(&connack(false, 0, &[])).await;
    connect.await.expect("join").expect("connect");
    peer.send(&disconnect(0x8E)).await;
    let (closed, after) = peer.closed_within(Duration::from_secs(4)).await;
    let connected = client.is_connected().await;
    assert!(
        closed,
        "MQTT-4.13.2-1 VIOLATION: after server DISCONNECT 0x8E the client did not close the Network Connection within 4s (is_connected={connected}, reconnect disabled); packets sent afterwards={after:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mqtt_4_13_2_1_server_disconnect_error_closes_connection_default_reconnect() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(
        ConnectOptions::new("d-41321c").with_keep_alive(Duration::from_secs(1)),
    );
    let connect = start_connect(&client, addr);
    let (mut peer, _) = accept_connect(&l, W).await.expect("CONNECT");
    peer.send(&connack(false, 0, &[])).await;
    connect.await.expect("join").expect("connect");
    drop(l);
    let t0 = Instant::now();
    peer.send(&disconnect(0x8E)).await;
    let (closed, after) = peer.closed_within(Duration::from_secs(6)).await;
    let elapsed = t0.elapsed();
    let _ = timeout(W, client.disconnect()).await;
    assert!(
        closed && elapsed < Duration::from_millis(500),
        "MQTT-4.13.2-1 VIOLATION (default reconnect config): connection closed={closed} only after {elapsed:?} (tied to the reconnect attempt, not the DISCONNECT); packets sent afterwards={after:?}"
    );
}

#[cfg(feature = "transport-websocket")]
mod websocket {
    use super::{
        connack, parse_filters, publish, suback, try_split_packet, Pkt, CONNECT, SUBSCRIBE, W,
    };
    use futures_util::{SinkExt, StreamExt};
    use mqtt5::{ConnectOptions, MqttClient};
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::time::{timeout, Instant};
    use tokio_tungstenite::tungstenite::handshake::server::{
        Callback, ErrorResponse, Request, Response,
    };
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::WebSocketStream;

    struct WsPeer {
        ws: WebSocketStream<TcpStream>,
        buf: Vec<u8>,
        non_binary_from_client: Vec<String>,
    }

    enum WsRead {
        Packet(Pkt),
        Closed,
        TimedOut,
    }

    impl WsPeer {
        async fn read(&mut self, wait: Duration) -> WsRead {
            let deadline = Instant::now() + wait;
            loop {
                if let Some(p) = try_split_packet(&mut self.buf) {
                    return WsRead::Packet(p);
                }
                match tokio::time::timeout_at(deadline, self.ws.next()).await {
                    Err(_) => return WsRead::TimedOut,
                    Ok(None | Some(Err(_) | Ok(Message::Close(_)))) => return WsRead::Closed,
                    Ok(Some(Ok(Message::Binary(b)))) => self.buf.extend_from_slice(&b),
                    Ok(Some(Ok(other))) => self.non_binary_from_client.push(format!("{other:?}")),
                }
            }
        }

        async fn send_binary(&mut self, bytes: Vec<u8>) {
            let _ = self.ws.send(Message::Binary(bytes.into())).await;
        }

        async fn closed_within(&mut self, wait: Duration) -> (bool, Vec<u8>) {
            let deadline = Instant::now() + wait;
            let mut seen = Vec::new();
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                match self.read(remaining).await {
                    WsRead::Packet(p) => seen.push(p.kind),
                    WsRead::Closed => return (true, seen),
                    WsRead::TimedOut => return (false, seen),
                }
            }
        }
    }

    async fn ws_listener() -> (TcpListener, SocketAddr) {
        let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let a = l.local_addr().expect("addr");
        (l, a)
    }

    async fn ws_accept(l: &TcpListener) -> (WsPeer, Option<String>) {
        let (stream, _) = timeout(W, l.accept())
            .await
            .expect("accept")
            .expect("accept");
        let offered = Arc::new(Mutex::new(None));
        let capture = OfferedProtocols {
            offered: Arc::clone(&offered),
        };
        let ws = tokio_tungstenite::accept_hdr_async(stream, capture)
            .await
            .expect("ws handshake");
        let value = offered.lock().expect("lock").clone();
        (
            WsPeer {
                ws,
                buf: Vec::new(),
                non_binary_from_client: Vec::new(),
            },
            value,
        )
    }

    struct OfferedProtocols {
        offered: Arc<Mutex<Option<String>>>,
    }

    impl Callback for OfferedProtocols {
        fn on_request(self, req: &Request, mut resp: Response) -> Result<Response, ErrorResponse> {
            let value = req
                .headers()
                .get("Sec-WebSocket-Protocol")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            *self.offered.lock().expect("lock") = value;
            resp.headers_mut()
                .insert("Sec-WebSocket-Protocol", "mqtt".parse().expect("header"));
            Ok(resp)
        }
    }

    fn ws_opts(id: &str) -> ConnectOptions {
        ConnectOptions::new(id).with_automatic_reconnect(false)
    }

    async fn ws_connected(id: &str) -> (MqttClient, WsPeer, Option<String>) {
        let (l, addr) = ws_listener().await;
        let client = MqttClient::with_options(ws_opts(id));
        let c = client.clone();
        let connect = tokio::spawn(async move { c.connect(&format!("ws://{addr}/mqtt")).await });
        let (mut peer, offered) = ws_accept(&l).await;
        match peer.read(W).await {
            WsRead::Packet(p) => assert_eq!(p.kind, CONNECT),
            _ => panic!("setup: expected CONNECT over WebSocket"),
        }
        peer.send_binary(connack(false, 0, &[])).await;
        connect.await.expect("join").expect("ws connect");
        (client, peer, offered)
    }

    async fn ws_subscribed(id: &str) -> (MqttClient, WsPeer, Arc<Mutex<Vec<Vec<u8>>>>) {
        let (client, mut peer, _) = ws_connected(id).await;
        let got = Arc::new(Mutex::new(Vec::new()));
        let got2 = Arc::clone(&got);
        let c = client.clone();
        let sub = tokio::spawn(async move {
            c.subscribe("ws/#", move |m| got2.lock().expect("lock").push(m.payload))
                .await
        });
        loop {
            match peer.read(W).await {
                WsRead::Packet(p) if p.kind == SUBSCRIBE => {
                    let (pid, f) = parse_filters(&p, true).expect("subscribe");
                    peer.send_binary(suback(pid, f.len())).await;
                    break;
                }
                WsRead::Packet(_) => {}
                _ => panic!("setup: expected SUBSCRIBE over WebSocket"),
            }
        }
        sub.await.expect("join").expect("subscribe");
        (client, peer, got)
    }

    async fn delivered(got: &Arc<Mutex<Vec<Vec<u8>>>>, want: usize) -> Vec<Vec<u8>> {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && got.lock().expect("lock").len() < want {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        got.lock().expect("lock").clone()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn mqtt_6_0_0_3_client_offers_mqtt_subprotocol() {
        let (_client, _peer, offered) = ws_connected("d-6003").await;
        let offered = offered.unwrap_or_default();
        assert!(
            offered.split(',').any(|p| p.trim() == "mqtt"),
            "MQTT-6.0.0-3 VIOLATION: offered Sec-WebSocket-Protocol={offered:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn mqtt_6_0_0_1_client_sends_only_binary_frames() {
        let (client, mut peer, _) = ws_connected("d-6001a").await;
        let c = client.clone();
        let pubs = tokio::spawn(async move {
            let _ = c.publish("ws/a", b"x".to_vec()).await;
            let _ = c.disconnect().await;
        });
        let (_closed, _) = peer.closed_within(W).await;
        let _ = pubs.await;
        assert!(
            peer.non_binary_from_client.is_empty(),
            "MQTT-6.0.0-1 VIOLATION: client sent non-binary data frames: {:?}",
            peer.non_binary_from_client
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn mqtt_6_0_0_1_text_frame_after_connect_closes_connection() {
        let (client, mut peer, _) = ws_connected("d-6001b").await;
        let _ = peer.ws.send(Message::Text("not mqtt".into())).await;
        let (closed, after) = peer.closed_within(Duration::from_secs(4)).await;
        let connected = client.is_connected().await;
        assert!(
            closed,
            "MQTT-6.0.0-1 VIOLATION: after a WebSocket text data frame the client did not close the Network Connection within 4s (is_connected={connected}); MQTT packets sent afterwards={after:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn mqtt_6_0_0_1_text_frame_before_connack_closes_connection() {
        let (l, addr) = ws_listener().await;
        let client = MqttClient::with_options(ws_opts("d-6001c"));
        let c = client.clone();
        let connect = tokio::spawn(async move { c.connect(&format!("ws://{addr}/mqtt")).await });
        let (mut peer, _) = ws_accept(&l).await;
        let _ = peer.read(W).await;
        let _ = peer.ws.send(Message::Text("not mqtt".into())).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        peer.send_binary(connack(false, 0, &[])).await;
        let result = timeout(W, connect).await;
        let (closed, _) = peer.closed_within(Duration::from_millis(500)).await;
        assert!(
            closed && !matches!(result, Ok(Ok(Ok(())))),
            "MQTT-6.0.0-1 VIOLATION: text data frame before CONNACK was ignored; connect result={result:?}, connection closed={closed}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn mqtt_6_0_0_2_multiple_packets_in_one_frame() {
        let (_client, mut peer, got) = ws_subscribed("d-6002a").await;
        let mut both = publish("ws/a", 0, 0, b"one");
        both.extend(publish("ws/b", 0, 0, b"two"));
        peer.send_binary(both).await;
        let msgs = delivered(&got, 2).await;
        assert_eq!(
            msgs,
            vec![b"one".to_vec(), b"two".to_vec()],
            "MQTT-6.0.0-2 VIOLATION: two PUBLISH packets in one WebSocket frame; delivered={msgs:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn mqtt_6_0_0_2_packet_split_across_frames() {
        let (client, mut peer, got) = ws_subscribed("d-6002b").await;
        let bytes = publish("ws/split", 0, 0, b"payload-split");
        let (a, b) = bytes.split_at(5);
        peer.send_binary(a.to_vec()).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        peer.send_binary(b.to_vec()).await;
        let msgs = delivered(&got, 1).await;
        let connected = client.is_connected().await;
        assert_eq!(
            msgs,
            vec![b"payload-split".to_vec()],
            "MQTT-6.0.0-2 VIOLATION: PUBLISH split across two WebSocket frames not reassembled; delivered={msgs:?} still_connected={connected}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn mqtt_6_0_0_2_connack_split_across_frames() {
        let (l, addr) = ws_listener().await;
        let client = MqttClient::with_options(ws_opts("d-6002c"));
        let c = client.clone();
        let connect = tokio::spawn(async move { c.connect(&format!("ws://{addr}/mqtt")).await });
        let (mut peer, _) = ws_accept(&l).await;
        let _ = peer.read(W).await;
        let bytes = connack(false, 0, &[]);
        peer.send_binary(bytes[..2].to_vec()).await;
        peer.send_binary(bytes[2..].to_vec()).await;
        let result = timeout(W, connect).await;
        assert!(
            matches!(result, Ok(Ok(Ok(())))),
            "MQTT-6.0.0-2: CONNACK split across frames must be reassembled: {result:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn rfc6455_ping_control_frame_does_not_drop_mqtt_session() {
        let (client, mut peer, got) = ws_subscribed("d-6000ping").await;
        let _ = peer.ws.send(Message::Ping(b"hi".to_vec().into())).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        peer.send_binary(publish("ws/after-ping", 0, 0, b"after"))
            .await;
        let msgs = delivered(&got, 1).await;
        let connected = client.is_connected().await;
        assert!(
            connected && msgs == vec![b"after".to_vec()],
            "WebSocket robustness (not an MQTT-6.0.0-1 data frame): a Ping control frame tore down the MQTT session; is_connected={connected} delivered={msgs:?}"
        );
    }
}
