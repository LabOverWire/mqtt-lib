use mqtt5::protocol::v5::reason_codes::ReasonCode;
use mqtt5::{ConnectOptions, ConnectionEvent, MqttClient, MqttError};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const USERNAME: &str = "revoked-user";
const OBSERVATION: Duration = Duration::from_secs(4);

async fn read_connect(stream: &mut TcpStream) -> Option<Vec<u8>> {
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
    assert_eq!(first[0], 0x10, "expected CONNECT");
    Some(body)
}

async fn start_server(disconnect_reason: u8) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("mqtt://{}", listener.local_addr().unwrap());
    let connects = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&connects);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let Some(connect) = read_connect(&mut stream).await else {
                continue;
            };
            if connect
                .windows(USERNAME.len())
                .any(|window| window == USERNAME.as_bytes())
            {
                counter.fetch_add(1, Ordering::SeqCst);
            }
            let reply = [
                0x20,
                0x03,
                0x00,
                0x00,
                0x00,
                0xE0,
                0x02,
                disconnect_reason,
                0x00,
            ];
            if stream.write_all(&reply).await.is_err() {
                continue;
            }
            let mut drain = [0u8; 64];
            let _ = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut drain)).await;
        }
    });
    (address, connects)
}

async fn observe(disconnect_reason: u8) -> (usize, Vec<String>) {
    let (address, connects) = start_server(disconnect_reason).await;
    let options = ConnectOptions::new("reconnect-probe")
        .with_credentials(USERNAME, std::process::id().to_be_bytes())
        .with_automatic_reconnect(true)
        .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(200));
    let client = MqttClient::with_options(options.clone());
    let gave_up = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&gave_up);
    client
        .on_connection_event(move |event| {
            if let ConnectionEvent::ReconnectFailed { error } = event {
                recorded.lock().unwrap().push(format!("{error:?}"));
            }
        })
        .await
        .unwrap();
    let initial = Box::pin(client.connect_with_options(&address, options)).await;
    assert!(initial.is_ok(), "initial connect failed: {initial:?}");
    tokio::time::sleep(OBSERVATION).await;
    let _ = client.disconnect().await;
    let gave_up = gave_up.lock().unwrap().clone();
    (connects.load(Ordering::SeqCst), gave_up)
}

#[tokio::test]
async fn control_the_client_reconnects_after_a_server_shutting_down_disconnect() {
    let (connects, gave_up) = observe(0x8B).await;
    assert!(
        connects > 1,
        "after DISCONNECT 0x8B the client is expected to reconnect, saw {connects} CONNECT(s) in {OBSERVATION:?}"
    );
    assert!(gave_up.is_empty(), "reconnection gave up: {gave_up:?}");
}

#[tokio::test]
async fn the_client_stops_reconnecting_after_a_not_authorized_disconnect() {
    let (connects, gave_up) = observe(0x87).await;
    assert_eq!(
        connects, 1,
        "after CONNACK 0x00 then DISCONNECT 0x87 the client sent {connects} CONNECT(s) with the same credentials in {OBSERVATION:?}"
    );
    let expected = format!(
        "{:?}",
        MqttError::ServerDisconnect(ReasonCode::NotAuthorized)
    );
    assert_eq!(gave_up, vec![expected]);
}
