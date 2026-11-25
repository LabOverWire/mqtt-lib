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

#[derive(Clone, Copy)]
struct Conn {
    remote: usize,
    session_present: bool,
    ids: bool,
    drop_on_subscribe: Option<usize>,
    refuse: Option<(&'static str, u8)>,
    close_after_ms: Option<u64>,
    reject_connect: bool,
}

const BASE: Conn = Conn {
    remote: 0,
    session_present: false,
    ids: true,
    drop_on_subscribe: None,
    refuse: None,
    close_after_ms: None,
    reject_connect: false,
};

#[derive(Default)]
struct Remote {
    sessions: [BTreeMap<String, Option<u32>>; 2],
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
        assert_eq!(prop, 0x0B, "unexpected subscribe property");
        id = Some(u32::try_from(read_varint(body, &mut at)).unwrap());
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

fn connack(conn: &Conn) -> Vec<u8> {
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

struct Script {
    conns: Vec<Conn>,
    retained: [Option<&'static str>; 2],
    publish_on_last: Vec<&'static str>,
}

async fn handle_packet(
    stream: &mut tokio::net::TcpStream,
    remote: &Mutex<Remote>,
    script: &Script,
    conn: &Conn,
    index: usize,
    subscribes: &mut usize,
    (first, body): (u8, &[u8]),
) -> bool {
    match first >> 4 {
        8 => {
            *subscribes += 1;
            let (id, filters) = parse_subscribe(body);
            if conn.drop_on_subscribe == Some(*subscribes) {
                remote
                    .lock()
                    .unwrap()
                    .log
                    .push(format!("conn{index} DROP on SUB {:?}", filters[0].0));
                return false;
            }
            let mut replies = Vec::new();
            let mut codes = Vec::new();
            for (filter, qos, rh) in &filters {
                let refused = conn
                    .refuse
                    .and_then(|(name, code)| (name == filter).then_some(code));
                let mut r = remote.lock().unwrap();
                if let Some(code) = refused {
                    r.log
                        .push(format!("conn{index} SUB {filter} REFUSED {code:#x}"));
                    codes.push(code);
                    continue;
                }
                let existed = r.sessions[conn.remote].insert(filter.clone(), id).is_some();
                r.log.push(format!(
                    "conn{index} r{} SUB {filter} id={id:?} rh={rh} existed={existed}",
                    conn.remote
                ));
                codes.push(*qos);
                if let Some(topic) = script.retained[conn.remote] {
                    if matches(topic, filter) && (*rh == 0 || (*rh == 1 && !existed)) {
                        replies.push(publish_qos0(topic, true, if conn.ids { id } else { None }));
                    }
                }
            }
            for reply in replies {
                stream.write_all(&reply).await.unwrap();
            }
            stream
                .write_all(&ack(0x90, body, codes.into_iter()))
                .await
                .unwrap();
        }
        10 => {
            let filters = parse_unsubscribe(body);
            {
                let mut r = remote.lock().unwrap();
                for filter in &filters {
                    r.sessions[conn.remote].remove(filter);
                    r.log
                        .push(format!("conn{index} r{} UNSUB {filter}", conn.remote));
                }
            }
            stream
                .write_all(&ack(0xB0, body, std::iter::repeat_n(0u8, filters.len())))
                .await
                .unwrap();
        }
        12 => stream.write_all(&[0xD0, 0]).await.unwrap(),
        14 => return false,
        _ => {}
    }
    true
}

fn published_copies(
    remote: &Mutex<Remote>,
    script: &Script,
    conn: Conn,
    index: usize,
) -> Vec<Vec<u8>> {
    let mut r = remote.lock().unwrap();
    let mut packets = Vec::new();
    for topic in &script.publish_on_last {
        let copies: Vec<Vec<u8>> = r.sessions[conn.remote]
            .iter()
            .filter(|(filter, _)| matches(topic, filter))
            .map(|(_, id)| publish_qos0(topic, false, if conn.ids { *id } else { None }))
            .collect();
        r.log.push(format!(
            "conn{index} publish {topic} copies={}",
            copies.len()
        ));
        packets.extend(copies);
    }
    packets
}

async fn scripted_remote(
    listener: tokio::net::TcpListener,
    script: Script,
    remote: Arc<Mutex<Remote>>,
) {
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let mut index = 0usize;
    while let Ok((mut stream, _)) = listener.accept().await {
        let Some((0x10, _)) = read_packet(&mut stream).await else {
            continue;
        };
        let conn = script.conns[index.min(script.conns.len() - 1)];
        let last = index >= script.conns.len() - 1;
        index += 1;
        if conn.reject_connect {
            remote
                .lock()
                .unwrap()
                .log
                .push(format!("conn{index} r{} REJECT", conn.remote));
            continue;
        }
        {
            let mut r = remote.lock().unwrap();
            r.connections += 1;
            if !conn.session_present {
                r.sessions[conn.remote].clear();
            }
            r.log.push(format!(
                "conn{index} r{} sp={} ids={}",
                conn.remote, conn.session_present, conn.ids
            ));
        }
        stream.write_all(&connack(&conn)).await.unwrap();
        let mut subscribes = 0usize;
        let mut published = false;
        let close_at = conn
            .close_after_ms
            .map(|ms| tokio::time::Instant::now() + Duration::from_millis(ms));
        loop {
            let wait = if last && !published && !script.publish_on_last.is_empty() {
                Some(Duration::from_millis(1500))
            } else {
                None
            };
            let next = tokio::select! {
                packet = read_packet(&mut stream) => Some(packet),
                () = async {
                    match close_at {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    remote.lock().unwrap().log.push(format!("conn{index} CLOSE"));
                    break;
                }
                () = async {
                    match wait {
                        Some(d) => tokio::time::sleep(d).await,
                        None => std::future::pending().await,
                    }
                } => None,
            };
            let Some(packet) = next else {
                published = true;
                let packets = published_copies(&remote, &script, conn, index);
                for packet in packets {
                    stream.write_all(&packet).await.unwrap();
                }
                continue;
            };
            let Some((first, body)) = packet else { break };
            if !handle_packet(
                &mut stream,
                &remote,
                &script,
                &conn,
                index,
                &mut subscribes,
                (first, &body),
            )
            .await
            {
                break;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failback_after_interrupted_setup_resubscribes_missing_filter() {
    let primary_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_addr = primary_listener.local_addr().unwrap();
    let backup_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backup_addr = backup_listener.local_addr().unwrap();
    let remote = Arc::new(Mutex::new(Remote::default()));
    let primary = tokio::spawn(scripted_remote(
        primary_listener,
        Script {
            conns: vec![
                Conn {
                    drop_on_subscribe: Some(2),
                    ..BASE
                },
                Conn {
                    reject_connect: true,
                    ..BASE
                },
                Conn {
                    reject_connect: true,
                    ..BASE
                },
                Conn {
                    reject_connect: true,
                    ..BASE
                },
                Conn {
                    session_present: true,
                    ..BASE
                },
            ],
            retained: [None, None],
            publish_on_last: vec!["a/x", "b/x"],
        },
        Arc::clone(&remote),
    ));
    let backup = tokio::spawn(scripted_remote(
        backup_listener,
        Script {
            conns: vec![Conn { remote: 1, ..BASE }],
            retained: [None, None],
            publish_on_last: vec![],
        },
        Arc::clone(&remote),
    ));
    let mut bridge = BridgeConfig::new("session", primary_addr.to_string())
        .add_topic("a/#", BridgeDirection::In, QoS::AtMostOnce)
        .add_topic("b/#", BridgeDirection::In, QoS::AtMostOnce);
    bridge.topics[0].local_prefix = Some("la/".to_string());
    bridge.topics[1].local_prefix = Some("lb/".to_string());
    bridge.backup_brokers = vec![backup_addr.to_string()];
    bridge.enable_failback = true;
    bridge.primary_health_check_interval = Duration::from_millis(1500);
    bridge.initial_reconnect_delay = Duration::from_millis(200);
    bridge.first_retry_delay = Duration::from_millis(100);
    bridge.max_reconnect_delay = Duration::from_millis(300);
    bridge.retry_jitter = false;
    let mut local_config = broker_config(free_port());
    local_config.bridges = vec![bridge];
    let local = start_broker(local_config).await;
    let client = MqttClient::new("local");
    client
        .connect(&format!("mqtt://{}", local.addr))
        .await
        .unwrap();
    let received = subscribe_all(&client).await;
    let started = tokio::time::Instant::now();
    while !remote
        .lock()
        .unwrap()
        .log
        .iter()
        .any(|line| line.contains("publish b/x"))
        && started.elapsed() < Duration::from_secs(25)
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let log = remote.lock().unwrap().log.clone();
    let _ = client.disconnect().await;
    local.stop().await;
    primary.abort();
    backup.abort();
    let a = count(&received, "la/a/x");
    let b = count(&received, "lb/b/x");
    assert_eq!(
        (a, b),
        (1, 1),
        "la/a/x={a} lb/b/x={b}; remote log: {log:#?}"
    );
}

async fn failback_case(
    mappings: &[(&str, &str)],
    retained: &'static str,
    backup_ids: bool,
    publish_on_last: Vec<&'static str>,
    done: &str,
) -> (Arc<Mutex<Vec<String>>>, Vec<String>) {
    let primary_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_addr = primary_listener.local_addr().unwrap();
    let backup_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backup_addr = backup_listener.local_addr().unwrap();
    let remote = Arc::new(Mutex::new(Remote::default()));
    let rejected = Conn {
        reject_connect: true,
        ..BASE
    };
    let primary = tokio::spawn(scripted_remote(
        primary_listener,
        Script {
            conns: vec![
                Conn {
                    close_after_ms: Some(1500),
                    ..BASE
                },
                rejected,
                rejected,
                rejected,
                Conn {
                    session_present: true,
                    ..BASE
                },
            ],
            retained: [Some(retained), None],
            publish_on_last,
        },
        Arc::clone(&remote),
    ));
    let backup = tokio::spawn(scripted_remote(
        backup_listener,
        Script {
            conns: vec![Conn {
                remote: 1,
                ids: backup_ids,
                ..BASE
            }],
            retained: [None, None],
            publish_on_last: vec![],
        },
        Arc::clone(&remote),
    ));
    let mut bridge = BridgeConfig::new("session", primary_addr.to_string());
    for (filter, prefix) in mappings {
        bridge = bridge.add_topic(*filter, BridgeDirection::In, QoS::AtMostOnce);
        if let Some(mapping) = bridge.topics.last_mut() {
            mapping.local_prefix = Some((*prefix).to_string());
        }
    }
    bridge.backup_brokers = vec![backup_addr.to_string()];
    bridge.enable_failback = true;
    bridge.primary_health_check_interval = Duration::from_millis(1500);
    bridge.initial_reconnect_delay = Duration::from_millis(200);
    bridge.first_retry_delay = Duration::from_millis(100);
    bridge.max_reconnect_delay = Duration::from_millis(300);
    bridge.retry_jitter = false;
    let mut local_config = broker_config(free_port());
    local_config.bridges = vec![bridge];
    let local = start_broker(local_config).await;
    let client = MqttClient::new("local");
    client
        .connect(&format!("mqtt://{}", local.addr))
        .await
        .unwrap();
    let received = subscribe_all(&client).await;
    let started = tokio::time::Instant::now();
    while !remote
        .lock()
        .unwrap()
        .log
        .iter()
        .any(|line| line.contains(done))
        && started.elapsed() < Duration::from_secs(25)
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let log = remote.lock().unwrap().log.clone();
    let _ = client.disconnect().await;
    local.stop().await;
    primary.abort();
    backup.abort();
    (received, log)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_mode_failback_retained_count() {
    let (received, log) = failback_case(
        &[("a/#", "la/"), ("b/#", "lb/")],
        "a/r",
        true,
        vec!["a/x", "b/x"],
        "publish b/x",
    )
    .await;
    let a = count(&received, "la/a/r");
    let b = count(&received, "lb/b/x");
    assert_eq!(
        (a, b),
        (1, 1),
        "la/a/r={a} lb/b/x={b}; remote log: {log:#?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failback_from_a_backup_without_identifiers_does_not_resend_retained() {
    let (received, log) = failback_case(
        &[("sensors/#", "l1/"), ("sensors/+/temp", "l2/")],
        "sensors/x/temp",
        false,
        vec![],
        "conn5 r0 sp=true",
    )
    .await;
    let one = count(&received, "l1/sensors/x/temp");
    let two = count(&received, "l2/sensors/x/temp");
    assert_eq!(
        (one, two),
        (1, 1),
        "l1={one} l2={two}; remote log: {log:#?}"
    );
}

async fn local_with_bridge(remote: SocketAddr) -> RunningBroker {
    let mut bridge = BridgeConfig::new("restart", remote.to_string()).add_topic(
        "r/#",
        BridgeDirection::In,
        QoS::AtLeastOnce,
    );
    bridge.initial_reconnect_delay = Duration::from_millis(200);
    bridge.first_retry_delay = Duration::from_millis(200);
    bridge.max_reconnect_delay = Duration::from_millis(300);
    let mut config = broker_config(free_port());
    config.bridges = vec![bridge];
    start_broker(config).await
}

async fn wait_for_topic(received: &Arc<Mutex<Vec<String>>>, topic: &str) -> bool {
    let started = tokio::time::Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        if count(received, topic) > 0 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restarted_local_broker_gets_remote_retained_again() {
    let remote = start_broker(
        BrokerConfig::default()
            .with_bind_address(free_port())
            .with_max_clients(100)
            .with_storage(StorageConfig {
                backend: mqtt5::broker::config::StorageBackend::Memory,
                enable_persistence: true,
                ..Default::default()
            }),
    )
    .await;
    let publisher = MqttClient::new("pub");
    publisher
        .connect(&format!("mqtt://{}", remote.addr))
        .await
        .unwrap();
    publisher
        .publish_retain("r/state", b"on".to_vec())
        .await
        .unwrap();

    let first = local_with_bridge(remote.addr).await;
    let watcher = MqttClient::new("watch-1");
    watcher
        .connect(&format!("mqtt://{}", first.addr))
        .await
        .unwrap();
    let seen_first = subscribe_all(&watcher).await;
    assert!(
        wait_for_topic(&seen_first, "r/state").await,
        "first local broker never got the retained message"
    );
    let _ = watcher.disconnect().await;
    first.stop().await;

    let second = local_with_bridge(remote.addr).await;
    let live_watch = MqttClient::new("watch-2");
    live_watch
        .connect(&format!("mqtt://{}", second.addr))
        .await
        .unwrap();
    let live = subscribe_all(&live_watch).await;
    let started = tokio::time::Instant::now();
    while count(&live, "r/probe") == 0 && started.elapsed() < Duration::from_secs(10) {
        publisher
            .publish_qos1("r/probe", b"x".to_vec())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(count(&live, "r/probe") > 0, "bridge never reconnected");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let late = MqttClient::new("watch-3");
    late.connect(&format!("mqtt://{}", second.addr))
        .await
        .unwrap();
    let seen_second = subscribe_all(&late).await;
    let got = wait_for_topic(&seen_second, "r/state").await;
    let _ = late.disconnect().await;
    let _ = live_watch.disconnect().await;
    let _ = publisher.disconnect().await;
    second.stop().await;
    remote.stop().await;
    assert!(
        got,
        "restarted local broker never got the remote's retained r/state"
    );
}

#[cfg(feature = "transport-quic")]
async fn quic_overlap_case(strategy: mqtt5::transport::StreamStrategy) -> (usize, usize) {
    use mqtt5::broker::bridge::BridgeProtocol;
    use mqtt5::broker::config::QuicConfig;
    use std::path::PathBuf;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut remote_config = broker_config(free_port()).with_quic(
        QuicConfig::new(
            PathBuf::from("../../test_certs/server.pem"),
            PathBuf::from("../../test_certs/server.key"),
        )
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap()),
    );
    remote_config.subscription_identifier_available = false;
    let mut broker = MqttBroker::with_config(remote_config).await.unwrap();
    let tcp_addr = broker.local_addr().unwrap();
    let quic_addr = broker.quic_local_addr().unwrap();
    let mut ready = broker.ready_receiver();
    let shutdown = broker.shutdown_handle();
    let task = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    while !*ready.borrow_and_update() {
        ready.changed().await.unwrap();
    }
    let remote = RunningBroker {
        addr: tcp_addr,
        task,
        shutdown,
    };
    let mut bridge = BridgeConfig::new("quicover", quic_addr.to_string())
        .add_topic("sensors/#", BridgeDirection::In, QoS::AtLeastOnce)
        .add_topic("sensors/+/temp", BridgeDirection::In, QoS::AtLeastOnce);
    bridge.topics[0].local_prefix = Some("l1/".to_string());
    bridge.topics[1].local_prefix = Some("l2/".to_string());
    bridge.protocol = BridgeProtocol::Quic;
    bridge.insecure = Some(true);
    bridge.quic_stream_strategy = Some(strategy);
    let mut config = broker_config(free_port());
    config.bridges = vec![bridge];
    let local = start_broker(config).await;
    let watcher = MqttClient::new("quic-watch");
    watcher
        .connect(&format!("mqtt://{}", local.addr))
        .await
        .unwrap();
    let received = subscribe_all(&watcher).await;
    let publisher = MqttClient::new("quic-pub");
    publisher
        .connect(&format!("mqtt://{}", remote.addr))
        .await
        .unwrap();
    let started = tokio::time::Instant::now();
    while count(&received, "l1/sensors/probe") == 0 && started.elapsed() < Duration::from_secs(10) {
        publisher
            .publish_qos1("sensors/probe", b"x".to_vec())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    publisher
        .publish_qos1("sensors/x/temp", b"t".to_vec())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let result = (
        count(&received, "l1/sensors/x/temp"),
        count(&received, "l2/sensors/x/temp"),
    );
    let _ = publisher.disconnect().await;
    let _ = watcher.disconnect().await;
    local.stop().await;
    remote.stop().await;
    result
}

#[cfg(feature = "transport-quic")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quic_without_identifiers_delivers_each_mapping_once() {
    for strategy in [
        mqtt5::transport::StreamStrategy::ControlOnly,
        mqtt5::transport::StreamStrategy::DataPerTopic,
        mqtt5::transport::StreamStrategy::DataPerPublish,
    ] {
        let got = quic_overlap_case(strategy).await;
        assert_eq!(got, (1, 1), "{strategy:?}");
    }
}
