#![cfg(feature = "broker")]

use mqtt5::broker::auth::{
    AuthProvider, AuthResult, CompositeAuthProvider, EnhancedAuthResult, PasswordAuthProvider,
};
use mqtt5::broker::config::{AuthConfig, RateLimitConfig, StorageBackend, StorageConfig};
use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::error::Result;
use mqtt5::packet::auth::AuthPacket;
use mqtt5::packet::connect::ConnectPacket;
use mqtt5::packet::{MqttPacket, Packet};
use mqtt5::protocol::v5::reason_codes::ReasonCode;
use mqtt5::transport::packet_io::read_packet_from_stream;
use mqtt5::types::ConnectOptions;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

const MAX_ATTEMPTS: u32 = 3;
const USERNAME: &str = "alice";
const METHOD: &str = "CHALLENGE";

fn secret() -> String {
    static SECRET: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SECRET
        .get_or_init(|| {
            let mut random = [0u8; 16];
            getrandom::fill(&mut random).unwrap();
            format!("{:032x}", u128::from_be_bytes(random))
        })
        .clone()
}

fn wrong_secret() -> String {
    format!("{}-wrong", secret())
}

fn config(password_file: Option<PathBuf>) -> BrokerConfig {
    BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: false,
            ..Default::default()
        })
        .with_auth(AuthConfig {
            allow_anonymous: password_file.is_none(),
            password_file,
            rate_limit: RateLimitConfig {
                enabled: true,
                max_attempts: MAX_ATTEMPTS,
                window_secs: 60,
                lockout_secs: 60,
            },
            ..Default::default()
        })
}

async fn run(broker: MqttBroker) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let mut broker = broker;
    let addr = broker.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    (addr, handle)
}

async fn start_with(auth: Arc<dyn AuthProvider>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let broker = MqttBroker::with_config(config(None))
        .await
        .unwrap()
        .with_auth_provider(auth);
    run(broker).await
}

fn password_provider() -> Arc<PasswordAuthProvider> {
    let provider = PasswordAuthProvider::new();
    provider.add_user(USERNAME, &secret()).unwrap();
    Arc::new(provider)
}

struct Wire {
    stream: TcpStream,
    buffer: bytes::BytesMut,
}

impl Wire {
    async fn open(addr: SocketAddr) -> Self {
        Self {
            stream: TcpStream::connect(addr).await.unwrap(),
            buffer: bytes::BytesMut::new(),
        }
    }

    async fn send(&mut self, packet: &impl MqttPacket) {
        let mut bytes = Vec::new();
        packet.encode(&mut bytes).unwrap();
        self.stream.write_all(&bytes).await.unwrap();
    }

    async fn next(&mut self) -> Option<Packet> {
        tokio::time::timeout(
            Duration::from_secs(5),
            read_packet_from_stream(&mut self.stream, 5, &mut self.buffer, 1 << 20),
        )
        .await
        .ok()?
        .ok()
    }
}

async fn password_connack(addr: SocketAddr, client_id: &str, password: &str) -> ReasonCode {
    let mut wire = Wire::open(addr).await;
    wire.send(&ConnectPacket::new(
        ConnectOptions::new(client_id)
            .with_credentials(USERNAME, password)
            .protocol_options,
    ))
    .await;
    match wire.next().await {
        Some(Packet::ConnAck(connack)) => connack.reason_code,
        other => panic!("expected CONNACK, got {other:?}"),
    }
}

async fn assert_password_failures_lock_out(addr: SocketAddr) {
    for attempt in 0..MAX_ATTEMPTS {
        let code = password_connack(addr, &format!("bad-{attempt}"), &wrong_secret()).await;
        assert_ne!(
            code,
            ReasonCode::Success,
            "attempt {attempt} with a wrong password"
        );
        assert_ne!(
            code,
            ReasonCode::ConnectionRateExceeded,
            "attempt {attempt} is within the limit"
        );
    }
    assert_eq!(
        password_connack(addr, "good", &secret()).await,
        ReasonCode::ConnectionRateExceeded,
        "a login after {MAX_ATTEMPTS} failures from the same address must be rate limited"
    );
}

struct OtherMethodProvider;

impl AuthProvider for OtherMethodProvider {
    fn authenticate<'a>(
        &'a self,
        _connect: &'a ConnectPacket,
        _client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        Box::pin(async { Ok(AuthResult::fail(ReasonCode::BadAuthenticationMethod)) })
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
}

#[tokio::test]
async fn failures_of_an_installed_provider_are_rate_limited() {
    let (addr, broker) = start_with(password_provider()).await;
    assert_password_failures_lock_out(addr).await;
    broker.abort();
}

#[tokio::test]
async fn failures_of_an_installed_composite_provider_are_rate_limited() {
    let composite = CompositeAuthProvider::new(Arc::new(OtherMethodProvider), password_provider());
    let (addr, broker) = start_with(Arc::new(composite)).await;
    assert_password_failures_lock_out(addr).await;
    broker.abort();
}

#[tokio::test]
async fn configured_provider_reinstalled_in_a_composite_counts_each_failure_once() {
    let dir = tempfile::tempdir().unwrap();
    let password_file = dir.path().join("passwords");
    let hash = PasswordAuthProvider::hash_password(&secret()).unwrap();
    std::fs::write(&password_file, format!("{USERNAME}:{hash}\n")).unwrap();
    let broker = MqttBroker::with_config(config(Some(password_file)))
        .await
        .unwrap();
    let composite =
        CompositeAuthProvider::new(Arc::new(OtherMethodProvider), broker.auth_provider());
    let (addr, handle) = run(broker.with_auth_provider(Arc::new(composite))).await;
    for attempt in 1..MAX_ATTEMPTS {
        let code = password_connack(addr, &format!("bad-{attempt}"), &wrong_secret()).await;
        assert_ne!(
            code,
            ReasonCode::Success,
            "attempt {attempt} with a wrong password"
        );
    }
    assert_eq!(
        password_connack(addr, "good-under-limit", &secret()).await,
        ReasonCode::Success,
        "{} failures are under the limit of {MAX_ATTEMPTS}",
        MAX_ATTEMPTS - 1
    );
    assert_password_failures_lock_out(addr).await;
    handle.abort();
}

struct ChallengeProvider;

impl ChallengeProvider {
    fn answer(auth_method: &str, auth_data: Option<&[u8]>) -> EnhancedAuthResult {
        match auth_data {
            None => EnhancedAuthResult::continue_auth(auth_method, Some(b"challenge".to_vec())),
            Some(response) if response == secret().as_bytes() => {
                EnhancedAuthResult::success_with_user(auth_method, USERNAME)
            }
            Some(_) => EnhancedAuthResult::fail(auth_method, ReasonCode::NotAuthorized),
        }
    }
}

impl AuthProvider for ChallengeProvider {
    fn authenticate<'a>(
        &'a self,
        _connect: &'a ConnectPacket,
        _client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        Box::pin(async { Ok(AuthResult::fail(ReasonCode::BadAuthenticationMethod)) })
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
        auth_data: Option<&'a [u8]>,
        _client_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        let result = Self::answer(auth_method, auth_data);
        Box::pin(async move { Ok(result) })
    }

    fn reauthenticate<'a>(
        &'a self,
        auth_method: &'a str,
        auth_data: Option<&'a [u8]>,
        _client_id: &'a str,
        _user_id: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        let result = Self::answer(auth_method, auth_data);
        Box::pin(async move { Ok(result) })
    }
}

async fn send_challenge_connect(addr: SocketAddr, client_id: &str) -> (Wire, Option<Packet>) {
    let mut wire = Wire::open(addr).await;
    wire.send(&ConnectPacket::new(
        ConnectOptions::new(client_id)
            .with_authentication_method(METHOD)
            .protocol_options,
    ))
    .await;
    let reply = wire.next().await;
    (wire, reply)
}

async fn answer_challenge(wire: &mut Wire, response: &str) -> Option<Packet> {
    wire.send(
        &AuthPacket::continue_authentication(
            METHOD.to_string(),
            Some(response.as_bytes().to_vec()),
        )
        .unwrap(),
    )
    .await;
    wire.next().await
}

async fn connect_with_challenge(
    addr: SocketAddr,
    client_id: &str,
    response: &str,
) -> (Wire, ReasonCode) {
    let (mut wire, reply) = send_challenge_connect(addr, client_id).await;
    match reply {
        Some(Packet::Auth(auth)) if auth.reason_code == ReasonCode::ContinueAuthentication => {}
        other => panic!("{client_id}: expected AUTH 0x18, got {other:?}"),
    }
    match answer_challenge(&mut wire, response).await {
        Some(Packet::ConnAck(connack)) => {
            let code = connack.reason_code;
            (wire, code)
        }
        other => panic!("{client_id}: expected CONNACK, got {other:?}"),
    }
}

async fn connected(addr: SocketAddr, client_id: &str) -> Wire {
    let (wire, code) = connect_with_challenge(addr, client_id, &secret()).await;
    assert_eq!(code, ReasonCode::Success, "{client_id} setup");
    wire
}

async fn assert_challenge_connect_is_rate_limited(addr: SocketAddr) {
    let (_wire, reply) = send_challenge_connect(addr, "after-failures").await;
    match reply {
        Some(Packet::ConnAck(connack)) => assert_eq!(
            connack.reason_code,
            ReasonCode::ConnectionRateExceeded,
            "an enhanced-auth CONNECT after {MAX_ATTEMPTS} failures must be rate limited"
        ),
        other => panic!(
            "an enhanced-auth CONNECT after {MAX_ATTEMPTS} failures must get CONNACK 0x9F, got {other:?}"
        ),
    }
}

#[tokio::test]
async fn enhanced_authentication_failures_are_rate_limited() {
    let (addr, broker) = start_with(Arc::new(ChallengeProvider)).await;
    for attempt in 0..MAX_ATTEMPTS {
        let (_wire, code) =
            connect_with_challenge(addr, &format!("bad-{attempt}"), &wrong_secret()).await;
        assert_eq!(code, ReasonCode::NotAuthorized, "attempt {attempt}");
    }
    assert_challenge_connect_is_rate_limited(addr).await;
    broker.abort();
}

#[tokio::test]
async fn reauthentication_failures_are_rate_limited() {
    let (addr, broker) = start_with(Arc::new(ChallengeProvider)).await;
    let mut wires: [Wire; MAX_ATTEMPTS as usize] = [
        connected(addr, "client-0").await,
        connected(addr, "client-1").await,
        connected(addr, "client-2").await,
    ];
    for (index, wire) in wires.iter_mut().enumerate() {
        wire.send(&AuthPacket::re_authenticate(METHOD.to_string(), None).unwrap())
            .await;
        match wire.next().await {
            Some(Packet::Auth(auth)) if auth.reason_code == ReasonCode::ContinueAuthentication => {}
            other => panic!("client-{index} re-authentication: expected AUTH 0x18, got {other:?}"),
        }
        match answer_challenge(wire, &wrong_secret()).await {
            Some(Packet::Disconnect(disconnect)) => assert_eq!(
                disconnect.reason_code,
                ReasonCode::NotAuthorized,
                "client-{index} re-authentication"
            ),
            other => panic!("client-{index} re-authentication: expected DISCONNECT, got {other:?}"),
        }
    }
    assert_challenge_connect_is_rate_limited(addr).await;
    broker.abort();
}
