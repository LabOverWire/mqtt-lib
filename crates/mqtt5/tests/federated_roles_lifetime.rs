#![cfg(feature = "broker")]

use base64::prelude::*;
use mqtt5::broker::acl::{AclManager, Permission};
use mqtt5::broker::auth::{AuthProvider, CompositeAuthProvider};
use mqtt5::broker::auth_mechanisms::FederatedJwtAuthProvider;
use mqtt5::broker::config::{
    BrokerConfig, FederatedAuthMode, JwtAlgorithm, JwtIssuerConfig, JwtKeySource, StorageBackend,
    StorageConfig,
};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::MqttBroker;
use mqtt5::ConnectOptions;
use mqtt5_protocol::packet::auth::AuthPacket;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::QoS;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::RwLock;

const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";
async fn start(auth: Arc<dyn AuthProvider>) -> (SocketAddr, Arc<MessageRouter>) {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        });
    let mut broker = MqttBroker::with_config(config)
        .await
        .unwrap()
        .with_auth_provider(auth);
    let addr = broker.local_addr().unwrap();
    let router = broker.router();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    (addr, router)
}

fn token(sub: &str) -> String {
    let header = BASE64_URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let payload = BASE64_URL_SAFE_NO_PAD.encode(format!(
        r#"{{"sub":"{sub}","iss":"https://idp.test","exp":{exp}}}"#
    ));
    let message = format!("{header}.{payload}");
    let signing = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, KEY);
    let signature = ring::hmac::sign(&signing, message.as_bytes());
    format!(
        "{message}.{}",
        BASE64_URL_SAFE_NO_PAD.encode(signature.as_ref())
    )
}

fn connect_packet(id: &str, sub: &str, clean_start: bool) -> ConnectPacket {
    ConnectPacket::new(
        ConnectOptions::new(id)
            .with_clean_start(clean_start)
            .with_session_expiry_interval(3600)
            .with_authentication_method("JWT")
            .with_authentication_data(token(sub))
            .protocol_options,
    )
}

fn federated(dir: &tempfile::TempDir, acl: Arc<RwLock<AclManager>>) -> FederatedJwtAuthProvider {
    let key_path = dir.path().join("key");
    std::fs::write(&key_path, KEY).unwrap();
    let issuer = JwtIssuerConfig::new(
        "idp",
        "https://idp.test",
        JwtKeySource::StaticFile {
            algorithm: JwtAlgorithm::HS256,
            path: key_path,
        },
    )
    .with_auth_mode(FederatedAuthMode::ClaimBinding)
    .with_default_roles(vec!["reader".to_string()])
    .with_session_scoped_roles(true);
    FederatedJwtAuthProvider::new(vec![issuer])
        .unwrap()
        .with_acl_manager(acl)
}

async fn reader_acl() -> Arc<RwLock<AclManager>> {
    let acl = AclManager::new();
    acl.add_role("reader").await;
    acl.add_role_rule("reader", "data/#", Permission::ReadWrite)
        .await
        .unwrap();
    acl.add_role_rule("reader", "data/secret/#", Permission::Deny)
        .await
        .unwrap();
    Arc::new(RwLock::new(acl))
}

struct Raw {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl Raw {
    async fn open(addr: SocketAddr) -> Self {
        Self {
            stream: TcpStream::connect(addr).await.unwrap(),
            buf: Vec::new(),
        }
    }

    async fn send<P: MqttPacket>(&mut self, packet: &P) {
        let mut bytes = bytes::BytesMut::new();
        packet.encode(&mut bytes).unwrap();
        self.stream.write_all(&bytes).await.unwrap();
    }

    async fn raw(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).await.unwrap();
    }

    async fn frames(&mut self, wait_ms: u64) -> Vec<Vec<u8>> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
        let mut chunk = [0u8; 4096];
        while let Ok(Ok(n)) = tokio::time::timeout_at(deadline, self.stream.read(&mut chunk)).await
        {
            if n == 0 {
                break;
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
        let mut out = Vec::new();
        let mut at = 0;
        while at < self.buf.len() {
            let mut len = 0usize;
            let mut shift = 0;
            let mut i = at + 1;
            loop {
                let b = self.buf[i];
                len |= usize::from(b & 0x7f) << shift;
                shift += 7;
                i += 1;
                if b & 0x80 == 0 {
                    break;
                }
            }
            out.push(self.buf[at..i + len].to_vec());
            at = i + len;
        }
        self.buf.clear();
        out
    }
}

fn subscribe_bytes(topic: &str) -> Vec<u8> {
    let topic_len = u8::try_from(topic.len()).unwrap();
    let mut bytes = vec![0x82, 6 + topic_len, 0x00, 0x01, 0x00, 0x00, topic_len];
    bytes.extend_from_slice(topic.as_bytes());
    bytes.push(0);
    bytes
}

fn publish_topic(frame: &[u8]) -> Option<String> {
    if frame[0] & 0xF0 != 0x30 {
        return None;
    }
    let mut i = 1;
    while frame[i] & 0x80 != 0 {
        i += 1;
    }
    i += 1;
    let len = usize::from(frame[i]) << 8 | usize::from(frame[i + 1]);
    Some(String::from_utf8_lossy(&frame[i + 2..i + 2 + len]).into_owned())
}

const DISCONNECT: [u8; 2] = [0xE0, 0x00];

#[tokio::test]
async fn an_auth_continue_outside_an_exchange_is_a_protocol_error() {
    let dir = tempfile::tempdir().unwrap();
    let acl = reader_acl().await;
    let (addr, _router) = start(Arc::new(federated(&dir, Arc::clone(&acl)))).await;

    let mut conn = Raw::open(addr).await;
    conn.send(&connect_packet("cont", "user1", true)).await;
    conn.frames(300).await;
    let auth =
        AuthPacket::continue_authentication("JWT".to_string(), Some(token("user1").into_bytes()))
            .unwrap();
    conn.send(&auth).await;
    let reply = conn.frames(300).await;
    assert_eq!(
        reply.first().map(|frame| (frame[0], frame[2])),
        Some((0xE0, 0x82))
    );
}

#[tokio::test]
async fn a_composite_of_two_federated_providers_keeps_the_roles_of_an_open_connection() {
    let dir = tempfile::tempdir().unwrap();
    let acl = reader_acl().await;
    let primary = federated(&dir, Arc::clone(&acl));
    let fallback = federated(&dir, Arc::clone(&acl));
    let composite = CompositeAuthProvider::new(Arc::new(primary), Arc::new(fallback));
    let (addr, router) = start(Arc::new(composite)).await;

    let mut watcher = Raw::open(addr).await;
    watcher
        .send(&connect_packet("watcher", "user1", true))
        .await;
    watcher.frames(300).await;
    watcher.raw(&subscribe_bytes("data/#")).await;
    watcher.frames(300).await;

    let mut other = Raw::open(addr).await;
    other.send(&connect_packet("other", "user1", true)).await;
    other.frames(300).await;
    other.raw(&DISCONNECT).await;
    other.frames(200).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    for topic in ["data/open", "data/secret/x"] {
        router
            .route_message(&PublishPacket::new(topic, &b"x"[..], QoS::AtMostOnce), None)
            .await;
    }
    let got: Vec<String> = watcher
        .frames(400)
        .await
        .iter()
        .filter_map(|f| publish_topic(f))
        .collect();
    assert_eq!(got, ["data/open"]);
}

#[tokio::test]
async fn a_composite_with_a_non_federated_fallback_keeps_the_roles_of_an_open_connection() {
    let dir = tempfile::tempdir().unwrap();
    let acl = reader_acl().await;
    let primary = federated(&dir, Arc::clone(&acl));
    let composite = CompositeAuthProvider::new(
        Arc::new(primary),
        Arc::new(mqtt5::broker::auth::AllowAllAuthProvider),
    );
    let (addr, router) = start(Arc::new(composite)).await;

    let mut watcher = Raw::open(addr).await;
    watcher
        .send(&connect_packet("watcher", "user1", true))
        .await;
    watcher.frames(300).await;
    watcher.raw(&subscribe_bytes("data/#")).await;
    watcher.frames(300).await;

    let mut other = Raw::open(addr).await;
    other.send(&connect_packet("other", "user1", true)).await;
    other.frames(300).await;
    other.raw(&DISCONNECT).await;
    other.frames(200).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    for topic in ["data/open", "data/secret/x"] {
        router
            .route_message(&PublishPacket::new(topic, &b"x"[..], QoS::AtMostOnce), None)
            .await;
    }
    let got: Vec<String> = watcher
        .frames(400)
        .await
        .iter()
        .filter_map(|f| publish_topic(f))
        .collect();
    assert_eq!(got, ["data/open"]);
}

#[tokio::test]
async fn a_reauthentication_as_another_user_gives_the_connection_that_users_roles() {
    let dir = tempfile::tempdir().unwrap();
    let acl = reader_acl().await;
    let (addr, _router) = start(Arc::new(federated(&dir, Arc::clone(&acl)))).await;
    let mut conn = Raw::open(addr).await;
    conn.send(&connect_packet("moving", "user1", true)).await;
    conn.frames(300).await;
    let mut reauth =
        AuthPacket::new(mqtt5_protocol::protocol::v5::reason_codes::ReasonCode::ReAuthenticate);
    reauth
        .properties
        .set_authentication_method("JWT".to_string());
    reauth
        .properties
        .set_authentication_data(token("user2").into_bytes().into());
    conn.send(&reauth).await;
    conn.frames(300).await;
    assert!(acl
        .read()
        .await
        .get_federated_roles("idp.test:user2")
        .await
        .is_some());
}
