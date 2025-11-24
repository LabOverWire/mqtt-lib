use mqtt5::error::MqttError;
use mqtt5::packet::auth::AuthPacket;
use mqtt5::packet::MqttPacket;
use mqtt5::protocol::v5::reason_codes::ReasonCode;

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
    // Test AUTH packet with large authentication data
    let large_data = vec![0xAB; 10000]; // 10KB of data
    let auth_packet =
        AuthPacket::continue_authentication("CUSTOM".to_string(), Some(large_data.clone()))
            .unwrap();

    assert_eq!(
        auth_packet.authentication_data(),
        Some(large_data.as_slice())
    );
}
