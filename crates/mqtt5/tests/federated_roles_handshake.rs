#![cfg(feature = "broker")]

use base64::prelude::*;
use mqtt5::broker::acl::{AclManager, Permission};
use mqtt5::broker::auth::AuthProvider;
use mqtt5::broker::auth_mechanisms::FederatedJwtAuthProvider;
use mqtt5::broker::config::{
    BrokerConfig, FederatedAuthMode, JwtAlgorithm, JwtIssuerConfig, JwtKeySource, StorageBackend,
    StorageConfig,
};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::MqttBroker;
use mqtt5::ConnectOptions;
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
const USER1: &str = "idp.test:user1";

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

struct Slow {
    inner: FederatedJwtAuthProvider,
    delay: std::sync::atomic::AtomicBool,
}

impl AuthProvider for Slow {
    fn authenticate<'a>(
        &'a self,
        connect: &'a ConnectPacket,
        addr: SocketAddr,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = mqtt5::Result<mqtt5::broker::auth::AuthResult>>
                + Send
                + 'a,
        >,
    > {
        self.inner.authenticate(connect, addr)
    }
    fn authorize_publish<'a>(
        &'a self,
        c: &str,
        u: Option<&'a str>,
        t: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        self.inner.authorize_publish(c, u, t)
    }
    fn authorize_subscribe<'a>(
        &'a self,
        c: &str,
        u: Option<&'a str>,
        t: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        let slow = self.delay.load(std::sync::atomic::Ordering::SeqCst);
        let fut = self.inner.authorize_subscribe(c, u, t);
        Box::pin(async move {
            let verdict = fut.await;
            if slow {
                tokio::time::sleep(Duration::from_millis(800)).await;
            }
            verdict
        })
    }
    fn checks_delivery(&self) -> bool {
        self.inner.checks_delivery()
    }
    fn authorize_delivery<'a>(
        &'a self,
        c: &str,
        u: Option<&'a str>,
        t: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        self.inner.authorize_delivery(c, u, t)
    }
    fn supports_enhanced_auth(&self) -> bool {
        true
    }
    fn authenticate_enhanced<'a>(
        &'a self,
        m: &'a str,
        d: Option<&'a [u8]>,
        c: &'a str,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = mqtt5::Result<mqtt5::broker::auth::EnhancedAuthResult>>
                + Send
                + 'a,
        >,
    > {
        self.inner.authenticate_enhanced(m, d, c)
    }
    fn reauthenticate<'a>(
        &'a self,
        m: &'a str,
        d: Option<&'a [u8]>,
        c: &'a str,
        u: Option<&'a str>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = mqtt5::Result<mqtt5::broker::auth::EnhancedAuthResult>>
                + Send
                + 'a,
        >,
    > {
        self.inner.reauthenticate(m, d, c, u)
    }
    fn cleanup_session<'a>(
        &'a self,
        u: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        self.inner.cleanup_session(u)
    }
}

async fn route(router: &MessageRouter, topic: &str) {
    let publish = PublishPacket::new(topic, &b"x"[..], QoS::AtMostOnce);
    router.route_message(&publish, None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sibling_closing_during_a_handshake_keeps_the_new_connections_roles() {
    scenario(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sibling_staying_open_keeps_the_new_connections_roles() {
    scenario(false).await;
}

async fn scenario(sibling_closes: bool) {
    let dir = tempfile::tempdir().unwrap();
    let acl = reader_acl().await;
    let slow = Arc::new(Slow {
        inner: federated(&dir, Arc::clone(&acl)),
        delay: std::sync::atomic::AtomicBool::new(false),
    });
    let (addr, router) = start(Arc::clone(&slow) as Arc<dyn AuthProvider>).await;

    let mut a1 = Raw::open(addr).await;
    a1.send(&connect_packet("dev-a", "user1", true)).await;
    a1.frames(200).await;
    a1.raw(&subscribe_bytes("data/#")).await;
    let suback = a1.frames(200).await;
    assert_eq!(suback[0][0], 0x90);
    a1.raw(&DISCONNECT).await;
    a1.frames(200).await;

    let mut b = Raw::open(addr).await;
    b.send(&connect_packet("dev-b", "user1", true)).await;
    let b_connack = b.frames(200).await;
    assert_eq!(b_connack[0][0], 0x20);

    slow.delay.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut a2 = Raw::open(addr).await;
    a2.send(&connect_packet("dev-a", "user1", false)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    if sibling_closes {
        b.raw(&DISCONNECT).await;
    }
    let a2_connack = a2.frames(1500).await;
    slow.delay.store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(a2_connack[0][0], 0x20, "a2 connected");
    assert_eq!(a2_connack[0][2], 1, "session present");

    let roles = acl.read().await.get_federated_roles(USER1).await;
    route(&router, "data/open").await;
    route(&router, "data/secret/x").await;
    let topics: Vec<String> = a2
        .frames(400)
        .await
        .iter()
        .filter_map(|f| publish_topic(f))
        .collect();
    assert!(roles.is_some(), "roles cleared while dev-a is connected");
    assert_eq!(topics, ["data/open"]);
}
