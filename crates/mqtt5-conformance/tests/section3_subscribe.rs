use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5_conformance::harness::unique_client_id;
use mqtt5_conformance::raw_client::{RawMqttClient, RawPacketBuilder};
use mqtt5_conformance::sut::{inprocess_sut, inprocess_sut_with_config};
use mqtt5_conformance::test_client::TestClient;
use mqtt5_protocol::types::{QoS, SubscribeOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::time::Duration;
use tempfile::NamedTempFile;

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
async fn suback_reason_codes_ordering() {
    let mut acl_file = NamedTempFile::new().unwrap();
    writeln!(acl_file, "user * topic allowed/# permission readwrite").unwrap();
    acl_file.flush().unwrap();

    let config = memory_config().with_auth(
        mqtt5::broker::config::AuthConfig::new().with_acl_file(acl_file.path().to_path_buf()),
    );
    let sut = inprocess_sut_with_config(config).await;

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("suback-order");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    let filters = [("allowed/a", 0u8), ("denied/b", 1), ("allowed/c", 2)];
    raw.send_raw(&RawPacketBuilder::subscribe_multiple(&filters, 5))
        .await
        .unwrap();

    let (ack_id, reason_codes) = raw
        .expect_suback(TIMEOUT)
        .await
        .expect("expected SUBACK from broker");

    assert_eq!(ack_id, 5);
    assert_eq!(reason_codes.len(), 3, "one reason code per filter");
    assert!(
        reason_codes[0] <= 0x02,
        "first filter (allowed) should succeed, got 0x{:02X}",
        reason_codes[0]
    );
    assert_eq!(
        reason_codes[1], 0x87,
        "second filter (denied) should be NotAuthorized (0x87)"
    );
    assert!(
        reason_codes[2] <= 0x02,
        "third filter (allowed) should succeed, got 0x{:02X}",
        reason_codes[2]
    );
}

#[tokio::test]
async fn suback_downgrades_to_max_qos() {
    let config = memory_config().with_maximum_qos(1);
    let sut = inprocess_sut_with_config(config).await;

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("suback-downgrade");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "test/downgrade",
        2,
        1,
    ))
    .await
    .unwrap();

    let (_, reason_codes) = raw
        .expect_suback(TIMEOUT)
        .await
        .expect("expected SUBACK from broker");

    assert_eq!(
        reason_codes[0], 0x01,
        "SUBACK should downgrade QoS 2 to QoS 1, got 0x{:02X}",
        reason_codes[0]
    );
}

#[tokio::test]
async fn suback_message_delivery_at_granted_qos() {
    let sut = inprocess_sut().await;
    let subscriber = TestClient::connect_with_prefix(&sut, "sub-deliver")
        .await
        .unwrap();
    let opts = SubscribeOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    let subscription = subscriber
        .subscribe("test/sub-deliver", opts)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "sub-deliver-pub")
        .await
        .unwrap();
    publisher
        .publish("test/sub-deliver", b"delivered")
        .await
        .unwrap();

    assert!(
        subscription.wait_for_messages(1, TIMEOUT).await,
        "subscriber should receive message at granted QoS"
    );
    let msgs = subscription.snapshot();
    assert_eq!(msgs[0].payload, b"delivered");
}

#[tokio::test]
async fn suback_not_authorized() {
    let mut acl_file = NamedTempFile::new().unwrap();
    writeln!(acl_file, "user * topic public/# permission readwrite").unwrap();
    acl_file.flush().unwrap();

    let config = memory_config().with_auth(
        mqtt5::broker::config::AuthConfig::new().with_acl_file(acl_file.path().to_path_buf()),
    );
    let sut = inprocess_sut_with_config(config).await;

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("sub-noauth");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "secret/topic",
        0,
        1,
    ))
    .await
    .unwrap();

    let (_, reason_codes) = raw
        .expect_suback(TIMEOUT)
        .await
        .expect("expected SUBACK from broker");

    assert_eq!(
        reason_codes[0], 0x87,
        "SUBACK reason code should be NotAuthorized (0x87), got 0x{:02X}",
        reason_codes[0]
    );
}

#[tokio::test]
async fn suback_quota_exceeded() {
    let config = memory_config().with_max_subscriptions_per_client(2);
    let sut = inprocess_sut_with_config(config).await;

    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("sub-quota");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "test/quota/a",
        0,
        1,
    ))
    .await
    .unwrap();
    let (_, rc1) = raw.expect_suback(TIMEOUT).await.expect("SUBACK 1");
    assert!(rc1[0] <= 0x02, "first subscribe should succeed");

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "test/quota/b",
        0,
        2,
    ))
    .await
    .unwrap();
    let (_, rc2) = raw.expect_suback(TIMEOUT).await.expect("SUBACK 2");
    assert!(rc2[0] <= 0x02, "second subscribe should succeed");

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "test/quota/c",
        0,
        3,
    ))
    .await
    .unwrap();
    let (_, rc3) = raw.expect_suback(TIMEOUT).await.expect("SUBACK 3");
    assert_eq!(
        rc3[0], 0x97,
        "third subscribe should be QuotaExceeded (0x97), got 0x{:02X}",
        rc3[0]
    );
}
