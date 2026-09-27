#![cfg(feature = "broker")]
use mqtt5::broker::auth::{AuthProvider, AuthResult, EnhancedAuthResult};
use mqtt5::broker::config::{StorageBackend, StorageConfig};
use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::client::{AuthHandler, AuthResponse};
use mqtt5::error::{MqttError, Result};
use mqtt5::packet::auth::AuthPacket;
use mqtt5::packet::connack::ConnAckPacket;
use mqtt5::packet::connect::ConnectPacket;
use mqtt5::packet::publish::PublishPacket;
use mqtt5::packet::subscribe::SubscribePacket;
use mqtt5::packet::{MqttPacket, Packet};
use mqtt5::protocol::v5::reason_codes::ReasonCode;
use mqtt5::transport::packet_io::read_packet_from_stream;
use mqtt5::types::ConnectOptions;
use mqtt5::MqttClient;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

fn test_broker_config() -> BrokerConfig {
    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };
    BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage_config)
}

#[tokio::test]
async fn test_auth_packet_creation() {
    // Test creating AUTH packet for continuing authentication
    let auth_packet = AuthPacket::continue_authentication(
        "SCRAM-SHA-256".to_string(),
        Some(b"client_nonce_data".to_vec()),
    )
    .unwrap();

    assert_eq!(auth_packet.reason_code, ReasonCode::ContinueAuthentication);
    assert_eq!(auth_packet.authentication_method(), Some("SCRAM-SHA-256"));
    assert_eq!(
        auth_packet.authentication_data(),
        Some(b"client_nonce_data".as_ref())
    );
}

#[tokio::test]
async fn test_auth_packet_re_authenticate() {
    // Test creating AUTH packet for re-authentication
    let auth_packet =
        AuthPacket::re_authenticate("OAUTH2".to_string(), Some(b"refresh_token".to_vec())).unwrap();

    assert_eq!(auth_packet.reason_code, ReasonCode::ReAuthenticate);
    assert_eq!(auth_packet.authentication_method(), Some("OAUTH2"));
    assert_eq!(
        auth_packet.authentication_data(),
        Some(b"refresh_token".as_ref())
    );
}

#[tokio::test]
async fn test_auth_packet_success() {
    // Test creating successful AUTH response
    let auth_packet = AuthPacket::success("PLAIN".to_string()).unwrap();

    assert_eq!(auth_packet.reason_code, ReasonCode::Success);
    assert_eq!(auth_packet.authentication_method(), Some("PLAIN"));
    assert!(auth_packet.authentication_data().is_none());
}

#[tokio::test]
async fn test_auth_packet_failure() {
    // Test creating AUTH failure response
    let auth_packet = AuthPacket::failure(
        ReasonCode::BadAuthenticationMethod,
        Some("Unsupported authentication method".to_string()),
    )
    .unwrap();

    assert_eq!(auth_packet.reason_code, ReasonCode::BadAuthenticationMethod);
    assert_eq!(
        auth_packet.reason_string(),
        Some("Unsupported authentication method")
    );
    assert!(auth_packet.authentication_method().is_none());
}

#[tokio::test]
async fn test_auth_packet_validation() {
    // Test that AUTH packet validation works for ContinueAuthentication
    let invalid_packet = AuthPacket::new(ReasonCode::ContinueAuthentication);
    let result = invalid_packet.validate();
    assert!(result.is_err());

    // Test that AUTH packet validation works for ReAuthenticate
    let invalid_packet = AuthPacket::new(ReasonCode::ReAuthenticate);
    let result = invalid_packet.validate();
    assert!(result.is_err());

    // Test that Success packets can be valid without authentication method
    let valid_packet = AuthPacket::new(ReasonCode::Success);
    let result = valid_packet.validate();
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_auth_packet_failure_with_success_code() {
    // Test that creating failure with success code fails
    let result = AuthPacket::failure(ReasonCode::Success, None);
    assert!(result.is_err());
    if let Err(MqttError::ProtocolError(msg)) = result {
        assert!(msg.contains("Cannot create failure AUTH packet with success reason code"));
    }
}

#[tokio::test]
async fn test_auth_packet_encode_decode_cycle() {
    use bytes::BytesMut;
    use mqtt5::packet::FixedHeader;

    // Test complete encode/decode cycle for AUTH packet with properties
    let original_packet = AuthPacket::continue_authentication(
        "DIGEST-MD5".to_string(),
        Some(b"challenge_response_data".to_vec()),
    )
    .unwrap();

    // Encode the packet
    let mut buf = BytesMut::new();
    original_packet.encode(&mut buf).unwrap();

    // Decode the packet
    let fixed_header = FixedHeader::decode(&mut buf).unwrap();
    let decoded_packet = <AuthPacket as MqttPacket>::decode_body(&mut buf, &fixed_header).unwrap();

    // Verify the decoded packet matches the original
    assert_eq!(decoded_packet.reason_code, original_packet.reason_code);
    assert_eq!(
        decoded_packet.authentication_method(),
        original_packet.authentication_method()
    );
    assert_eq!(
        decoded_packet.authentication_data(),
        original_packet.authentication_data()
    );
}

#[tokio::test]
async fn test_auth_packet_properties_conversion() {
    use mqtt5::protocol::v5::properties::{PropertyId, PropertyValue};
    use mqtt5::types::WillProperties;

    // Test conversion from WillProperties to protocol Properties
    // This was implemented as part of the AUTH packet requirements
    let mut will_props = WillProperties {
        will_delay_interval: Some(30),
        message_expiry_interval: Some(3600),
        content_type: Some("application/json".to_string()),
        ..WillProperties::default()
    };
    will_props
        .user_properties
        .push(("client".to_string(), "v1.0".to_string()));

    // Convert to protocol properties (this tests the From implementation)
    let protocol_props: mqtt5::protocol::v5::properties::Properties = will_props.into();

    // Verify conversion worked correctly
    assert_eq!(
        protocol_props.get(PropertyId::WillDelayInterval),
        Some(&PropertyValue::FourByteInteger(30))
    );
    assert_eq!(
        protocol_props.get(PropertyId::MessageExpiryInterval),
        Some(&PropertyValue::FourByteInteger(3600))
    );
    assert_eq!(
        protocol_props.get(PropertyId::ContentType),
        Some(&PropertyValue::Utf8String("application/json".to_string()))
    );
}

#[tokio::test]
async fn test_auth_methods_supported() {
    // Test various authentication methods that should be supported
    let methods = vec![
        "PLAIN",
        "SCRAM-SHA-1",
        "SCRAM-SHA-256",
        "DIGEST-MD5",
        "GSSAPI",
        "OAUTH2",
        "JWT",
        "CUSTOM-METHOD",
    ];

    for method in methods {
        let auth_packet =
            AuthPacket::continue_authentication(method.to_string(), Some(b"test_data".to_vec()))
                .unwrap();

        assert_eq!(auth_packet.authentication_method(), Some(method));
        assert!(auth_packet.validate().is_ok());
    }
}

#[tokio::test]
async fn test_auth_packet_no_data() {
    // Test AUTH packet without authentication data
    let auth_packet = AuthPacket::continue_authentication(
        "SCRAM-SHA-256".to_string(),
        None, // No authentication data
    )
    .unwrap();

    assert_eq!(auth_packet.reason_code, ReasonCode::ContinueAuthentication);
    assert_eq!(auth_packet.authentication_method(), Some("SCRAM-SHA-256"));
    assert!(auth_packet.authentication_data().is_none());
}

#[tokio::test]
async fn test_auth_packet_large_data() {
    let large_data = vec![0xAB; 10000];
    let auth_packet =
        AuthPacket::continue_authentication("CUSTOM".to_string(), Some(large_data.clone()))
            .unwrap();

    assert_eq!(
        auth_packet.authentication_data(),
        Some(large_data.as_slice())
    );
}

struct TestChallengeResponseAuthProvider {
    challenge: Vec<u8>,
    expected_response: Vec<u8>,
    server_final: Option<Vec<u8>>,
}

impl AuthProvider for TestChallengeResponseAuthProvider {
    fn authenticate<'a>(
        &'a self,
        _connect: &'a ConnectPacket,
        _client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        Box::pin(async move { Ok(AuthResult::success()) })
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

    fn supports_enhanced_auth(&self) -> bool {
        true
    }

    fn authenticate_enhanced<'a>(
        &'a self,
        auth_method: &'a str,
        auth_data: Option<&'a [u8]>,
        _client_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        let method = auth_method.to_string();
        let challenge = self.challenge.clone();
        let expected = self.expected_response.clone();
        let server_final = self.server_final.clone();

        Box::pin(async move {
            if method != "CHALLENGE-RESPONSE" {
                return Ok(EnhancedAuthResult::fail(
                    method,
                    ReasonCode::BadAuthenticationMethod,
                ));
            }

            match auth_data {
                None => Ok(EnhancedAuthResult::continue_auth(method, Some(challenge))),
                Some(response) if response == expected => {
                    let mut success = EnhancedAuthResult::success(method);
                    success.auth_data = server_final;
                    Ok(success)
                }
                Some(_) => Ok(EnhancedAuthResult::fail(method, ReasonCode::NotAuthorized)),
            }
        })
    }

    fn reauthenticate<'a>(
        &'a self,
        auth_method: &'a str,
        auth_data: Option<&'a [u8]>,
        client_id: &'a str,
        _user_id: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        self.authenticate_enhanced(auth_method, auth_data, client_id)
    }
}

struct TestClientAuthHandler {
    expected_challenge: Vec<u8>,
    response: Vec<u8>,
}

impl AuthHandler for TestClientAuthHandler {
    fn handle_challenge<'a>(
        &'a self,
        _auth_method: &'a str,
        challenge_data: Option<&'a [u8]>,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResponse>> + Send + 'a>> {
        let expected = self.expected_challenge.clone();
        let response = self.response.clone();

        Box::pin(async move {
            if challenge_data == Some(expected.as_slice()) {
                Ok(AuthResponse::Continue(response))
            } else {
                Ok(AuthResponse::Abort("Unexpected challenge".to_string()))
            }
        })
    }
}

#[tokio::test]
async fn test_client_enhanced_auth_success() {
    let challenge = b"server-challenge-xyz".to_vec();
    let response = b"client-response-abc".to_vec();

    let auth_provider = Arc::new(TestChallengeResponseAuthProvider {
        challenge: challenge.clone(),
        expected_response: response.clone(),
        server_final: None,
    });

    let mut broker = MqttBroker::with_config(test_broker_config())
        .await
        .unwrap()
        .with_auth_provider(auth_provider);

    let addr = broker.local_addr().expect("failed to get broker address");

    let broker_handle = tokio::spawn(async move { broker.run().await });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let options =
        ConnectOptions::new("auth-test-client").with_authentication_method("CHALLENGE-RESPONSE");

    let client = MqttClient::with_options(options);
    client
        .set_auth_handler(TestClientAuthHandler {
            expected_challenge: challenge,
            response,
        })
        .await;

    let result = client.connect(&format!("mqtt://{addr}")).await;
    assert!(
        result.is_ok(),
        "Client should connect with enhanced auth: {result:?}"
    );

    assert!(client.is_connected().await);

    client.disconnect().await.unwrap();
    broker_handle.abort();
}

#[tokio::test]
async fn test_client_enhanced_auth_failure() {
    let challenge = b"server-challenge-xyz".to_vec();
    let correct_response = b"client-response-abc".to_vec();
    let wrong_response = b"wrong-response".to_vec();

    let auth_provider = Arc::new(TestChallengeResponseAuthProvider {
        challenge: challenge.clone(),
        expected_response: correct_response,
        server_final: None,
    });

    let mut broker = MqttBroker::with_config(test_broker_config())
        .await
        .unwrap()
        .with_auth_provider(auth_provider);

    let addr = broker.local_addr().expect("failed to get broker address");

    let broker_handle = tokio::spawn(async move { broker.run().await });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let options =
        ConnectOptions::new("auth-fail-client").with_authentication_method("CHALLENGE-RESPONSE");

    let client = MqttClient::with_options(options);
    client
        .set_auth_handler(TestClientAuthHandler {
            expected_challenge: challenge,
            response: wrong_response,
        })
        .await;

    let result = client.connect(&format!("mqtt://{addr}")).await;
    assert!(
        result.is_err(),
        "Client should fail with wrong response, but got: {result:?}"
    );

    broker_handle.abort();
}

#[tokio::test]
async fn test_client_enhanced_auth_no_handler() {
    let auth_provider = Arc::new(TestChallengeResponseAuthProvider {
        challenge: b"challenge".to_vec(),
        expected_response: b"response".to_vec(),
        server_final: None,
    });

    let mut broker = MqttBroker::with_config(test_broker_config())
        .await
        .unwrap()
        .with_auth_provider(auth_provider);

    let addr = broker.local_addr().expect("failed to get broker address");

    let broker_handle = tokio::spawn(async move { broker.run().await });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let options =
        ConnectOptions::new("no-handler-client").with_authentication_method("CHALLENGE-RESPONSE");

    let client = MqttClient::with_options(options);

    let result = client.connect(&format!("mqtt://{addr}")).await;
    assert!(result.is_err(), "Client should fail without auth handler");

    if let Err(MqttError::AuthenticationFailed) = result {
    } else {
        panic!("Expected AuthenticationFailed error, got {result:?}");
    }

    broker_handle.abort();
}

struct Wire {
    stream: tokio::net::TcpStream,
    buffer: bytes::BytesMut,
}

impl Wire {
    async fn open(addr: SocketAddr) -> Self {
        Self {
            stream: tokio::net::TcpStream::connect(addr).await.unwrap(),
            buffer: bytes::BytesMut::new(),
        }
    }

    async fn send(&mut self, packet: &impl MqttPacket) {
        use tokio::io::AsyncWriteExt;
        let mut bytes = Vec::new();
        packet.encode(&mut bytes).unwrap();
        self.stream.write_all(&bytes).await.unwrap();
    }

    async fn next(&mut self, millis: u64) -> Option<Packet> {
        tokio::time::timeout(
            Duration::from_millis(millis),
            read_packet_from_stream(&mut self.stream, 5, &mut self.buffer, 1 << 20),
        )
        .await
        .ok()?
        .ok()
    }
}

const METHOD: &str = "CHALLENGE-RESPONSE";
const CHALLENGE: &[u8] = b"server-challenge-xyz";
const RESPONSE: &[u8] = b"client-response-abc";
const SERVER_FINAL: &[u8] = b"server-final-signature";

async fn start_challenge_broker(config: BrokerConfig) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let auth_provider = Arc::new(TestChallengeResponseAuthProvider {
        challenge: CHALLENGE.to_vec(),
        expected_response: RESPONSE.to_vec(),
        server_final: Some(SERVER_FINAL.to_vec()),
    });
    let mut broker = MqttBroker::with_config(config)
        .await
        .unwrap()
        .with_auth_provider(auth_provider);
    let addr = broker.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        if let Err(e) = broker.run().await {
            tracing::debug!("broker stopped: {e}");
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    (addr, handle)
}

async fn two_step_exchange(
    addr: SocketAddr,
    options: ConnectOptions,
    response: &[u8],
) -> (Wire, Option<Packet>) {
    let mut wire = Wire::open(addr).await;
    wire.send(&ConnectPacket::new(
        options.with_authentication_method(METHOD).protocol_options,
    ))
    .await;
    match wire.next(5000).await {
        Some(Packet::Auth(auth)) => {
            assert_eq!(auth.reason_code, ReasonCode::ContinueAuthentication);
            assert_eq!(auth.authentication_data(), Some(CHALLENGE));
        }
        other => panic!("expected AUTH continue, got {other:?}"),
    }
    wire.send(
        &AuthPacket::continue_authentication(METHOD.to_string(), Some(response.to_vec())).unwrap(),
    )
    .await;
    let reply = wire.next(5000).await;
    (wire, reply)
}

async fn two_step_connect(addr: SocketAddr, options: ConnectOptions) -> (Wire, ConnAckPacket) {
    match two_step_exchange(addr, options, RESPONSE).await {
        (wire, Some(Packet::ConnAck(connack))) => (wire, connack),
        (_, other) => panic!("expected CONNACK, got {other:?}"),
    }
}

#[tokio::test]
async fn enhanced_auth_connack_advertises_capped_session_expiry() {
    let (addr, handle) =
        start_challenge_broker(test_broker_config().with_session_expiry(Duration::from_secs(10)))
            .await;
    let (_wire, connack) = two_step_connect(
        addr,
        ConnectOptions::new("capped-enhanced").with_session_expiry_interval(3600),
    )
    .await;
    handle.abort();
    assert_eq!(connack.reason_code, ReasonCode::Success);
    assert_eq!(
        connack.properties.get_session_expiry_interval(),
        Some(10),
        "the broker granted its 10s maximum instead of the requested 3600s, so CONNACK must say so"
    );
}

#[tokio::test]
async fn two_step_enhanced_auth_rejects_an_unsupported_will_qos() {
    let (addr, handle) = start_challenge_broker(test_broker_config().with_maximum_qos(1)).await;
    let will = mqtt5::types::WillMessage::new("will/t", b"gone".to_vec())
        .with_qos(mqtt5::QoS::ExactlyOnce);
    let (_wire, connack) =
        two_step_connect(addr, ConnectOptions::new("will-qos").with_will(will)).await;
    handle.abort();
    assert_eq!(
        connack.reason_code,
        ReasonCode::QoSNotSupported,
        "the Will QoS check must apply after multi-step authentication too"
    );
}

#[tokio::test]
async fn two_step_enhanced_auth_honours_the_client_receive_maximum() {
    let (addr, handle) = start_challenge_broker(test_broker_config()).await;
    let (mut subscriber, connack) =
        two_step_connect(addr, ConnectOptions::new("rm-one").with_receive_maximum(1)).await;
    assert_eq!(connack.reason_code, ReasonCode::Success);
    subscriber
        .send(&SubscribePacket::new(1).add_filter("rm/t", mqtt5::QoS::AtLeastOnce))
        .await;
    assert!(matches!(
        subscriber.next(5000).await,
        Some(Packet::SubAck(_))
    ));

    let mut publisher = Wire::open(addr).await;
    publisher
        .send(&ConnectPacket::new(
            ConnectOptions::new("rm-pub").protocol_options,
        ))
        .await;
    assert!(matches!(
        publisher.next(5000).await,
        Some(Packet::ConnAck(_))
    ));
    for packet_id in 1..=3u16 {
        let mut publish =
            PublishPacket::new("rm/t".to_string(), b"hi".to_vec(), mqtt5::QoS::AtLeastOnce);
        publish.packet_id = Some(packet_id);
        publisher.send(&publish).await;
    }
    let mut delivered = 0;
    while let Some(packet) = subscriber.next(1000).await {
        if matches!(packet, Packet::Publish(_)) {
            delivered += 1;
        }
    }
    handle.abort();
    assert_eq!(
        delivered, 1,
        "Receive Maximum 1 allows one unacknowledged QoS 1 PUBLISH, yet {delivered} were sent"
    );
}

#[tokio::test]
async fn two_step_enhanced_auth_success_returns_method_and_server_data_in_connack() {
    let (addr, handle) = start_challenge_broker(test_broker_config()).await;
    let (_wire, connack) = two_step_connect(addr, ConnectOptions::new("server-final")).await;
    handle.abort();
    assert_eq!(connack.reason_code, ReasonCode::Success);
    assert_eq!(
        connack
            .properties
            .get_authentication_method()
            .map(String::as_str),
        Some(METHOD),
        "CONNACK after enhanced authentication must carry the Authentication Method"
    );
    assert_eq!(
        connack.properties.get_authentication_data(),
        Some(SERVER_FINAL),
        "the server's final authentication data never reached the client"
    );
}

#[tokio::test]
async fn failed_two_step_enhanced_auth_is_refused_by_connack() {
    let (addr, handle) = start_challenge_broker(test_broker_config()).await;
    let (mut wire, reply) =
        two_step_exchange(addr, ConnectOptions::new("wrong-proof"), b"wrong").await;
    let closed = wire.next(2000).await.is_none();
    handle.abort();
    match reply {
        Some(Packet::ConnAck(connack)) => {
            assert_eq!(connack.reason_code, ReasonCode::NotAuthorized);
        }
        other => {
            panic!("a failed authentication must be answered with CONNACK 0x87, got {other:?}")
        }
    }
    assert!(closed);
}

async fn reauthenticate(wire: &mut Wire, response: &[u8]) -> Option<Packet> {
    wire.send(&AuthPacket::re_authenticate(METHOD.to_string(), None).unwrap())
        .await;
    match wire.next(5000).await {
        Some(Packet::Auth(auth)) => {
            assert_eq!(auth.reason_code, ReasonCode::ContinueAuthentication);
        }
        other => panic!("expected AUTH continue, got {other:?}"),
    }
    wire.send(
        &AuthPacket::continue_authentication(METHOD.to_string(), Some(response.to_vec())).unwrap(),
    )
    .await;
    wire.next(5000).await
}

#[tokio::test]
async fn reauthentication_success_returns_the_server_data() {
    let (addr, handle) = start_challenge_broker(test_broker_config()).await;
    let (mut wire, connack) = two_step_connect(addr, ConnectOptions::new("reauth-ok")).await;
    assert_eq!(connack.reason_code, ReasonCode::Success);
    let reply = reauthenticate(&mut wire, RESPONSE).await;
    handle.abort();
    match reply {
        Some(Packet::Auth(auth)) => {
            assert_eq!(auth.reason_code, ReasonCode::Success);
            assert_eq!(auth.authentication_method(), Some(METHOD));
            assert_eq!(auth.authentication_data(), Some(SERVER_FINAL));
        }
        other => panic!("expected AUTH success, got {other:?}"),
    }
}

#[tokio::test]
async fn failed_reauthentication_is_refused_by_disconnect() {
    let (addr, handle) = start_challenge_broker(test_broker_config()).await;
    let (mut wire, connack) = two_step_connect(addr, ConnectOptions::new("reauth-bad")).await;
    assert_eq!(connack.reason_code, ReasonCode::Success);
    let reply = reauthenticate(&mut wire, b"wrong").await;
    handle.abort();
    match reply {
        Some(Packet::Disconnect(disconnect)) => {
            assert_eq!(disconnect.reason_code, ReasonCode::NotAuthorized);
        }
        other => {
            panic!(
                "a failed re-authentication must be answered with DISCONNECT 0x87, got {other:?}"
            )
        }
    }
}
