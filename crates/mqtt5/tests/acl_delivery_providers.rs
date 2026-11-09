#![cfg(feature = "broker")]

use base64::prelude::*;
use mqtt5::broker::acl::{AclManager, AclRule, Permission};
use mqtt5::broker::auth::{
    AuthProvider, AuthResult, AuthorizationMode, CompositeAuthProvider, ComprehensiveAuthProvider,
    EnhancedAuthResult, PasswordAuthProvider,
};
use mqtt5::broker::auth_mechanisms::FederatedJwtAuthProvider;
use mqtt5::broker::config::{
    BrokerConfig, FederatedAuthMode, JwtAlgorithm, JwtIssuerConfig, JwtKeySource, StorageBackend,
    StorageConfig,
};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::MqttBroker;
use mqtt5::error::Result;
use mqtt5::{ConnectOptions, MqttClient, SubscribeOptions};
use mqtt5_protocol::packet::auth::AuthPacket;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use mqtt5_protocol::QoS;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::RwLock;

fn storage() -> StorageConfig {
    StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    }
}

fn config() -> BrokerConfig {
    BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage())
}

async fn start(auth: Arc<dyn AuthProvider>) -> (SocketAddr, Arc<MessageRouter>) {
    let mut broker = MqttBroker::with_config(config())
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

fn comprehensive(acl: AclManager) -> Arc<ComprehensiveAuthProvider> {
    Arc::new(ComprehensiveAuthProvider::with_providers(
        PasswordAuthProvider::new().with_anonymous(true),
        acl,
    ))
}

async fn route(router: &MessageRouter, topic: &str, qos: QoS, retain: bool) {
    let mut publish = PublishPacket::new(topic, &b"x"[..], qos);
    publish.retain = retain;
    router.route_message(&publish, None).await;
}

async fn sub(
    addr: SocketAddr,
    id: &str,
    filter: &str,
    qos: QoS,
) -> (MqttClient, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let client = MqttClient::new(id);
    client
        .connect_with_options(
            &format!("mqtt://{addr}"),
            ConnectOptions::new(id)
                .with_clean_start(true)
                .with_session_expiry_interval(3600),
        )
        .await
        .unwrap();
    let sink = Arc::clone(&seen);
    client
        .subscribe_with_options(
            filter,
            SubscribeOptions {
                qos,
                ..Default::default()
            },
            move |message| {
                sink.lock().unwrap().push(message.topic.clone());
            },
        )
        .await
        .unwrap();
    (client, seen)
}

async fn settled(seen: &Mutex<Vec<String>>) -> Vec<String> {
    tokio::time::sleep(Duration::from_millis(400)).await;
    seen.lock().unwrap().clone()
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

fn subscribe_bytes(topic: &str, qos: u8) -> Vec<u8> {
    let topic_len = u8::try_from(topic.len()).unwrap();
    let mut bytes = vec![0x82, 6 + topic_len, 0x00, 0x01, 0x00, 0x00, topic_len];
    bytes.extend_from_slice(topic.as_bytes());
    bytes.push(qos);
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

fn publish_topics(frames: &[Vec<u8>]) -> Vec<String> {
    frames.iter().filter_map(|f| publish_topic(f)).collect()
}

fn data_acl_rules() -> AclManager {
    AclManager::allow_all()
}

async fn deny_secret(acl: &AclManager) {
    acl.add_rule(AclRule::new(
        "*".to_string(),
        "data/secret/#".to_string(),
        Permission::Deny,
    ))
    .await;
}

fn federated_token(sub_claim: &str, key: &[u8]) -> String {
    let header = BASE64_URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let payload = BASE64_URL_SAFE_NO_PAD.encode(format!(
        r#"{{"sub":"{sub_claim}","iss":"https://idp.test","exp":{exp}}}"#
    ));
    let message = format!("{header}.{payload}");
    let signing = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    let signature = ring::hmac::sign(&signing, message.as_bytes());
    format!(
        "{message}.{}",
        BASE64_URL_SAFE_NO_PAD.encode(signature.as_ref())
    )
}

async fn federated(
    session_scoped: bool,
) -> (SocketAddr, Arc<MessageRouter>, Vec<u8>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let key = b"0123456789abcdef0123456789abcdef".to_vec();
    let key_path = dir.path().join("key");
    std::fs::write(&key_path, &key).unwrap();
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
    .with_session_scoped_roles(session_scoped);
    let acl = AclManager::new();
    acl.add_role("reader").await;
    acl.add_role_rule("reader", "data/#", Permission::ReadWrite)
        .await
        .unwrap();
    let provider = FederatedJwtAuthProvider::new(vec![issuer])
        .unwrap()
        .with_acl_manager(Arc::new(RwLock::new(acl)));
    let (addr, router) = start(Arc::new(provider)).await;
    (addr, router, key, dir)
}

fn federated_options(id: &str, key: &[u8], clean: bool) -> ConnectOptions {
    ConnectOptions::new(id)
        .with_clean_start(clean)
        .with_session_expiry_interval(3600)
        .with_authentication_method("JWT")
        .with_authentication_data(federated_token("user1", key))
}

async fn federated_second_connection_closing(session_scoped: bool) -> Vec<String> {
    let (addr, router, key, _dir) = federated(session_scoped).await;
    let url = format!("mqtt://{addr}");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let first = MqttClient::new("device-a");
    first
        .connect_with_options(&url, federated_options("device-a", &key, true))
        .await
        .unwrap();
    let sink = Arc::clone(&seen);
    first
        .subscribe("data/#", move |m| {
            sink.lock().unwrap().push(m.topic.clone());
        })
        .await
        .unwrap();
    route(&router, "data/one", QoS::AtMostOnce, false).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let second = MqttClient::new("device-b");
    second
        .connect_with_options(&url, federated_options("device-b", &key, true))
        .await
        .unwrap();
    second.disconnect().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    route(&router, "data/two", QoS::AtMostOnce, false).await;
    settled(&seen).await
}

#[tokio::test]
async fn federated_same_user_second_connection_closing_keeps_first_delivering() {
    let got = federated_second_connection_closing(true).await;
    assert_eq!(got, ["data/one", "data/two"]);
}

#[tokio::test]
async fn control_federated_same_user_with_unscoped_roles() {
    let got = federated_second_connection_closing(false).await;
    assert_eq!(got, ["data/one", "data/two"]);
}

#[tokio::test]
async fn federated_takeover_keeps_delivering_to_the_new_connection() {
    federated_takeover(true).await;
}

#[tokio::test]
async fn control_federated_takeover_with_unscoped_roles() {
    federated_takeover(false).await;
}

async fn federated_takeover(session_scoped: bool) {
    let (addr, router, key, _dir) = federated(session_scoped).await;
    let mut first = Raw::open(addr).await;
    first
        .send(&ConnectPacket::new(
            federated_options("takeover", &key, true).protocol_options,
        ))
        .await;
    first.frames(200).await;
    first.raw(&subscribe_bytes("data/#", 0)).await;
    first.frames(200).await;

    let mut second = Raw::open(addr).await;
    second
        .send(&ConnectPacket::new(
            federated_options("takeover", &key, false).protocol_options,
        ))
        .await;
    second.frames(500).await;
    route(&router, "data/after", QoS::AtMostOnce, false).await;
    let frames = second.frames(400).await;
    assert_eq!(publish_topics(&frames), ["data/after"]);
}

struct NarrowCustom;

impl AuthProvider for NarrowCustom {
    fn authenticate<'a>(
        &'a self,
        _connect: &'a ConnectPacket,
        _client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        Box::pin(async { Ok(AuthResult::success()) })
    }
    fn authorize_publish<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { false })
    }
    fn authorize_subscribe<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { false })
    }
}

#[tokio::test]
async fn composite_or_applies_the_primary_deny_when_the_fallback_refuses_the_topic() {
    let acl = data_acl_rules();
    deny_secret(&acl).await;
    let primary = comprehensive(acl);
    let composite = Arc::new(
        CompositeAuthProvider::new(primary, Arc::new(NarrowCustom))
            .with_authorization_mode(AuthorizationMode::Or),
    );
    let refused_directly = !composite
        .authorize_subscribe("c", None, "data/secret/x")
        .await;
    assert!(refused_directly, "control");
    let (addr, router) = start(composite).await;
    let (_client, seen) = sub(addr, "or", "data/#", QoS::AtLeastOnce).await;
    route(&router, "data/secret/x", QoS::AtLeastOnce, false).await;
    route(&router, "data/open", QoS::AtLeastOnce, false).await;
    let got = settled(&seen).await;
    assert_eq!(got, ["data/open"]);
}

#[tokio::test]
async fn read_all_with_narrower_write_rule_keeps_receiving() {
    let acl = AclManager::new();
    acl.add_rule(AclRule::new("*", "data/#", Permission::Read))
        .await;
    acl.add_rule(AclRule::new("*", "data/in/#", Permission::Write))
        .await;
    let (addr, router) = start(comprehensive(acl)).await;
    let (_client, seen) = sub(addr, "write-narrow", "data/#", QoS::AtLeastOnce).await;
    route(&router, "data/in/x", QoS::AtLeastOnce, false).await;
    route(&router, "data/open", QoS::AtLeastOnce, false).await;
    let got = settled(&seen).await;
    assert_eq!(got, ["data/in/x", "data/open"]);
}

struct Renaming;

impl AuthProvider for Renaming {
    fn authenticate<'a>(
        &'a self,
        _connect: &'a ConnectPacket,
        _client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        Box::pin(async { Ok(AuthResult::success()) })
    }
    fn authorize_publish<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { true })
    }
    fn authorize_subscribe<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { true })
    }
    fn supports_enhanced_auth(&self) -> bool {
        true
    }
    fn authenticate_enhanced<'a>(
        &'a self,
        auth_method: &'a str,
        _auth_data: Option<&'a [u8]>,
        _client_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        let method = auth_method.to_string();
        Box::pin(async move {
            Ok(EnhancedAuthResult::success_with_user(
                method,
                "alice".to_string(),
            ))
        })
    }
    fn reauthenticate<'a>(
        &'a self,
        auth_method: &'a str,
        _auth_data: Option<&'a [u8]>,
        _client_id: &'a str,
        _user_id: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        let method = auth_method.to_string();
        Box::pin(async move {
            Ok(EnhancedAuthResult::success_with_user(
                method,
                "bob".to_string(),
            ))
        })
    }
}

#[tokio::test]
async fn native_reauth_updates_the_stored_session_owner() {
    let composite = Arc::new(
        CompositeAuthProvider::new(Arc::new(Renaming), comprehensive(AclManager::allow_all()))
            .with_authorization_mode(AuthorizationMode::And),
    );
    let (addr, _router) = start(composite).await;
    let mut raw = Raw::open(addr).await;
    raw.send(&ConnectPacket::new(
        ConnectOptions::new("owner")
            .with_clean_start(true)
            .with_session_expiry_interval(3600)
            .with_authentication_method("X")
            .protocol_options,
    ))
    .await;
    raw.frames(200).await;
    let mut auth = AuthPacket::new(ReasonCode::ReAuthenticate);
    auth.properties.set_authentication_method("X".to_string());
    raw.send(&auth).await;
    raw.frames(200).await;
    drop(raw);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut resumed = Raw::open(addr).await;
    resumed
        .send(&ConnectPacket::new(
            ConnectOptions::new("owner")
                .with_clean_start(false)
                .with_session_expiry_interval(3600)
                .with_authentication_method("X")
                .protocol_options,
        ))
        .await;
    let frames = resumed.frames(300).await;
    assert_eq!(
        frames[0][2] & 1,
        0,
        "alice must not resume the session bob last owned"
    );
}
