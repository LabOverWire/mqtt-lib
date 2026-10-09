#![cfg(feature = "broker")]
use mqtt5::broker::config::{StorageBackend, StorageConfig};
use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::{ConnectOptions, MqttClient, PublishOptions, QoS, SubscribeOptions};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

async fn start_broker() -> (u16, tokio::task::JoinHandle<mqtt5::error::Result<()>>) {
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory));
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let port = broker.local_addr().unwrap().port();
    let task = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    (port, task)
}

fn mqtt_string(value: &str) -> Vec<u8> {
    let mut bytes = u16::try_from(value.len()).unwrap().to_be_bytes().to_vec();
    bytes.extend_from_slice(value.as_bytes());
    bytes
}

fn packet(header: u8, body: &[u8]) -> Vec<u8> {
    let mut bytes = vec![header];
    let mut remaining = body.len();
    loop {
        let mut byte = u8::try_from(remaining % 128).unwrap();
        remaining /= 128;
        if remaining > 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if remaining == 0 {
            break;
        }
    }
    bytes.extend_from_slice(body);
    bytes
}

async fn stalled_reader(port: u16, client_id: &str, topic: &str) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut connect = mqtt_string("MQTT");
    connect.extend_from_slice(&[5, 0x02, 0, 60, 5, 0x11, 0, 0, 0x0E, 0x10]);
    connect.extend(mqtt_string(client_id));
    stream.write_all(&packet(0x10, &connect)).await.unwrap();
    let mut subscribe = vec![0, 1, 0];
    subscribe.extend(mqtt_string(topic));
    subscribe.push(0);
    stream.write_all(&packet(0x82, &subscribe)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    stream
}

async fn retained_received_after_takeover_during_hand_off(clean_start: bool) -> usize {
    let (port, broker) = start_broker().await;
    let url = format!("mqtt://127.0.0.1:{port}");

    let publisher = MqttClient::new("retainer");
    publisher.connect(&url).await.unwrap();
    publisher
        .publish_with_options(
            "state/r",
            b"retained".to_vec(),
            PublishOptions {
                qos: QoS::AtLeastOnce,
                retain: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let stalled = stalled_reader(port, "c", "flood").await;
    let payload = vec![0u8; 64 * 1024];
    for _ in 0..500 {
        publisher
            .publish_qos0("flood", payload.clone())
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    let received = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&received);
    let takeover = MqttClient::with_options(
        ConnectOptions::new("c")
            .with_clean_start(clean_start)
            .with_resume_existing_session(!clean_start)
            .with_session_expiry_interval(3600)
            .with_automatic_reconnect(false),
    );
    takeover.connect(&url).await.unwrap();
    takeover
        .subscribe_with_options(
            "state/r",
            SubscribeOptions {
                qos: QoS::AtLeastOnce,
                ..Default::default()
            },
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            },
        )
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(stalled);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while received.load(Ordering::SeqCst) == 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    broker.abort();
    received.load(Ordering::SeqCst)
}

#[tokio::test]
async fn clean_start_subscribe_during_hand_off_still_gets_retained_message() {
    assert_eq!(
        Box::pin(retained_received_after_takeover_during_hand_off(true)).await,
        1
    );
}

#[tokio::test]
async fn resumed_subscribe_during_hand_off_gets_retained_message() {
    assert_eq!(
        Box::pin(retained_received_after_takeover_during_hand_off(false)).await,
        1
    );
}
