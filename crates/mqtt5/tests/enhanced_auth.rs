use mqtt5::broker::auth::{AuthProvider, AuthResult, EnhancedAuthResult};
use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::client::{AuthHandler, AuthResponse};
use mqtt5::error::{MqttError, Result};
use mqtt5::packet::auth::AuthPacket;
use mqtt5::packet::connect::ConnectPacket;
use mqtt5::packet::MqttPacket;
use mqtt5::protocol::v5::reason_codes::ReasonCode;
use mqtt5::types::ConnectOptions;
use mqtt5::MqttClient;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

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
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move { Ok(true) })
    }

    fn authorize_subscribe<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move { Ok(true) })
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

        Box::pin(async move {
            if method != "CHALLENGE-RESPONSE" {
                return Ok(EnhancedAuthResult::fail(
                    method,
                    ReasonCode::BadAuthenticationMethod,
                ));
            }

            match auth_data {
                None => Ok(EnhancedAuthResult::continue_auth(method, Some(challenge))),
                Some(response) if response == expected => Ok(EnhancedAuthResult::success(method)),
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

async fn find_available_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

#[tokio::test]
async fn test_client_enhanced_auth_success() {
    let port = find_available_port().await;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    let challenge = b"server-challenge-xyz".to_vec();
    let response = b"client-response-abc".to_vec();

    let auth_provider = Arc::new(TestChallengeResponseAuthProvider {
        challenge: challenge.clone(),
        expected_response: response.clone(),
    });

    let config = BrokerConfig {
        bind_addresses: vec![addr],
        max_clients: 10,
        session_expiry_interval: Duration::from_secs(60),
        ..Default::default()
    };

    let mut broker = MqttBroker::with_config(config)
        .await
        .unwrap()
        .with_auth_provider(auth_provider);

    let broker_handle = tokio::spawn(async move {
        broker.run().await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let options = ConnectOptions::new("auth-test-client")
        .with_authentication_method("CHALLENGE-RESPONSE");

    let client = MqttClient::with_options(options);
    client
        .set_auth_handler(TestClientAuthHandler {
            expected_challenge: challenge,
            response,
        })
        .await;

    let result = client.connect(&format!("mqtt://127.0.0.1:{port}")).await;
    assert!(result.is_ok(), "Client should connect with enhanced auth: {:?}", result);

    assert!(client.is_connected().await);

    client.disconnect().await.unwrap();
    broker_handle.abort();
}

#[tokio::test]
async fn test_client_enhanced_auth_failure() {
    let port = find_available_port().await;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    let challenge = b"server-challenge-xyz".to_vec();
    let correct_response = b"client-response-abc".to_vec();
    let wrong_response = b"wrong-response".to_vec();

    let auth_provider = Arc::new(TestChallengeResponseAuthProvider {
        challenge: challenge.clone(),
        expected_response: correct_response,
    });

    let config = BrokerConfig {
        bind_addresses: vec![addr],
        max_clients: 10,
        session_expiry_interval: Duration::from_secs(60),
        ..Default::default()
    };

    let mut broker = MqttBroker::with_config(config)
        .await
        .unwrap()
        .with_auth_provider(auth_provider);

    let broker_handle = tokio::spawn(async move {
        broker.run().await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let options = ConnectOptions::new("auth-fail-client")
        .with_authentication_method("CHALLENGE-RESPONSE");

    let client = MqttClient::with_options(options);
    client
        .set_auth_handler(TestClientAuthHandler {
            expected_challenge: challenge,
            response: wrong_response,
        })
        .await;

    let result = client.connect(&format!("mqtt://127.0.0.1:{port}")).await;
    assert!(
        result.is_err(),
        "Client should fail with wrong response, but got: {:?}",
        result
    );

    broker_handle.abort();
}

#[tokio::test]
async fn test_client_enhanced_auth_no_handler() {
    let port = find_available_port().await;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    let auth_provider = Arc::new(TestChallengeResponseAuthProvider {
        challenge: b"challenge".to_vec(),
        expected_response: b"response".to_vec(),
    });

    let config = BrokerConfig {
        bind_addresses: vec![addr],
        max_clients: 10,
        session_expiry_interval: Duration::from_secs(60),
        ..Default::default()
    };

    let mut broker = MqttBroker::with_config(config)
        .await
        .unwrap()
        .with_auth_provider(auth_provider);

    let broker_handle = tokio::spawn(async move {
        broker.run().await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let options = ConnectOptions::new("no-handler-client")
        .with_authentication_method("CHALLENGE-RESPONSE");

    let client = MqttClient::with_options(options);

    let result = client.connect(&format!("mqtt://127.0.0.1:{port}")).await;
    assert!(result.is_err(), "Client should fail without auth handler");

    if let Err(MqttError::AuthenticationFailed) = result {
    } else {
        panic!("Expected AuthenticationFailed error, got {:?}", result);
    }

    broker_handle.abort();
}
