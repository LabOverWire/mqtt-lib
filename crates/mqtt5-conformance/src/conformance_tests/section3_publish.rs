//! Section 3.3 — PUBLISH packet conformance tests.

use crate::assertions;
use crate::conformance_test;
use crate::harness::unique_client_id;
use crate::raw_client::{RawMqttClient, RawPacketBuilder};
use crate::sut::SutHandle;
use crate::test_client::TestClient;
use mqtt5_protocol::types::{
    PublishOptions, PublishProperties, QoS, RetainHandling, SubscribeOptions,
};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

/// `[MQTT-3.3.1-4]` A PUBLISH packet MUST NOT have both `QoS` bits set to 1.
#[conformance_test(
    ids = ["MQTT-3.3.1-4"],
    requires = ["transport.tcp"],
)]
async fn publish_qos3_is_malformed(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("qos3");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::publish_qos3_malformed(
        "test/qos3",
        b"bad",
    ))
    .await
    .unwrap();

    assertions::expect_disconnect(&mut raw, "MQTT-3.3.1-4", TIMEOUT).await;
}

/// `[MQTT-3.3.1-2]` The DUP flag MUST be set to 0 for all `QoS` 0 messages.
#[conformance_test(
    ids = ["MQTT-3.3.1-2"],
    requires = ["transport.tcp"],
)]
async fn publish_dup_must_be_zero_for_qos0(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("dupq0");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::publish_dup_qos0("test/dup", b"bad"))
        .await
        .unwrap();

    assertions::expect_disconnect(&mut raw, "MQTT-3.3.1-2", TIMEOUT).await;
}

/// `[MQTT-3.3.2-2]` The Topic Name in the PUBLISH packet MUST NOT contain
/// wildcard characters.
#[conformance_test(
    ids = ["MQTT-3.3.2-2"],
    requires = ["transport.tcp"],
)]
async fn publish_topic_must_not_contain_wildcards(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("wild");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::publish_with_wildcard_topic())
        .await
        .unwrap();

    assertions::expect_disconnect(&mut raw, "MQTT-3.3.2-2", TIMEOUT).await;
}

/// `[MQTT-3.3.2-1]` The Topic Name MUST be present as the first field in the
/// PUBLISH packet Variable Header.
#[conformance_test(
    ids = ["MQTT-3.3.2-1"],
    requires = ["transport.tcp"],
)]
async fn publish_topic_must_not_be_empty_without_alias(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("empty");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::publish_with_empty_topic())
        .await
        .unwrap();

    assertions::expect_disconnect(&mut raw, "MQTT-3.3.2-1", TIMEOUT).await;
}

/// `[MQTT-3.3.2-7]` A Topic Alias of 0 is not permitted.
#[conformance_test(
    ids = ["MQTT-3.3.2-7"],
    requires = ["transport.tcp"],
)]
async fn publish_topic_alias_zero_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("alias0");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::publish_with_topic_alias_zero(
        "test/alias",
        b"bad",
    ))
    .await
    .unwrap();

    assertions::expect_disconnect(&mut raw, "MQTT-3.3.2-7", TIMEOUT).await;
}

/// `[MQTT-3.3.4-6]` A PUBLISH packet sent from a Client to a Server MUST NOT
/// contain a Subscription Identifier.
#[conformance_test(
    ids = ["MQTT-3.3.4-6"],
    requires = ["transport.tcp"],
)]
async fn publish_subscription_id_from_client_rejected(sut: SutHandle) {
    let mut raw = RawMqttClient::connect_tcp(sut.expect_tcp_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("subid");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::publish_with_subscription_id(
        "test/subid",
        b"bad",
        42,
    ))
    .await
    .unwrap();

    assertions::expect_disconnect(&mut raw, "MQTT-3.3.4-6", TIMEOUT).await;
}

/// `[MQTT-3.3.1-5]` If the RETAIN flag is set to 1, the Server MUST store
/// the Application Message and replace any existing retained message.
#[conformance_test(
    ids = ["MQTT-3.3.1-5"],
    requires = ["transport.tcp", "retain_available"],
)]
async fn publish_retain_stores_message(sut: SutHandle) {
    let topic = format!("retain/{}", unique_client_id("store"));

    let publisher = TestClient::connect_with_prefix(&sut, "ret-pub")
        .await
        .unwrap();
    let opts = PublishOptions {
        retain: true,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"retained-payload", opts)
        .await
        .expect("publish failed");
    tokio::time::sleep(Duration::from_millis(200)).await;
    publisher.disconnect().await.expect("disconnect failed");

    let subscriber = TestClient::connect_with_prefix(&sut, "ret-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");

    assert!(
        subscription.wait_for_messages(1, TIMEOUT).await,
        "[MQTT-3.3.1-5] New subscriber must receive retained message"
    );
    let msgs = subscription.snapshot();
    assert_eq!(msgs[0].payload, b"retained-payload");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.1-6]` `[MQTT-3.3.1-7]` A retained message with empty payload
/// removes any existing retained message for that topic and MUST NOT be stored.
#[conformance_test(
    ids = ["MQTT-3.3.1-6", "MQTT-3.3.1-7"],
    requires = ["transport.tcp", "retain_available"],
)]
async fn publish_retain_empty_payload_clears(sut: SutHandle) {
    let topic = format!("retain/{}", unique_client_id("clear"));

    let publisher = TestClient::connect_with_prefix(&sut, "clr-pub")
        .await
        .unwrap();
    let retain_opts = PublishOptions {
        retain: true,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"will-be-cleared", retain_opts.clone())
        .await
        .expect("publish failed");
    tokio::time::sleep(Duration::from_millis(200)).await;

    publisher
        .publish_with_options(&topic, b"", retain_opts)
        .await
        .expect("clear publish failed");
    tokio::time::sleep(Duration::from_millis(200)).await;
    publisher.disconnect().await.expect("disconnect failed");

    let subscriber = TestClient::connect_with_prefix(&sut, "clr-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        subscription.count(),
        0,
        "[MQTT-3.3.1-6/7] Retained message must be cleared by empty payload publish"
    );
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.1-8]` If the RETAIN flag is 0, the Server MUST NOT store the
/// message as a retained message and MUST NOT remove or replace any existing
/// retained message.
#[conformance_test(
    ids = ["MQTT-3.3.1-8"],
    requires = ["transport.tcp", "retain_available"],
)]
async fn publish_no_retain_does_not_store(sut: SutHandle) {
    let topic = format!("retain/{}", unique_client_id("noret"));

    let publisher = TestClient::connect_with_prefix(&sut, "nr-pub")
        .await
        .unwrap();
    let retain_opts = PublishOptions {
        retain: true,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"original-retained", retain_opts)
        .await
        .expect("retained publish failed");
    tokio::time::sleep(Duration::from_millis(200)).await;

    publisher
        .publish(&topic, b"non-retained-update")
        .await
        .expect("non-retained publish failed");
    tokio::time::sleep(Duration::from_millis(200)).await;
    publisher.disconnect().await.expect("disconnect failed");

    let subscriber = TestClient::connect_with_prefix(&sut, "nr-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");

    assert!(
        subscription.wait_for_messages(1, TIMEOUT).await,
        "[MQTT-3.3.1-8] Original retained message must still be delivered"
    );
    let msgs = subscription.snapshot();
    assert_eq!(
        msgs[0].payload, b"original-retained",
        "[MQTT-3.3.1-8] Non-retained publish must not replace retained message"
    );
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.1-9]` Retain Handling 0 — the Server MUST send retained
/// messages matching the Topic Filter of the subscription.
#[conformance_test(
    ids = ["MQTT-3.3.1-9"],
    requires = ["transport.tcp", "retain_available"],
)]
async fn publish_retain_handling_zero_sends_retained(sut: SutHandle) {
    let topic = format!("rh0/{}", unique_client_id("rh"));

    let publisher = TestClient::connect_with_prefix(&sut, "rh0-pub")
        .await
        .unwrap();
    let retain_opts = PublishOptions {
        retain: true,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"retained-rh0", retain_opts)
        .await
        .expect("publish failed");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let subscriber = TestClient::connect_with_prefix(&sut, "rh0-sub")
        .await
        .unwrap();
    let sub_opts = SubscribeOptions {
        retain_handling: RetainHandling::SendAtSubscribe,
        ..Default::default()
    };
    let subscription = subscriber
        .subscribe(&topic, sub_opts)
        .await
        .expect("subscribe failed");

    assert!(
        subscription.wait_for_messages(1, TIMEOUT).await,
        "[MQTT-3.3.1-9] RetainHandling=0 must deliver retained message"
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.1-10]` Retain Handling 1 — send retained messages only for new
/// subscriptions, not for re-subscriptions.
#[conformance_test(
    ids = ["MQTT-3.3.1-10"],
    requires = ["transport.tcp", "retain_available"],
)]
async fn publish_retain_handling_one_only_new_subs(sut: SutHandle) {
    let topic = format!("rh1/{}", unique_client_id("rh"));

    let publisher = TestClient::connect_with_prefix(&sut, "rh1-pub")
        .await
        .unwrap();
    let retain_opts = PublishOptions {
        retain: true,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"retained-rh1", retain_opts)
        .await
        .expect("publish failed");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let sub_opts = SubscribeOptions {
        retain_handling: RetainHandling::SendIfNew,
        ..Default::default()
    };
    let subscriber = TestClient::connect_with_prefix(&sut, "rh1-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&topic, sub_opts.clone())
        .await
        .expect("first subscribe failed");

    assert!(
        subscription.wait_for_messages(1, TIMEOUT).await,
        "[MQTT-3.3.1-10] First subscription must receive retained message"
    );
    subscription.clear();

    let subscription2 = subscriber
        .subscribe(&topic, sub_opts)
        .await
        .expect("re-subscribe failed");
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        subscription2.count(),
        0,
        "[MQTT-3.3.1-10] Re-subscription must NOT receive retained message"
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.1-11]` Retain Handling 2 — the Server MUST NOT send retained
/// messages.
#[conformance_test(
    ids = ["MQTT-3.3.1-11"],
    requires = ["transport.tcp", "retain_available"],
)]
async fn publish_retain_handling_two_no_retained(sut: SutHandle) {
    let topic = format!("rh2/{}", unique_client_id("rh"));

    let publisher = TestClient::connect_with_prefix(&sut, "rh2-pub")
        .await
        .unwrap();
    let retain_opts = PublishOptions {
        retain: true,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"retained-rh2", retain_opts)
        .await
        .expect("publish failed");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let subscriber = TestClient::connect_with_prefix(&sut, "rh2-sub")
        .await
        .unwrap();
    let sub_opts = SubscribeOptions {
        retain_handling: RetainHandling::DontSend,
        ..Default::default()
    };
    let subscription = subscriber
        .subscribe(&topic, sub_opts)
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        subscription.count(),
        0,
        "[MQTT-3.3.1-11] RetainHandling=2 must NOT deliver retained messages"
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.1-12]` If `retain_as_published` is 0, the Server MUST set the
/// RETAIN flag to 0 when forwarding.
#[conformance_test(
    ids = ["MQTT-3.3.1-12"],
    requires = ["transport.tcp", "retain_available"],
)]
async fn publish_retain_as_published_zero_clears_flag(sut: SutHandle) {
    let topic = format!("rap0/{}", unique_client_id("rap"));

    let subscriber = TestClient::connect_with_prefix(&sut, "rap0-sub")
        .await
        .unwrap();
    let sub_opts = SubscribeOptions {
        retain_as_published: false,
        ..Default::default()
    };
    let subscription = subscriber
        .subscribe(&topic, sub_opts)
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "rap0-pub")
        .await
        .unwrap();
    let pub_opts = PublishOptions {
        retain: true,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"rap0-payload", pub_opts)
        .await
        .expect("publish failed");

    assert!(
        subscription.wait_for_messages(1, TIMEOUT).await,
        "Subscriber must receive message"
    );
    let msgs = subscription.snapshot();
    assert!(
        !msgs[0].retain,
        "[MQTT-3.3.1-12] retain_as_published=0 must clear RETAIN flag on delivery"
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.1-13]` If `retain_as_published` is 1, the Server MUST set the
/// RETAIN flag equal to the received PUBLISH packet's RETAIN flag.
#[conformance_test(
    ids = ["MQTT-3.3.1-13"],
    requires = ["transport.tcp", "retain_available"],
)]
async fn publish_retain_as_published_one_preserves_flag(sut: SutHandle) {
    let topic = format!("rap1/{}", unique_client_id("rap"));

    let subscriber = TestClient::connect_with_prefix(&sut, "rap1-sub")
        .await
        .unwrap();
    let sub_opts = SubscribeOptions {
        retain_as_published: true,
        ..Default::default()
    };
    let subscription = subscriber
        .subscribe(&topic, sub_opts)
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "rap1-pub")
        .await
        .unwrap();
    let pub_opts = PublishOptions {
        retain: true,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"rap1-payload", pub_opts)
        .await
        .expect("publish failed");

    assert!(
        subscription.wait_for_messages(1, TIMEOUT).await,
        "Subscriber must receive message"
    );
    let msgs = subscription.snapshot();
    assert!(
        msgs[0].retain,
        "[MQTT-3.3.1-13] retain_as_published=1 must preserve RETAIN flag on delivery"
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.4-1]` `QoS` 0 message delivered to subscriber.
#[conformance_test(
    ids = ["MQTT-3.3.4-1"],
    requires = ["transport.tcp"],
)]
async fn publish_qos0_delivery(sut: SutHandle) {
    let topic = format!("qos0/{}", unique_client_id("del"));

    let subscriber = TestClient::connect_with_prefix(&sut, "q0-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "q0-pub")
        .await
        .unwrap();
    publisher
        .publish(&topic, b"qos0-msg")
        .await
        .expect("publish failed");

    assert!(
        subscription.wait_for_messages(1, TIMEOUT).await,
        "[MQTT-3.3.4-1] QoS 0 message must be delivered to subscriber"
    );
    let msgs = subscription.snapshot();
    assert_eq!(msgs[0].payload, b"qos0-msg");
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.4-1]` `QoS` 1 → broker sends PUBACK, subscriber receives
/// message.
#[conformance_test(
    ids = ["MQTT-3.3.4-1"],
    requires = ["transport.tcp", "max_qos>=1"],
)]
async fn publish_qos1_puback_response(sut: SutHandle) {
    let topic = format!("qos1/{}", unique_client_id("ack"));

    let sub_opts = SubscribeOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    let subscriber = TestClient::connect_with_prefix(&sut, "q1-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&topic, sub_opts)
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "q1-pub")
        .await
        .unwrap();
    let pub_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"qos1-msg", pub_opts)
        .await
        .expect("publish failed");

    assert!(
        subscription.wait_for_messages(1, TIMEOUT).await,
        "[MQTT-3.3.4-1] QoS 1 message must be delivered to subscriber"
    );
    let msgs = subscription.snapshot();
    assert_eq!(msgs[0].payload, b"qos1-msg");
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.4-1]` `QoS` 2 → full PUBREC/PUBREL/PUBCOMP exchange and
/// delivery.
#[conformance_test(
    ids = ["MQTT-3.3.4-1"],
    requires = ["transport.tcp", "max_qos>=2"],
)]
async fn publish_qos2_full_flow(sut: SutHandle) {
    let topic = format!("qos2/{}", unique_client_id("flow"));

    let sub_opts = SubscribeOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    };
    let subscriber = TestClient::connect_with_prefix(&sut, "q2-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&topic, sub_opts)
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "q2-pub")
        .await
        .unwrap();
    let pub_opts = PublishOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"qos2-msg", pub_opts)
        .await
        .expect("publish failed");

    assert!(
        subscription
            .wait_for_messages(1, Duration::from_secs(5))
            .await,
        "[MQTT-3.3.4-1] QoS 2 message must be delivered to subscriber"
    );
    let msgs = subscription.snapshot();
    assert_eq!(msgs[0].payload, b"qos2-msg");
    assert_eq!(msgs.len(), 1, "QoS 2 must deliver exactly once");
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.2-4]` The Server MUST send the Payload Format Indicator
/// unaltered to all subscribers.
#[conformance_test(
    ids = ["MQTT-3.3.2-4"],
    requires = ["transport.tcp"],
)]
async fn publish_payload_format_indicator_forwarded(sut: SutHandle) {
    let topic = format!("pfi/{}", unique_client_id("fwd"));

    let subscriber = TestClient::connect_with_prefix(&sut, "pfi-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "pfi-pub")
        .await
        .unwrap();
    let pub_opts = PublishOptions {
        properties: PublishProperties {
            payload_format_indicator: Some(true),
            ..Default::default()
        },
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"utf8-text", pub_opts)
        .await
        .expect("publish failed");

    assert!(
        subscription.wait_for_messages(1, TIMEOUT).await,
        "Subscriber must receive message"
    );
    let msgs = subscription.snapshot();
    assert_eq!(
        msgs[0].payload_format_indicator,
        Some(true),
        "[MQTT-3.3.2-4] Payload Format Indicator must be forwarded unaltered"
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.2-20]` The Server MUST send the Content Type unaltered to all
/// subscribers.
#[conformance_test(
    ids = ["MQTT-3.3.2-20"],
    requires = ["transport.tcp"],
)]
async fn publish_content_type_forwarded(sut: SutHandle) {
    let topic = format!("ct/{}", unique_client_id("fwd"));

    let subscriber = TestClient::connect_with_prefix(&sut, "ct-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "ct-pub")
        .await
        .unwrap();
    let pub_opts = PublishOptions {
        properties: PublishProperties {
            content_type: Some("application/json".to_owned()),
            ..Default::default()
        },
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"{}", pub_opts)
        .await
        .expect("publish failed");

    assert!(
        subscription.wait_for_messages(1, TIMEOUT).await,
        "Subscriber must receive message"
    );
    let msgs = subscription.snapshot();
    assert_eq!(
        msgs[0].content_type.as_deref(),
        Some("application/json"),
        "[MQTT-3.3.2-20] Content Type must be forwarded unaltered"
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.2-15]` `[MQTT-3.3.2-16]` The Server MUST send the Response
/// Topic and Correlation Data unaltered.
#[conformance_test(
    ids = ["MQTT-3.3.2-15", "MQTT-3.3.2-16"],
    requires = ["transport.tcp"],
)]
async fn publish_response_topic_and_correlation_data_forwarded(sut: SutHandle) {
    let topic = format!("rt/{}", unique_client_id("fwd"));

    let subscriber = TestClient::connect_with_prefix(&sut, "rt-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "rt-pub")
        .await
        .unwrap();
    let pub_opts = PublishOptions {
        properties: PublishProperties {
            response_topic: Some("reply/topic".to_owned()),
            correlation_data: Some(vec![0xDE, 0xAD, 0xBE, 0xEF]),
            ..Default::default()
        },
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"request", pub_opts)
        .await
        .expect("publish failed");

    assert!(
        subscription.wait_for_messages(1, TIMEOUT).await,
        "Subscriber must receive message"
    );
    let msgs = subscription.snapshot();
    assert_eq!(
        msgs[0].response_topic.as_deref(),
        Some("reply/topic"),
        "[MQTT-3.3.2-15] Response Topic must be forwarded unaltered"
    );
    assert_eq!(
        msgs[0].correlation_data.as_deref(),
        Some(&[0xDE, 0xAD, 0xBE, 0xEF][..]),
        "[MQTT-3.3.2-16] Correlation Data must be forwarded unaltered"
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.2-17]` `[MQTT-3.3.2-18]` The Server MUST send all User
/// Properties unaltered and maintain their order.
#[conformance_test(
    ids = ["MQTT-3.3.2-17", "MQTT-3.3.2-18"],
    requires = ["transport.tcp"],
)]
async fn publish_user_properties_forwarded_and_ordered(sut: SutHandle) {
    let topic = format!("up/{}", unique_client_id("fwd"));

    let subscriber = TestClient::connect_with_prefix(&sut, "up-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&topic, SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let sent_props = vec![
        ("key-a".to_owned(), "value-1".to_owned()),
        ("key-b".to_owned(), "value-2".to_owned()),
        ("key-a".to_owned(), "value-3".to_owned()),
    ];
    let publisher = TestClient::connect_with_prefix(&sut, "up-pub")
        .await
        .unwrap();
    let pub_opts = PublishOptions {
        properties: PublishProperties {
            user_properties: sent_props.clone(),
            ..Default::default()
        },
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"props", pub_opts)
        .await
        .expect("publish failed");

    assert!(
        subscription.wait_for_messages(1, TIMEOUT).await,
        "Subscriber must receive message"
    );
    let msgs = subscription.snapshot();
    assertions::expect_user_properties_subset(
        &msgs[0].user_properties,
        &sent_props,
        &["x-mqtt-sender", "x-mqtt-client-id"],
        "MQTT-3.3.2-17/18",
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.2-3]` The Topic Name sent to subscribing Clients MUST match the
/// Subscription's Topic Filter.
#[conformance_test(
    ids = ["MQTT-3.3.2-3"],
    requires = ["transport.tcp", "wildcard_subscription_available"],
)]
async fn publish_topic_matches_subscription_filter(sut: SutHandle) {
    let prefix = unique_client_id("match");
    let filter = format!("tm/{prefix}/+");
    let publish_topic = format!("tm/{prefix}/sensor");

    let subscriber = TestClient::connect_with_prefix(&sut, "tm-sub")
        .await
        .unwrap();
    let subscription = subscriber
        .subscribe(&filter, SubscribeOptions::default())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = TestClient::connect_with_prefix(&sut, "tm-pub")
        .await
        .unwrap();
    publisher
        .publish(&publish_topic, b"match-test")
        .await
        .expect("publish failed");

    assert!(
        subscription.wait_for_messages(1, TIMEOUT).await,
        "[MQTT-3.3.2-3] Wildcard subscription must match published topic"
    );
    let msgs = subscription.snapshot();
    assert_eq!(
        msgs[0].topic, publish_topic,
        "[MQTT-3.3.2-3] Delivered topic name must be the published topic, not the filter"
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}
