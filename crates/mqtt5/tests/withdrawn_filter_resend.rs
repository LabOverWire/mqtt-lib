#![cfg(feature = "broker")]

use mqtt5::broker::auth::{AuthProvider, AuthResult};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::error::Result;
use mqtt5::packet::connect::ConnectPacket;
use mqtt5::MqttClient;
use std::collections::HashSet;
use std::future::Future;
use std::net::SocketAddr;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const TOPIC: &str = "secret/x";
const CLIENT: &[u8] = b"half-open";
const CONNACK: u8 = 2;
const PUBLISH: u8 = 3;
const SETTLE: Duration = Duration::from_millis(500);

#[derive(Default)]
struct Provider {
    denied: Mutex<HashSet<String>>,
}

impl AuthProvider for Provider {
    fn authenticate<'a>(
        &'a self,
        _connect: &'a ConnectPacket,
        _address: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        Box::pin(async move { Ok(AuthResult::success()) })
    }

    fn authorize_publish<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { true })
    }

    fn authorize_subscribe<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        let denied = self.denied.lock().unwrap().contains(filter);
        Box::pin(async move { !denied })
    }
}

async fn start(
    provider: Arc<Provider>,
    storage: StorageConfig,
) -> (String, tokio::task::JoinHandle<()>) {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage);
    let mut broker = MqttBroker::with_config(config)
        .await
        .unwrap()
        .with_auth_provider(provider);
    let address = broker.local_addr().unwrap().to_string();
    let mut ready = broker.ready_receiver();
    let task = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&is_ready| is_ready).await.unwrap();
    (address, task)
}

fn memory() -> StorageConfig {
    StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    }
}

fn file(directory: &Path) -> StorageConfig {
    StorageConfig {
        backend: StorageBackend::File,
        base_dir: directory.to_path_buf(),
        enable_persistence: true,
        ..Default::default()
    }
}

fn prefixed(bytes: &[u8]) -> Vec<u8> {
    let mut prefixed = u16::try_from(bytes.len()).unwrap().to_be_bytes().to_vec();
    prefixed.extend_from_slice(bytes);
    prefixed
}

fn frame(first_byte: u8, body: &[u8]) -> Vec<u8> {
    let length = u8::try_from(body.len()).unwrap();
    assert!(length < 128);
    let mut frame = vec![first_byte, length];
    frame.extend_from_slice(body);
    frame
}

fn connect_frame(clean_start: bool) -> Vec<u8> {
    let mut body = prefixed(b"MQTT");
    body.extend_from_slice(&[5, if clean_start { 2 } else { 0 }, 0, 60]);
    body.extend_from_slice(&[5, 0x11, 0, 0, 0x0E, 0x10]);
    body.extend_from_slice(&prefixed(CLIENT));
    frame(0x10, &body)
}

fn subscribe_frame() -> Vec<u8> {
    let mut body = vec![0, 1, 0];
    body.extend_from_slice(&prefixed(TOPIC.as_bytes()));
    body.push(1);
    frame(0x82, &body)
}

async fn first_bytes_of_packets(stream: &mut TcpStream) -> Vec<u8> {
    let mut received = Vec::new();
    let mut buffer = [0u8; 4096];
    while let Ok(Ok(read)) = tokio::time::timeout(SETTLE, stream.read(&mut buffer)).await {
        if read == 0 {
            break;
        }
        received.extend_from_slice(&buffer[..read]);
    }
    let mut first_bytes = Vec::new();
    let mut rest = received.as_slice();
    while let [first_byte, length, tail @ ..] = rest {
        assert!(*length < 128);
        first_bytes.push(*first_byte);
        rest = &tail[usize::from(*length)..];
    }
    first_bytes
}

fn resent_publishes(first_bytes: &[u8]) -> Vec<u8> {
    first_bytes
        .iter()
        .copied()
        .filter(|first_byte| first_byte >> 4 == PUBLISH)
        .collect()
}

async fn connect_raw(address: &str, clean_start: bool) -> TcpStream {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(&connect_frame(clean_start)).await.unwrap();
    stream
}

async fn subscribed_half_open_connection(address: &str) -> TcpStream {
    let mut stream = connect_raw(address, true).await;
    let connack = first_bytes_of_packets(&mut stream).await;
    assert_eq!(
        connack.first().map(|first_byte| first_byte >> 4),
        Some(CONNACK)
    );
    stream.write_all(&subscribe_frame()).await.unwrap();
    first_bytes_of_packets(&mut stream).await;
    stream
}

async fn publish(address: &str) {
    let publisher = MqttClient::new("publisher");
    publisher
        .connect(&format!("mqtt://{address}"))
        .await
        .unwrap();
    publisher
        .publish_qos1(TOPIC, b"published-after-the-withdrawal".to_vec())
        .await
        .unwrap();
    publisher.disconnect().await.unwrap();
}

async fn resume_after_publish(withdrawn: bool) -> Vec<u8> {
    let provider = Arc::new(Provider::default());
    let (address, task) = start(Arc::clone(&provider), memory()).await;
    let half_open = subscribed_half_open_connection(&address).await;
    if withdrawn {
        provider.denied.lock().unwrap().insert(TOPIC.to_string());
    }
    publish(&address).await;
    tokio::time::sleep(SETTLE).await;
    let mut resumed = connect_raw(&address, false).await;
    let first_bytes = first_bytes_of_packets(&mut resumed).await;
    task.abort();
    drop(half_open);
    resent_publishes(&first_bytes)
}

#[tokio::test]
async fn a_message_sent_to_a_half_open_connection_after_the_withdrawal_is_resent_with_dup() {
    assert_eq!(resume_after_publish(true).await, [0x3A]);
}

#[tokio::test]
async fn control_a_message_sent_to_a_half_open_connection_is_resent_with_dup() {
    assert_eq!(resume_after_publish(false).await, [0x3A]);
}

fn copy_directory(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_directory(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[tokio::test]
async fn the_same_message_is_resent_after_a_restart() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Arc::new(Provider::default());
    let (address, task) = start(Arc::clone(&provider), file(directory.path())).await;
    let half_open = subscribed_half_open_connection(&address).await;
    provider.denied.lock().unwrap().insert(TOPIC.to_string());
    publish(&address).await;
    tokio::time::sleep(SETTLE).await;
    let snapshot = tempfile::tempdir().unwrap();
    copy_directory(directory.path(), snapshot.path());
    task.abort();
    let (restarted_address, restarted) = start(provider, file(snapshot.path())).await;
    let mut resumed = connect_raw(&restarted_address, false).await;
    let first_bytes = first_bytes_of_packets(&mut resumed).await;
    restarted.abort();
    drop(half_open);
    assert_eq!(resent_publishes(&first_bytes), [0x3A]);
}
