use mqtt5::{
    ConnectOptions, Delivery, IndeterminateReason, MqttClient, MqttError, PublishHandle,
    PublishOptions, PublishOutcome, PublishRejection, PublishResult, QoS,
};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{timeout, Instant};

const CONNECT: u8 = 1;
const PUBLISH: u8 = 3;
const PUBREL: u8 = 6;
const PINGREQ: u8 = 12;

const PROP_RECEIVE_MAXIMUM: u8 = 0x21;
const PROP_MAXIMUM_QOS: u8 = 0x24;
const PROP_RETAIN_AVAILABLE: u8 = 0x25;
const PROP_MAXIMUM_PACKET_SIZE: u8 = 0x27;

const W: Duration = Duration::from_secs(3);

struct Pkt {
    kind: u8,
    flags: u8,
    body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    topic: String,
    qos: u8,
    dup: bool,
    retain: bool,
    pid: Option<u16>,
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

fn connack(session_present: bool, props: &[u8]) -> Vec<u8> {
    let mut body = vec![u8::from(session_present), 0];
    body.extend(varint(props.len()));
    body.extend_from_slice(props);
    frame(0x20, &body)
}

fn prop_byte(id: u8, v: u8) -> Vec<u8> {
    vec![id, v]
}

fn prop_u16(id: u8, v: u16) -> Vec<u8> {
    let mut out = vec![id];
    out.extend(v.to_be_bytes());
    out
}

fn prop_u32(id: u8, v: u32) -> Vec<u8> {
    let mut out = vec![id];
    out.extend(v.to_be_bytes());
    out
}

fn puback(pid: u16) -> Vec<u8> {
    frame(0x40, &pid.to_be_bytes())
}

fn pubrec(pid: u16) -> Vec<u8> {
    frame(0x50, &pid.to_be_bytes())
}

fn pubcomp(pid: u16) -> Vec<u8> {
    frame(0x70, &pid.to_be_bytes())
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

fn take_u16(b: &[u8], pos: &mut usize) -> Option<u16> {
    let v = u16::from_be_bytes([*b.get(*pos)?, *b.get(*pos + 1)?]);
    *pos += 2;
    Some(v)
}

fn parse_publish(p: &Pkt) -> Option<Seen> {
    let b = &p.body;
    let mut pos = 0;
    let len = usize::from(take_u16(b, &mut pos)?);
    let topic = String::from_utf8_lossy(b.get(pos..pos + len)?).into_owned();
    pos += len;
    let qos = (p.flags >> 1) & 0x03;
    let pid = if qos > 0 {
        Some(take_u16(b, &mut pos)?)
    } else {
        None
    };
    Some(Seen {
        topic,
        qos,
        dup: p.flags & 0x08 != 0,
        retain: p.flags & 0x01 != 0,
        pid,
    })
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
    async fn read(&mut self, wait: Duration) -> Option<Pkt> {
        let deadline = Instant::now() + wait;
        loop {
            if let Some(p) = try_split_packet(&mut self.buf) {
                return Some(p);
            }
            let mut chunk = [0u8; 4096];
            match tokio::time::timeout_at(deadline, self.stream.read(&mut chunk)).await {
                Err(_) | Ok(Ok(0) | Err(_)) => return None,
                Ok(Ok(n)) => self.buf.extend_from_slice(&chunk[..n]),
            }
        }
    }

    async fn send(&mut self, bytes: &[u8]) {
        let _ = self.stream.write_all(bytes).await;
        let _ = self.stream.flush().await;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Acks {
    All,
    None,
}

async fn observe(peer: &mut Peer, window: Duration, acks: Acks) -> Vec<Seen> {
    let deadline = Instant::now() + window;
    let mut seen = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Some(p) = peer.read(remaining).await else {
            return seen;
        };
        match p.kind {
            PUBLISH => {
                let Some(info) = parse_publish(&p) else {
                    continue;
                };
                match (info.qos, info.pid, acks) {
                    (1, Some(pid), Acks::All) => peer.send(&puback(pid)).await,
                    (2, Some(pid), Acks::All) => {
                        peer.send(&pubrec(pid)).await;
                    }
                    _ => {}
                }
                seen.push(info);
            }
            PUBREL if acks == Acks::All => {
                if let Some(pid) = packet_id(&p) {
                    peer.send(&pubcomp(pid)).await;
                }
            }
            PINGREQ => peer.send(&[0xD0, 0x00]).await,
            _ => {}
        }
    }
}

async fn listener() -> (TcpListener, SocketAddr) {
    let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let a = l.local_addr().expect("addr");
    (l, a)
}

async fn accept_connect(l: &TcpListener) -> Option<Peer> {
    let (stream, _) = timeout(W, l.accept()).await.ok()?.ok()?;
    let mut peer = Peer {
        stream,
        buf: Vec::new(),
    };
    let p = peer.read(W).await?;
    (p.kind == CONNECT).then_some(peer)
}

fn persistent_opts(id: &str) -> ConnectOptions {
    ConnectOptions::new(id)
        .with_clean_start(false)
        .with_session_expiry_interval(3600)
        .with_automatic_reconnect(false)
}

async fn connect(client: &MqttClient, l: &TcpListener, addr: SocketAddr, reply: &[u8]) -> Peer {
    let pending = {
        let c = client.clone();
        tokio::spawn(async move { c.connect(&format!("mqtt://{addr}")).await })
    };
    let mut peer = accept_connect(l).await.expect("CONNECT");
    peer.send(reply).await;
    pending.await.expect("join").expect("connect");
    peer
}

async fn lose(client: &MqttClient, peer: Peer) {
    drop(peer);
    let deadline = Instant::now() + W;
    while Instant::now() < deadline {
        if !client.is_connected().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("setup: client did not notice the connection loss");
}

fn qos_opts(qos: QoS, retain: bool) -> PublishOptions {
    PublishOptions {
        qos,
        retain,
        ..Default::default()
    }
}

async fn queue(
    client: &MqttClient,
    topic: &str,
    payload: Vec<u8>,
    qos: QoS,
    retain: bool,
) -> PublishHandle {
    match client
        .publish_with_options(topic, payload, qos_opts(qos, retain))
        .await
    {
        Ok(PublishResult::Queued(handle)) => handle,
        other => panic!("setup: offline publish to {topic} must be queued, got {other:?}"),
    }
}

async fn settled(handle: PublishHandle) -> PublishOutcome {
    timeout(W, handle.outcome())
        .await
        .expect("publish outcome must settle")
}

fn topics(seen: &[Seen]) -> Vec<&str> {
    seen.iter().map(|s| s.topic.as_str()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_retained_publish_rejected_at_enqueue_when_retain_unavailable() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(persistent_opts("oq-ra-enqueue"));
    let peer = connect(
        &client,
        &l,
        addr,
        &connack(false, &prop_byte(PROP_RETAIN_AVAILABLE, 0)),
    )
    .await;
    lose(&client, peer).await;

    let retained = client
        .publish_with_options(
            "oq/retained",
            b"r".to_vec(),
            qos_opts(QoS::AtLeastOnce, true),
        )
        .await;
    assert!(
        matches!(retained, Err(MqttError::RetainNotSupported)),
        "an offline RETAIN publish must be rejected at once when the last CONNACK said Retain Available 0, got {retained:?}"
    );
    let plain = client
        .publish_with_options("oq/plain", b"p".to_vec(), qos_opts(QoS::AtLeastOnce, false))
        .await;
    assert!(
        matches!(plain, Ok(PublishResult::Queued(_))),
        "a non-retained offline publish must still be queued, got {plain:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flush_rejects_retained_message_after_retain_becomes_unavailable() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(persistent_opts("oq-ra-flush"));
    let peer = connect(&client, &l, addr, &connack(false, &[])).await;
    lose(&client, peer).await;

    let retained = queue(
        &client,
        "oq/retained",
        b"r".to_vec(),
        QoS::AtLeastOnce,
        true,
    )
    .await;
    let next = queue(&client, "oq/next", b"n".to_vec(), QoS::AtLeastOnce, false).await;

    let mut peer = connect(
        &client,
        &l,
        addr,
        &connack(true, &prop_byte(PROP_RETAIN_AVAILABLE, 0)),
    )
    .await;
    let seen = observe(&mut peer, Duration::from_millis(800), Acks::All).await;
    assert_eq!(
        topics(&seen),
        vec!["oq/next"],
        "the retained message must not be sent with Retain Available 0; the next one must be"
    );
    assert!(seen.iter().all(|s| !s.retain));

    assert_eq!(
        settled(retained.clone()).await,
        PublishOutcome::Rejected(PublishRejection::RetainNotSupported)
    );
    assert!(matches!(
        settled(next).await,
        PublishOutcome::Delivered(Delivery::AtLeastOnce { .. })
    ));

    let live = {
        let c = client.clone();
        tokio::spawn(async move { c.publish_qos1("oq/live", b"l".to_vec()).await })
    };
    let later = observe(&mut peer, Duration::from_millis(500), Acks::All).await;
    assert_eq!(topics(&later), vec!["oq/live"]);
    assert!(matches!(
        live.await.expect("join"),
        Ok(PublishResult::Sent(Delivery::AtLeastOnce { .. }))
    ));
    assert_eq!(
        retained.try_outcome(),
        Some(PublishOutcome::Rejected(
            PublishRejection::RetainNotSupported
        )),
        "a settled outcome must not be changed by later acknowledgements of reused identifiers"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flush_rejects_oversized_message_after_maximum_packet_size_shrinks() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(persistent_opts("oq-mps-flush"));
    let peer = connect(&client, &l, addr, &connack(false, &[])).await;
    lose(&client, peer).await;

    let big = queue(&client, "oq/big", vec![0u8; 300], QoS::AtLeastOnce, false).await;
    let small = queue(&client, "oq/small", b"s".to_vec(), QoS::AtLeastOnce, false).await;

    let mut peer = connect(
        &client,
        &l,
        addr,
        &connack(true, &prop_u32(PROP_MAXIMUM_PACKET_SIZE, 100)),
    )
    .await;
    let seen = observe(&mut peer, Duration::from_millis(800), Acks::All).await;
    assert_eq!(
        topics(&seen),
        vec!["oq/small"],
        "the oversized message must not be sent; the next one must be"
    );
    assert_eq!(
        settled(big).await,
        PublishOutcome::Rejected(PublishRejection::PacketTooLarge)
    );
    assert!(matches!(
        settled(small).await,
        PublishOutcome::Delivered(Delivery::AtLeastOnce { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flush_downgrades_to_maximum_qos_and_reports_qos_used() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(persistent_opts("oq-maxqos-flush"));
    let peer = connect(&client, &l, addr, &connack(false, &[])).await;
    lose(&client, peer).await;

    let exactly_once = queue(&client, "oq/two", b"2".to_vec(), QoS::ExactlyOnce, false).await;

    let mut peer = connect(
        &client,
        &l,
        addr,
        &connack(true, &prop_byte(PROP_MAXIMUM_QOS, 1)),
    )
    .await;
    let seen = observe(&mut peer, Duration::from_millis(800), Acks::All).await;
    assert_eq!(topics(&seen), vec!["oq/two"]);
    assert_eq!(
        seen[0].qos, 1,
        "the queued QoS 2 message must go out at the new Maximum QoS 1"
    );

    let outcome = settled(exactly_once).await;
    let PublishOutcome::Delivered(delivery) = outcome else {
        panic!("downgraded message must be delivered, got {outcome:?}");
    };
    assert_eq!(delivery.qos_used(), QoS::AtLeastOnce);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flush_downgrade_to_qos0_resolves_unconfirmed() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(persistent_opts("oq-maxqos0-flush"));
    let peer = connect(&client, &l, addr, &connack(false, &[])).await;
    lose(&client, peer).await;

    let at_least_once = queue(&client, "oq/one", b"1".to_vec(), QoS::AtLeastOnce, false).await;

    let mut peer = connect(
        &client,
        &l,
        addr,
        &connack(true, &prop_byte(PROP_MAXIMUM_QOS, 0)),
    )
    .await;
    let seen = observe(&mut peer, Duration::from_millis(800), Acks::None).await;
    assert_eq!(topics(&seen), vec!["oq/one"]);
    assert_eq!(seen[0].qos, 0);
    assert_eq!(
        settled(at_least_once).await,
        PublishOutcome::Delivered(Delivery::Unconfirmed),
        "a message downgraded to QoS 0 settles as written but unconfirmed"
    );
}

async fn flush_unacknowledged(
    client: &MqttClient,
    l: &TcpListener,
    addr: SocketAddr,
    acks: Acks,
    expected: usize,
) -> Vec<Seen> {
    let mut peer = connect(client, l, addr, &connack(true, &[])).await;
    let seen = observe(&mut peer, Duration::from_millis(800), acks).await;
    assert_eq!(
        seen.len(),
        expected,
        "setup: every queued message must be flushed once: {seen:?}"
    );
    lose(client, peer).await;
    seen
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replay_skips_unacked_publishes_that_no_longer_conform() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(persistent_opts("oq-replay-qos1"));
    let peer = connect(&client, &l, addr, &connack(false, &[])).await;
    lose(&client, peer).await;

    let retained = queue(
        &client,
        "oq/retained",
        b"r".to_vec(),
        QoS::AtLeastOnce,
        true,
    )
    .await;
    let big = queue(&client, "oq/big", vec![0u8; 300], QoS::AtLeastOnce, false).await;
    let small = queue(&client, "oq/small", b"s".to_vec(), QoS::AtLeastOnce, false).await;
    flush_unacknowledged(&client, &l, addr, Acks::None, 3).await;

    let mut caps = prop_byte(PROP_RETAIN_AVAILABLE, 0);
    caps.extend(prop_u32(PROP_MAXIMUM_PACKET_SIZE, 100));
    let mut peer = connect(&client, &l, addr, &connack(true, &caps)).await;
    let seen = observe(&mut peer, Duration::from_millis(800), Acks::All).await;
    assert_eq!(
        topics(&seen),
        vec!["oq/small"],
        "only the unacknowledged PUBLISH that still conforms may be replayed: {seen:?}"
    );
    assert!(seen[0].dup, "the replayed PUBLISH must carry DUP=1");

    assert_eq!(
        settled(retained).await,
        PublishOutcome::Indeterminate(IndeterminateReason::ReplayNotConforming)
    );
    assert_eq!(
        settled(big).await,
        PublishOutcome::Indeterminate(IndeterminateReason::ReplayNotConforming)
    );
    assert!(matches!(
        settled(small).await,
        PublishOutcome::Delivered(Delivery::AtLeastOnce { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replay_skips_unacked_qos2_after_maximum_qos_drops() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(persistent_opts("oq-replay-qos2"));
    let peer = connect(&client, &l, addr, &connack(false, &[])).await;
    lose(&client, peer).await;

    let exactly_once = queue(&client, "oq/two", b"2".to_vec(), QoS::ExactlyOnce, false).await;
    let first = flush_unacknowledged(&client, &l, addr, Acks::None, 1).await;
    assert_eq!(first[0].qos, 2);

    let mut peer = connect(
        &client,
        &l,
        addr,
        &connack(true, &prop_byte(PROP_MAXIMUM_QOS, 1)),
    )
    .await;
    let seen = observe(&mut peer, Duration::from_millis(800), Acks::All).await;
    assert!(
        seen.iter().all(|s| s.qos <= 1),
        "a QoS 2 PUBLISH must not be replayed after the server lowered Maximum QoS to 1: {seen:?}"
    );
    assert_eq!(
        settled(exactly_once).await,
        PublishOutcome::Indeterminate(IndeterminateReason::ReplayNotConforming)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_lost_requeues_unacked_qos1_in_order() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(persistent_opts("oq-sp0-qos1"));
    let peer = connect(&client, &l, addr, &connack(false, &[])).await;
    lose(&client, peer).await;

    let first = queue(&client, "oq/1", b"1".to_vec(), QoS::AtLeastOnce, false).await;
    let second = queue(&client, "oq/2", b"2".to_vec(), QoS::AtLeastOnce, false).await;
    flush_unacknowledged(&client, &l, addr, Acks::None, 2).await;
    let third = queue(&client, "oq/3", b"3".to_vec(), QoS::AtLeastOnce, false).await;

    let mut peer = connect(&client, &l, addr, &connack(false, &[])).await;
    let seen = observe(&mut peer, Duration::from_millis(800), Acks::All).await;
    assert_eq!(
        topics(&seen),
        vec!["oq/1", "oq/2", "oq/3"],
        "after Session Present 0 the unacknowledged QoS 1 messages must be re-sent first, in order"
    );
    assert!(
        seen.iter().all(|s| !s.dup),
        "messages re-sent on a new session are new PUBLISH packets (DUP=0): {seen:?}"
    );
    for handle in [first, second, third] {
        assert!(matches!(
            settled(handle).await,
            PublishOutcome::Delivered(Delivery::AtLeastOnce { .. })
        ));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_lost_resolves_unacked_qos2_by_exchange_stage() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(persistent_opts("oq-sp0-qos2"));
    let peer = connect(&client, &l, addr, &connack(false, &[])).await;
    lose(&client, peer).await;

    let unreceived = queue(
        &client,
        "oq/publish",
        b"p".to_vec(),
        QoS::ExactlyOnce,
        false,
    )
    .await;
    let released = queue(&client, "oq/pubrel", b"r".to_vec(), QoS::ExactlyOnce, false).await;
    let mut peer = connect(&client, &l, addr, &connack(true, &[])).await;
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_millis(800);
    while let Some(p) = peer
        .read(deadline.saturating_duration_since(Instant::now()))
        .await
    {
        if let Some(info) = (p.kind == PUBLISH).then(|| parse_publish(&p)).flatten() {
            if let (Some(pid), "oq/pubrel") = (info.pid, info.topic.as_str()) {
                peer.send(&pubrec(pid)).await;
            }
            seen.push(info);
        }
    }
    assert_eq!(seen.len(), 2, "setup: both QoS 2 messages must be flushed");
    lose(&client, peer).await;

    let mut peer = connect(&client, &l, addr, &connack(false, &[])).await;
    let replayed = observe(&mut peer, Duration::from_millis(800), Acks::All).await;
    assert!(
        replayed.is_empty(),
        "unacknowledged QoS 2 messages must not be re-sent on a new session: {replayed:?}"
    );
    assert_eq!(
        settled(unreceived).await,
        PublishOutcome::Indeterminate(IndeterminateReason::SessionLost)
    );
    assert!(
        matches!(
            settled(released).await,
            PublishOutcome::Delivered(Delivery::ExactlyOnce { .. })
        ),
        "after PUBREC Success the server owns the message; losing the session does not make it indeterminate"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clean_start_discards_unacked_session_state_but_keeps_offline_queue() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(
        ConnectOptions::new("oq-clean-start")
            .with_clean_start(true)
            .with_automatic_reconnect(false),
    );
    client.set_queue_on_disconnect(true).await;

    let sent = queue(&client, "oq/sent", b"s".to_vec(), QoS::AtLeastOnce, false).await;
    let mut peer = connect(&client, &l, addr, &connack(false, &[])).await;
    let first = observe(&mut peer, Duration::from_millis(800), Acks::None).await;
    assert_eq!(
        topics(&first),
        vec!["oq/sent"],
        "setup: queued message flushed"
    );
    lose(&client, peer).await;

    let unsent = queue(&client, "oq/unsent", b"u".to_vec(), QoS::AtLeastOnce, false).await;
    let mut peer = connect(&client, &l, addr, &connack(false, &[])).await;
    let seen = observe(&mut peer, Duration::from_millis(800), Acks::All).await;
    assert_eq!(
        topics(&seen),
        vec!["oq/unsent"],
        "Clean Start 1 discards unacknowledged session state [MQTT-3.1.2-4]; never-sent queued messages still go out"
    );
    assert_eq!(
        settled(sent).await,
        PublishOutcome::Indeterminate(IndeterminateReason::SessionDiscarded)
    );
    assert!(matches!(
        settled(unsent).await,
        PublishOutcome::Delivered(Delivery::AtLeastOnce { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flush_respects_receive_maximum_and_settles_each_publish() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(persistent_opts("oq-rm-flush"));
    let peer = connect(&client, &l, addr, &connack(false, &[])).await;
    lose(&client, peer).await;

    let mut handles = Vec::new();
    for i in 0..3u8 {
        handles.push(
            queue(
                &client,
                &format!("oq/{i}"),
                vec![i],
                QoS::AtLeastOnce,
                false,
            )
            .await,
        );
    }
    let mut peer = connect(
        &client,
        &l,
        addr,
        &connack(true, &prop_u16(PROP_RECEIVE_MAXIMUM, 1)),
    )
    .await;
    let unacked = observe(&mut peer, Duration::from_millis(500), Acks::None).await;
    assert_eq!(
        topics(&unacked),
        vec!["oq/0"],
        "only Receive Maximum 1 unacknowledged PUBLISH may be in flight"
    );
    assert_eq!(handles[0].try_outcome(), None);
    if let Some(pid) = unacked[0].pid {
        peer.send(&puback(pid)).await;
    }
    let rest = observe(&mut peer, Duration::from_millis(800), Acks::All).await;
    assert_eq!(topics(&rest), vec!["oq/1", "oq/2"]);
    for handle in handles {
        assert!(matches!(
            settled(handle).await,
            PublishOutcome::Delivered(Delivery::AtLeastOnce { .. })
        ));
    }
}

async fn dropped_client_settles_every_handle_abandoned(receive_maximum: Option<u16>, id: &str) {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(persistent_opts(id));
    let peer = connect(&client, &l, addr, &connack(false, &[])).await;
    lose(&client, peer).await;

    let mut handles = Vec::new();
    for i in 0..3u8 {
        handles.push(
            queue(
                &client,
                &format!("oq/{i}"),
                vec![i],
                QoS::AtLeastOnce,
                false,
            )
            .await,
        );
    }
    let caps = receive_maximum.map_or_else(Vec::new, |rm| prop_u16(PROP_RECEIVE_MAXIMUM, rm));
    let mut peer = connect(&client, &l, addr, &connack(true, &caps)).await;
    let unacked = observe(&mut peer, Duration::from_millis(300), Acks::None).await;
    let expected = if receive_maximum.is_some() { 1 } else { 3 };
    assert_eq!(
        unacked.len(),
        expected,
        "setup: flushed within Receive Maximum"
    );
    lose(&client, peer).await;
    let _ = client.disconnect().await;
    drop(client);

    for (i, handle) in handles.into_iter().enumerate() {
        let got = timeout(Duration::from_secs(2), handle.outcome()).await;
        assert_eq!(
            got.ok(),
            Some(PublishOutcome::Indeterminate(
                IndeterminateReason::Abandoned
            )),
            "handle {i} must settle Abandoned once the client is dropped"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropped_client_after_loss_mid_flush_abandons_every_handle() {
    dropped_client_settles_every_handle_abandoned(Some(1), "oq-drop-mid-flush").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropped_client_after_completed_flush_abandons_every_handle() {
    dropped_client_settles_every_handle_abandoned(None, "oq-drop-flushed").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quota_waiter_fails_fast_when_the_connection_is_lost() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(persistent_opts("oq-quota-loss"));
    let mut peer = connect(
        &client,
        &l,
        addr,
        &connack(false, &prop_u16(PROP_RECEIVE_MAXIMUM, 1)),
    )
    .await;
    let first = {
        let c = client.clone();
        tokio::spawn(async move { c.publish_qos1("live/first", b"1".to_vec()).await })
    };
    let seen = observe(&mut peer, Duration::from_millis(300), Acks::None).await;
    assert_eq!(topics(&seen), vec!["live/first"]);
    let waiting = {
        let c = client.clone();
        tokio::spawn(async move { c.publish_qos1("live/second", b"2".to_vec()).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    lose(&client, peer).await;

    let second = timeout(W, waiting).await;
    assert!(
        matches!(second, Ok(Ok(Err(MqttError::NotConnected)))),
        "a publish waiting for send quota must fail with NotConnected when the connection is lost, got {second:?}"
    );
    assert!(matches!(
        timeout(W, first).await,
        Ok(Ok(Ok(PublishResult::Queued(_))))
    ));
}

async fn live_publish_in_flight_across_loss(session_present: bool, id: &str) -> Vec<Seen> {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(persistent_opts(id));
    let mut peer = connect(&client, &l, addr, &connack(false, &[])).await;

    let publishing = {
        let c = client.clone();
        tokio::spawn(async move {
            c.publish_with_options("live/1", b"x".to_vec(), qos_opts(QoS::AtLeastOnce, false))
                .await
        })
    };
    let first = observe(&mut peer, Duration::from_millis(300), Acks::None).await;
    assert_eq!(topics(&first), vec!["live/1"]);
    lose(&client, peer).await;
    let result = timeout(W, publishing)
        .await
        .expect("publish returns")
        .expect("join");
    let Ok(PublishResult::Queued(handle)) = result else {
        panic!("a publish in flight across a connection loss must return a handle, got {result:?}");
    };
    assert_eq!(handle.try_outcome(), None);

    let mut peer = connect(&client, &l, addr, &connack(session_present, &[])).await;
    let resent = observe(&mut peer, Duration::from_millis(500), Acks::All).await;
    assert_eq!(topics(&resent), vec!["live/1"]);
    assert!(matches!(
        settled(handle).await,
        PublishOutcome::Delivered(Delivery::AtLeastOnce { .. })
    ));
    resent
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_publish_in_flight_across_loss_settles_after_resume() {
    let resent = live_publish_in_flight_across_loss(true, "oq-live-resume").await;
    assert!(resent[0].dup, "resent on the resumed session with DUP=1");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_publish_in_flight_across_loss_settles_after_session_lost() {
    let resent = live_publish_in_flight_across_loss(false, "oq-live-sp0").await;
    assert!(
        !resent[0].dup,
        "re-sent as a new PUBLISH on the new session"
    );
}

async fn hung_broker(peer: &mut Peer, client: &MqttClient) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        let _ = peer.read(Duration::from_millis(50)).await;
        if !client.is_connected().await {
            return;
        }
    }
    panic!("setup: keepalive did not detect the hung broker");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keepalive_loss_fails_quota_waiters_fast() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(
        persistent_opts("oq-ka-quota").with_keep_alive(Duration::from_secs(1)),
    );
    let mut peer = connect(
        &client,
        &l,
        addr,
        &connack(false, &prop_u16(PROP_RECEIVE_MAXIMUM, 1)),
    )
    .await;
    let first = {
        let c = client.clone();
        tokio::spawn(async move { c.publish_qos1("live/first", b"1".to_vec()).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let waiting = {
        let c = client.clone();
        tokio::spawn(async move { c.publish_qos1("live/second", b"2".to_vec()).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    hung_broker(&mut peer, &client).await;
    let second = timeout(W, waiting).await;
    let first = timeout(W, first).await;
    assert!(
        matches!(second, Ok(Ok(Err(MqttError::NotConnected)))),
        "a quota waiter must fail fast after keepalive-detected loss: {second:?}"
    );
    assert!(
        matches!(first, Ok(Ok(Ok(PublishResult::Queued(_))))),
        "the in-flight publish must return its handle promptly after keepalive-detected loss: {first:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keepalive_loss_then_drop_abandons_handles() {
    let (l, addr) = listener().await;
    let client = MqttClient::with_options(
        persistent_opts("oq-ka-drop").with_keep_alive(Duration::from_secs(1)),
    );
    let peer = connect(&client, &l, addr, &connack(false, &[])).await;
    lose(&client, peer).await;
    let mut handles = Vec::new();
    for i in 0..3u8 {
        handles.push(
            queue(
                &client,
                &format!("oq/{i}"),
                vec![i],
                QoS::AtLeastOnce,
                false,
            )
            .await,
        );
    }
    let mut peer = connect(
        &client,
        &l,
        addr,
        &connack(true, &prop_u16(PROP_RECEIVE_MAXIMUM, 1)),
    )
    .await;
    hung_broker(&mut peer, &client).await;
    drop(client);
    for (i, handle) in handles.into_iter().enumerate() {
        let got = timeout(Duration::from_secs(2), handle.outcome()).await;
        let got = got.ok();
        assert_eq!(
            got,
            Some(PublishOutcome::Indeterminate(
                IndeterminateReason::Abandoned
            )),
            "handle {i} after keepalive-detected loss and drop: {got:?}"
        );
    }
    drop(peer);
}
