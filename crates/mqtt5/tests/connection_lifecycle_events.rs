#![cfg(feature = "broker")]
#![allow(clippy::large_futures)]

mod common;

use common::{test_client_id, TestBroker};
use mqtt5::time::Duration;
use mqtt5::types::ReasonCode;
use mqtt5::{ConnectOptions, ConnectionEvent, DisconnectReason, MqttClient};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Events = Arc<Mutex<Vec<ConnectionEvent>>>;

const CONNACK_V5_SUCCESS: [u8; 5] = [0x20, 0x03, 0x00, 0x00, 0x00];

enum AfterConnack {
    Disconnect(u8),
    Close,
}

async fn fake_broker(after_connack: AfterConnack) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("local addr").to_string();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut connect = [0u8; 1024];
        let _ = socket.read(&mut connect).await;
        socket
            .write_all(&CONNACK_V5_SUCCESS)
            .await
            .expect("write CONNACK");
        tokio::time::sleep(Duration::from_millis(200)).await;
        if let AfterConnack::Disconnect(reason_code) = after_connack {
            socket
                .write_all(&[0xE0, 0x02, reason_code, 0x00])
                .await
                .expect("write DISCONNECT");
        }
        drop(socket);
    });
    address
}

async fn record_events(client: &MqttClient) -> Events {
    let events: Events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    client
        .on_connection_event(move |event| sink.lock().unwrap().push(event))
        .await
        .expect("register connection event callback");
    events
}

fn disconnect_reasons(events: &Events) -> Vec<DisconnectReason> {
    events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            ConnectionEvent::Disconnected { reason } => Some(reason.clone()),
            _ => None,
        })
        .collect()
}

fn has_event(events: &Events, predicate: impl Fn(&ConnectionEvent) -> bool) -> bool {
    events.lock().unwrap().iter().any(predicate)
}

async fn wait_until(timeout: Duration, condition: impl Fn() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    condition()
}

fn no_reconnect(name: &str) -> ConnectOptions {
    ConnectOptions::new(test_client_id(name)).with_automatic_reconnect(false)
}

#[tokio::test]
async fn connecting_fires_before_connected() {
    let broker = TestBroker::start().await;
    let client = MqttClient::new(test_client_id("connecting"));
    let events = record_events(&client).await;

    client.connect(broker.address()).await.expect("connect");
    assert!(
        wait_until(Duration::from_secs(2), || has_event(&events, |e| matches!(
            e,
            ConnectionEvent::Connected { .. }
        )))
        .await,
        "Connected never fired"
    );

    let first_two: Vec<bool> = events
        .lock()
        .unwrap()
        .iter()
        .take(2)
        .map(|e| matches!(e, ConnectionEvent::Connecting))
        .collect();
    assert_eq!(
        first_two,
        vec![true, false],
        "Connecting must precede Connected"
    );

    client.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn server_disconnect_carries_reason_code() {
    let address = fake_broker(AfterConnack::Disconnect(0x8E)).await;
    let client = MqttClient::new(test_client_id("takeover"));
    let events = record_events(&client).await;
    client
        .connect_with_options(&address, no_reconnect("takeover"))
        .await
        .expect("connect");

    assert!(
        wait_until(Duration::from_secs(3), || !disconnect_reasons(&events)
            .is_empty())
        .await,
        "client never observed the broker DISCONNECT"
    );
    assert_eq!(
        disconnect_reasons(&events),
        vec![DisconnectReason::ServerDisconnect(
            ReasonCode::SessionTakenOver
        )]
    );
    assert!(!client.is_connected().await);
}

#[tokio::test]
async fn lost_connection_fires_network_error() {
    let address = fake_broker(AfterConnack::Close).await;
    let client = MqttClient::new(test_client_id("lost"));
    let events = record_events(&client).await;
    client
        .connect_with_options(&address, no_reconnect("lost"))
        .await
        .expect("connect");

    assert!(
        wait_until(Duration::from_secs(3), || !disconnect_reasons(&events)
            .is_empty())
        .await,
        "client never observed the connection drop"
    );
    let reasons = disconnect_reasons(&events);
    assert_eq!(
        reasons.len(),
        1,
        "exactly one disconnect expected: {reasons:?}"
    );
    assert!(
        matches!(reasons[0], DisconnectReason::NetworkError(_)),
        "unexpected reason: {:?}",
        reasons[0]
    );
    assert!(!client.is_connected().await);
}

#[tokio::test]
async fn client_disconnect_fires_only_client_initiated() {
    let broker = TestBroker::start().await;
    let client = MqttClient::new(test_client_id("clean"));
    let events = record_events(&client).await;

    client.connect(broker.address()).await.expect("connect");
    client.disconnect().await.expect("disconnect");
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        disconnect_reasons(&events),
        vec![DisconnectReason::ClientInitiated]
    );
}

#[tokio::test]
async fn reconnect_failed_fires_after_max_attempts() {
    let _broker = TestBroker::start().await;
    let client = MqttClient::new(test_client_id("give-up"));
    let events = record_events(&client).await;

    let opts = ConnectOptions::new(test_client_id("give-up"))
        .with_automatic_reconnect(true)
        .with_reconnect_delay(Duration::from_millis(50), Duration::from_millis(200))
        .with_max_reconnect_attempts(2);
    let result = client.connect_with_options("localhost:9999", opts).await;
    assert!(result.is_err());

    assert!(
        wait_until(Duration::from_secs(10), || has_event(
            &events,
            |e| matches!(e, ConnectionEvent::ReconnectFailed { .. })
        ))
        .await,
        "ReconnectFailed never fired after exhausting max attempts"
    );
    assert!(has_event(&events, |e| matches!(
        e,
        ConnectionEvent::Reconnecting { .. }
    )));
    assert!(!client.is_connected().await);
}
