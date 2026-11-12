#![cfg(feature = "broker")]
use mqtt5::broker::bridge::BridgeConnection;
use mqtt5::broker::bridge::{BridgeConfig, BridgeDirection};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::{BrokerConfig, BrokerShutdownHandle, MqttBroker, StorageConfig};
use mqtt5::client::MqttClient;
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
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

#[derive(Clone)]
struct Conn {
    remote: usize,
    session_present: bool,
    ids: bool,
    drop_on_subscribe: Option<usize>,
    refuse: Option<(&'static str, u8)>,
    close_after_ms: Option<u64>,
    silent_unsubscribe: bool,
}

const BASE: Conn = Conn {
    remote: 0,
    session_present: false,
    ids: true,
    drop_on_subscribe: None,
    refuse: None,
    close_after_ms: None,
    silent_unsubscribe: false,
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
            if conn.silent_unsubscribe {
                let mut r = remote.lock().unwrap();
                for filter in &filters {
                    r.log.push(format!(
                        "conn{index} r{} UNSUB {filter} IGNORED",
                        conn.remote
                    ));
                }
                return true;
            }
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

async fn scripted_remote(
    listener: tokio::net::TcpListener,
    script: Script,
    remote: Arc<Mutex<Remote>>,
) {
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let mut index = 0usize;
    while let Ok((mut stream, _)) = listener.accept().await {
        let conn = script.conns[index.min(script.conns.len() - 1)].clone();
        let last = index >= script.conns.len() - 1;
        index += 1;
        let Some((0x10, _)) = read_packet(&mut stream).await else {
            continue;
        };
        {
            let mut r = remote.lock().unwrap();
            r.connections = index;
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
                let packets: Vec<Vec<u8>> = {
                    let mut r = remote.lock().unwrap();
                    let mut packets = Vec::new();
                    for topic in &script.publish_on_last {
                        let copies: Vec<Vec<u8>> = r.sessions[conn.remote]
                            .iter()
                            .filter(|(filter, _)| matches(topic, filter))
                            .map(|(_, id)| {
                                publish_qos0(topic, false, if conn.ids { *id } else { None })
                            })
                            .collect();
                        r.log.push(format!(
                            "conn{index} publish {topic} copies={}",
                            copies.len()
                        ));
                        packets.extend(copies);
                    }
                    packets
                };
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

async fn run_case(
    mappings: &[(&str, &str)],
    script: Script,
    settle: Duration,
) -> (Arc<Mutex<Vec<String>>>, Vec<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let remote = Arc::new(Mutex::new(Remote::default()));
    let script_len = script.conns.len();
    let server = tokio::spawn(scripted_remote(listener, script, Arc::clone(&remote)));
    let mut bridge = BridgeConfig::new("session", addr.to_string());
    for (pattern, _) in mappings {
        bridge = bridge.add_topic(*pattern, BridgeDirection::In, QoS::AtMostOnce);
    }
    for (index, (_, prefix)) in mappings.iter().enumerate() {
        bridge.topics[index].local_prefix = Some((*prefix).to_string());
    }
    bridge.initial_reconnect_delay = Duration::from_millis(200);
    bridge.first_retry_delay = Duration::from_millis(200);
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
    while remote.lock().unwrap().connections < script_len
        && started.elapsed() < Duration::from_secs(20)
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(settle).await;
    let log = remote.lock().unwrap().log.clone();
    let _ = client.disconnect().await;
    local.stop().await;
    server.abort();
    (received, log)
}

fn conn(session_present: bool, ids: bool) -> Conn {
    Conn {
        session_present,
        ids,
        ..BASE
    }
}

async fn stale_unsubscribe_case(silent: bool) -> (Arc<Mutex<Vec<String>>>, Vec<String>) {
    run_case(
        &[("s/#", "a/"), ("s/+/t", "b/")],
        Script {
            conns: vec![
                Conn {
                    close_after_ms: Some(2000),
                    ..conn(false, true)
                },
                Conn {
                    silent_unsubscribe: silent,
                    close_after_ms: Some(13000),
                    ..conn(true, false)
                },
                conn(true, false),
            ],
            retained: [None, None],
            publish_on_last: vec!["s/x/t"],
        },
        Duration::from_secs(4),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_unsubscribe_that_times_out_is_sent_again_on_the_next_connection() {
    let (received, log) = stale_unsubscribe_case(true).await;
    let a = count(&received, "a/s/x/t");
    let b = count(&received, "b/s/x/t");
    assert!(
        a == 1 && b == 1,
        "each mapping should get one copy: a = {a}, b = {b}\nremote log:\n{}",
        log.join("\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_stale_unsubscribe_answered() {
    let (received, log) = stale_unsubscribe_case(false).await;
    let a = count(&received, "a/s/x/t");
    let b = count(&received, "b/s/x/t");
    assert!(
        a == 1 && b == 1,
        "each mapping should get one copy: a = {a}, b = {b}\nremote log:\n{}",
        log.join("\n")
    );
}

struct RefusingRemote {
    connections: AtomicUsize,
    open: AtomicUsize,
}

async fn refusing_remote() -> (Arc<RefusingRemote>, SocketAddr) {
    let remote = Arc::new(RefusingRemote {
        connections: AtomicUsize::new(0),
        open: AtomicUsize::new(0),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = Arc::clone(&remote);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            serving.connections.fetch_add(1, Ordering::SeqCst);
            serving.open.fetch_add(1, Ordering::SeqCst);
            let conn_remote = Arc::clone(&serving);
            tokio::spawn(async move {
                while let Some((first, body)) = read_packet(&mut stream).await {
                    let reply = match first >> 4 {
                        1 => Some(vec![0x20, 3, 0, 0, 0]),
                        8 => {
                            let (_, filters) = parse_subscribe(&body);
                            Some(ack(0x90, &body, std::iter::repeat_n(0x87u8, filters.len())))
                        }
                        12 => Some(vec![0xD0, 0]),
                        14 => break,
                        _ => None,
                    };
                    if let Some(reply) = reply {
                        if stream.write_all(&reply).await.is_err() {
                            break;
                        }
                    }
                }
                conn_remote.open.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    (remote, addr)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stats_after_refusal_match_the_connection() {
    let (remote, addr) = refusing_remote().await;
    let mut config = BridgeConfig::new("refused", addr.to_string()).add_topic(
        "in/#",
        BridgeDirection::In,
        QoS::AtLeastOnce,
    );
    config.retry_jitter = false;
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let result = tokio::time::timeout(Duration::from_secs(5), bridge.run())
        .await
        .expect("run() did not return");
    assert!(result.is_err(), "run() did not fail on refusal");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let open = remote.open.load(Ordering::SeqCst) > 0;
    let stats = bridge.get_stats().await;
    let address = bridge.current_broker_address().await;
    assert_eq!(
        stats.connected, open,
        "stats.connected = {}, connection to the remote open = {open}, current_broker_address = {address:?}, stats.current_broker = {:?}",
        stats.connected, stats.current_broker
    );
    assert_eq!(
        address.is_some(),
        open,
        "current_broker_address = {address:?}, connection open = {open}"
    );
}
