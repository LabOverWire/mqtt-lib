use mqtt5::{ConnectOptions, MqttClient};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const ACCEPTED: &str = "accepted/topic";
const REFUSED: &str = "refused/topic";

async fn read_packet(stream: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
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
    Some((first[0], body))
}

fn subscribe_filters(body: &[u8]) -> Vec<String> {
    let properties_length = usize::from(body[2]);
    let mut at = 3 + properties_length;
    let mut filters = Vec::new();
    while at + 2 <= body.len() {
        let length = usize::from(u16::from_be_bytes([body[at], body[at + 1]]));
        let filter = &body[at + 2..at + 2 + length];
        filters.push(String::from_utf8_lossy(filter).into_owned());
        at += 2 + length + 1;
    }
    filters
}

fn suback(body: &[u8], reason_code: u8) -> [u8; 6] {
    [0x90, 0x04, body[0], body[1], 0x00, reason_code]
}

struct Server {
    address: String,
    connections: Arc<AtomicUsize>,
    restored: Arc<Mutex<Vec<String>>>,
}

const CONNACK: &[u8] = &[0x20, 0x03, 0x00, 0x00, 0x00];

async fn serve_first(mut stream: TcpStream) {
    while let Some((header, body)) = read_packet(&mut stream).await {
        let reply = match (header, subscribe_filters_of(header, &body).first()) {
            (0x10, _) => CONNACK.to_vec(),
            (0x82, Some(filter)) if filter == REFUSED => suback(&body, 0x87).to_vec(),
            (0x82, _) => suback(&body, 0x01).to_vec(),
            _ => continue,
        };
        if stream.write_all(&reply).await.is_err() {
            return;
        }
    }
}

fn subscribe_filters_of(header: u8, body: &[u8]) -> Vec<String> {
    if header == 0x82 {
        subscribe_filters(body)
    } else {
        Vec::new()
    }
}

const RESUMED_CONNACK: &[u8] = &[0x20, 0x03, 0x01, 0x00, 0x00];

fn publish_on_accepted() -> Vec<u8> {
    let mut packet = vec![0x30, 0x00, 0x00, u8::try_from(ACCEPTED.len()).unwrap()];
    packet.extend_from_slice(ACCEPTED.as_bytes());
    packet.push(0x00);
    packet.extend_from_slice(b"hi");
    packet[1] = u8::try_from(packet.len() - 2).unwrap();
    packet
}

async fn serve_later(mut stream: TcpStream, restored: Arc<Mutex<Vec<String>>>) {
    while let Some((header, body)) = read_packet(&mut stream).await {
        let reply = match header {
            0x10 => RESUMED_CONNACK.to_vec(),
            0x82 => {
                let filters = subscribe_filters(&body);
                let deliver = filters.iter().any(|filter| filter == ACCEPTED);
                restored.lock().unwrap().extend(filters);
                let mut reply = suback(&body, 0x01).to_vec();
                if deliver {
                    reply.extend(publish_on_accepted());
                }
                reply
            }
            0xC0 => vec![0xD0, 0x00],
            _ => continue,
        };
        if stream.write_all(&reply).await.is_err() {
            return;
        }
    }
}

async fn start_server() -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("mqtt://{}", listener.local_addr().unwrap());
    let connections = Arc::new(AtomicUsize::new(0));
    let restored = Arc::new(Mutex::new(Vec::new()));
    let counter = Arc::clone(&connections);
    let filters = Arc::clone(&restored);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                tokio::spawn(serve_first(stream));
            } else {
                tokio::spawn(serve_later(stream, Arc::clone(&filters)));
            }
        }
    });
    Server {
        address,
        connections,
        restored,
    }
}

#[tokio::test]
async fn after_a_client_id_change_only_the_accepted_subscription_is_restored_and_delivers() {
    let server = start_server().await;
    let first = ConnectOptions::new("id-change-probe-a").with_clean_start(false);
    let client = MqttClient::with_options(first.clone());
    Box::pin(client.connect_with_options(&server.address, first))
        .await
        .unwrap();
    let delivered = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&delivered);
    client
        .subscribe(ACCEPTED, move |_| seen.store(true, Ordering::SeqCst))
        .await
        .unwrap();
    let refused = client.subscribe(REFUSED, |_| {}).await;
    assert!(
        refused.is_err(),
        "the subscribe to {REFUSED} succeeded: {refused:?}"
    );
    client.disconnect().await.unwrap();

    let second = ConnectOptions::new("id-change-probe-b").with_clean_start(false);
    let reconnected = Box::pin(client.connect_with_options(&server.address, second)).await;
    assert!(
        reconnected.is_ok(),
        "second connect failed: {reconnected:?}"
    );
    tokio::time::sleep(Duration::from_secs(1)).await;
    let connections = server.connections.load(Ordering::SeqCst);
    let restored = server.restored.lock().unwrap().clone();
    let got_message = delivered.load(Ordering::SeqCst);
    let _ = client.disconnect().await;
    assert_eq!(connections, 2);
    assert_eq!(restored, vec![ACCEPTED.to_string()]);
    assert!(got_message, "the restored subscription did not deliver");
}
