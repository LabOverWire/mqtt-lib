//! A reconnection left running from an earlier connection stops once the
//! client is connected again some other way: it never connects to its old
//! address with the options given since, credentials included.
#![cfg(feature = "broker")]

use mqtt5::time::Duration;
use mqtt5::transport::tls::TlsConfig;
use mqtt5::{ConnectOptions, MqttClient};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PASSWORD: &[u8] = b"given-to-connect-with-tls";

/// An address nothing listens on, for now
async fn unused_address() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
}

/// Listens on `addr` and keeps all it is sent
async fn record(addr: SocketAddr) -> Arc<Mutex<Vec<u8>>> {
    let listener = TcpListener::bind(addr).await.unwrap();
    let received = Arc::new(Mutex::new(Vec::new()));
    let kept = received.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let kept = kept.clone();
            tokio::spawn(async move {
                let mut buffer = [0u8; 1024];
                while let Ok(n) = stream.read(&mut buffer).await {
                    if n == 0 {
                        break;
                    }
                    kept.lock().unwrap().extend_from_slice(&buffer[..n]);
                }
            });
        }
    });
    received
}

fn options() -> ConnectOptions {
    ConnectOptions::new("reconnection-after-another-connect")
        .with_reconnect_delay(Duration::from_millis(300), Duration::from_millis(300))
}

/// Connects to `plain` in the clear, which fails and leaves a reconnection
/// trying it again, then with TLS to an address that refuses, with a password;
/// `plain` then listens
async fn plain_then_tls(disconnect_between: bool) -> Arc<Mutex<Vec<u8>>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let plain = unused_address().await;
    let client = MqttClient::with_options(options());
    client
        .connect(&format!("mqtt://{plain}"))
        .await
        .expect_err("nothing listens there yet");
    // The reconnection takes the address once it has seen the connection
    // down, which it checks for every second, and then tries it again and
    // again.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    if disconnect_between {
        let _ = client.disconnect().await;
    }
    client
        .connect_with_tls_and_options(
            TlsConfig::new(unused_address().await, "localhost"),
            options().with_credentials("user", PASSWORD),
        )
        .await
        .expect_err("nothing listens there");
    let received = record(plain).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    // Not disconnected: had the client sent a CONNECT to `plain`, which never
    // answers, it would wait for the CONNACK first.
    received
}

fn contains_password(received: &[u8]) -> bool {
    received
        .windows(PASSWORD.len())
        .any(|window| window == PASSWORD)
}

#[tokio::test]
async fn an_earlier_reconnection_does_not_send_later_credentials() {
    let received = plain_then_tls(false).await;
    assert!(
        !contains_password(&received.lock().unwrap()),
        "the password went to the earlier address in the clear"
    );
}

#[tokio::test]
async fn an_earlier_reconnection_does_not_send_later_credentials_after_disconnect() {
    let received = plain_then_tls(true).await;
    assert!(
        !contains_password(&received.lock().unwrap()),
        "the password went to the earlier address in the clear"
    );
}

const CONNACK_ACCEPTED: [u8; 5] = [0x20, 3, 0, 0, 0];
const CONNACK_BAD_CREDENTIALS: [u8; 5] = [0x20, 3, 0, 0x86, 0];

struct Restarting {
    url: String,
    accepted: Arc<AtomicUsize>,
    answer: Arc<Mutex<Option<[u8; 5]>>>,
    cut: tokio::sync::watch::Sender<bool>,
}

async fn restarting_server() -> Restarting {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let accepted = Arc::new(AtomicUsize::new(0));
    let answer = Arc::new(Mutex::new(None));
    let (cut, cut_rx) = tokio::sync::watch::channel(false);
    let (counted, answering) = (accepted.clone(), answer.clone());
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let first = counted.fetch_add(1, Ordering::SeqCst) == 0;
            let reply: Option<[u8; 5]> = *answering.lock().unwrap();
            let mut cut = cut_rx.clone();
            tokio::spawn(async move {
                let mut buffer = [0u8; 1024];
                if first {
                    let _ = stream.read(&mut buffer).await;
                    let _ = stream.write_all(&CONNACK_ACCEPTED).await;
                    let _ = cut.wait_for(|cut| *cut).await;
                } else if let Some(reply) = reply {
                    let _ = stream.read(&mut buffer).await;
                    let _ = stream.write_all(&reply).await;
                    while stream.read(&mut buffer).await.unwrap_or(0) > 0 {}
                }
            });
        }
    });
    Restarting {
        url,
        accepted,
        answer,
        cut,
    }
}

async fn lost_connection(server: &Restarting) -> (MqttClient, ConnectOptions) {
    let retrying = ConnectOptions::new("retrying")
        .with_reconnect_delay(Duration::from_millis(1100), Duration::from_millis(1100));
    let client = MqttClient::with_options(retrying.clone());
    client
        .connect_with_options(&server.url, retrying.clone())
        .await
        .unwrap();
    server.cut.send(true).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!client.is_connected().await);
    (client, retrying)
}

#[tokio::test]
async fn a_connect_that_fails_on_the_connection_keeps_reconnecting() {
    let server = restarting_server().await;
    let (client, retrying) = lost_connection(&server).await;
    assert!(client
        .connect_with_options(&server.url, retrying)
        .await
        .is_err());
    *server.answer.lock().unwrap() = Some(CONNACK_ACCEPTED);
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(client.is_connected().await);
}

#[tokio::test]
async fn a_connect_the_server_refuses_ends_the_reconnection() {
    let server = restarting_server().await;
    let (client, retrying) = lost_connection(&server).await;
    *server.answer.lock().unwrap() = Some(CONNACK_BAD_CREDENTIALS);
    assert!(client
        .connect_with_options(&server.url, retrying)
        .await
        .is_err());
    let after_refusal = server.accepted.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(server.accepted.load(Ordering::SeqCst), after_refusal);
}

#[tokio::test]
async fn a_connect_refused_with_an_unspecified_error_keeps_reconnecting() {
    let server = restarting_server().await;
    let (client, retrying) = lost_connection(&server).await;
    *server.answer.lock().unwrap() = Some([0x20, 3, 0, 0x80, 0]);
    assert!(client
        .connect_with_options(&server.url, retrying)
        .await
        .is_err());
    *server.answer.lock().unwrap() = Some(CONNACK_ACCEPTED);
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(client.is_connected().await);
}

#[tokio::test]
async fn a_malformed_connack_keeps_reconnecting() {
    let server = restarting_server().await;
    let (client, retrying) = lost_connection(&server).await;
    *server.answer.lock().unwrap() = Some([0x20, 3, 0xFE, 0, 0]);
    let error = client
        .connect_with_options(&server.url, retrying)
        .await
        .expect_err("the CONNACK is malformed");
    assert!(
        matches!(error, mqtt5::MqttError::MalformedPacket(_)),
        "{error:?}"
    );
    *server.answer.lock().unwrap() = Some(CONNACK_ACCEPTED);
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(client.is_connected().await);
}

#[tokio::test]
async fn a_reconnection_the_server_refuses_for_good_stops() {
    let server = restarting_server().await;
    *server.answer.lock().unwrap() = Some(CONNACK_BAD_CREDENTIALS);
    let (client, _) = lost_connection(&server).await;
    let gave_up = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&gave_up);
    client
        .on_connection_event(move |event| {
            if let mqtt5::ConnectionEvent::ReconnectFailed { .. } = event {
                counted.fetch_add(1, Ordering::SeqCst);
            }
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(!client.is_connected().await);
    assert_eq!(server.accepted.load(Ordering::SeqCst), 2);
    assert_eq!(gave_up.load(Ordering::SeqCst), 1);
}
