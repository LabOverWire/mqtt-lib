use mqtt5::{ConnectOptions, MqttClient};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const TOPIC: &str = "review/topic";

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

fn publish_packet() -> Vec<u8> {
    let topic = TOPIC.as_bytes();
    let topic_length = u8::try_from(topic.len()).unwrap();
    let mut packet = vec![0x30, 2 + topic_length + 1 + 1, 0x00, topic_length];
    packet.extend_from_slice(topic);
    packet.push(0x00);
    packet.push(b'x');
    packet
}

async fn serve(
    mut stream: TcpStream,
    index: usize,
    refuse_second: bool,
    drop_after: usize,
    restored: Arc<Mutex<Vec<String>>>,
) {
    let mut subscribes = 0;
    while let Some((header, body)) = read_packet(&mut stream).await {
        match header {
            0x10 => {
                if stream
                    .write_all(&[0x20, 0x03, 0x00, 0x00, 0x00])
                    .await
                    .is_err()
                {
                    return;
                }
            }
            0x82 => {
                subscribes += 1;
                let code = if index == 1 && subscribes == 2 && refuse_second {
                    0x87
                } else {
                    0x01
                };
                if stream
                    .write_all(&[0x90, 0x04, body[0], body[1], 0x00, code])
                    .await
                    .is_err()
                {
                    return;
                }
                if index == 1 && subscribes == drop_after {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    return;
                }
                if index > 1 {
                    restored
                        .lock()
                        .unwrap()
                        .push(format!("subscribe#{subscribes}"));
                    if stream.write_all(&publish_packet()).await.is_err() {
                        return;
                    }
                }
            }
            _ => {}
        }
    }
}

async fn start_server(refuse_second: bool) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("mqtt://{}", listener.local_addr().unwrap());
    let restored = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&restored);
    let drop_after = if refuse_second { 2 } else { 1 };
    tokio::spawn(async move {
        let mut index = 0;
        while let Ok((stream, _)) = listener.accept().await {
            index += 1;
            tokio::spawn(serve(
                stream,
                index,
                refuse_second,
                drop_after,
                Arc::clone(&log),
            ));
        }
    });
    (address, restored)
}

fn options(client_id: &str) -> ConnectOptions {
    ConnectOptions::new(client_id)
        .with_clean_start(true)
        .with_automatic_reconnect(true)
        .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(200))
}

fn counting_callback(counter: &Arc<AtomicUsize>) -> impl Fn(mqtt5::types::Message) + Send + Sync {
    let counter = Arc::clone(counter);
    move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    }
}

async fn wait_for_delivery(counter: &AtomicUsize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while counter.load(Ordering::SeqCst) == 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn a_refused_resubscribe_keeps_the_accepted_callback_across_a_reconnect() {
    let (address, restored) = start_server(true).await;
    let options = options("refused-resubscribe");
    let client = MqttClient::with_options(options.clone());
    Box::pin(client.connect_with_options(&address, options))
        .await
        .unwrap();

    let first = Arc::new(AtomicUsize::new(0));
    client
        .subscribe(TOPIC, counting_callback(&first))
        .await
        .unwrap();
    let second = Arc::new(AtomicUsize::new(0));
    let refused = client.subscribe(TOPIC, counting_callback(&second)).await;
    assert!(refused.is_err(), "second subscribe accepted: {refused:?}");

    wait_for_delivery(&first).await;
    let restored = restored.lock().unwrap().clone();
    let first = first.load(Ordering::SeqCst);
    let second = second.load(Ordering::SeqCst);
    let _ = client.disconnect().await;
    assert_eq!(
        (restored.len(), first, second),
        (1, 1, 0),
        "restored SUBSCRIBEs {restored:?}, first callback got {first}, refused callback got {second}"
    );
}

#[tokio::test]
async fn control_a_single_subscribe_is_delivered_after_reconnect() {
    let (address, restored) = start_server(false).await;
    let options = options("refused-resubscribe-control");
    let client = MqttClient::with_options(options.clone());
    Box::pin(client.connect_with_options(&address, options))
        .await
        .unwrap();
    let deliveries = Arc::new(AtomicUsize::new(0));
    client
        .subscribe(TOPIC, counting_callback(&deliveries))
        .await
        .unwrap();
    wait_for_delivery(&deliveries).await;
    let restored = restored.lock().unwrap().clone();
    let delivered = deliveries.load(Ordering::SeqCst);
    let _ = client.disconnect().await;
    assert_eq!(
        (restored.len(), delivered),
        (1, 1),
        "restored {restored:?}, delivered {delivered}"
    );
}
