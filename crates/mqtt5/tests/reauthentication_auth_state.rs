#![cfg(feature = "broker")]

use mqtt5::broker::auth::{AuthProvider, AuthResult, EnhancedAuthResult};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::MqttBroker;
use mqtt5::error::Result;
use mqtt5::ConnectOptions;
use mqtt5_protocol::packet::auth::AuthPacket;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

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
struct TokenOrChallenge;

impl AuthProvider for TokenOrChallenge {
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
        auth_data: Option<&'a [u8]>,
        _client_id: &'a str,
        _user_id: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        let method = auth_method.to_string();
        let challenge = auth_data == Some(&b"challenge-me"[..]);
        Box::pin(async move {
            if challenge {
                Ok(EnhancedAuthResult::continue_auth(
                    method,
                    Some(b"nonce".to_vec()),
                ))
            } else {
                Ok(EnhancedAuthResult::success_with_user(
                    method,
                    "alice".to_string(),
                ))
            }
        })
    }
}

fn kinds(frames: &[Vec<u8>]) -> String {
    frames
        .iter()
        .map(|f| {
            let mut i = 1;
            while f[i] & 0x80 != 0 {
                i += 1;
            }
            let rc = f.get(i + 1).copied().unwrap_or(0);
            match f[0] >> 4 {
                2 => format!("CONNACK(rc=0x{:02x})", f[i + 2]),
                15 => format!("AUTH(rc=0x{rc:02x})"),
                14 => format!("DISCONNECT(rc=0x{rc:02x})"),
                t => format!("type{t}"),
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn auth(code: ReasonCode, data: &[u8]) -> AuthPacket {
    let mut packet = AuthPacket::new(code);
    packet.properties.set_authentication_method("X".to_string());
    packet
        .properties
        .set_authentication_data(data.to_vec().into());
    packet
}

#[tokio::test]
async fn a_continue_after_a_restarted_reauthentication_is_a_protocol_error() {
    let (addr, _router) = start(Arc::new(TokenOrChallenge)).await;
    let mut raw = Raw::open(addr).await;
    raw.send(&ConnectPacket::new(
        ConnectOptions::new("auth-state")
            .with_clean_start(true)
            .with_authentication_method("X")
            .protocol_options,
    ))
    .await;
    raw.frames(400).await;

    raw.send(&auth(ReasonCode::ReAuthenticate, b"challenge-me"))
        .await;
    raw.frames(400).await;
    raw.send(&auth(ReasonCode::ReAuthenticate, b"token")).await;
    raw.frames(400).await;

    raw.send(&auth(ReasonCode::ContinueAuthentication, b"anything"))
        .await;
    let after = kinds(&raw.frames(400).await);
    assert_eq!(after, "DISCONNECT(rc=0x82)");
}

#[tokio::test]
async fn a_continue_after_a_single_step_reauthentication_is_a_protocol_error() {
    let (addr, _router) = start(Arc::new(TokenOrChallenge)).await;
    let mut raw = Raw::open(addr).await;
    raw.send(&ConnectPacket::new(
        ConnectOptions::new("auth-state-control")
            .with_clean_start(true)
            .with_authentication_method("X")
            .protocol_options,
    ))
    .await;
    raw.frames(400).await;
    raw.send(&auth(ReasonCode::ReAuthenticate, b"token")).await;
    raw.frames(400).await;
    raw.send(&auth(ReasonCode::ContinueAuthentication, b"anything"))
        .await;
    let after = kinds(&raw.frames(400).await);
    assert_eq!(after, "DISCONNECT(rc=0x82)");
}
