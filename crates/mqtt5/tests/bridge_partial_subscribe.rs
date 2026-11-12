#![cfg(feature = "broker")]
use mqtt5::broker::bridge::{BridgeConfig, BridgeDirection};
use mqtt5::broker::{BrokerConfig, BrokerShutdownHandle, MqttBroker, StorageConfig};
use mqtt5::client::MqttClient;
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinHandle;

fn broker_config(bind: SocketAddr) -> BrokerConfig {
    BrokerConfig::default()
        .with_bind_address(bind)
        .with_max_clients(100)
        .with_storage(StorageConfig::new().with_persistence(false))
}

struct RunningBroker {
    addr: SocketAddr,
    task: JoinHandle<()>,
    shutdown: BrokerShutdownHandle,
}

impl RunningBroker {
    async fn stop(self) {
        self.shutdown.shutdown();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.task).await;
    }
}

async fn start_broker(config: BrokerConfig) -> RunningBroker {
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let addr = broker.local_addr().unwrap();
    let mut ready = broker.ready_receiver();
    let shutdown = broker.shutdown_handle();
    let task = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    while !*ready.borrow_and_update() {
        ready.changed().await.unwrap();
    }
    RunningBroker {
        addr,
        task,
        shutdown,
    }
}

fn free_port() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

async fn subscribe_all(client: &MqttClient) -> Arc<Mutex<Vec<String>>> {
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    client
        .subscribe_with_options(
            "#",
            mqtt5::SubscribeOptions {
                qos: QoS::ExactlyOnce,
                ..Default::default()
            },
            move |msg| sink.lock().unwrap().push(msg.topic.clone()),
        )
        .await
        .unwrap();
    received
}

fn count(received: &Arc<Mutex<Vec<String>>>, topic: &str) -> usize {
    received
        .lock()
        .unwrap()
        .iter()
        .filter(|t| *t == topic)
        .count()
}

fn fast(mut config: BridgeConfig) -> BridgeConfig {
    config.initial_reconnect_delay = Duration::from_millis(200);
    config.first_retry_delay = Duration::from_millis(200);
    config.max_reconnect_delay = Duration::from_millis(300);
    config.retry_jitter = false;
    config
}

#[derive(Clone, Copy)]
struct Conn {
    session_present: bool,
    ids: bool,
    close_after_subscribes: Option<usize>,
    drop_on_subscribe: Option<usize>,
}

#[derive(Default)]
struct Remote {
    session: BTreeMap<String, Option<u32>>,
    log: Vec<String>,
    connections: usize,
}

fn read_varint(body: &[u8], at: &mut usize) -> usize {
    let mut value = 0usize;
    let mut shift = 0;
    loop {
        let byte = body[*at];
        *at += 1;
        value |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return value;
        }
        shift += 7;
    }
}

fn write_varint(out: &mut Vec<u8>, mut value: usize) {
    loop {
        let mut byte = u8::try_from(value & 0x7f).unwrap();
        value >>= 7;
        if value > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            return;
        }
    }
}

async fn read_packet(stream: &mut tokio::net::TcpStream) -> Option<(u8, Vec<u8>)> {
    let first = stream.read_u8().await.ok()?;
    let mut length = 0usize;
    let mut shift = 0;
    loop {
        let byte = stream.read_u8().await.ok()?;
        length |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).await.ok()?;
    Some((first, body))
}

fn parse_subscribe(body: &[u8]) -> (Option<u32>, Vec<(String, u8, u8)>) {
    let mut at = 2;
    let props_len = read_varint(body, &mut at);
    let props_end = at + props_len;
    let mut id = None;
    while at < props_end {
        let prop = body[at];
        at += 1;
        if prop == 0x0B {
            id = Some(u32::try_from(read_varint(body, &mut at)).unwrap());
        } else {
            panic!("unexpected subscribe property {prop}");
        }
    }
    let mut filters = Vec::new();
    while at < body.len() {
        let len = usize::from(u16::from_be_bytes([body[at], body[at + 1]]));
        at += 2;
        let filter = String::from_utf8(body[at..at + len].to_vec()).unwrap();
        at += len;
        let options = body[at];
        at += 1;
        filters.push((filter, options & 0x03, (options >> 4) & 0x03));
    }
    (id, filters)
}

fn parse_unsubscribe(body: &[u8]) -> Vec<String> {
    let mut at = 2;
    let props_len = read_varint(body, &mut at);
    at += props_len;
    let mut filters = Vec::new();
    while at < body.len() {
        let len = usize::from(u16::from_be_bytes([body[at], body[at + 1]]));
        at += 2;
        filters.push(String::from_utf8(body[at..at + len].to_vec()).unwrap());
        at += len;
    }
    filters
}

fn matches(topic: &str, filter: &str) -> bool {
    let mut t = topic.split('/');
    let mut f = filter.split('/');
    loop {
        match (t.next(), f.next()) {
            (_, Some("#")) | (None, None) => return true,
            (Some(a), Some(b)) if a == b || b == "+" => {}
            _ => return false,
        }
    }
}

fn publish_qos0(topic: &str, retain: bool, id: Option<u32>) -> Vec<u8> {
    let mut props = Vec::new();
    if let Some(id) = id {
        props.push(0x0B);
        write_varint(&mut props, usize::try_from(id).unwrap());
    }
    let mut body = Vec::new();
    body.extend_from_slice(&u16::try_from(topic.len()).unwrap().to_be_bytes());
    body.extend_from_slice(topic.as_bytes());
    write_varint(&mut body, props.len());
    body.extend_from_slice(&props);
    body.extend_from_slice(b"p");
    let mut packet = vec![0x30 | u8::from(retain)];
    write_varint(&mut packet, body.len());
    packet.extend_from_slice(&body);
    packet
}

fn copies_for(remote: &Remote, topic: &str, ids: bool, retain: bool) -> Vec<Vec<u8>> {
    remote
        .session
        .iter()
        .filter(|(filter, _)| matches(topic, filter))
        .map(|(_, id)| publish_qos0(topic, retain, if ids { *id } else { None }))
        .collect()
}

fn connack(conn: Conn) -> Vec<u8> {
    let mut connack = vec![0x20];
    let mut body = vec![u8::from(conn.session_present), 0];
    if conn.ids {
        body.push(0);
    } else {
        body.extend_from_slice(&[2, 0x29, 0]);
    }
    write_varint(&mut connack, body.len());
    connack.extend_from_slice(&body);
    connack
}

fn ack(header: u8, request: &[u8], codes: impl Iterator<Item = u8>) -> Vec<u8> {
    let mut packet = vec![header];
    let mut body = vec![request[0], request[1], 0];
    body.extend(codes);
    write_varint(&mut packet, body.len());
    packet.extend_from_slice(&body);
    packet
}

fn subscribe(
    remote: &Mutex<Remote>,
    index: usize,
    conn: Conn,
    id: Option<u32>,
    filters: &[(String, u8, u8)],
    retained: Option<&str>,
) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();
    for (filter, _, rh) in filters {
        let existed = {
            let mut r = remote.lock().unwrap();
            let existed = r.session.insert(filter.clone(), id).is_some();
            r.log.push(format!(
                "conn{index} SUB {filter} id={id:?} rh={rh} existed={existed}"
            ));
            existed
        };
        if let Some(topic) = retained {
            if matches(topic, filter) && (*rh == 0 || (*rh == 1 && !existed)) {
                packets.push(publish_qos0(topic, true, if conn.ids { id } else { None }));
            }
        }
    }
    packets
}

fn published_copies(remote: &Mutex<Remote>, index: usize, topic: &str, ids: bool) -> Vec<Vec<u8>> {
    let mut r = remote.lock().unwrap();
    let packets = copies_for(&r, topic, ids, false);
    r.log.push(format!(
        "conn{index} publish {topic} copies={}",
        packets.len()
    ));
    packets
}

async fn scripted_remote(
    listener: tokio::net::TcpListener,
    script: Vec<Conn>,
    retained: Option<String>,
    publish_on_last: Option<String>,
    remote: Arc<Mutex<Remote>>,
) {
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let mut index = 0usize;
    while let Ok((mut stream, _)) = listener.accept().await {
        let conn = script[index.min(script.len() - 1)];
        let last = index >= script.len() - 1;
        index += 1;
        let Some((0x10, _)) = read_packet(&mut stream).await else {
            continue;
        };
        {
            let mut r = remote.lock().unwrap();
            r.connections = index;
            if !conn.session_present {
                r.session.clear();
            }
            r.log.push(format!(
                "conn{index} sp={} ids={}",
                conn.session_present, conn.ids
            ));
        }
        stream.write_all(&connack(conn)).await.unwrap();
        let mut subscribes = 0usize;
        let mut published = false;
        loop {
            let next = if last && !published && publish_on_last.is_some() {
                if let Ok(packet) =
                    tokio::time::timeout(Duration::from_millis(1500), read_packet(&mut stream))
                        .await
                {
                    packet
                } else {
                    published = true;
                    let topic = publish_on_last.clone().unwrap();
                    let packets = published_copies(&remote, index, &topic, conn.ids);
                    for packet in packets {
                        stream.write_all(&packet).await.unwrap();
                    }
                    continue;
                }
            } else {
                read_packet(&mut stream).await
            };
            let Some((first, body)) = next else { break };
            match first >> 4 {
                8 => {
                    subscribes += 1;
                    let (id, filters) = parse_subscribe(&body);
                    if conn.drop_on_subscribe == Some(subscribes) {
                        remote.lock().unwrap().log.push(format!(
                            "conn{index} DROP on subscribe {:?}",
                            filters.first().map(|f| f.0.clone())
                        ));
                        break;
                    }
                    let replies =
                        subscribe(&remote, index, conn, id, &filters, retained.as_deref());
                    for reply in replies {
                        stream.write_all(&reply).await.unwrap();
                    }
                    stream
                        .write_all(&ack(0x90, &body, filters.iter().map(|(_, qos, _)| *qos)))
                        .await
                        .unwrap();
                    if conn.close_after_subscribes == Some(subscribes) {
                        tokio::time::sleep(Duration::from_millis(400)).await;
                        remote
                            .lock()
                            .unwrap()
                            .log
                            .push(format!("conn{index} CLOSE"));
                        break;
                    }
                }
                10 => {
                    let filters = parse_unsubscribe(&body);
                    {
                        let mut r = remote.lock().unwrap();
                        for filter in &filters {
                            r.session.remove(filter);
                            r.log.push(format!("conn{index} UNSUB {filter}"));
                        }
                    }
                    stream
                        .write_all(&ack(0xB0, &body, std::iter::repeat_n(0u8, filters.len())))
                        .await
                        .unwrap();
                }
                12 => stream.write_all(&[0xD0, 0]).await.unwrap(),
                14 => break,
                _ => {}
            }
        }
    }
}

async fn run_case(
    mappings: &[(&str, &str)],
    script: Vec<Conn>,
    retained: Option<&str>,
    publish_on_last: Option<&str>,
    settle: Duration,
) -> (
    Arc<Mutex<Vec<String>>>,
    Vec<String>,
    BTreeMap<String, Option<u32>>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let remote = Arc::new(Mutex::new(Remote::default()));
    let script_len = script.len();
    let server = tokio::spawn(scripted_remote(
        listener,
        script,
        retained.map(str::to_string),
        publish_on_last.map(str::to_string),
        Arc::clone(&remote),
    ));
    let mut bridge = BridgeConfig::new("partial", addr.to_string());
    for (pattern, _) in mappings {
        bridge = bridge.add_topic(*pattern, BridgeDirection::In, QoS::AtMostOnce);
    }
    for (index, (_, prefix)) in mappings.iter().enumerate() {
        bridge.topics[index].local_prefix = Some((*prefix).to_string());
    }
    let mut local_config = broker_config(free_port());
    local_config.bridges = vec![fast(bridge)];
    let local = start_broker(local_config).await;
    let client = MqttClient::new("local");
    client
        .connect(&format!("mqtt://{}", local.addr))
        .await
        .unwrap();
    let received = subscribe_all(&client).await;
    let started = tokio::time::Instant::now();
    while remote.lock().unwrap().connections < script_len
        && started.elapsed() < Duration::from_secs(15)
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(settle).await;
    let (log, session) = {
        let r = remote.lock().unwrap();
        (r.log.clone(), r.session.clone())
    };
    let _ = client.disconnect().await;
    local.stop().await;
    server.abort();
    (received, log, session)
}

const PLAIN_IDS: Conn = Conn {
    session_present: false,
    ids: true,
    close_after_subscribes: None,
    drop_on_subscribe: None,
};

async fn partial_switch_then_back(third_session_present: bool) -> (usize, usize, Vec<String>) {
    let (received, log, _) = run_case(
        &[
            ("x/1", "one/"),
            ("x/#", "all/"),
            ("y/1", "yone/"),
            ("y/#", "yall/"),
        ],
        vec![
            Conn {
                close_after_subscribes: Some(4),
                ..PLAIN_IDS
            },
            Conn {
                session_present: true,
                ids: false,
                close_after_subscribes: None,
                drop_on_subscribe: Some(2),
            },
            Conn {
                session_present: third_session_present,
                ..PLAIN_IDS
            },
        ],
        None,
        Some("x/1"),
        Duration::from_millis(3000),
    )
    .await;
    (
        count(&received, "one/x/1"),
        count(&received, "all/x/1"),
        log,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_switch_to_no_ids_then_back_session_present() {
    let (one, all, log) = partial_switch_then_back(true).await;
    assert_eq!(
        (one, all),
        (1, 1),
        "one/x/1={one} all/x/1={all}; remote log: {log:#?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_switch_to_no_ids_then_back_session_lost() {
    let (one, all, log) = partial_switch_then_back(false).await;
    assert_eq!(
        (one, all),
        (1, 1),
        "one/x/1={one} all/x/1={all}; remote log: {log:#?}"
    );
}

async fn partial_first_setup_retained(second_session_present: bool) -> (usize, Vec<String>) {
    let (received, log, _) = run_case(
        &[("a/#", "la/"), ("b/#", "lb/")],
        vec![
            Conn {
                drop_on_subscribe: Some(2),
                ..PLAIN_IDS
            },
            Conn {
                session_present: second_session_present,
                ..PLAIN_IDS
            },
        ],
        Some("a/r"),
        None,
        Duration::from_millis(2000),
    )
    .await;
    (count(&received, "la/a/r"), log)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_first_setup_session_lost_retained_once_per_connection() {
    let (copies, log) = partial_first_setup_retained(false).await;
    assert!(
        copies <= 2,
        "retained copies={copies} (1 on conn1 + at most 1 on the new session); remote log: {log:#?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_first_setup_session_present_retained_once() {
    let (copies, log) = partial_first_setup_retained(true).await;
    assert_eq!(copies, 1, "retained copies={copies}; remote log: {log:#?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_first_setup_then_no_ids_leaves_no_covered_subscription() {
    let (received, log, session) = run_case(
        &[("a/1", "one/"), ("a/#", "all/")],
        vec![
            Conn {
                drop_on_subscribe: Some(2),
                ..PLAIN_IDS
            },
            Conn {
                session_present: true,
                ids: false,
                close_after_subscribes: None,
                drop_on_subscribe: None,
            },
        ],
        None,
        Some("a/1"),
        Duration::from_millis(3000),
    )
    .await;
    let one = count(&received, "one/a/1");
    let all = count(&received, "all/a/1");
    assert_eq!(
        (one, all),
        (1, 1),
        "one/a/1={one} all/a/1={all}; remote session {session:?}; log: {log:#?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_retained_once_without_drop() {
    let (received, log, _) = run_case(
        &[("a/#", "la/"), ("b/#", "lb/")],
        vec![PLAIN_IDS],
        Some("a/r"),
        None,
        Duration::from_millis(2000),
    )
    .await;
    let copies = count(&received, "la/a/r");
    assert_eq!(copies, 1, "retained copies={copies}; remote log: {log:#?}");
}

fn remote_config(bind: SocketAddr, ids: bool) -> BrokerConfig {
    let mut config = broker_config(bind);
    config.subscription_identifier_available = ids;
    config
}

async fn real_remote_switch(
    mappings: &[(&str, &str)],
    first_ids: bool,
    second_ids: bool,
    retained: Option<&str>,
    publish: Option<&str>,
) -> Arc<Mutex<Vec<String>>> {
    let bind = free_port();
    let remote = start_broker(remote_config(bind, first_ids)).await;
    if let Some(topic) = retained {
        let publisher = MqttClient::new("pre");
        publisher.connect(&format!("mqtt://{bind}")).await.unwrap();
        publisher
            .publish_retain(topic, b"r".to_vec())
            .await
            .unwrap();
        publisher.disconnect().await.unwrap();
    }
    let mut bridge = BridgeConfig::new("partial-real", bind.to_string());
    for (pattern, _) in mappings {
        bridge = bridge.add_topic(*pattern, BridgeDirection::In, QoS::AtLeastOnce);
    }
    for (index, (_, prefix)) in mappings.iter().enumerate() {
        bridge.topics[index].local_prefix = Some((*prefix).to_string());
    }
    bridge.first_retry_delay = Duration::from_millis(1000);
    bridge.initial_reconnect_delay = Duration::from_millis(1000);
    bridge.max_reconnect_delay = Duration::from_millis(1000);
    bridge.retry_jitter = false;
    let mut local_config = broker_config(free_port());
    local_config.bridges = vec![bridge];
    let local = start_broker(local_config).await;
    let client = MqttClient::new("real-local");
    client
        .connect(&format!("mqtt://{}", local.addr))
        .await
        .unwrap();
    let received = subscribe_all(&client).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    remote.stop().await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let remote = start_broker(remote_config(bind, second_ids)).await;
    if let Some(topic) = retained {
        let publisher = MqttClient::new("pre2");
        publisher.connect(&format!("mqtt://{bind}")).await.unwrap();
        publisher
            .publish_retain(topic, b"r".to_vec())
            .await
            .unwrap();
        publisher.disconnect().await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(3000)).await;
    if let Some(topic) = publish {
        let publisher = MqttClient::new("pub");
        publisher.connect(&format!("mqtt://{bind}")).await.unwrap();
        publisher.publish_qos1(topic, b"x".to_vec()).await.unwrap();
        publisher.disconnect().await.unwrap();
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
    let _ = client.disconnect().await;
    local.stop().await;
    remote.stop().await;
    received
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_session_losing_identifiers_retained_once() {
    let received = real_remote_switch(&[("s/#", "l/")], true, false, Some("s/r"), None).await;
    let all = received.lock().unwrap().clone();
    assert_eq!(
        count(&received, "l/s/r"),
        2,
        "one per remote lifetime; got {all:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsubscribe_covered_exact_keeps_shared_and_cover_delivering() {
    let received = real_remote_switch(
        &[("$share/g/a/b", "g/"), ("a/b", "ab/"), ("a/+", "ap/")],
        true,
        false,
        None,
        Some("a/b"),
    )
    .await;
    let all = received.lock().unwrap().clone();
    assert!(
        count(&received, "g/a/b") >= 1
            && count(&received, "ab/a/b") >= 1
            && count(&received, "ap/a/b") >= 1,
        "got {all:?}"
    );
}
