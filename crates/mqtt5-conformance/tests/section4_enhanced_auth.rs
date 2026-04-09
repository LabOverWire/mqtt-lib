use mqtt5::broker::auth::{AuthProvider, AuthResult, EnhancedAuthResult};
use mqtt5::error::Result;
use mqtt5::packet::connect::ConnectPacket;
use mqtt5::protocol::v5::reason_codes::ReasonCode;
use mqtt5_conformance::harness::unique_client_id;
use mqtt5_conformance::raw_client::{RawMqttClient, RawPacketBuilder};
use mqtt5_conformance::sut::inprocess_sut_with_auth_provider;
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

/// `[MQTT-4.12.0-4]` If the initial CONNECT-triggered enhanced auth
/// fails, the Server MUST send a CONNACK with an error Reason Code and
/// close the connection.
#[tokio::test]
async fn auth_failure_closes_connection() {
    let sut = inprocess_sut_with_auth_provider(challenge_response_provider()).await;
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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

/// `[MQTT-4.12.0-2]` Server AUTH packet has reason code 0x18 (Continue).
/// `[MQTT-4.12.0-3]` Client AUTH response has reason code 0x18.
#[tokio::test]
async fn auth_continue_has_correct_reason_code() {
    let sut = inprocess_sut_with_auth_provider(challenge_response_provider()).await;
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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

/// `[MQTT-4.12.0-5]` The Authentication Method MUST be the same in all
/// AUTH packets within the flow.
#[tokio::test]
async fn auth_method_consistent_in_flow() {
    let sut = inprocess_sut_with_auth_provider(challenge_response_provider()).await;
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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
#[tokio::test]
async fn reauth_failure_disconnects() {
    let sut = inprocess_sut_with_auth_provider(challenge_response_provider()).await;
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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
