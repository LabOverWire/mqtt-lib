#![cfg(feature = "broker")]

use base64::prelude::*;
use mqtt5::broker::acl::{AclManager, AclRule, Permission};
use mqtt5::broker::auth::{
    AuthProvider, AuthResult, AuthorizationMode, CompositeAuthProvider, ComprehensiveAuthProvider,
    EnhancedAuthResult, PasswordAuthProvider,
};
use mqtt5::broker::auth_mechanisms::{
    generate_scram_credential_line_with_iterations, FileBasedScramCredentialStore,
    ScramSha256AuthProvider,
};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::MqttBroker;
use mqtt5::error::Result;
use mqtt5::ConnectOptions;
use mqtt5_protocol::packet::auth::AuthPacket;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use mqtt5_protocol::QoS;
use std::future::Future;
use std::net::SocketAddr;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn config() -> BrokerConfig {
    BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        })
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

fn scram_first(user: &str) -> String {
    format!("n,,n={user},r=clientnonce")
}

fn server_first(frame: &[u8]) -> String {
    for p in 0..frame.len().saturating_sub(5) {
        if frame[p] == 0x16 && &frame[p + 3..p + 5] == b"r=" {
            let len = usize::from(frame[p + 1]) << 8 | usize::from(frame[p + 2]);
            return String::from_utf8_lossy(&frame[p + 3..p + 3 + len]).into_owned();
        }
    }
    panic!("no server-first in {frame:?}");
}

fn scram_final(user: &str, password: &str, server_first: &str) -> String {
    let mut nonce = "";
    let mut salt = Vec::new();
    let mut iterations = 0u32;
    for part in server_first.split(',') {
        if let Some(v) = part.strip_prefix("r=") {
            nonce = v;
        } else if let Some(v) = part.strip_prefix("s=") {
            salt = BASE64_STANDARD.decode(v).unwrap();
        } else if let Some(v) = part.strip_prefix("i=") {
            iterations = v.parse().unwrap();
        }
    }
    let mut salted = [0u8; 32];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        std::num::NonZeroU32::new(iterations).unwrap(),
        &salt,
        password.as_bytes(),
        &mut salted,
    );
    let hmac = |key: &[u8], data: &[u8]| {
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
        ring::hmac::sign(&key, data).as_ref().to_vec()
    };
    let client_key = hmac(&salted, b"Client Key");
    let stored_key = ring::digest::digest(&ring::digest::SHA256, &client_key);
    let without_proof = format!("c=biws,r={nonce}");
    let auth_message = format!("n={user},r=clientnonce,{server_first},{without_proof}");
    let signature = hmac(stored_key.as_ref(), auth_message.as_bytes());
    let proof: Vec<u8> = client_key
        .iter()
        .zip(signature.iter())
        .map(|(a, b)| a ^ b)
        .collect();
    format!("{without_proof},p={}", BASE64_STANDARD.encode(proof))
}

fn scram_file(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("scram.txt");
    let lines = format!(
        "{}\n{}\n",
        generate_scram_credential_line_with_iterations("alice", "pw-a", 4096).unwrap(),
        generate_scram_credential_line_with_iterations("bob", "pw-b", 4096).unwrap()
    );
    std::fs::write(&path, lines).unwrap();
    path
}

fn scram_auth(reason: ReasonCode, data: &str) -> AuthPacket {
    let mut auth = AuthPacket::new(reason);
    auth.properties
        .set_authentication_method("SCRAM-SHA-256".to_string());
    auth.properties
        .set_authentication_data(data.as_bytes().to_vec().into());
    auth
}

fn scram_connect(id: &str, user: &str, clean: bool) -> ConnectPacket {
    ConnectPacket::new(
        ConnectOptions::new(id)
            .with_clean_start(clean)
            .with_session_expiry_interval(3600)
            .with_authentication_method("SCRAM-SHA-256")
            .with_authentication_data(scram_first(user))
            .protocol_options,
    )
}

async fn scram_login(raw: &mut Raw, id: &str, user: &str, pw: &str, clean: bool) -> Vec<u8> {
    raw.send(&scram_connect(id, user, clean)).await;
    let frames = raw.frames(300).await;
    let first = server_first(&frames[0]);
    raw.send(&scram_auth(
        ReasonCode::ContinueAuthentication,
        &scram_final(user, pw, &first),
    ))
    .await;
    let frames = raw.frames(300).await;
    frames.first().cloned().unwrap_or_default()
}

struct Renaming {
    reauth_user: Option<&'static str>,
    reauth_delay: Duration,
}

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
        let user = self.reauth_user;
        let delay = self.reauth_delay;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            Ok(match user {
                Some(user) => EnhancedAuthResult::success_with_user(method, user.to_string()),
                None => EnhancedAuthResult::success(method),
            })
        })
    }
}

fn x_connect(id: &str, clean: bool, method: bool) -> ConnectPacket {
    let mut options = ConnectOptions::new(id)
        .with_clean_start(clean)
        .with_session_expiry_interval(3600);
    if method {
        options = options.with_authentication_method("X");
    }
    ConnectPacket::new(options.protocol_options)
}

fn reauth_x() -> AuthPacket {
    let mut auth = AuthPacket::new(ReasonCode::ReAuthenticate);
    auth.properties.set_authentication_method("X".to_string());
    auth
}

#[tokio::test]
async fn a_multi_step_reauthentication_changes_the_owner_of_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileBasedScramCredentialStore::load_from_file(&scram_file(dir.path())).unwrap();
    let provider = ScramSha256AuthProvider::new(Arc::new(store));
    let (addr, _router) = start(Arc::new(provider)).await;

    let mut raw = Raw::open(addr).await;
    let connack = scram_login(&mut raw, "owned", "alice", "pw-a", true).await;
    assert_eq!(connack[3], 0);
    raw.send(&scram_auth(ReasonCode::ReAuthenticate, &scram_first("bob")))
        .await;
    let frames = raw.frames(300).await;
    let first = server_first(&frames[0]);
    raw.send(&scram_auth(
        ReasonCode::ContinueAuthentication,
        &scram_final("bob", "pw-b", &first),
    ))
    .await;
    assert_eq!(raw.frames(300).await[0][0], 0xF0);
    drop(raw);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut bob = Raw::open(addr).await;
    let bob_ack = scram_login(&mut bob, "owned", "bob", "pw-b", false).await;
    assert_eq!((bob_ack[2] & 1, bob_ack[3]), (1, 0));
    drop(bob);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut alice = Raw::open(addr).await;
    let alice_ack = scram_login(&mut alice, "owned", "alice", "pw-a", false).await;
    assert_eq!((alice_ack[2] & 1, alice_ack[3]), (0, 0x87));
}

#[tokio::test]
async fn a_reauthentication_that_names_no_user_keeps_the_owner_of_the_session() {
    let (addr, _router) = start(Arc::new(Renaming {
        reauth_user: None,
        reauth_delay: Duration::ZERO,
    }))
    .await;
    let mut raw = Raw::open(addr).await;
    raw.send(&x_connect("noowner", true, true)).await;
    raw.frames(200).await;
    raw.send(&reauth_x()).await;
    raw.frames(200).await;
    drop(raw);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut anonymous = Raw::open(addr).await;
    anonymous.send(&x_connect("noowner", false, false)).await;
    let frames = anonymous.frames(300).await;
    assert_eq!((frames[0][2] & 1, frames[0][3]), (0, 0x87));
}

#[tokio::test]
async fn a_reauthentication_as_another_user_drops_subscriptions_that_user_cant_have() {
    let acl = AclManager::new();
    for (user, pattern) in [("alice", "private/#"), ("*", "shared/#")] {
        acl.add_rule(AclRule::new(
            user.to_string(),
            pattern.to_string(),
            Permission::ReadWrite,
        ))
        .await;
    }
    let composite = Arc::new(
        CompositeAuthProvider::new(
            Arc::new(Renaming {
                reauth_user: Some("bob"),
                reauth_delay: Duration::ZERO,
            }),
            Arc::new(ComprehensiveAuthProvider::with_providers(
                PasswordAuthProvider::new().with_anonymous(true),
                acl,
            )),
        )
        .with_authorization_mode(AuthorizationMode::And),
    );
    let (addr, router) = start(composite).await;
    let mut raw = Raw::open(addr).await;
    raw.send(&x_connect("switcher", true, true)).await;
    raw.frames(200).await;
    for filter in ["private/#", "shared/#"] {
        raw.stream
            .write_all(&subscribe_bytes(filter, 0))
            .await
            .unwrap();
        raw.frames(200).await;
    }
    raw.send(&reauth_x()).await;
    raw.frames(200).await;
    route(&router, "private/x").await;
    route(&router, "shared/x").await;
    let publishes = raw
        .frames(300)
        .await
        .into_iter()
        .filter(|frame| frame[0] & 0xF0 == 0x30)
        .count();
    assert_eq!(publishes, 1);
}
async fn route(router: &MessageRouter, topic: &str) {
    let publish = PublishPacket::new(topic, &b"x"[..], QoS::AtMostOnce);
    router.route_message(&publish, None).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
}
fn subscribe_bytes(topic: &str, qos: u8) -> Vec<u8> {
    let topic_len = u8::try_from(topic.len()).unwrap();
    let mut bytes = vec![0x82, 6 + topic_len, 0x00, 0x01, 0x00, 0x00, topic_len];
    bytes.extend_from_slice(topic.as_bytes());
    bytes.push(qos);
    bytes
}

#[tokio::test]
async fn a_displaced_reauthentication_leaves_the_new_owners_routes_alone() {
    let (addr, router) = start(Arc::new(Renaming {
        reauth_user: Some("bob"),
        reauth_delay: Duration::from_millis(800),
    }))
    .await;
    let mut displaced = Raw::open(addr).await;
    displaced.send(&x_connect("taken", true, true)).await;
    displaced.frames(200).await;
    displaced
        .stream
        .write_all(&subscribe_bytes("old/x", 0))
        .await
        .unwrap();
    displaced.frames(200).await;
    displaced.send(&reauth_x()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut owner = Raw::open(addr).await;
    owner.send(&x_connect("taken", true, true)).await;
    owner.frames(200).await;
    owner
        .stream
        .write_all(&subscribe_bytes("new/y", 0))
        .await
        .unwrap();
    owner.frames(200).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    route(&router, "old/x").await;
    route(&router, "new/y").await;
    let publishes = owner
        .frames(300)
        .await
        .into_iter()
        .filter(|frame| frame[0] & 0xF0 == 0x30)
        .collect::<Vec<_>>();
    assert_eq!(publishes.len(), 1, "{publishes:?}");
    assert!(publishes[0].windows(5).any(|w| w == b"new/y"));
}
