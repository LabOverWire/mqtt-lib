use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5_conformance::harness::unique_client_id;
use mqtt5_conformance::raw_client::{RawMqttClient, RawPacketBuilder};
use mqtt5_conformance::sut::{inprocess_sut, inprocess_sut_with_config};
use mqtt5_conformance::test_client::TestClient;
use mqtt5_protocol::protocol::v5::properties::PropertyId;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use mqtt5_protocol::types::{QoS, SubscribeOptions};
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

#[tokio::test]
async fn connack_properties_server_capabilities() {
    let sut = inprocess_sut().await;
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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

#[tokio::test]
async fn connack_maximum_qos_advertised() {
    let config = memory_config().with_maximum_qos(1);
    let sut = inprocess_sut_with_config(config).await;

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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

#[tokio::test]
async fn server_keep_alive_override() {
    let mut config = memory_config();
    config.server_keep_alive = Some(Duration::from_secs(30));
    let sut = inprocess_sut_with_config(config).await;

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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

#[tokio::test]
async fn connack_will_qos_exceeds_maximum_rejected() {
    let config = memory_config().with_maximum_qos(0);
    let sut = inprocess_sut_with_config(config).await;

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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

#[tokio::test]
async fn connack_will_retain_rejected_when_unsupported() {
    let config = memory_config().with_retain_available(false);
    let sut = inprocess_sut_with_config(config).await;

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
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

#[tokio::test]
async fn connack_accepts_subscribe_any_qos_with_limited_max() {
    let config = memory_config().with_maximum_qos(0);
    let sut = inprocess_sut_with_config(config).await;

    let client = TestClient::connect_with_prefix(&sut, "sub-limited")
        .await
        .expect("connect must succeed");
    let result = client
        .subscribe("test/limited", SubscribeOptions::default())
        .await;
    assert!(
        result.is_ok(),
        "[MQTT-3.2.2-9] Server must accept SUBSCRIBE even with limited MaximumQoS"
    );

    let client2 = TestClient::connect_with_prefix(&sut, "sub-limited2")
        .await
        .expect("connect must succeed");
    let sub_opts = SubscribeOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    };
    let result2 = client2.subscribe("test/limited2", sub_opts).await;
    assert!(
        result2.is_ok(),
        "[MQTT-3.2.2-10] Server must accept QoS 2 SUBSCRIBE and downgrade"
    );

    client.disconnect().await.ok();
    client2.disconnect().await.ok();
}
