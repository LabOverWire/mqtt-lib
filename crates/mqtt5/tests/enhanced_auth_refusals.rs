#![cfg(feature = "broker")]

use mqtt5::broker::auth_mechanisms::{
    ScramCredentialStore, ScramCredentials, ScramSha256AuthProvider,
};
use mqtt5::broker::config::{StorageBackend, StorageConfig};
use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::client::{AuthHandler, AuthResponse, ScramSha256AuthHandler};
use mqtt5::error::MqttError;
use mqtt5::packet::auth::AuthPacket;
use mqtt5::packet::connack::ConnAckPacket;
use mqtt5::packet::connect::ConnectPacket;
use mqtt5::packet::{MqttPacket, Packet};
use mqtt5::protocol::v5::reason_codes::ReasonCode;
use mqtt5::transport::packet_io::read_packet_from_stream;
use mqtt5::types::ConnectOptions;
use mqtt5::MqttClient;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const METHOD: &str = "SCRAM-SHA-256";
const USERNAME: &str = "alice";
fn run_nonce() -> u128 {
    static NONCE: std::sync::OnceLock<u128> = std::sync::OnceLock::new();
    *NONCE.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(u128::from(std::process::id()), |elapsed| elapsed.as_nanos())
    })
}

fn password() -> String {
    format!("{:x}", run_nonce())
}
const EMPTY_AUTH: [u8; 2] = [0xF0, 0x00];
const CONTINUE_AUTH_WITHOUT_METHOD: [u8; 4] = [0xF0, 0x02, 0x18, 0x00];
const REAUTH_WITHOUT_METHOD: [u8; 4] = [0xF0, 0x02, 0x19, 0x00];

struct SingleUserStore {
    credentials: ScramCredentials,
}

impl ScramCredentialStore for SingleUserStore {
    fn get_credentials(&self, username: &str) -> Option<ScramCredentials> {
        (username == USERNAME).then(|| self.credentials.clone())
    }
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

    async fn send_raw(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).await.unwrap();
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

    async fn packets_until_closed(mut self) -> Vec<Packet> {
        let mut packets = Vec::new();
        while let Some(packet) = self.next().await {
            packets.push(packet);
        }
        let mut rest = Vec::new();
        let read =
            tokio::time::timeout(Duration::from_secs(5), self.stream.read_to_end(&mut rest)).await;
        assert!(
            matches!(read, Ok(Ok(0))),
            "the broker must close the connection, got {read:?} with {rest:02x?} after {packets:?}"
        );
        packets
    }
}

async fn start_broker(scram: bool) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: false,
            ..Default::default()
        });
    let broker = MqttBroker::with_config(config).await.unwrap();
    let mut broker = if scram {
        let store = SingleUserStore {
            credentials: ScramCredentials::from_password_and_salt(
                &password(),
                &run_nonce().to_be_bytes(),
                4096,
            ),
        };
        broker.with_auth_provider(Arc::new(ScramSha256AuthProvider::new(Arc::new(store))))
    } else {
        broker
    };
    let addr = broker.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    (addr, handle)
}

async fn client_final(handler: &ScramSha256AuthHandler, server_first: Option<&[u8]>) -> Vec<u8> {
    let AuthResponse::Continue(data) = handler
        .handle_challenge(METHOD, server_first)
        .await
        .unwrap()
    else {
        panic!("the SCRAM client did not produce a client-final message");
    };
    data
}

async fn connect_plain(addr: SocketAddr, client_id: &str) -> Wire {
    let mut wire = Wire::open(addr).await;
    wire.send(&ConnectPacket::new(
        ConnectOptions::new(client_id).protocol_options,
    ))
    .await;
    match wire.next().await {
        Some(Packet::ConnAck(connack)) if connack.reason_code == ReasonCode::Success => {}
        other => panic!("expected CONNACK 0x00, got {other:?}"),
    }
    wire
}

async fn send_scram_connect(
    addr: SocketAddr,
    client_id: &str,
    handler: &ScramSha256AuthHandler,
) -> (Wire, AuthPacket) {
    let mut wire = Wire::open(addr).await;
    let client_first = handler.initial_response(METHOD).await.unwrap().unwrap();
    wire.send(&ConnectPacket::new(
        ConnectOptions::new(client_id)
            .with_authentication_method(METHOD)
            .with_authentication_data(&client_first)
            .protocol_options,
    ))
    .await;
    match wire.next().await {
        Some(Packet::Auth(auth)) if auth.reason_code == ReasonCode::ContinueAuthentication => {
            (wire, auth)
        }
        other => panic!("expected AUTH 0x18 with server-first, got {other:?}"),
    }
}

async fn connect_scram(
    addr: SocketAddr,
    client_id: &str,
    handler: &ScramSha256AuthHandler,
) -> Wire {
    let (mut wire, server_first) = send_scram_connect(addr, client_id, handler).await;
    let final_message = client_final(handler, server_first.authentication_data()).await;
    wire.send(
        &AuthPacket::continue_authentication(METHOD.to_string(), Some(final_message)).unwrap(),
    )
    .await;
    match wire.next().await {
        Some(Packet::ConnAck(connack)) if connack.reason_code == ReasonCode::Success => {}
        other => panic!("expected CONNACK 0x00, got {other:?}"),
    }
    wire
}

async fn reauthenticate(wire: &mut Wire, handler: &ScramSha256AuthHandler) -> Option<Packet> {
    let client_first = handler.initial_response(METHOD).await.unwrap();
    wire.send(&AuthPacket::re_authenticate(METHOD.to_string(), client_first).unwrap())
        .await;
    let server_first = match wire.next().await {
        Some(Packet::Auth(auth)) if auth.reason_code == ReasonCode::ContinueAuthentication => auth,
        other => return other,
    };
    let final_message = client_final(handler, server_first.authentication_data()).await;
    wire.send(
        &AuthPacket::continue_authentication(METHOD.to_string(), Some(final_message)).unwrap(),
    )
    .await;
    wire.next().await
}

async fn assert_reauthentication_succeeded(
    round: &str,
    reply: Option<Packet>,
    handler: &ScramSha256AuthHandler,
) {
    match reply {
        Some(Packet::Auth(auth)) if auth.reason_code == ReasonCode::Success => {
            assert!(
                matches!(
                    handler
                        .handle_challenge(METHOD, auth.authentication_data())
                        .await
                        .unwrap(),
                    AuthResponse::Success
                ),
                "{round} re-authentication: server-final signature did not verify"
            );
        }
        other => panic!("{round} re-authentication: expected AUTH 0x00 Success, got {other:?}"),
    }
}

fn assert_only_disconnect(packets: &[Packet], expected: ReasonCode, rule: &str) {
    match packets {
        [Packet::Disconnect(disconnect)] => assert_eq!(
            disconnect.reason_code, expected,
            "{rule}: wrong DISCONNECT reason code"
        ),
        other => {
            panic!("{rule}: expected one DISCONNECT {expected:?} before the close, got {other:?}")
        }
    }
}

fn assert_only_connack(packets: &[Packet], expected: ReasonCode, rule: &str) {
    match packets {
        [Packet::ConnAck(connack)] => assert_eq!(
            connack.reason_code, expected,
            "{rule}: wrong CONNACK reason code"
        ),
        other => {
            panic!("{rule}: expected one CONNACK {expected:?} before the close, got {other:?}")
        }
    }
}

#[tokio::test]
async fn scram_reauthentication_restarted_before_it_completes_starts_a_new_exchange() {
    let (addr, broker) = start_broker(true).await;
    let handler = ScramSha256AuthHandler::new(USERNAME, password());
    let mut wire = connect_scram(addr, "scram-restart", &handler).await;
    let abandoned = handler.initial_response(METHOD).await.unwrap();
    wire.send(&AuthPacket::re_authenticate(METHOD.to_string(), abandoned).unwrap())
        .await;
    match wire.next().await {
        Some(Packet::Auth(auth)) if auth.reason_code == ReasonCode::ContinueAuthentication => {}
        other => panic!("setup: expected AUTH 0x18 with server-first, got {other:?}"),
    }
    let restarted = ScramSha256AuthHandler::new(USERNAME, password());
    let reply = reauthenticate(&mut wire, &restarted).await;
    let second = ScramSha256AuthHandler::new(USERNAME, password());
    let after = reauthenticate(&mut wire, &second).await;
    broker.abort();
    assert_reauthentication_succeeded("[MQTT-4.12.1-1] restarted", reply, &restarted).await;
    assert_reauthentication_succeeded("[MQTT-4.12.1-1] following", after, &second).await;
}

#[tokio::test]
async fn scram_reauthentication_twice_succeeds() {
    let (addr, broker) = start_broker(true).await;
    let handler = ScramSha256AuthHandler::new(USERNAME, password());
    let mut wire = connect_scram(addr, "scram-twice", &handler).await;
    let first = reauthenticate(&mut wire, &handler).await;
    assert_reauthentication_succeeded("first", first, &handler).await;
    let second = reauthenticate(&mut wire, &handler).await;
    broker.abort();
    assert_reauthentication_succeeded("second", second, &handler).await;
}

#[tokio::test]
async fn reauthenticate_on_plain_connection_is_answered_with_disconnect_protocol_error() {
    let (addr, broker) = start_broker(false).await;
    let mut wire = connect_plain(addr, "plain-reauth").await;
    wire.send(&AuthPacket::re_authenticate(METHOD.to_string(), None).unwrap())
        .await;
    let packets = wire.packets_until_closed().await;
    broker.abort();
    assert_only_disconnect(
        &packets,
        ReasonCode::ProtocolError,
        "[MQTT-4.12.0-7] [MQTT-4.13.1-1]",
    );
}

#[tokio::test]
async fn empty_auth_on_plain_connection_is_answered_with_disconnect_protocol_error() {
    let (addr, broker) = start_broker(false).await;
    let mut wire = connect_plain(addr, "plain-empty-auth").await;
    wire.send_raw(&EMPTY_AUTH).await;
    let packets = wire.packets_until_closed().await;
    broker.abort();
    assert_only_disconnect(
        &packets,
        ReasonCode::ProtocolError,
        "[MQTT-4.12.0-7] [MQTT-4.13.1-1]",
    );
}

#[tokio::test]
async fn duplicate_connect_is_answered_with_disconnect_protocol_error() {
    let (addr, broker) = start_broker(false).await;
    let mut wire = connect_plain(addr, "dup-connect").await;
    wire.send(&ConnectPacket::new(
        ConnectOptions::new("dup-connect").protocol_options,
    ))
    .await;
    let packets = wire.packets_until_closed().await;
    broker.abort();
    assert_only_disconnect(&packets, ReasonCode::ProtocolError, "[MQTT-3.1.0-2]");
}

#[tokio::test]
async fn auth_without_method_after_scram_connack_is_answered_with_disconnect_protocol_error() {
    let (addr, broker) = start_broker(true).await;
    let handler = ScramSha256AuthHandler::new(USERNAME, password());
    let mut wire = connect_scram(addr, "scram-empty-auth", &handler).await;
    wire.send_raw(&EMPTY_AUTH).await;
    let packets = wire.packets_until_closed().await;
    broker.abort();
    assert_only_disconnect(
        &packets,
        ReasonCode::ProtocolError,
        "[MQTT-4.12.0-5] [MQTT-4.13.1-1]",
    );
}

#[tokio::test]
async fn reauthenticate_before_connack_is_refused_with_connack_protocol_error() {
    let (addr, broker) = start_broker(true).await;
    let handler = ScramSha256AuthHandler::new(USERNAME, password());
    let (mut wire, _) = send_scram_connect(addr, "scram-early-reauth", &handler).await;
    wire.send(&AuthPacket::re_authenticate(METHOD.to_string(), None).unwrap())
        .await;
    let packets = wire.packets_until_closed().await;
    broker.abort();
    assert_only_connack(
        &packets,
        ReasonCode::ProtocolError,
        "[MQTT-4.12.1-1] [MQTT-3.14.0-1]",
    );
}

#[tokio::test]
async fn success_auth_before_connack_is_refused_with_connack_protocol_error() {
    let (addr, broker) = start_broker(true).await;
    let handler = ScramSha256AuthHandler::new(USERNAME, password());
    let (mut wire, _) = send_scram_connect(addr, "scram-early-success", &handler).await;
    wire.send(&AuthPacket::success(METHOD.to_string()).unwrap())
        .await;
    let packets = wire.packets_until_closed().await;
    broker.abort();
    assert_only_connack(
        &packets,
        ReasonCode::ProtocolError,
        "[MQTT-4.12.0-3] [MQTT-3.14.0-1]",
    );
}

#[tokio::test]
async fn other_method_before_connack_is_refused_with_connack_bad_authentication_method() {
    let (addr, broker) = start_broker(true).await;
    let handler = ScramSha256AuthHandler::new(USERNAME, password());
    let (mut wire, _) = send_scram_connect(addr, "scram-early-method", &handler).await;
    wire.send(&AuthPacket::continue_authentication("OTHER".to_string(), None).unwrap())
        .await;
    let packets = wire.packets_until_closed().await;
    broker.abort();
    assert_only_connack(
        &packets,
        ReasonCode::BadAuthenticationMethod,
        "[MQTT-4.12.0-5] [MQTT-3.14.0-1]",
    );
}

#[tokio::test]
async fn other_method_after_scram_connack_is_answered_with_disconnect_protocol_error() {
    let (addr, broker) = start_broker(true).await;
    let handler = ScramSha256AuthHandler::new(USERNAME, password());
    let mut wire = connect_scram(addr, "scram-late-method", &handler).await;
    wire.send(&AuthPacket::re_authenticate("OTHER".to_string(), None).unwrap())
        .await;
    let packets = wire.packets_until_closed().await;
    broker.abort();
    assert_only_disconnect(
        &packets,
        ReasonCode::ProtocolError,
        "[MQTT-4.12.1-1] [MQTT-3.14.2-1]",
    );
}

#[tokio::test]
async fn reauthenticate_without_method_after_scram_connack_is_answered_with_disconnect_protocol_error(
) {
    let (addr, broker) = start_broker(true).await;
    let handler = ScramSha256AuthHandler::new(USERNAME, password());
    let mut wire = connect_scram(addr, "scram-reauth-no-method", &handler).await;
    wire.send_raw(&REAUTH_WITHOUT_METHOD).await;
    let packets = wire.packets_until_closed().await;
    broker.abort();
    assert_only_disconnect(
        &packets,
        ReasonCode::ProtocolError,
        "[MQTT-4.12.1-1] [MQTT-4.13.1-1]",
    );
}

#[tokio::test]
async fn continue_without_method_after_scram_connack_is_answered_with_disconnect_protocol_error() {
    let (addr, broker) = start_broker(true).await;
    let handler = ScramSha256AuthHandler::new(USERNAME, password());
    let mut wire = connect_scram(addr, "scram-continue-no-method", &handler).await;
    wire.send_raw(&CONTINUE_AUTH_WITHOUT_METHOD).await;
    let packets = wire.packets_until_closed().await;
    broker.abort();
    assert_only_disconnect(
        &packets,
        ReasonCode::ProtocolError,
        "[MQTT-4.12.0-5] [MQTT-4.13.1-1]",
    );
}

#[tokio::test]
async fn reauthenticate_without_method_on_plain_connection_is_answered_with_disconnect_protocol_error(
) {
    let (addr, broker) = start_broker(false).await;
    let mut wire = connect_plain(addr, "plain-reauth-no-method").await;
    wire.send_raw(&REAUTH_WITHOUT_METHOD).await;
    let packets = wire.packets_until_closed().await;
    broker.abort();
    assert_only_disconnect(
        &packets,
        ReasonCode::ProtocolError,
        "[MQTT-4.12.0-7] [MQTT-4.13.1-1]",
    );
}

#[tokio::test]
async fn reauthenticate_without_method_before_connack_is_refused_with_connack_protocol_error() {
    let (addr, broker) = start_broker(true).await;
    let handler = ScramSha256AuthHandler::new(USERNAME, password());
    let (mut wire, _) = send_scram_connect(addr, "scram-early-reauth-no-method", &handler).await;
    wire.send_raw(&REAUTH_WITHOUT_METHOD).await;
    let packets = wire.packets_until_closed().await;
    broker.abort();
    assert_only_connack(
        &packets,
        ReasonCode::ProtocolError,
        "[MQTT-4.12.0-5] [MQTT-3.14.0-1]",
    );
}

#[tokio::test]
async fn continue_without_method_before_connack_is_refused_with_connack_protocol_error() {
    let (addr, broker) = start_broker(true).await;
    let handler = ScramSha256AuthHandler::new(USERNAME, password());
    let (mut wire, _) = send_scram_connect(addr, "scram-early-continue-no-method", &handler).await;
    wire.send_raw(&CONTINUE_AUTH_WITHOUT_METHOD).await;
    let packets = wire.packets_until_closed().await;
    broker.abort();
    assert_only_connack(
        &packets,
        ReasonCode::ProtocolError,
        "[MQTT-4.12.0-5] [MQTT-3.14.0-1]",
    );
}

struct CountingAuthHandler {
    challenges: Arc<AtomicUsize>,
}

impl AuthHandler for CountingAuthHandler {
    fn handle_challenge<'a>(
        &'a self,
        auth_method: &'a str,
        challenge_data: Option<&'a [u8]>,
    ) -> Pin<Box<dyn Future<Output = mqtt5::error::Result<AuthResponse>> + Send + 'a>> {
        self.challenges.fetch_add(1, Ordering::SeqCst);
        let reply = [auth_method.as_bytes(), challenge_data.unwrap_or_default()].concat();
        Box::pin(async move { Ok(AuthResponse::Continue(reply)) })
    }
}

#[tokio::test]
async fn client_refuses_continue_without_method_before_connack() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut wire = Wire {
            stream,
            buffer: bytes::BytesMut::new(),
        };
        match wire.next().await {
            Some(Packet::Connect(_)) => {}
            other => panic!("expected CONNECT, got {other:?}"),
        }
        wire.send_raw(&CONTINUE_AUTH_WITHOUT_METHOD).await;
        wire.next().await
    });
    let challenges = Arc::new(AtomicUsize::new(0));
    let client = MqttClient::with_options(
        ConnectOptions::new("client-continue-no-method").with_authentication_method(METHOD),
    );
    client
        .set_auth_handler(CountingAuthHandler {
            challenges: Arc::clone(&challenges),
        })
        .await;
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        client.connect(&format!("mqtt://{addr}")),
    )
    .await
    .unwrap();
    let answer = server.await.unwrap();
    assert!(
        matches!(result, Err(MqttError::ProtocolError(_))),
        "[MQTT-4.12.0-5]: connect must fail with a protocol error, got {result:?}"
    );
    assert_eq!(
        challenges.load(Ordering::SeqCst),
        0,
        "[MQTT-4.12.0-5]: the auth handler must not see an AUTH without a method"
    );
    assert!(
        !matches!(answer, Some(Packet::Auth(_))),
        "[MQTT-4.12.0-5]: the client must not answer an AUTH without a method, sent {answer:?}"
    );
}

#[tokio::test]
async fn client_does_not_answer_continue_without_method_after_connack() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut wire = Wire {
            stream,
            buffer: bytes::BytesMut::new(),
        };
        match wire.next().await {
            Some(Packet::Connect(_)) => {}
            other => panic!("expected CONNECT, got {other:?}"),
        }
        let mut connack = ConnAckPacket::new(false, ReasonCode::Success);
        connack
            .properties
            .set_authentication_method(METHOD.to_string());
        wire.send(&connack).await;
        wire.send_raw(&CONTINUE_AUTH_WITHOUT_METHOD).await;
        wire.next().await
    });
    let challenges = Arc::new(AtomicUsize::new(0));
    let client = MqttClient::with_options(
        ConnectOptions::new("client-late-continue-no-method").with_authentication_method(METHOD),
    );
    client
        .set_auth_handler(CountingAuthHandler {
            challenges: Arc::clone(&challenges),
        })
        .await;
    tokio::time::timeout(
        Duration::from_secs(10),
        client.connect(&format!("mqtt://{addr}")),
    )
    .await
    .unwrap()
    .unwrap();
    let answer = server.await.unwrap();
    assert_eq!(
        challenges.load(Ordering::SeqCst),
        0,
        "[MQTT-4.12.0-5]: the auth handler must not see an AUTH without a method"
    );
    assert!(
        !matches!(answer, Some(Packet::Auth(_))),
        "[MQTT-4.12.0-5]: the client must not answer an AUTH without a method, sent {answer:?}"
    );
}
