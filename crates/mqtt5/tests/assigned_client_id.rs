#![cfg(feature = "broker")]

mod common;

use common::TestBroker;
use mqtt5::{ConnectOptions, MqttClient};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

async fn unnamed_client_id(broker: &TestBroker) -> String {
    let options = ConnectOptions::new("")
        .with_clean_start(false)
        .with_session_expiry_interval(300);
    let client = MqttClient::with_options(options.clone());
    let session_present = Box::pin(client.connect_with_options(broker.address(), options))
        .await
        .map(|result| result.session_present);
    assert!(
        matches!(session_present, Ok(false)),
        "unnamed client was given an existing session: {session_present:?}"
    );
    let client_id = client.client_id().await;
    client
        .disconnect()
        .await
        .expect("unnamed client disconnects");
    client_id
}

#[tokio::test]
async fn an_unnamed_client_never_gets_an_existing_session() {
    let broker = TestBroker::start().await;
    let options = ConnectOptions::new("auto-0")
        .with_clean_start(true)
        .with_session_expiry_interval(300);
    let owner = MqttClient::with_options(options.clone());
    Box::pin(owner.connect_with_options(broker.address(), options))
        .await
        .expect("named client connects");
    owner
        .subscribe("assigned/owner", |_| {})
        .await
        .expect("named client subscribes");
    owner.disconnect().await.expect("named client disconnects");

    let first = unnamed_client_id(&broker).await;
    let second = unnamed_client_id(&broker).await;

    assert_ne!(first, second);
    for id in [&first, &second] {
        let suffix = id.strip_prefix("auto-").expect("assigned id prefix");
        assert_eq!(suffix.len(), 32, "{id}");
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
    }
}

async fn v311_connack(broker: &TestBroker, client_id: &str, clean_session: bool) -> [u8; 4] {
    let addr = broker
        .address()
        .strip_prefix("mqtt://")
        .expect("tcp broker address");
    let mut stream = TcpStream::connect(addr).await.expect("tcp connects");
    let id = client_id.as_bytes();
    let id_len = u16::try_from(id.len()).expect("short client id");
    let mut connect = vec![0x10, 0x00, 0x00, 0x04, b'M', b'Q', b'T', b'T', 0x04];
    connect.push(if clean_session { 0x02 } else { 0x00 });
    connect.extend_from_slice(&60u16.to_be_bytes());
    connect.extend_from_slice(&id_len.to_be_bytes());
    connect.extend_from_slice(id);
    connect[1] = u8::try_from(connect.len() - 2).expect("short connect");
    stream.write_all(&connect).await.expect("connect sent");
    let mut connack = [0u8; 4];
    tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut connack))
        .await
        .expect("connack arrives")
        .expect("connack read");
    connack
}

#[tokio::test]
async fn v311_empty_client_id_needs_clean_session() {
    let broker = TestBroker::start().await;
    assert_eq!(
        v311_connack(&broker, "", true).await,
        [0x20, 0x02, 0x00, 0x00]
    );
    assert_eq!(
        v311_connack(&broker, "", false).await,
        [0x20, 0x02, 0x00, 0x02]
    );
}

#[tokio::test]
async fn v311_rejected_client_id_gets_a_v311_connack() {
    let broker = TestBroker::start().await;
    assert_eq!(
        v311_connack(&broker, "a/b", true).await,
        [0x20, 0x02, 0x00, 0x02]
    );
}
