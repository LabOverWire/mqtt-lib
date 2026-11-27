use mqtt5::protocol::v5::reason_codes::ReasonCode;
use mqtt5::{ConnectOptions, ConnectionEvent, MqttClient, MqttError};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn read_packet(stream: &mut TcpStream) -> Option<u8> {
    let mut first = [0u8; 1];
    stream.read_exact(&mut first).await.ok()?;
    let mut remaining = 0usize;
    let mut shift = 0;
    loop {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).await.ok()?;
        remaining |= usize::from(byte[0] & 0x7F) << shift;
        if byte[0] & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    let mut body = vec![0u8; remaining];
    stream.read_exact(&mut body).await.ok()?;
    Some(first[0])
}

async fn start_server(first_reason: u8, later_reason: u8) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("mqtt://{}", listener.local_addr().unwrap());
    let connects = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&connects);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            if read_packet(&mut stream).await != Some(0x10) {
                continue;
            }
            let seen = counter.fetch_add(1, Ordering::SeqCst);
            let reason = if seen == 0 {
                first_reason
            } else {
                later_reason
            };
            let reply = [0x20, 0x03, 0x00, 0x00, 0x00, 0xE0, 0x02, reason, 0x00];
            if stream.write_all(&reply).await.is_err() {
                continue;
            }
            let mut drain = [0u8; 64];
            let _ = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut drain)).await;
        }
    });
    (address, connects)
}

async fn start_silent_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("mqtt://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    address
}

fn options(client_id: &str) -> ConnectOptions {
    ConnectOptions::new(client_id)
        .with_credentials(
            format!("reason-survival-{}", std::process::id()),
            std::process::id().to_be_bytes(),
        )
        .with_automatic_reconnect(true)
        .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(200))
}

async fn record_events(client: &MqttClient) -> (Arc<Mutex<Vec<String>>>, Arc<tokio::sync::Notify>) {
    let gave_up = Arc::new(Mutex::new(Vec::new()));
    let disconnected = Arc::new(tokio::sync::Notify::new());
    let recorded = Arc::clone(&gave_up);
    let notify = Arc::clone(&disconnected);
    client
        .on_connection_event(move |event| match event {
            ConnectionEvent::ReconnectFailed { error } => {
                recorded.lock().unwrap().push(format!("{error:?}"));
            }
            ConnectionEvent::Disconnected { .. } => notify.notify_one(),
            _ => {}
        })
        .await
        .unwrap();
    (gave_up, disconnected)
}

#[tokio::test]
async fn a_refusal_after_an_automatic_reconnection_stops_the_monitor_once() {
    let (address, connects) = start_server(0x8B, 0x87).await;
    let options = options("reason-survival-auto");
    let client = MqttClient::with_options(options.clone());
    let (gave_up, _) = record_events(&client).await;

    let initial = Box::pin(client.connect_with_options(&address, options)).await;
    assert!(initial.is_ok(), "initial connect failed: {initial:?}");
    tokio::time::sleep(Duration::from_secs(6)).await;
    let _ = client.disconnect().await;

    let connects = connects.load(Ordering::SeqCst);
    let gave_up = gave_up.lock().unwrap().clone();
    assert_eq!(
        connects, 2,
        "expected the 0x8B reconnect and then a stop on 0x87, saw {connects} CONNECT(s)"
    );
    let expected = format!(
        "{:?}",
        MqttError::ServerDisconnect(ReasonCode::NotAuthorized)
    );
    assert_eq!(gave_up, vec![expected]);
}

#[tokio::test]
async fn a_cancelled_connect_after_a_refusing_disconnect_keeps_the_refusal() {
    let (address, connects) = start_server(0x87, 0x87).await;
    let silent = start_silent_server().await;
    let options = options("reason-survival-cancel");
    let client = MqttClient::with_options(options.clone());
    let (gave_up, disconnected) = record_events(&client).await;

    let waiting = disconnected.notified();
    let initial = Box::pin(client.connect_with_options(&address, options.clone())).await;
    assert!(initial.is_ok(), "initial connect failed: {initial:?}");
    tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .expect("no Disconnected event after the refusing DISCONNECT");

    let cancelled = tokio::time::timeout(
        Duration::from_millis(200),
        Box::pin(client.connect_with_options(&silent, options)),
    )
    .await;
    assert!(
        cancelled.is_err(),
        "connect to the silent server returned: {cancelled:?}"
    );

    tokio::time::sleep(Duration::from_secs(5)).await;
    let _ = client.disconnect().await;

    let connects = connects.load(Ordering::SeqCst);
    let gave_up = gave_up.lock().unwrap().clone();
    assert_eq!(
        connects, 1,
        "after DISCONNECT 0x87 and a cancelled connect elsewhere the client sent {connects} CONNECT(s) with the refused credentials; ReconnectFailed: {gave_up:?}"
    );
}
