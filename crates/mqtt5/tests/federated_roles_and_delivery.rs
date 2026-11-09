#![cfg(feature = "broker")]

use base64::prelude::*;
use mqtt5::broker::acl::{AclManager, AclRule, Permission};
use mqtt5::broker::auth::{
    AuthProvider, AuthorizationMode, CompositeAuthProvider, ComprehensiveAuthProvider,
    PasswordAuthProvider,
};
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
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
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

fn topics(frames: &[Vec<u8>]) -> Vec<String> {
    frames.iter().filter_map(|f| publish_topic(f)).collect()
}

async fn route(router: &MessageRouter, topic: &str) {
    router
        .route_message(&PublishPacket::new(topic, &b"x"[..], QoS::AtMostOnce), None)
        .await;
}

fn comprehensive(acl: AclManager) -> Arc<ComprehensiveAuthProvider> {
    Arc::new(ComprehensiveAuthProvider::with_providers(
        PasswordAuthProvider::new().with_anonymous(true),
        acl,
    ))
}

#[tokio::test]
async fn composite_or_withholds_a_topic_both_providers_refuse() {
    let primary = comprehensive(AclManager::new());
    let fallback_acl = AclManager::new();
    fallback_acl
        .add_rule(AclRule::new("*", "data/#", Permission::ReadWrite))
        .await;
    fallback_acl
        .add_rule(AclRule::new("*", "data/secret/#", Permission::Deny))
        .await;
    let composite = Arc::new(
        CompositeAuthProvider::new(primary, comprehensive(fallback_acl))
            .with_authorization_mode(AuthorizationMode::Or),
    );
    assert!(
        !composite
            .authorize_subscribe("c", None, "data/secret/x")
            .await,
        "control: both providers refuse data/secret/x"
    );
    assert!(
        composite.authorize_subscribe("c", None, "data/#").await,
        "control: data/# is granted"
    );
    let (addr, router) = start(composite).await;
    let mut sub = Raw::open(addr).await;
    sub.send(&ConnectPacket::new(
        ConnectOptions::new("or-sub")
            .with_clean_start(true)
            .protocol_options,
    ))
    .await;
    sub.frames(300).await;
    sub.raw(&subscribe_bytes("data/#")).await;
    sub.frames(300).await;
    route(&router, "data/secret/x").await;
    route(&router, "data/open").await;
    let got = topics(&sub.frames(400).await);
    assert_eq!(got, ["data/open"]);
}

#[tokio::test]
async fn the_fallback_acl_alone_withholds_the_topic() {
    let acl = AclManager::new();
    acl.add_rule(AclRule::new("*", "data/#", Permission::ReadWrite))
        .await;
    acl.add_rule(AclRule::new("*", "data/secret/#", Permission::Deny))
        .await;
    let (addr, router) = start(comprehensive(acl)).await;
    let mut sub = Raw::open(addr).await;
    sub.send(&ConnectPacket::new(
        ConnectOptions::new("alone-sub")
            .with_clean_start(true)
            .protocol_options,
    ))
    .await;
    sub.frames(300).await;
    sub.raw(&subscribe_bytes("data/#")).await;
    sub.frames(300).await;
    route(&router, "data/secret/x").await;
    route(&router, "data/open").await;
    let got = topics(&sub.frames(400).await);
    assert_eq!(got, ["data/open"]);
}

fn token(sub: &str, roles: &[&str]) -> String {
    let header = BASE64_URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let roles = roles
        .iter()
        .map(|r| format!("\"{r}\""))
        .collect::<Vec<_>>()
        .join(",");
    let payload = BASE64_URL_SAFE_NO_PAD.encode(format!(
        r#"{{"sub":"{sub}","iss":"https://idp.test","exp":{exp},"roles":[{roles}]}}"#
    ));
    let message = format!("{header}.{payload}");
    let signing = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, KEY);
    let signature = ring::hmac::sign(&signing, message.as_bytes());
    format!(
        "{message}.{}",
        BASE64_URL_SAFE_NO_PAD.encode(signature.as_ref())
    )
}

async fn trusted_roles_broker(
    dir: &tempfile::TempDir,
    session_scoped: bool,
) -> (SocketAddr, Arc<MessageRouter>, Arc<RwLock<AclManager>>) {
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
    .with_auth_mode(FederatedAuthMode::TrustedRoles)
    .with_session_scoped_roles(session_scoped);
    let acl = AclManager::new();
    acl.add_role("secret").await;
    acl.add_role_rule("secret", "secret/#", Permission::ReadWrite)
        .await
        .unwrap();
    acl.add_role("open").await;
    acl.add_role_rule("open", "open/#", Permission::ReadWrite)
        .await
        .unwrap();
    let acl = Arc::new(RwLock::new(acl));
    let provider = FederatedJwtAuthProvider::new(vec![issuer])
        .unwrap()
        .with_acl_manager(Arc::clone(&acl));
    let (addr, router) = start(Arc::new(provider)).await;
    (addr, router, acl)
}

fn jwt_connect(id: &str, token: String) -> ConnectPacket {
    ConnectPacket::new(
        ConnectOptions::new(id)
            .with_clean_start(true)
            .with_authentication_method("JWT")
            .with_authentication_data(token)
            .protocol_options,
    )
}

#[tokio::test]
async fn a_same_user_reauthentication_losing_a_role_drops_its_subscription() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, router, acl) = trusted_roles_broker(&dir, true).await;
    let mut conn = Raw::open(addr).await;
    conn.send(&jwt_connect("shrink", token("user1", &["secret", "open"])))
        .await;
    let connack = conn.frames(300).await;
    assert_eq!(connack.first().map(|f| (f[0], f[3])), Some((0x20, 0x00)));
    conn.raw(&subscribe_bytes("secret/#")).await;
    let suback = conn.frames(300).await;
    assert_eq!(suback.first().map(|f| f[0]), Some(0x90));
    route(&router, "secret/before").await;
    assert_eq!(topics(&conn.frames(300).await), ["secret/before"]);

    let mut reauth = AuthPacket::new(ReasonCode::ReAuthenticate);
    reauth
        .properties
        .set_authentication_method("JWT".to_string());
    reauth
        .properties
        .set_authentication_data(token("user1", &["open"]).into_bytes().into());
    conn.send(&reauth).await;
    let reply = conn.frames(300).await;
    assert_eq!(reply.first().map(|f| f[0]), Some(0xF0), "AUTH success");
    let roles = acl
        .read()
        .await
        .get_federated_roles("idp.test:user1")
        .await
        .unwrap()
        .roles;
    assert!(!roles.contains("secret"), "control: secret role revoked");
    assert!(
        !acl.read()
            .await
            .check_subscribe(Some("idp.test:user1"), "secret/#")
            .await,
        "control: a new SUBSCRIBE to secret/# would be refused"
    );

    route(&router, "secret/after").await;
    let got = topics(&conn.frames(400).await);
    assert!(got.is_empty(), "revoked role still delivers: {got:?}");
}

#[tokio::test]
async fn a_federated_users_will_is_published_with_session_scoped_roles() {
    will_scenario(true).await;
}

#[tokio::test]
async fn a_federated_users_will_is_published_with_unscoped_roles() {
    will_scenario(false).await;
}

async fn will_scenario(session_scoped: bool) {
    let dir = tempfile::tempdir().unwrap();
    let (addr, router, _acl) = trusted_roles_broker(&dir, session_scoped).await;
    let mut watcher = Raw::open(addr).await;
    watcher
        .send(&jwt_connect("watcher", token("user2", &["open"])))
        .await;
    watcher.frames(300).await;
    watcher.raw(&subscribe_bytes("open/#")).await;
    watcher.frames(300).await;
    route(&router, "open/control").await;
    assert_eq!(topics(&watcher.frames(300).await), ["open/control"]);

    let mut dying = Raw::open(addr).await;
    dying
        .send(&ConnectPacket::new(
            ConnectOptions::new("dying")
                .with_clean_start(true)
                .with_authentication_method("JWT")
                .with_authentication_data(token("user1", &["open"]))
                .with_will(mqtt5_protocol::WillMessage::new(
                    "open/will",
                    b"gone".to_vec(),
                ))
                .protocol_options,
        ))
        .await;
    let connack = dying.frames(300).await;
    assert_eq!(connack.first().map(|f| (f[0], f[3])), Some((0x20, 0x00)));
    drop(dying);
    let got = topics(&watcher.frames(600).await);
    assert_eq!(got, ["open/will"]);
}

struct SlowCleanup;

impl AuthProvider for SlowCleanup {
    fn authenticate<'a>(
        &'a self,
        connect: &'a ConnectPacket,
        _client_addr: SocketAddr,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = mqtt5::error::Result<mqtt5::broker::auth::AuthResult>>
                + Send
                + 'a,
        >,
    > {
        let user = connect.username.clone().unwrap_or_default();
        Box::pin(async move { Ok(mqtt5::broker::auth::AuthResult::success_with_user(user)) })
    }
    fn authorize_publish<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async { true })
    }
    fn authorize_subscribe<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic_filter: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async { true })
    }
    fn cleanup_session<'a>(
        &'a self,
        _user_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async { tokio::time::sleep(Duration::from_secs(3)).await })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_cleanup_session_doesnt_hold_up_another_users_connect() {
    let (addr, _router) = start(Arc::new(SlowCleanup)).await;
    let mut alice = Raw::open(addr).await;
    alice
        .send(&ConnectPacket::new(
            ConnectOptions::new("alice-c")
                .with_credentials("alice", b"x")
                .protocol_options,
        ))
        .await;
    alice.frames(300).await;
    alice.raw(&[0xE0, 0x00]).await;
    alice.frames(100).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let started = tokio::time::Instant::now();
    let mut bob = Raw::open(addr).await;
    bob.send(&ConnectPacket::new(
        ConnectOptions::new("bob-c")
            .with_credentials("bob", b"x")
            .protocol_options,
    ))
    .await;
    let mut connack = Vec::new();
    while connack.is_empty() && started.elapsed() < Duration::from_secs(6) {
        connack = bob.frames(100).await;
    }
    let waited = started.elapsed();
    assert_eq!(connack.first().map(|f| f[0]), Some(0x20));
    assert!(waited < Duration::from_secs(1), "bob waited {waited:?}");
}
