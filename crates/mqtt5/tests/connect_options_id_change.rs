#![cfg(feature = "broker")]
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::{ConnectOptions, MqttClient, QoS};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const CONNACK: [u8; 5] = [0x20, 3, 0, 0, 0];

async fn broker() -> SocketAddr {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        });
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let addr = broker.local_addr().unwrap();
    let mut ready = broker.ready_receiver();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&up| up).await.unwrap();
    addr
}

async fn read_some(stream: &mut TcpStream) -> Vec<u8> {
    let mut buffer = [0u8; 512];
    let read = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut buffer))
        .await
        .unwrap()
        .unwrap();
    buffer[..read].to_vec()
}

async fn queue_qos2_for_offline_b(addr: SocketAddr) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let connect = [
        0x10, 19, 0, 4, b'M', b'Q', b'T', b'T', 5, 0x02, 0, 60, 5, 0x11, 0, 0, 1, 0x2C, 0, 1, b'b',
    ];
    stream.write_all(&connect).await.unwrap();
    assert_eq!(read_some(&mut stream).await[0], 0x20);
    let subscribe = [0x82, 7, 0, 1, 0, 0, 1, b'u', 0x02];
    stream.write_all(&subscribe).await.unwrap();
    assert_eq!(read_some(&mut stream).await[0], 0x90);
    drop(stream);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let publisher = MqttClient::new("id-change-publisher");
    publisher.connect(&format!("mqtt://{addr}")).await.unwrap();
    publisher
        .publish_qos2("u", b"queued-for-b".to_vec())
        .await
        .unwrap();
    publisher.disconnect().await.unwrap();
}

async fn server_leaving_inbound_qos2(
    release: bool,
) -> (String, tokio::sync::oneshot::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let (done, finished) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_some(&mut stream).await;
        stream.write_all(&CONNACK).await.unwrap();
        let subscribe = read_some(&mut stream).await;
        assert_eq!(subscribe[0], 0x82, "SUBSCRIBE: {subscribe:?}");
        let suback = [0x90, 4, subscribe[2], subscribe[3], 0, 0x02];
        stream.write_all(&suback).await.unwrap();
        let publish = [
            0x34, 12, 0, 1, b'u', 0, 1, 0, b'f', b'r', b'o', b'm', b'-', b'a',
        ];
        stream.write_all(&publish).await.unwrap();
        let pubrec = read_some(&mut stream).await;
        assert_eq!(pubrec[0], 0x50, "PUBREC: {pubrec:?}");
        if release {
            stream.write_all(&[0x62, 2, 0, 1]).await.unwrap();
            let pubcomp = read_some(&mut stream).await;
            assert_eq!(pubcomp[0], 0x70, "PUBCOMP: {pubcomp:?}");
        }
        let _ = done.send(());
        tokio::time::sleep(Duration::from_secs(10)).await;
        drop(stream);
    });
    (url, finished)
}

async fn switch_ids_with_inbound_qos2(release: bool) -> Vec<String> {
    let addr = broker().await;
    queue_qos2_for_offline_b(addr).await;
    let (url, finished) = server_leaving_inbound_qos2(release).await;
    let client = MqttClient::new("a");
    let first = ConnectOptions::new("a")
        .with_clean_start(false)
        .with_session_expiry_interval(300)
        .with_automatic_reconnect(false);
    client.connect_with_options(&url, first).await.unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    client
        .subscribe_with_options(
            "u",
            mqtt5::SubscribeOptions {
                qos: QoS::ExactlyOnce,
                ..Default::default()
            },
            move |message| {
                sink.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&message.payload).into_owned());
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), finished)
        .await
        .unwrap()
        .unwrap();
    client.disconnect().await.unwrap();
    let second = ConnectOptions::new("b")
        .with_clean_start(false)
        .with_session_expiry_interval(300)
        .with_automatic_reconnect(false);
    let result = client
        .connect_with_options(&format!("mqtt://{addr}"), second)
        .await
        .unwrap();
    assert!(result.session_present, "broker had a session for b");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let _ = client.disconnect().await;
    let payloads = seen.lock().unwrap().clone();
    payloads
}

#[tokio::test]
async fn a_message_queued_for_the_new_id_is_delivered_despite_old_inbound_qos2_state() {
    let payloads = switch_ids_with_inbound_qos2(false).await;
    assert!(
        payloads.iter().any(|p| p == "queued-for-b"),
        "received: {payloads:?}"
    );
}

#[tokio::test]
async fn control_a_message_queued_for_the_new_id_is_delivered_without_old_inbound_state() {
    let payloads = switch_ids_with_inbound_qos2(true).await;
    assert!(
        payloads.iter().any(|p| p == "queued-for-b"),
        "received: {payloads:?}"
    );
}
