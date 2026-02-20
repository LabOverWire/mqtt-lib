mod common;

use common::{unique_client_id, ConformanceBroker, RawMqttClient, RawPacketBuilder};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::{ConnectOptions, MqttClient, QoS, SubscribeOptions};
use mqtt5_protocol::protocol::v5::properties::PropertyId;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use std::net::SocketAddr;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

fn memory_config() -> BrokerConfig {
    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };
    BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage_config)
}

// ---------------------------------------------------------------------------
// Group 1: CONNACK Structure (raw client)
// ---------------------------------------------------------------------------

/// `[MQTT-3.2.2-1]` Byte 1 is the Connect Acknowledge Flags. Bits 7-1 are
/// reserved and MUST be set to 0.
///
/// Connects with a valid CONNECT and verifies the CONNACK flags byte has
/// bits 7-1 all zero.
#[tokio::test]
async fn connack_reserved_flags_zero() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    let client_id = unique_client_id("flags");
    raw.send_raw(&RawPacketBuilder::valid_connect(&client_id))
        .await
        .unwrap();

    let connack = raw.expect_connack(TIMEOUT).await;
    assert!(connack.is_some(), "Must receive CONNACK");
    let (flags, reason) = connack.unwrap();
    assert_eq!(reason, 0x00, "Reason code must be Success");
    assert_eq!(
        flags & 0xFE,
        0,
        "[MQTT-3.2.2-1] CONNACK flags bits 7-1 must be zero"
    );
}

/// `[MQTT-3.2.0-2]` The Server MUST NOT send more than one CONNACK in a
/// Network Connection.
///
/// Connects, subscribes, publishes, and reads all responses to verify no
/// second CONNACK appears.
#[tokio::test]
async fn connack_only_one_per_connection() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    let client_id = unique_client_id("one-connack");
    raw.send_raw(&RawPacketBuilder::valid_connect(&client_id))
        .await
        .unwrap();

    let connack = raw.expect_connack(TIMEOUT).await;
    assert!(connack.is_some(), "Must receive first CONNACK");
    let (_, reason) = connack.unwrap();
    assert_eq!(reason, 0x00, "Must accept connection");

    raw.send_raw(&RawPacketBuilder::subscribe("test/connack", 0))
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::publish_qos0("test/connack", b"hello"))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut connack_count = 0u32;
    loop {
        let data = raw.read_packet_bytes(Duration::from_millis(200)).await;
        match data {
            Some(bytes) if !bytes.is_empty() => {
                if bytes[0] == 0x20 {
                    connack_count += 1;
                }
            }
            _ => break,
        }
    }

    assert_eq!(
        connack_count, 0,
        "[MQTT-3.2.0-2] Server must not send a second CONNACK"
    );
}

// ---------------------------------------------------------------------------
// Group 2: Session Present + Error Handling (raw client)
// ---------------------------------------------------------------------------

/// `[MQTT-3.2.2-6]` If a Server sends a CONNACK packet containing a non-zero
/// Reason Code it MUST set Session Present to 0.
///
/// Sends a CONNECT with an unsupported protocol version (99) and verifies
/// the error CONNACK has `session_present=false`.
#[tokio::test]
async fn connack_session_present_zero_on_error() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::connect_with_protocol_version(99))
        .await
        .unwrap();

    let connack = raw.expect_connack(TIMEOUT).await;
    assert!(connack.is_some(), "Must receive error CONNACK");
    let (flags, reason) = connack.unwrap();
    assert_ne!(reason, 0x00, "Must be a non-success reason code");
    assert_eq!(
        flags & 0x01,
        0,
        "[MQTT-3.2.2-6] Session Present must be 0 when reason code is non-zero"
    );
}

/// `[MQTT-3.2.2-7]` If a Server sends a CONNACK packet containing a Reason
/// Code of 128 or greater it MUST then close the Network Connection.
///
/// Sends a CONNECT with unsupported protocol version, verifies the error
/// CONNACK, then verifies the connection is closed.
#[tokio::test]
async fn connack_error_code_closes_connection() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    raw.send_raw(&RawPacketBuilder::connect_with_protocol_version(99))
        .await
        .unwrap();

    let response = raw.read_packet_bytes(TIMEOUT).await;
    assert!(response.is_some(), "Must receive CONNACK");
    let data = response.unwrap();
    assert_eq!(data[0], 0x20, "Must be CONNACK");

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.2.2-7] Server must close connection after error CONNACK (reason >= 128)"
    );
}

/// `[MQTT-3.2.2-8]` The Server sending the CONNACK packet MUST use one of
/// the Connect Reason Code values.
///
/// Verifies that a successful CONNACK uses 0x00 (Success) and that an error
/// CONNACK uses a valid CONNACK reason code (0x84 for unsupported version).
#[tokio::test]
async fn connack_uses_valid_reason_code() {
    let broker = ConformanceBroker::start().await;

    let mut raw_ok = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("valid-rc");
    raw_ok
        .send_raw(&RawPacketBuilder::valid_connect(&client_id))
        .await
        .unwrap();
    let ok_connack = raw_ok.expect_connack_packet(TIMEOUT).await;
    assert!(ok_connack.is_some(), "Must receive success CONNACK");
    assert_eq!(
        ok_connack.unwrap().reason_code,
        ReasonCode::Success,
        "[MQTT-3.2.2-8] Success CONNACK must have reason code 0x00"
    );

    let mut raw_err = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    raw_err
        .send_raw(&RawPacketBuilder::connect_with_protocol_version(99))
        .await
        .unwrap();
    let err_connack = raw_err.expect_connack_packet(TIMEOUT).await;
    assert!(err_connack.is_some(), "Must receive error CONNACK");
    assert_eq!(
        err_connack.unwrap().reason_code,
        ReasonCode::UnsupportedProtocolVersion,
        "[MQTT-3.2.2-8] Unsupported protocol version must use reason code 0x84"
    );
}

// ---------------------------------------------------------------------------
// Group 3: CONNACK Properties (decoded ConnAckPacket)
// ---------------------------------------------------------------------------

/// Verifies the broker advertises server capability properties in CONNACK:
/// `TopicAliasMaximum`, `RetainAvailable`, `MaximumPacketSize`,
/// `WildcardSubscriptionAvailable`, `SubscriptionIdentifierAvailable`,
/// `SharedSubscriptionAvailable`.
#[tokio::test]
async fn connack_properties_server_capabilities() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();

    let client_id = unique_client_id("caps");
    raw.send_raw(&RawPacketBuilder::valid_connect(&client_id))
        .await
        .unwrap();

    let connack = raw
        .expect_connack_packet(TIMEOUT)
        .await
        .expect("Must receive CONNACK");

    assert_eq!(connack.reason_code, ReasonCode::Success);
    assert!(
        connack
            .properties
            .get(PropertyId::TopicAliasMaximum)
            .is_some(),
        "CONNACK must include TopicAliasMaximum"
    );
    assert!(
        connack
            .properties
            .get(PropertyId::RetainAvailable)
            .is_some(),
        "CONNACK must include RetainAvailable"
    );
    assert!(
        connack
            .properties
            .get(PropertyId::MaximumPacketSize)
            .is_some(),
        "CONNACK must include MaximumPacketSize"
    );
    assert!(
        connack
            .properties
            .get(PropertyId::WildcardSubscriptionAvailable)
            .is_some(),
        "CONNACK must include WildcardSubscriptionAvailable"
    );
    assert!(
        connack
            .properties
            .get(PropertyId::SubscriptionIdentifierAvailable)
            .is_some(),
        "CONNACK must include SubscriptionIdentifierAvailable"
    );
    assert!(
        connack
            .properties
            .get(PropertyId::SharedSubscriptionAvailable)
            .is_some(),
        "CONNACK must include SharedSubscriptionAvailable"
    );
}

/// `[MQTT-3.2.2-9]` When the broker limits Maximum `QoS`, the CONNACK must
/// advertise `MaximumQoS` property.
///
/// Starts a broker with `maximum_qos=1` and verifies the CONNACK contains
/// `MaximumQoS=1`.
#[tokio::test]
async fn connack_maximum_qos_advertised() {
    let config = memory_config().with_maximum_qos(1);
    let broker = ConformanceBroker::start_with_config(config).await;

    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("maxqos");
    raw.send_raw(&RawPacketBuilder::valid_connect(&client_id))
        .await
        .unwrap();

    let connack = raw
        .expect_connack_packet(TIMEOUT)
        .await
        .expect("Must receive CONNACK");

    assert_eq!(connack.reason_code, ReasonCode::Success);
    let max_qos = connack.properties.get(PropertyId::MaximumQoS);
    assert!(
        max_qos.is_some(),
        "[MQTT-3.2.2-9] CONNACK must include MaximumQoS when limited"
    );
    if let Some(mqtt5_protocol::PropertyValue::Byte(qos)) = max_qos {
        assert_eq!(
            *qos, 1,
            "[MQTT-3.2.2-9] MaximumQoS must match configured value"
        );
    } else {
        panic!("MaximumQoS property has wrong type");
    }
}

/// `[MQTT-3.2.2-16]` When two clients connect with empty client IDs, the
/// server must assign different unique `ClientID`s.
#[tokio::test]
async fn connack_assigned_client_id_unique() {
    let broker = ConformanceBroker::start().await;

    let mut ids = Vec::new();
    for _ in 0..2 {
        let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
            .await
            .unwrap();
        raw.send_raw(&build_connect_empty_client_id())
            .await
            .unwrap();

        let connack = raw
            .expect_connack_packet(TIMEOUT)
            .await
            .expect("Must receive CONNACK");

        assert_eq!(connack.reason_code, ReasonCode::Success);
        let assigned = connack.properties.get(PropertyId::AssignedClientIdentifier);
        assert!(
            assigned.is_some(),
            "[MQTT-3.2.2-16] CONNACK must include Assigned Client Identifier"
        );
        if let Some(mqtt5_protocol::PropertyValue::Utf8String(id)) = assigned {
            ids.push(id.clone());
        } else {
            panic!("Assigned Client Identifier has wrong type");
        }
    }

    assert_ne!(
        ids[0], ids[1],
        "[MQTT-3.2.2-16] Assigned Client Identifiers must be unique"
    );
}

/// `[MQTT-3.2.2-22]` If the Server receives a CONNECT packet containing a
/// non-zero Keep Alive and it does not support that value, the Server sets
/// Server Keep Alive to the value it supports.
///
/// Configures the broker with `server_keep_alive=30s`. A raw client connects
/// with keepalive=60s. The CONNACK must contain `ServerKeepAlive` property
/// equal to 30.
#[tokio::test]
async fn server_keep_alive_override() {
    let mut config = memory_config();
    config.server_keep_alive = Some(Duration::from_secs(30));
    let broker = ConformanceBroker::start_with_config(config).await;

    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("ska");
    raw.send_raw(&RawPacketBuilder::valid_connect(&client_id))
        .await
        .unwrap();

    let connack = raw
        .expect_connack_packet(TIMEOUT)
        .await
        .expect("Must receive CONNACK");

    assert_eq!(connack.reason_code, ReasonCode::Success);
    let ska = connack.properties.get(PropertyId::ServerKeepAlive);
    assert!(
        ska.is_some(),
        "[MQTT-3.2.2-22] CONNACK must include ServerKeepAlive when broker overrides keep alive"
    );
    if let Some(mqtt5_protocol::PropertyValue::TwoByteInteger(val)) = ska {
        assert_eq!(
            *val, 30,
            "[MQTT-3.2.2-22] ServerKeepAlive must be 30 (broker-configured value)"
        );
    } else {
        panic!("ServerKeepAlive property has wrong type");
    }
}

// ---------------------------------------------------------------------------
// Group 4: Will Rejection (raw client + custom config)
// ---------------------------------------------------------------------------

/// `[MQTT-3.2.2-12]` If the Server receives a CONNECT packet containing a
/// Will `QoS` that exceeds its capabilities, it MUST reject the connection.
///
/// Starts a broker with `maximum_qos=0`, sends a CONNECT with Will QoS=1,
/// and verifies rejection with reason code 0x9B (`QoSNotSupported`).
#[tokio::test]
async fn connack_will_qos_exceeds_maximum_rejected() {
    let config = memory_config().with_maximum_qos(0);
    let broker = ConformanceBroker::start_with_config(config).await;

    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("will-qos");
    raw.send_raw(&RawPacketBuilder::connect_with_will_qos(&client_id, 1))
        .await
        .unwrap();

    let connack = raw
        .expect_connack_packet(TIMEOUT)
        .await
        .expect("Must receive CONNACK");

    assert_eq!(
        connack.reason_code,
        ReasonCode::QoSNotSupported,
        "[MQTT-3.2.2-12] Will QoS exceeding maximum must be rejected with 0x9B"
    );
    assert!(
        !connack.session_present,
        "[MQTT-3.2.2-6] Session present must be 0 on error"
    );
}

/// `[MQTT-3.2.2-13]` If the Server receives a CONNECT packet containing a
/// Will Message with Will Retain set to 1, and it does not support retained
/// messages, the Server MUST reject the connection request.
///
/// Starts a broker with `retain_available=false`, sends a CONNECT with
/// Will Retain=1, and verifies rejection with 0x9A (`RetainNotSupported`).
#[tokio::test]
async fn connack_will_retain_rejected_when_unsupported() {
    let config = memory_config().with_retain_available(false);
    let broker = ConformanceBroker::start_with_config(config).await;

    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("will-ret");
    raw.send_raw(&RawPacketBuilder::connect_with_will_retain(&client_id))
        .await
        .unwrap();

    let connack = raw
        .expect_connack_packet(TIMEOUT)
        .await
        .expect("Must receive CONNACK");

    assert_eq!(
        connack.reason_code,
        ReasonCode::RetainNotSupported,
        "[MQTT-3.2.2-13] Will Retain on unsupported broker must be rejected with 0x9A"
    );
    assert!(
        !connack.session_present,
        "[MQTT-3.2.2-6] Session present must be 0 on error"
    );
}

// ---------------------------------------------------------------------------
// Group 5: Subscribe with Limited QoS (high-level client + custom config)
// ---------------------------------------------------------------------------

/// `[MQTT-3.2.2-9]` `[MQTT-3.2.2-10]` If a Server does not support `QoS` 1
/// or `QoS` 2, it MUST still accept SUBSCRIBE packets and downgrade the
/// granted `QoS`.
///
/// Starts a broker with `maximum_qos=0`, subscribes with `QoS` 1, and
/// verifies the subscription is accepted (downgraded, not rejected).
#[tokio::test]
async fn connack_accepts_subscribe_any_qos_with_limited_max() {
    let config = memory_config().with_maximum_qos(0);
    let broker = ConformanceBroker::start_with_config(config).await;

    let client = MqttClient::new(unique_client_id("sub-limited"));
    client
        .connect(broker.address())
        .await
        .expect("connect must succeed");

    let result = client.subscribe("test/limited", |_| {}).await;
    assert!(
        result.is_ok(),
        "[MQTT-3.2.2-9] Server must accept SUBSCRIBE even with limited MaximumQoS"
    );

    let opts = ConnectOptions::new(unique_client_id("sub-limited2")).with_clean_start(true);
    let client2 = MqttClient::with_options(opts.clone());
    Box::pin(client2.connect_with_options(broker.address(), opts))
        .await
        .expect("connect must succeed");

    let sub_opts = SubscribeOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    };
    let result2 = client2
        .subscribe_with_options("test/limited2", sub_opts, |_| {})
        .await;
    assert!(
        result2.is_ok(),
        "[MQTT-3.2.2-10] Server must accept QoS 2 SUBSCRIBE and downgrade"
    );

    client.disconnect().await.ok();
    client2.disconnect().await.ok();
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_connect_empty_client_id() -> Vec<u8> {
    use bytes::{BufMut, BytesMut};

    let mut body = BytesMut::new();
    body.put_u16(4);
    body.put_slice(b"MQTT");
    body.put_u8(5);
    body.put_u8(0x02);
    body.put_u16(60);
    body.put_u8(0);
    put_mqtt_string(&mut body, "");

    wrap_fixed_header(0x10, &body)
}

fn put_mqtt_string(buf: &mut bytes::BytesMut, s: &str) {
    use bytes::BufMut;
    let bytes = s.as_bytes();
    buf.put_u16(bytes.len() as u16);
    buf.put_slice(bytes);
}

fn encode_variable_int(buf: &mut bytes::BytesMut, mut value: u32) {
    use bytes::BufMut;
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value > 0 {
            byte |= 0x80;
        }
        buf.put_u8(byte);
        if value == 0 {
            break;
        }
    }
}

fn wrap_fixed_header(first_byte: u8, body: &[u8]) -> Vec<u8> {
    use bytes::{BufMut, BytesMut};
    let mut packet = BytesMut::new();
    packet.put_u8(first_byte);
    encode_variable_int(&mut packet, body.len() as u32);
    packet.put_slice(body);
    packet.to_vec()
}
