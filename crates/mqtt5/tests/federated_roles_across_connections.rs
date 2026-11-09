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
use mqtt5::{ConnectOptions, MqttClient};
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::QoS;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::RwLock;

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

fn token(key: &[u8]) -> String {
    let header = BASE64_URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let payload = BASE64_URL_SAFE_NO_PAD.encode(format!(
        r#"{{"sub":"user1","iss":"https://idp.test","exp":{exp}}}"#
    ));
    let message = format!("{header}.{payload}");
    let signing = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    let signature = ring::hmac::sign(&signing, message.as_bytes());
    format!(
        "{message}.{}",
        BASE64_URL_SAFE_NO_PAD.encode(signature.as_ref())
    )
}

fn options(id: &str, key: &[u8]) -> ConnectOptions {
    ConnectOptions::new(id)
        .with_clean_start(true)
        .with_session_expiry_interval(3600)
        .with_authentication_method("JWT")
        .with_authentication_data(token(key))
}

async fn route(router: &MessageRouter, topic: &str) {
    let publish = PublishPacket::new(topic, &b"x"[..], QoS::AtMostOnce);
    router.route_message(&publish, None).await;
}

async fn role_deny_after_other_connection_closes(session_scoped: bool) -> Vec<String> {
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
    acl.add_role_rule("reader", "data/secret/#", Permission::Deny)
        .await
        .unwrap();
    let provider = FederatedJwtAuthProvider::new(vec![issuer])
        .unwrap()
        .with_acl_manager(Arc::new(RwLock::new(acl)));
    let (addr, router) = start(Arc::new(provider)).await;
    let url = format!("mqtt://{addr}");

    let seen = Arc::new(Mutex::new(Vec::new()));
    let first = MqttClient::new("device-a");
    first
        .connect_with_options(&url, options("device-a", &key))
        .await
        .unwrap();
    let sink = Arc::clone(&seen);
    first
        .subscribe("data/#", move |m| {
            sink.lock().unwrap().push(m.topic.clone());
        })
        .await
        .unwrap();
    route(&router, "data/secret/before").await;
    route(&router, "data/open").await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let second = MqttClient::new("device-b");
    second
        .connect_with_options(&url, options("device-b", &key))
        .await
        .unwrap();
    second.disconnect().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    route(&router, "data/secret/after").await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let got = seen.lock().unwrap().clone();
    got
}

#[tokio::test]
async fn a_role_deny_withholds_after_another_connection_of_the_user_closes() {
    let got = role_deny_after_other_connection_closes(true).await;
    assert_eq!(got, ["data/open"]);
}

#[tokio::test]
async fn a_role_deny_withholds_after_another_connection_closes_with_unscoped_roles() {
    let got = role_deny_after_other_connection_closes(false).await;
    assert_eq!(got, ["data/open"]);
}

async fn role_deny_after_takeover(session_scoped: bool) -> Vec<String> {
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
    acl.add_role_rule("reader", "data/secret/#", Permission::Deny)
        .await
        .unwrap();
    let provider = FederatedJwtAuthProvider::new(vec![issuer])
        .unwrap()
        .with_acl_manager(Arc::new(RwLock::new(acl)));
    let (addr, router) = start(Arc::new(provider)).await;
    let url = format!("mqtt://{addr}");

    let mut first = Raw::open(addr).await;
    first
        .send(&ConnectPacket::new(
            options("takeover", &key).protocol_options,
        ))
        .await;
    first.frames(200).await;
    first.raw(&subscribe_bytes("data/#", 0)).await;
    first.frames(200).await;
    drop(url);

    let mut second = Raw::open(addr).await;
    second
        .send(&ConnectPacket::new(
            options("takeover", &key)
                .with_clean_start(false)
                .protocol_options,
        ))
        .await;
    second.frames(500).await;
    route(&router, "data/open").await;
    route(&router, "data/secret/after").await;
    let frames = second.frames(400).await;
    frames.iter().filter_map(|f| publish_topic(f)).collect()
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

#[tokio::test]
async fn a_role_deny_withholds_after_a_takeover() {
    let got = role_deny_after_takeover(true).await;
    assert_eq!(got, ["data/open"]);
}

#[tokio::test]
async fn a_role_deny_withholds_after_a_takeover_with_unscoped_roles() {
    let got = role_deny_after_takeover(false).await;
    assert_eq!(got, ["data/open"]);
}

struct Token(String);

impl mqtt5::AuthHandler for Token {
    fn initial_response<'a>(
        &'a self,
        _auth_method: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = mqtt5::Result<Option<Vec<u8>>>> + Send + 'a>,
    > {
        Box::pin(async move { Ok(Some(self.0.clone().into_bytes())) })
    }

    fn handle_challenge<'a>(
        &'a self,
        _auth_method: &'a str,
        _challenge_data: Option<&'a [u8]>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = mqtt5::Result<mqtt5::AuthResponse>> + Send + 'a>,
    > {
        Box::pin(async move { Ok(mqtt5::AuthResponse::Success) })
    }
}

#[tokio::test]
async fn a_federated_users_roles_stay_after_its_last_connection_ends() {
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
    .with_default_roles(vec!["reader".to_string()]);
    let acl = Arc::new(RwLock::new(AclManager::new()));
    let provider = FederatedJwtAuthProvider::new(vec![issuer])
        .unwrap()
        .with_acl_manager(Arc::clone(&acl));
    let (addr, _router) = start(Arc::new(provider)).await;
    let client = MqttClient::new("reauth-roles");
    client.set_auth_handler(Token(token(&key))).await;
    client
        .connect_with_options(&format!("mqtt://{addr}"), options("reauth-roles", &key))
        .await
        .unwrap();
    let user = "idp.test:user1";
    assert!(acl.read().await.get_federated_roles(user).await.is_some());
    client.reauthenticate().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(acl.read().await.get_federated_roles(user).await.is_some());
    client.disconnect().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(acl.read().await.get_federated_roles(user).await.is_some());
}
