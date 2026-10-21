#![cfg(feature = "broker")]

use mqtt5::broker::auth::{AuthProvider, AuthResult};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::error::Result;
use mqtt5_protocol::packet::connect::ConnectPacket;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct UsernameIsUser;

impl AuthProvider for UsernameIsUser {
    fn authenticate<'a>(
        &'a self,
        connect: &'a ConnectPacket,
        _client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        Box::pin(async move {
            Ok(connect
                .username
                .clone()
                .map_or_else(AuthResult::success, AuthResult::success_with_user))
        })
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
        _topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { true })
    }
}

async fn connack(addr: SocketAddr, username: &str) -> [u8; 4] {
    let mut stream = TcpStream::connect(addr).await.expect("tcp connects");
    let client_id = b"owned-v311";
    let user = username.as_bytes();
    let mut connect = vec![0x10, 0x00, 0x00, 0x04, b'M', b'Q', b'T', b'T', 0x04, 0x80];
    connect.extend_from_slice(&60u16.to_be_bytes());
    connect.extend_from_slice(
        &u16::try_from(client_id.len())
            .expect("short id")
            .to_be_bytes(),
    );
    connect.extend_from_slice(client_id);
    connect.extend_from_slice(&u16::try_from(user.len()).expect("short user").to_be_bytes());
    connect.extend_from_slice(user);
    connect[1] = u8::try_from(connect.len() - 2).expect("short connect");
    stream.write_all(&connect).await.expect("connect sent");
    let mut connack = [0u8; 4];
    tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut connack))
        .await
        .expect("connack arrives")
        .expect("connack read");
    stream.write_all(&[0xE0, 0x00]).await.ok();
    connack
}

#[tokio::test]
async fn v311_session_owned_by_another_user_is_refused_with_not_authorized() {
    let config = BrokerConfig::default()
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        })
        .with_bind_address(([127, 0, 0, 1], 0));
    let mut broker = MqttBroker::with_config(config)
        .await
        .expect("broker starts")
        .with_auth_provider(Arc::new(UsernameIsUser) as Arc<dyn AuthProvider>);
    let addr = broker.local_addr().expect("TCP listener bound");
    let shutdown = broker.shutdown_handle();
    let run = tokio::spawn(async move { broker.run().await });

    assert_eq!(connack(addr, "owner").await, [0x20, 0x02, 0x00, 0x00]);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(connack(addr, "intruder").await, [0x20, 0x02, 0x00, 0x05]);

    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("broker stops")
        .expect("broker task joins")
        .expect("broker run succeeds");
}
