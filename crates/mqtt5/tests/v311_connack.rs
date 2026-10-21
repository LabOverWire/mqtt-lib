#![cfg(feature = "broker")]

use mqtt5::broker::auth::{AuthProvider, AuthResult};
use mqtt5::broker::config::{BrokerConfig, LoadBalancerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::server::BrokerShutdownHandle;
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

struct Broker {
    addr: SocketAddr,
    shutdown: BrokerShutdownHandle,
    run: tokio::task::JoinHandle<Result<()>>,
}

fn config() -> BrokerConfig {
    BrokerConfig::default()
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        })
        .with_bind_address(([127, 0, 0, 1], 0))
}

impl Broker {
    async fn start() -> Self {
        Self::start_with(config()).await
    }

    async fn start_with(config: BrokerConfig) -> Self {
        let mut broker = MqttBroker::with_config(config)
            .await
            .expect("broker starts")
            .with_auth_provider(Arc::new(UsernameIsUser) as Arc<dyn AuthProvider>);
        let addr = broker.local_addr().expect("TCP listener bound");
        let shutdown = broker.shutdown_handle();
        let run = tokio::spawn(async move { broker.run().await });
        Self {
            addr,
            shutdown,
            run,
        }
    }

    async fn stop(self) {
        self.shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(5), self.run)
            .await
            .expect("broker stops")
            .expect("broker task joins")
            .expect("broker run succeeds");
    }
}

struct Connect<'a> {
    level: u8,
    client_id: &'a str,
    clean_session: bool,
    username: Option<&'a str>,
}

const CLEAN: Connect<'static> = Connect {
    level: 4,
    client_id: "v311-client",
    clean_session: true,
    username: None,
};

fn encode(connect: &Connect<'_>) -> ([u8; 128], usize) {
    let id = connect.client_id.as_bytes();
    let user = connect.username.unwrap_or_default().as_bytes();
    let user_flag = if connect.username.is_some() {
        0x80
    } else {
        0x00
    };
    let clean_flag = if connect.clean_session { 0x02 } else { 0x00 };
    let id_len = u16::try_from(id.len())
        .expect("short client id")
        .to_be_bytes();
    let user_len = u16::try_from(user.len())
        .expect("short username")
        .to_be_bytes();
    let user_len: &[u8] = if connect.username.is_some() {
        &user_len
    } else {
        &[]
    };
    let mut frame = [0u8; 128];
    frame[0] = 0x10;
    let mut len = 2;
    for part in [
        b"\x00\x04MQTT".as_slice(),
        &[connect.level, user_flag | clean_flag],
        &60u16.to_be_bytes(),
        &id_len,
        id,
        user_len,
        user,
    ] {
        frame[len..len + part.len()].copy_from_slice(part);
        len += part.len();
    }
    frame[1] = u8::try_from(len - 2).expect("remaining length fits one byte");
    (frame, len)
}

async fn connack(addr: SocketAddr, connect: &Connect<'_>) -> (TcpStream, [u8; 4]) {
    let mut stream = TcpStream::connect(addr).await.expect("tcp connects");
    let (frame, len) = encode(connect);
    stream.write_all(&frame[..len]).await.expect("connect sent");
    let mut connack = [0u8; 4];
    tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut connack))
        .await
        .expect("connack arrives")
        .expect("connack read");
    (stream, connack)
}

async fn disconnect(mut stream: TcpStream) {
    stream
        .write_all(&[0xE0, 0x00])
        .await
        .expect("disconnect sent");
    let mut rest = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut rest))
        .await
        .expect("broker closes the connection");
    assert!(matches!(read, Ok(0)), "connection still open: {read:?}");
}

#[tokio::test]
async fn v311_empty_client_id_needs_clean_session() {
    let broker = Broker::start().await;
    let empty = Connect {
        client_id: "",
        ..CLEAN
    };
    assert_eq!(
        connack(broker.addr, &empty).await.1,
        [0x20, 0x02, 0x00, 0x00]
    );
    let persistent = Connect {
        clean_session: false,
        ..empty
    };
    assert_eq!(
        connack(broker.addr, &persistent).await.1,
        [0x20, 0x02, 0x00, 0x02]
    );
    broker.stop().await;
}

#[tokio::test]
async fn v311_rejected_client_id_gets_a_v311_connack() {
    let broker = Broker::start().await;
    let invalid = Connect {
        client_id: "a/b",
        ..CLEAN
    };
    assert_eq!(
        connack(broker.addr, &invalid).await.1,
        [0x20, 0x02, 0x00, 0x02]
    );
    broker.stop().await;
}

#[tokio::test]
async fn unsupported_protocol_levels_below_5_get_a_v311_connack() {
    let broker = Broker::start().await;
    for level in [2, 3] {
        let old = Connect { level, ..CLEAN };
        assert_eq!(
            connack(broker.addr, &old).await.1,
            [0x20, 0x02, 0x00, 0x01],
            "level {level}"
        );
    }
    broker.stop().await;
}

#[tokio::test]
async fn v311_session_owned_by_another_user_is_refused_with_not_authorized() {
    let broker = Broker::start().await;
    let owner = Connect {
        client_id: "owned-v311",
        clean_session: false,
        username: Some("owner"),
        ..CLEAN
    };
    let (stream, owner_connack) = connack(broker.addr, &owner).await;
    assert_eq!(owner_connack, [0x20, 0x02, 0x00, 0x00]);
    disconnect(stream).await;
    let intruder = Connect {
        username: Some("intruder"),
        ..owner
    };
    assert_eq!(
        connack(broker.addr, &intruder).await.1,
        [0x20, 0x02, 0x00, 0x05]
    );
    broker.stop().await;
}

#[tokio::test]
async fn v311_client_is_refused_by_a_load_balancer() {
    let broker = Broker::start_with(config().with_load_balancer(LoadBalancerConfig::new(vec![
        "mqtt://backend-a:1883".to_string(),
    ])))
    .await;
    let (mut stream, refused) = connack(broker.addr, &CLEAN).await;
    assert_eq!(refused, [0x20, 0x02, 0x00, 0x03]);
    let mut rest = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut rest))
        .await
        .expect("broker closes the connection");
    assert!(matches!(read, Ok(0)), "connection still open: {read:?}");
    broker.stop().await;
}
