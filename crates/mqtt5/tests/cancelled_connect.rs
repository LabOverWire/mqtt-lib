use mqtt5::{ConnectOptions, ConnectionEvent, MqttClient, QoS};
use mqtt5_protocol::packet::connack::ConnAckPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;

const FIRST_PASSWORD: &[u8] = b"first-password";
const SECOND_PASSWORD: &[u8] = b"second-password";

struct Server {
    addr: SocketAddr,
    connections: Arc<AtomicUsize>,
    received: Arc<Mutex<Vec<u8>>>,
    cut: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}

async fn server_at(
    addr: SocketAddr,
    connections: Arc<AtomicUsize>,
    received: Arc<Mutex<Vec<u8>>>,
) -> Server {
    let listener = TcpListener::bind(addr).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cut = Arc::new(Notify::new());
    let cutting = cut.clone();
    let counted = connections.clone();
    let kept = received.clone();
    let task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            let cutting = cutting.clone();
            let kept = kept.clone();
            tokio::spawn(async move {
                let mut buffer = [0u8; 2048];
                let Ok(read) = stream.read(&mut buffer).await else {
                    return;
                };
                kept.lock().unwrap().extend_from_slice(&buffer[..read]);
                let mut encoded = Vec::new();
                ConnAckPacket::new(false, ReasonCode::Success)
                    .encode(&mut encoded)
                    .unwrap();
                let _ = stream.write_all(&encoded).await;
                loop {
                    tokio::select! {
                        () = cutting.notified() => break,
                        read = stream.read(&mut buffer) => match read {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        },
                    }
                }
            });
        }
    });
    Server {
        addr,
        connections,
        received,
        cut,
        task,
    }
}

async fn new_server() -> Server {
    server_at(
        "127.0.0.1:0".parse().unwrap(),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(Mutex::new(Vec::new())),
    )
    .await
}

fn options(id: &str, password: &[u8]) -> ConnectOptions {
    ConnectOptions::new(id)
        .with_credentials("user", password)
        .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(100))
}

async fn take_down(server: &Server) {
    server.task.abort();
    tokio::time::sleep(Duration::from_millis(50)).await;
    server.cut.notify_waiters();
    tokio::time::sleep(Duration::from_millis(1500)).await;
}

async fn silent_address() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("mqtt://{}", listener.local_addr().unwrap());
    (listener, address)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[tokio::test]
async fn a_cancelled_connect_leaves_the_reconnection_running_with_what_it_had() {
    let first = new_server().await;
    let client = MqttClient::new("cancelled");
    client
        .connect_with_options(
            &format!("mqtt://{}", first.addr),
            options("cancelled", FIRST_PASSWORD),
        )
        .await
        .unwrap();
    take_down(&first).await;
    let (_silent, silent) = silent_address().await;
    let cancelled = tokio::time::timeout(
        Duration::from_millis(300),
        client.connect_with_options(&silent, options("cancelled", SECOND_PASSWORD)),
    )
    .await;
    assert!(cancelled.is_err(), "the connect was to be cancelled");
    let before = first.connections.load(Ordering::SeqCst);
    first.received.lock().unwrap().clear();
    let restored = server_at(
        first.addr,
        first.connections.clone(),
        first.received.clone(),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let after = first.connections.load(Ordering::SeqCst);
    let received = first.received.lock().unwrap().clone();
    client.disconnect().await.ok();
    restored.task.abort();
    assert!(
        after > before,
        "no reconnection remains after a cancelled connect"
    );
    assert!(
        contains(&received, FIRST_PASSWORD),
        "the reconnection did not use the options it had"
    );
    assert!(
        !contains(&received, SECOND_PASSWORD),
        "the first server was sent the cancelled connect's credentials"
    );
}

#[tokio::test]
async fn cancelled_connects_use_up_no_reconnection_attempts() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let dials = Arc::new(AtomicUsize::new(0));
    let counted = dials.clone();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            held.push(stream);
        }
    });
    let (_silent, cancelled_url) = silent_address().await;
    let delay = Duration::from_millis(500);
    let retrying = ConnectOptions::new("cancelled-attempts")
        .with_reconnect_delay(delay, delay)
        .with_max_reconnect_attempts(3)
        .with_connect_packet_timeout(Duration::from_millis(100));
    let client = MqttClient::with_options(retrying.clone());
    let attempts = Arc::new(AtomicUsize::new(0));
    let gave_up = Arc::new(Notify::new());
    let (seen_attempts, seen_gave_up) = (attempts.clone(), gave_up.clone());
    client
        .on_connection_event(move |event| match event {
            ConnectionEvent::Reconnecting { .. } => {
                seen_attempts.fetch_add(1, Ordering::SeqCst);
            }
            ConnectionEvent::ReconnectFailed { .. } => seen_gave_up.notify_one(),
            _ => {}
        })
        .await
        .unwrap();
    assert!(client.connect_with_options(&url, retrying).await.is_err());
    let dials_before = dials.load(Ordering::SeqCst);

    for announced in 1..=3 {
        tokio::time::timeout(Duration::from_secs(5), async {
            while attempts.load(Ordering::SeqCst) < announced {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let cancelled = tokio::time::timeout(
            Duration::from_millis(30),
            client.connect_with_options(&cancelled_url, ConnectOptions::new("cancelled-attempts")),
        )
        .await;
        assert!(cancelled.is_err());
    }

    tokio::time::timeout(Duration::from_secs(15), gave_up.notified())
        .await
        .unwrap();
    assert_eq!(dials.load(Ordering::SeqCst) - dials_before, 3);
}

#[tokio::test]
async fn a_cancelled_first_connect_starts_no_reconnection() {
    let (silent_listener, silent) = silent_address().await;
    let accepted = Arc::new(AtomicUsize::new(0));
    let counted = accepted.clone();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = silent_listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            held.push(stream);
        }
    });
    let client = MqttClient::new("cancelled-first");
    let cancelled = tokio::time::timeout(
        Duration::from_millis(300),
        client.connect_with_options(&silent, options("cancelled-first", FIRST_PASSWORD)),
    )
    .await;
    assert!(cancelled.is_err(), "the connect was to be cancelled");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_disconnect_stays_in_force_after_a_cancelled_connect() {
    let first = new_server().await;
    let client = MqttClient::new("cancelled-stopped");
    client
        .connect_with_options(
            &format!("mqtt://{}", first.addr),
            options("cancelled-stopped", FIRST_PASSWORD),
        )
        .await
        .unwrap();
    client.disconnect().await.unwrap();
    let (_silent, silent) = silent_address().await;
    let cancelled = tokio::time::timeout(
        Duration::from_millis(300),
        client.connect_with_options(&silent, options("cancelled-stopped", SECOND_PASSWORD)),
    )
    .await;
    assert!(cancelled.is_err(), "the connect was to be cancelled");
    let before = first.connections.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(2)).await;
    let after = first.connections.load(Ordering::SeqCst);
    assert_eq!(after, before, "the disconnected client reconnected");
    assert!(!client.is_connected().await);
    first.task.abort();
}

async fn answers_subscribe_once_then_ignores_it() -> (SocketAddr, Arc<AtomicUsize>, Arc<Notify>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let cut = Arc::new(Notify::new());
    let counted = connections.clone();
    let cutting = cut.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let number = counted.fetch_add(1, Ordering::SeqCst);
            let cutting = cutting.clone();
            tokio::spawn(async move {
                let mut buffer = [0u8; 2048];
                if stream.read(&mut buffer).await.is_err() {
                    return;
                }
                let mut encoded = Vec::new();
                ConnAckPacket::new(false, ReasonCode::Success)
                    .encode(&mut encoded)
                    .unwrap();
                let _ = stream.write_all(&encoded).await;
                loop {
                    tokio::select! {
                        () = cutting.notified() => break,
                        read = stream.read(&mut buffer) => match read {
                            Ok(0) | Err(_) => break,
                            Ok(read) if number == 0 && buffer[0] >> 4 == 8 && read >= 4 => {
                                let suback = [0x90, 4, buffer[2], buffer[3], 0, 0];
                                let _ = stream.write_all(&suback).await;
                            }
                            Ok(_) => {}
                        },
                    }
                }
            });
        }
    });
    (addr, connections, cut)
}

#[tokio::test]
async fn a_connection_made_before_the_cancel_stays_and_is_reconnected() {
    let (addr, connections, cut) = answers_subscribe_once_then_ignores_it().await;
    let url = format!("mqtt://{addr}");
    let client = MqttClient::new("cancelled-connected");
    client
        .connect_with_options(&url, options("cancelled-connected", FIRST_PASSWORD))
        .await
        .unwrap();
    client
        .subscribe_with_options(
            "t/#",
            mqtt5::SubscribeOptions {
                qos: QoS::AtMostOnce,
                ..Default::default()
            },
            |_| {},
        )
        .await
        .unwrap();
    client.disconnect().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let cancelled = tokio::time::timeout(
        Duration::from_millis(500),
        client.connect_with_options(&url, options("cancelled-connected", FIRST_PASSWORD)),
    )
    .await;
    assert!(
        cancelled.is_err(),
        "the connect was to be cancelled as it restores its subscription"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        client.is_connected().await,
        "the connection made did not stay"
    );
    let before = connections.load(Ordering::SeqCst);
    cut.notify_waiters();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let after = connections.load(Ordering::SeqCst);
    client.disconnect().await.ok();
    assert!(after > before, "the connection was not reconnected");
}

async fn next_connect_sends_the_cancelled_connects_credentials(id: &str) -> bool {
    let first = new_server().await;
    let client = MqttClient::new(id);
    client
        .connect_with_options(
            &format!("mqtt://{}", first.addr),
            options(id, FIRST_PASSWORD),
        )
        .await
        .unwrap();
    client.disconnect().await.unwrap();
    let (_silent, silent) = silent_address().await;
    let cancelled = tokio::time::timeout(
        Duration::from_millis(50),
        client.connect_with_options(&silent, options(id, SECOND_PASSWORD)),
    )
    .await;
    assert!(cancelled.is_err(), "the connect was to be cancelled");
    let next = new_server().await;
    client
        .connect(&format!("mqtt://{}", next.addr))
        .await
        .unwrap();
    client.disconnect().await.ok();
    first.task.abort();
    next.task.abort();
    let received = next.received.lock().unwrap().clone();
    contains(&received, SECOND_PASSWORD)
}

#[tokio::test]
async fn the_next_connect_does_not_use_a_cancelled_connects_options() {
    for attempt in 0..5 {
        let id = format!("next-current-{attempt}");
        assert!(!next_connect_sends_the_cancelled_connects_credentials(&id).await);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_next_connect_does_not_use_a_cancelled_connects_options_on_many_threads() {
    for attempt in 0..20 {
        let id = format!("next-threads-{attempt}");
        assert!(!next_connect_sends_the_cancelled_connects_credentials(&id).await);
    }
}

#[tokio::test]
async fn two_cancelled_connects_in_a_row_leave_the_reconnection_running() {
    let first = new_server().await;
    let client = MqttClient::new("cancelled-twice");
    client
        .connect_with_options(
            &format!("mqtt://{}", first.addr),
            options("cancelled-twice", FIRST_PASSWORD),
        )
        .await
        .unwrap();
    take_down(&first).await;
    let (_silent, silent) = silent_address().await;
    for _ in 0..2 {
        let cancelled = tokio::time::timeout(
            Duration::from_millis(100),
            client.connect_with_options(&silent, options("cancelled-twice", SECOND_PASSWORD)),
        )
        .await;
        assert!(cancelled.is_err(), "the connect was to be cancelled");
    }
    let before = first.connections.load(Ordering::SeqCst);
    first.received.lock().unwrap().clear();
    let restored = server_at(
        first.addr,
        first.connections.clone(),
        first.received.clone(),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let after = first.connections.load(Ordering::SeqCst);
    let received = first.received.lock().unwrap().clone();
    client.disconnect().await.ok();
    restored.task.abort();
    assert!(
        after > before,
        "no reconnection remains after two cancelled connects"
    );
    assert!(contains(&received, FIRST_PASSWORD));
    assert!(!contains(&received, SECOND_PASSWORD));
}

#[tokio::test]
async fn a_connection_kept_after_a_cancel_gets_all_its_subscriptions_back() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let resubscribed: Arc<Mutex<Vec<u8>>> = Arc::default();
    let (counted, seen) = (connections.clone(), resubscribed.clone());
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let number = counted.fetch_add(1, Ordering::SeqCst);
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buffer = [0u8; 2048];
                let _ = stream.read(&mut buffer).await;
                let mut encoded = Vec::new();
                ConnAckPacket::new(false, ReasonCode::Success)
                    .encode(&mut encoded)
                    .unwrap();
                let _ = stream.write_all(&encoded).await;
                while let Ok(read) = stream.read(&mut buffer).await {
                    if read == 0 {
                        break;
                    }
                    if buffer[0] >> 4 == 8 {
                        if number > 0 {
                            seen.lock().unwrap().extend_from_slice(&buffer[..read]);
                            tokio::time::sleep(Duration::from_millis(800)).await;
                        }
                        let suback = [0x90, 4, buffer[2], buffer[3], 0, 0];
                        let _ = stream.write_all(&suback).await;
                    }
                }
            });
        }
    });
    let url = format!("mqtt://{addr}");
    let client = MqttClient::new("cancelled-restore");
    client
        .connect_with_options(&url, options("cancelled-restore", FIRST_PASSWORD))
        .await
        .unwrap();
    for filter in ["a/1", "b/2"] {
        client.subscribe(filter, |_| {}).await.unwrap();
    }
    client.disconnect().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let cancelled = tokio::time::timeout(
        Duration::from_millis(400),
        client.connect_with_options(&url, options("cancelled-restore", FIRST_PASSWORD)),
    )
    .await;
    assert!(cancelled.is_err(), "the connect was to be cancelled");
    tokio::time::sleep(Duration::from_secs(3)).await;
    let connected = client.is_connected().await;
    let seen = resubscribed.lock().unwrap().clone();
    client.disconnect().await.ok();
    assert!(connected);
    assert!(
        contains(&seen, b"b/2"),
        "the second subscription was never restored"
    );
}
