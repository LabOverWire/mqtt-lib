use mqtt5::broker::auth::{AuthProvider, AuthResult, EnhancedAuthResult};
use mqtt5::error::Result;
use mqtt5::packet::connect::ConnectPacket;
use mqtt5::protocol::v5::reason_codes::ReasonCode;
use mqtt5_conformance::harness::{unique_client_id, ConformanceBroker};
use mqtt5_conformance::raw_client::{RawMqttClient, RawPacketBuilder};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

struct ChallengeResponseAuth {
    challenge: Vec<u8>,
    expected_response: Vec<u8>,
}

impl ChallengeResponseAuth {
    fn new(challenge: Vec<u8>, expected_response: Vec<u8>) -> Self {
        Self {
            challenge,
            expected_response,
        }
    }
}

impl AuthProvider for ChallengeResponseAuth {
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
                    Ok(EnhancedAuthResult::success_with_user(method, "test-user"))
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

fn challenge_response_provider() -> Arc<dyn AuthProvider> {
    Arc::new(ChallengeResponseAuth::new(
        b"server-challenge".to_vec(),
        b"correct-response".to_vec(),
    ))
}

/// `[MQTT-4.12.0-1]` If the Server does not support the Authentication
/// Method supplied by the Client, it MAY send a CONNACK with a Reason
/// Code of 0x8C (Bad Authentication Method).
///
/// Sends CONNECT with an unknown Authentication Method to a broker using
/// `AllowAllAuth` (no enhanced auth support). Expects CONNACK 0x8C.
#[tokio::test]
async fn unsupported_auth_method_closes() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    let client_id = unique_client_id("unsup-auth");
    let connect = RawPacketBuilder::connect_with_auth_method(&client_id, "UNKNOWN-METHOD");
    raw.send_raw(&connect).await.unwrap();

    let connack = raw.expect_connack(Duration::from_secs(3)).await;
    assert!(
        connack.is_some(),
        "[MQTT-4.12.0-1] Server must send CONNACK for unsupported auth method"
    );
    let (_, reason_code) = connack.unwrap();
    assert_eq!(
        reason_code, 0x8C,
        "[MQTT-4.12.0-1] CONNACK reason code must be 0x8C (Bad Authentication Method)"
    );

    assert!(
        raw.expect_disconnect(Duration::from_secs(2)).await,
        "[MQTT-4.12.0-1] Server must close connection after rejecting auth method"
    );
}

/// `[MQTT-4.12.0-6]` If the Client does not include an Authentication
/// Method in the CONNECT, the Server MUST NOT send an AUTH packet.
///
/// Sends a plain CONNECT (no auth method), verifies CONNACK is received
/// without any preceding AUTH packet. Then sends a PINGREQ to confirm
/// the connection is operational with no AUTH packets in the stream.
#[tokio::test]
async fn no_auth_method_no_server_auth() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    let client_id = unique_client_id("no-auth");
    let connect = RawPacketBuilder::valid_connect(&client_id);
    raw.send_raw(&connect).await.unwrap();

    let connack = raw.expect_connack(Duration::from_secs(3)).await;
    assert!(
        connack.is_some(),
        "[MQTT-4.12.0-6] Server must send CONNACK for plain connect"
    );
    let (_, reason_code) = connack.unwrap();
    assert_eq!(
        reason_code, 0x00,
        "[MQTT-4.12.0-6] CONNACK must be success for plain connect"
    );

    raw.send_raw(&RawPacketBuilder::pingreq()).await.unwrap();
    assert!(
        raw.expect_pingresp(Duration::from_secs(3)).await,
        "[MQTT-4.12.0-6] Connection must remain operational with no AUTH packets sent"
    );
}

/// `[MQTT-4.12.0-7]` If the Client does not include an Authentication
/// Method in the CONNECT, the Client MUST NOT send an AUTH packet to the
/// Server. The Server treats this as a Protocol Error.
///
/// Connects normally (no auth method), then sends an unsolicited AUTH
/// packet. Expects the broker to DISCONNECT or close.
#[tokio::test]
async fn unsolicited_auth_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    let client_id = unique_client_id("unsol-auth");
    let connack = raw
        .connect_and_establish(&client_id, Duration::from_secs(3))
        .await;
    assert!(connack.is_some(), "CONNECT must succeed");

    let auth = RawPacketBuilder::auth_with_method(0x19, "SOME-METHOD");
    raw.send_raw(&auth).await.unwrap();

    assert!(
        raw.expect_disconnect(Duration::from_secs(3)).await,
        "[MQTT-4.12.0-7] Server must disconnect client that sends AUTH without prior auth method"
    );
}

/// `[MQTT-4.12.0-4]` If the initial CONNECT-triggered enhanced auth
/// fails, the Server MUST send a CONNACK with an error Reason Code and
/// close the connection.
///
/// Uses a `ChallengeResponseAuth` provider. Sends CONNECT with the correct
/// method but wrong credentials (bad auth data on the continue step).
#[tokio::test]
async fn auth_failure_closes_connection() {
    let broker = ConformanceBroker::start_with_auth_provider(challenge_response_provider()).await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    let client_id = unique_client_id("auth-fail");
    let connect = RawPacketBuilder::connect_with_auth_method(&client_id, "CHALLENGE-RESPONSE");
    raw.send_raw(&connect).await.unwrap();

    let auth_continue = raw.expect_auth_packet(Duration::from_secs(3)).await;
    assert!(
        auth_continue.is_some(),
        "[MQTT-4.12.0-4] Server must send AUTH continue for challenge-response"
    );
    let (rc, _) = auth_continue.unwrap();
    assert_eq!(rc, 0x18, "AUTH reason code must be Continue Authentication");

    let bad_response =
        RawPacketBuilder::auth_with_method_and_data(0x18, "CHALLENGE-RESPONSE", b"wrong-response");
    raw.send_raw(&bad_response).await.unwrap();

    assert!(
        raw.expect_disconnect(Duration::from_secs(3)).await,
        "[MQTT-4.12.0-4] Server must close connection after auth failure"
    );
}

/// `[MQTT-4.12.0-2]` The Server that does not support the Authentication
/// Method … If the Server requires additional information it sends an
/// AUTH packet with Reason Code 0x18 (Continue Authentication).
///
/// `[MQTT-4.12.0-3]` The Client responds to an AUTH packet from the
/// Server by sending a further AUTH packet. This packet MUST contain
/// Reason Code 0x18 (Continue Authentication).
///
/// Verifies the server sends AUTH with exactly Reason Code 0x18 during
/// the challenge-response flow.
#[tokio::test]
async fn auth_continue_has_correct_reason_code() {
    let broker = ConformanceBroker::start_with_auth_provider(challenge_response_provider()).await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    let client_id = unique_client_id("auth-cont");
    let connect = RawPacketBuilder::connect_with_auth_method(&client_id, "CHALLENGE-RESPONSE");
    raw.send_raw(&connect).await.unwrap();

    let auth_continue = raw.expect_auth_packet(Duration::from_secs(3)).await;
    assert!(
        auth_continue.is_some(),
        "[MQTT-4.12.0-2] Server must send AUTH packet during enhanced auth"
    );
    let (reason_code, _method) = auth_continue.unwrap();
    assert_eq!(
        reason_code, 0x18,
        "[MQTT-4.12.0-2] Server AUTH reason code must be 0x18 (Continue Authentication)"
    );
}

/// `[MQTT-4.12.0-5]` The Authentication Method is a UTF-8 Encoded String.
/// If the Authentication Method is present, it MUST be the same in all
/// AUTH packets within the flow.
///
/// Verifies the server echoes the same Authentication Method property
/// in its AUTH packet as was sent in CONNECT.
#[tokio::test]
async fn auth_method_consistent_in_flow() {
    let broker = ConformanceBroker::start_with_auth_provider(challenge_response_provider()).await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    let client_id = unique_client_id("auth-meth");
    let connect = RawPacketBuilder::connect_with_auth_method(&client_id, "CHALLENGE-RESPONSE");
    raw.send_raw(&connect).await.unwrap();

    let auth_continue = raw.expect_auth_packet(Duration::from_secs(3)).await;
    assert!(
        auth_continue.is_some(),
        "[MQTT-4.12.0-5] Server must send AUTH packet"
    );
    let (_, method) = auth_continue.unwrap();
    assert_eq!(
        method.as_deref(),
        Some("CHALLENGE-RESPONSE"),
        "[MQTT-4.12.0-5] Auth method in server AUTH must match CONNECT auth method"
    );
}

/// `[MQTT-4.12.1-2]` If re-authentication fails, the Server MUST send a
/// DISCONNECT with an appropriate Reason Code and close the connection.
///
/// Successfully authenticates with enhanced auth, then sends re-auth
/// with bad credentials. Expects DISCONNECT.
#[tokio::test]
async fn reauth_failure_disconnects() {
    let broker = ConformanceBroker::start_with_auth_provider(challenge_response_provider()).await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    let client_id = unique_client_id("reauth-fail");
    let connect = RawPacketBuilder::connect_with_auth_method(&client_id, "CHALLENGE-RESPONSE");
    raw.send_raw(&connect).await.unwrap();

    let auth_continue = raw.expect_auth_packet(Duration::from_secs(3)).await;
    assert!(auth_continue.is_some(), "Server must send AUTH continue");
    let (rc, _) = auth_continue.unwrap();
    assert_eq!(rc, 0x18);

    let good_response = RawPacketBuilder::auth_with_method_and_data(
        0x18,
        "CHALLENGE-RESPONSE",
        b"correct-response",
    );
    raw.send_raw(&good_response).await.unwrap();

    let connack = raw.expect_connack(Duration::from_secs(3)).await;
    assert!(
        connack.is_some(),
        "Server must send CONNACK after successful auth"
    );
    let (_, reason) = connack.unwrap();
    assert_eq!(reason, 0x00, "CONNACK must be success");

    let reauth_bad =
        RawPacketBuilder::auth_with_method_and_data(0x19, "CHALLENGE-RESPONSE", b"wrong-response");
    raw.send_raw(&reauth_bad).await.unwrap();

    assert!(
        raw.expect_disconnect(Duration::from_secs(3)).await,
        "[MQTT-4.12.1-2] Server must disconnect after re-authentication failure"
    );
}
