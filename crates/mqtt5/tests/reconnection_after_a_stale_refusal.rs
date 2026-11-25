use mqtt5::{ConnectOptions, ConnectionEvent, MqttClient, MqttError};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const REFUSE: u8 = 0;
const SILENT: u8 = 1;
const ACCEPT: u8 = 2;

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

async fn serve(mut stream: TcpStream, mode: u8) {
    if read_packet(&mut stream).await != Some(0x10) {
        return;
    }
    match mode {
        REFUSE => {
            let reply = [0x20, 0x03, 0x00, 0x00, 0x00, 0xE0, 0x02, 0x87, 0x00];
            let _ = stream.write_all(&reply).await;
            let mut drain = [0u8; 64];
            let _ = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut drain)).await;
        }
        SILENT => {
            let mut drain = [0u8; 64];
            let _ = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut drain)).await;
        }
        _ => {
            let reply = [0x20, 0x03, 0x00, 0x00, 0x00];
            if stream.write_all(&reply).await.is_err() {
                return;
            }
            while let Some(kind) = read_packet(&mut stream).await {
                if kind == 0xC0 && stream.write_all(&[0xD0, 0x00]).await.is_err() {
                    return;
                }
            }
        }
    }
}

async fn start_server() -> (String, Arc<AtomicU8>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("mqtt://{}", listener.local_addr().unwrap());
    let mode = Arc::new(AtomicU8::new(REFUSE));
    let shared = Arc::clone(&mode);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let current = shared.load(Ordering::SeqCst);
            tokio::spawn(serve(stream, current));
        }
    });
    (address, mode)
}

#[tokio::test]
async fn a_timed_out_connect_after_a_refusing_disconnect_still_reconnects() {
    let (address, mode) = start_server().await;
    let options = ConnectOptions::new("stale-refusal-probe")
        .with_credentials(
            format!("stale-{}", std::process::id()),
            std::process::id().to_be_bytes(),
        )
        .with_automatic_reconnect(true)
        .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(200))
        .with_connect_packet_timeout(Duration::from_millis(500));
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

    let first = Box::pin(client.connect_with_options(&address, options.clone())).await;
    assert!(first.is_ok(), "first connect failed: {first:?}");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let _ = client.disconnect().await;
    gave_up.lock().unwrap().clear();

    mode.store(SILENT, Ordering::SeqCst);
    let second = Box::pin(client.connect_with_options(&address, options)).await;
    assert!(
        matches!(second, Err(MqttError::Timeout)),
        "second connect expected to time out: {second:?}"
    );

    mode.store(ACCEPT, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(5)).await;
    let connected = client.is_connected().await;
    let gave_up = gave_up.lock().unwrap().clone();
    let _ = client.disconnect().await;
    assert!(
        connected,
        "after a connect that timed out, the server accepts again but the client is not connected; ReconnectFailed: {gave_up:?}"
    );
    assert!(gave_up.is_empty(), "reconnection gave up: {gave_up:?}");
}
