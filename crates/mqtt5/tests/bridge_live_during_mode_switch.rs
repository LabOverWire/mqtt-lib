#![cfg(feature = "broker")]
use mqtt5::broker::bridge::{BridgeConfig, BridgeDirection, TopicMapping};
use mqtt5::broker::{BrokerConfig, MqttBroker, StorageConfig};
use mqtt5::client::MqttClient;
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

fn parse_subscribe(body: &[u8]) -> (Option<u32>, String) {
    let mut at = 2;
    let props_len = read_varint(body, &mut at);
    let props_end = at + props_len;
    let mut id = None;
    while at < props_end {
        at += 1;
        id = Some(u32::try_from(read_varint(body, &mut at)).unwrap());
    }
    let len = usize::from(u16::from_be_bytes([body[at], body[at + 1]]));
    at += 2;
    (id, String::from_utf8(body[at..at + len].to_vec()).unwrap())
}

fn parse_unsubscribe(body: &[u8]) -> String {
    let mut at = 2;
    let props_len = read_varint(body, &mut at);
    at += props_len;
    let len = usize::from(u16::from_be_bytes([body[at], body[at + 1]]));
    at += 2;
    String::from_utf8(body[at..at + len].to_vec()).unwrap()
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

fn publish_qos0(topic: &str, id: Option<u32>) -> Vec<u8> {
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
    body.extend_from_slice(b"live");
    let mut packet = vec![0x30];
    write_varint(&mut packet, body.len());
    packet.extend_from_slice(&body);
    packet
}

fn connack(session_present: bool, ids: bool) -> Vec<u8> {
    let mut body = vec![u8::from(session_present), 0];
    if ids {
        body.push(0);
    } else {
        body.extend_from_slice(&[2, 0x29, 0]);
    }
    let mut packet = vec![0x20];
    write_varint(&mut packet, body.len());
    packet.extend_from_slice(&body);
    packet
}

fn ack(header: u8, request: &[u8], code: u8) -> Vec<u8> {
    vec![header, 4, request[0], request[1], 0, code]
}

const LIVE_TOPIC: &str = "sensors/x/temp";

async fn scripted_remote(
    listener: tokio::net::TcpListener,
    first_ids: bool,
    second_ids: bool,
    log: Arc<Mutex<Vec<String>>>,
) {
    let mut session: BTreeMap<String, Option<u32>> = BTreeMap::new();
    let mut index = 0usize;
    while let Ok((mut stream, _)) = listener.accept().await {
        let Some((0x10, _)) = read_packet(&mut stream).await else {
            continue;
        };
        index += 1;
        let (session_present, ids) = if index == 1 {
            session.clear();
            (false, first_ids)
        } else {
            (true, second_ids)
        };
        log.lock()
            .unwrap()
            .push(format!("conn{index} sp={session_present} ids={ids}"));
        stream
            .write_all(&connack(session_present, ids))
            .await
            .unwrap();
        let close_at =
            (index == 1).then(|| tokio::time::Instant::now() + Duration::from_millis(2000));
        let mut subscribes = 0usize;
        loop {
            let packet = tokio::select! {
                packet = read_packet(&mut stream) => packet,
                () = async {
                    match close_at {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    log.lock().unwrap().push(format!("conn{index} CLOSE"));
                    None
                }
            };
            let Some((first, body)) = packet else { break };
            match first >> 4 {
                8 => {
                    subscribes += 1;
                    let (id, filter) = parse_subscribe(&body);
                    session.insert(filter.clone(), if ids { id } else { None });
                    log.lock()
                        .unwrap()
                        .push(format!("conn{index} SUB {filter} id={id:?}"));
                    if index == 2 && subscribes == 1 {
                        for (held, held_id) in &session {
                            if matches(LIVE_TOPIC, held) {
                                let send_id = if ids { *held_id } else { None };
                                log.lock().unwrap().push(format!(
                                    "conn{index} live copy via {held} id={send_id:?}"
                                ));
                                stream
                                    .write_all(&publish_qos0(LIVE_TOPIC, send_id))
                                    .await
                                    .unwrap();
                            }
                        }
                    }
                    stream.write_all(&ack(0x90, &body, 0)).await.unwrap();
                }
                10 => {
                    let filter = parse_unsubscribe(&body);
                    session.remove(&filter);
                    log.lock()
                        .unwrap()
                        .push(format!("conn{index} UNSUB {filter}"));
                    stream.write_all(&ack(0xB0, &body, 0)).await.unwrap();
                }
                12 => stream.write_all(&[0xD0, 0]).await.unwrap(),
                14 => break,
                _ => {}
            }
        }
    }
}

fn mapping(pattern: &str, local_prefix: &str) -> TopicMapping {
    TopicMapping {
        pattern: pattern.to_string(),
        direction: BridgeDirection::In,
        qos: QoS::AtMostOnce,
        local_prefix: Some(local_prefix.to_string()),
        remote_prefix: None,
    }
}

async fn live_copies(
    patterns: [(&str, &str); 2],
    first_ids: bool,
    second_ids: bool,
) -> (usize, usize, Vec<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote_addr = listener.local_addr().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let server = tokio::spawn(scripted_remote(
        listener,
        first_ids,
        second_ids,
        Arc::clone(&log),
    ));

    let received = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut bridge = BridgeConfig::new("live-mode-switch", remote_addr.to_string());
    bridge.topics = patterns
        .iter()
        .map(|(pattern, prefix)| mapping(pattern, prefix))
        .collect();
    bridge.initial_reconnect_delay = Duration::from_millis(200);
    bridge.first_retry_delay = Duration::from_millis(100);
    bridge.max_reconnect_delay = Duration::from_millis(300);
    bridge.retry_jitter = false;
    let bind = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let config = BrokerConfig::default()
        .with_bind_address(bind)
        .with_storage(StorageConfig::new().with_persistence(false));
    let mut config = config;
    config.bridges = vec![bridge];
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let bind = broker.local_addr().unwrap();
    let mut ready = broker.ready_receiver();
    let shutdown = broker.shutdown_handle();
    let task = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    while !*ready.borrow_and_update() {
        ready.changed().await.unwrap();
    }
    let client = MqttClient::new("live-mode-switch-local2");
    client.connect(&format!("mqtt://{bind}")).await.unwrap();
    let sink = Arc::clone(&received);
    client
        .subscribe("#", move |msg| sink.lock().unwrap().push(msg.topic.clone()))
        .await
        .unwrap();

    let started = tokio::time::Instant::now();
    while !log
        .lock()
        .unwrap()
        .iter()
        .any(|line| line.starts_with("conn2") && line.contains("live copy"))
        && started.elapsed() < Duration::from_secs(20)
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let topics = received.lock().unwrap().clone();
    let count = |topic: &str| topics.iter().filter(|t| *t == topic).count();
    let result = (
        count(&format!("l1/{LIVE_TOPIC}")),
        count(&format!("l2/{LIVE_TOPIC}")),
        log.lock().unwrap().clone(),
    );
    let _ = client.disconnect().await;
    shutdown.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    server.abort();
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_message_during_setup_same_mode_reaches_each_mapping_once() {
    let (l1, l2, log) = live_copies(
        [("sensors/#", "l1/"), ("sensors/+/temp", "l2/")],
        false,
        false,
    )
    .await;
    assert_eq!((l1, l2), (1, 1), "remote log: {log:#?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_message_during_setup_after_gaining_identifiers_reaches_every_mapping() {
    let (l1, l2, log) = live_copies(
        [("sensors/#", "l1/"), ("sensors/+/temp", "l2/")],
        false,
        true,
    )
    .await;
    assert!(l1 >= 1 && l2 >= 1, "l1={l1} l2={l2}; remote log: {log:#?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_message_during_setup_after_gaining_identifiers_covered_first_reaches_every_mapping() {
    let (l1, l2, log) = live_copies(
        [("sensors/+/temp", "l2/"), ("sensors/#", "l1/")],
        false,
        true,
    )
    .await;
    assert!(l1 >= 1 && l2 >= 1, "l1={l1} l2={l2}; remote log: {log:#?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_message_during_setup_after_losing_identifiers_reaches_every_mapping() {
    let (l1, l2, log) = live_copies(
        [("sensors/#", "l1/"), ("sensors/+/temp", "l2/")],
        true,
        false,
    )
    .await;
    assert!(l1 >= 1 && l2 >= 1, "l1={l1} l2={l2}; remote log: {log:#?}");
}
