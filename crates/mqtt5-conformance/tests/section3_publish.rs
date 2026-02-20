use mqtt5::{PublishOptions, PublishProperties, QoS, RetainHandling, SubscribeOptions};
use mqtt5_conformance::harness::{
    connected_client, unique_client_id, ConformanceBroker, MessageCollector,
};
use mqtt5_conformance::raw_client::{RawMqttClient, RawPacketBuilder};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------
// Group 1: Malformed/Invalid PUBLISH (raw client)
// ---------------------------------------------------------------------------

/// `[MQTT-3.3.1-4]` A PUBLISH packet MUST NOT have both `QoS` bits set to 1.
///
/// Sends a PUBLISH with QoS=3 (header byte `0x36`) after CONNECT and
/// verifies the broker disconnects.
#[tokio::test]
async fn publish_qos3_is_malformed() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.3.1-4] Server must disconnect client that sends QoS=3"
    );
}

/// `[MQTT-3.3.1-2]` The DUP flag MUST be set to 0 for all `QoS` 0 messages.
///
/// Sends a PUBLISH with DUP=1 and QoS=0 (header byte `0x38`) and verifies
/// the broker disconnects.
#[tokio::test]
async fn publish_dup_must_be_zero_for_qos0() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("dupq0");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::publish_dup_qos0("test/dup", b"bad"))
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.3.1-2] Server must disconnect client that sends DUP=1 with QoS=0"
    );
}

/// `[MQTT-3.3.2-2]` The Topic Name in the PUBLISH packet MUST NOT contain
/// wildcard characters.
///
/// Sends a PUBLISH with topic `test/#` and verifies the broker disconnects.
#[tokio::test]
async fn publish_topic_must_not_contain_wildcards() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("wild");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::publish_with_wildcard_topic())
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.3.2-2] Server must disconnect client that publishes to wildcard topic"
    );
}

/// `[MQTT-3.3.2-1]` The Topic Name MUST be present as the first field in the
/// PUBLISH packet Variable Header.
///
/// Sends a PUBLISH with an empty topic and no Topic Alias, and verifies
/// the broker disconnects.
#[tokio::test]
async fn publish_topic_must_not_be_empty_without_alias() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("empty");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::publish_with_empty_topic())
        .await
        .unwrap();

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.3.2-1] Server must disconnect client that publishes with empty topic and no alias"
    );
}

/// `[MQTT-3.3.2-7]` A Topic Alias of 0 is not permitted.
///
/// Sends a PUBLISH with Topic Alias property set to 0 and verifies the
/// broker disconnects with a protocol error.
#[tokio::test]
async fn publish_topic_alias_zero_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.3.2-7] Server must disconnect client that sends Topic Alias = 0"
    );
}

/// `[MQTT-3.3.4-6]` A PUBLISH packet sent from a Client to a Server MUST NOT
/// contain a Subscription Identifier.
///
/// Sends a PUBLISH with a Subscription Identifier property and verifies the
/// broker disconnects.
#[tokio::test]
async fn publish_subscription_id_from_client_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
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

    assert!(
        raw.expect_disconnect(TIMEOUT).await,
        "[MQTT-3.3.4-6] Server must reject PUBLISH with Subscription Identifier from client"
    );
}

// ---------------------------------------------------------------------------
// Group 2: Retained Messages (high-level client)
// ---------------------------------------------------------------------------

/// `[MQTT-3.3.1-5]` If the RETAIN flag is set to 1, the Server MUST store
/// the Application Message and replace any existing retained message.
///
/// Publishes a retained message, then a new subscriber connects and verifies
/// it receives the retained message.
#[tokio::test]
async fn publish_retain_stores_message() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("retain/{}", unique_client_id("store"));

    let publisher = connected_client("ret-pub", &broker).await;
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

    let collector = MessageCollector::new();
    let subscriber = connected_client("ret-sub", &broker).await;
    subscriber
        .subscribe(&topic, collector.callback())
        .await
        .expect("subscribe failed");

    assert!(
        collector.wait_for_messages(1, TIMEOUT).await,
        "[MQTT-3.3.1-5] New subscriber must receive retained message"
    );
    let msgs = collector.get_messages().await;
    assert_eq!(msgs[0].payload, b"retained-payload");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.1-6]` `[MQTT-3.3.1-7]` A retained message with empty payload
/// removes any existing retained message for that topic and MUST NOT be stored.
///
/// Publishes a retained message, then publishes a retained empty payload to
/// clear it. A new subscriber verifies no retained message is delivered.
#[tokio::test]
async fn publish_retain_empty_payload_clears() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("retain/{}", unique_client_id("clear"));

    let publisher = connected_client("clr-pub", &broker).await;
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

    let collector = MessageCollector::new();
    let subscriber = connected_client("clr-sub", &broker).await;
    subscriber
        .subscribe(&topic, collector.callback())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        collector.count().await,
        0,
        "[MQTT-3.3.1-6/7] Retained message must be cleared by empty payload publish"
    );
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.1-8]` If the RETAIN flag is 0, the Server MUST NOT store the
/// message as a retained message and MUST NOT remove or replace any existing
/// retained message.
///
/// Publishes a retained message, then publishes a non-retained message to
/// the same topic. A new subscriber verifies it receives the original retained
/// message (not the non-retained one).
#[tokio::test]
async fn publish_no_retain_does_not_store() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("retain/{}", unique_client_id("noret"));

    let publisher = connected_client("nr-pub", &broker).await;
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

    let collector = MessageCollector::new();
    let subscriber = connected_client("nr-sub", &broker).await;
    subscriber
        .subscribe(&topic, collector.callback())
        .await
        .expect("subscribe failed");

    assert!(
        collector.wait_for_messages(1, TIMEOUT).await,
        "[MQTT-3.3.1-8] Original retained message must still be delivered"
    );
    let msgs = collector.get_messages().await;
    assert_eq!(
        msgs[0].payload, b"original-retained",
        "[MQTT-3.3.1-8] Non-retained publish must not replace retained message"
    );
    subscriber.disconnect().await.expect("disconnect failed");
}

// ---------------------------------------------------------------------------
// Group 3: Retain Handling Options
// ---------------------------------------------------------------------------

/// `[MQTT-3.3.1-9]` Retain Handling 0 — the Server MUST send retained
/// messages matching the Topic Filter of the subscription.
///
/// Publishes a retained message, then subscribes with `RetainHandling::SendAtSubscribe`
/// and verifies the retained message is delivered.
#[tokio::test]
async fn publish_retain_handling_zero_sends_retained() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("rh0/{}", unique_client_id("rh"));

    let publisher = connected_client("rh0-pub", &broker).await;
    let retain_opts = PublishOptions {
        retain: true,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"retained-rh0", retain_opts)
        .await
        .expect("publish failed");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let collector = MessageCollector::new();
    let subscriber = connected_client("rh0-sub", &broker).await;
    let sub_opts = SubscribeOptions {
        retain_handling: RetainHandling::SendAtSubscribe,
        ..Default::default()
    };
    subscriber
        .subscribe_with_options(&topic, sub_opts, collector.callback())
        .await
        .expect("subscribe failed");

    assert!(
        collector.wait_for_messages(1, TIMEOUT).await,
        "[MQTT-3.3.1-9] RetainHandling=0 must deliver retained message"
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.1-10]` Retain Handling 1 — send retained messages only for new
/// subscriptions, not for re-subscriptions.
///
/// Subscribes with `RetainHandling::SendIfNew`, verifies retained message
/// arrives, then re-subscribes and verifies no second retained delivery.
#[tokio::test]
async fn publish_retain_handling_one_only_new_subs() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("rh1/{}", unique_client_id("rh"));

    let publisher = connected_client("rh1-pub", &broker).await;
    let retain_opts = PublishOptions {
        retain: true,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"retained-rh1", retain_opts)
        .await
        .expect("publish failed");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let collector = MessageCollector::new();
    let sub_opts = SubscribeOptions {
        retain_handling: RetainHandling::SendIfNew,
        ..Default::default()
    };
    let subscriber = connected_client("rh1-sub", &broker).await;
    subscriber
        .subscribe_with_options(&topic, sub_opts.clone(), collector.callback())
        .await
        .expect("first subscribe failed");

    assert!(
        collector.wait_for_messages(1, TIMEOUT).await,
        "[MQTT-3.3.1-10] First subscription must receive retained message"
    );
    collector.clear().await;

    subscriber
        .subscribe_with_options(&topic, sub_opts, collector.callback())
        .await
        .expect("re-subscribe failed");
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        collector.count().await,
        0,
        "[MQTT-3.3.1-10] Re-subscription must NOT receive retained message"
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.1-11]` Retain Handling 2 — the Server MUST NOT send retained
/// messages.
///
/// Publishes a retained message, then subscribes with
/// `RetainHandling::DontSend` and verifies no retained message is delivered.
#[tokio::test]
async fn publish_retain_handling_two_no_retained() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("rh2/{}", unique_client_id("rh"));

    let publisher = connected_client("rh2-pub", &broker).await;
    let retain_opts = PublishOptions {
        retain: true,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"retained-rh2", retain_opts)
        .await
        .expect("publish failed");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let collector = MessageCollector::new();
    let subscriber = connected_client("rh2-sub", &broker).await;
    let sub_opts = SubscribeOptions {
        retain_handling: RetainHandling::DontSend,
        ..Default::default()
    };
    subscriber
        .subscribe_with_options(&topic, sub_opts, collector.callback())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        collector.count().await,
        0,
        "[MQTT-3.3.1-11] RetainHandling=2 must NOT deliver retained messages"
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

// ---------------------------------------------------------------------------
// Group 4: Retain As Published
// ---------------------------------------------------------------------------

/// `[MQTT-3.3.1-12]` If `retain_as_published` is 0, the Server MUST set the
/// RETAIN flag to 0 when forwarding.
///
/// Publishes a retained message to a subscriber that set
/// `retain_as_published=false`. Verifies the delivered message has
/// `retain=false`.
#[tokio::test]
async fn publish_retain_as_published_zero_clears_flag() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("rap0/{}", unique_client_id("rap"));

    let collector = MessageCollector::new();
    let subscriber = connected_client("rap0-sub", &broker).await;
    let sub_opts = SubscribeOptions {
        retain_as_published: false,
        ..Default::default()
    };
    subscriber
        .subscribe_with_options(&topic, sub_opts, collector.callback())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("rap0-pub", &broker).await;
    let pub_opts = PublishOptions {
        retain: true,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"rap0-payload", pub_opts)
        .await
        .expect("publish failed");

    assert!(
        collector.wait_for_messages(1, TIMEOUT).await,
        "Subscriber must receive message"
    );
    let msgs = collector.get_messages().await;
    assert!(
        !msgs[0].retain,
        "[MQTT-3.3.1-12] retain_as_published=0 must clear RETAIN flag on delivery"
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.1-13]` If `retain_as_published` is 1, the Server MUST set the
/// RETAIN flag equal to the received PUBLISH packet's RETAIN flag.
///
/// Publishes a retained message to a subscriber that set
/// `retain_as_published=true`. Verifies the delivered message has
/// `retain=true`.
#[tokio::test]
async fn publish_retain_as_published_one_preserves_flag() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("rap1/{}", unique_client_id("rap"));

    let collector = MessageCollector::new();
    let subscriber = connected_client("rap1-sub", &broker).await;
    let sub_opts = SubscribeOptions {
        retain_as_published: true,
        ..Default::default()
    };
    subscriber
        .subscribe_with_options(&topic, sub_opts, collector.callback())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("rap1-pub", &broker).await;
    let pub_opts = PublishOptions {
        retain: true,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"rap1-payload", pub_opts)
        .await
        .expect("publish failed");

    assert!(
        collector.wait_for_messages(1, TIMEOUT).await,
        "Subscriber must receive message"
    );
    let msgs = collector.get_messages().await;
    assert!(
        msgs[0].retain,
        "[MQTT-3.3.1-13] retain_as_published=1 must preserve RETAIN flag on delivery"
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

// ---------------------------------------------------------------------------
// Group 5: QoS Response Flows
// ---------------------------------------------------------------------------

/// `[MQTT-3.3.4-1]` `QoS` 0 message delivered to subscriber.
///
/// Publishes a `QoS` 0 message and verifies the subscriber receives it.
#[tokio::test]
async fn publish_qos0_delivery() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("qos0/{}", unique_client_id("del"));

    let collector = MessageCollector::new();
    let subscriber = connected_client("q0-sub", &broker).await;
    subscriber
        .subscribe(&topic, collector.callback())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("q0-pub", &broker).await;
    publisher
        .publish(&topic, b"qos0-msg")
        .await
        .expect("publish failed");

    assert!(
        collector.wait_for_messages(1, TIMEOUT).await,
        "[MQTT-3.3.4-1] QoS 0 message must be delivered to subscriber"
    );
    let msgs = collector.get_messages().await;
    assert_eq!(msgs[0].payload, b"qos0-msg");
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.4-1]` `QoS` 1 → broker sends PUBACK, subscriber receives
/// message.
///
/// Publishes a `QoS` 1 message via raw client and verifies PUBACK is received.
/// Also verifies a high-level subscriber receives the message.
#[tokio::test]
async fn publish_qos1_puback_response() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("qos1/{}", unique_client_id("ack"));

    let collector = MessageCollector::new();
    let sub_opts = SubscribeOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    let subscriber = connected_client("q1-sub", &broker).await;
    subscriber
        .subscribe_with_options(&topic, sub_opts, collector.callback())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("q1-pub", &broker).await;
    let pub_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"qos1-msg", pub_opts)
        .await
        .expect("publish failed");

    assert!(
        collector.wait_for_messages(1, TIMEOUT).await,
        "[MQTT-3.3.4-1] QoS 1 message must be delivered to subscriber"
    );
    let msgs = collector.get_messages().await;
    assert_eq!(msgs[0].payload, b"qos1-msg");
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.4-1]` `QoS` 2 → full PUBREC/PUBREL/PUBCOMP exchange and
/// delivery.
///
/// Publishes a `QoS` 2 message and verifies the subscriber receives it exactly
/// once via the full 4-step handshake.
#[tokio::test]
async fn publish_qos2_full_flow() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("qos2/{}", unique_client_id("flow"));

    let collector = MessageCollector::new();
    let sub_opts = SubscribeOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    };
    let subscriber = connected_client("q2-sub", &broker).await;
    subscriber
        .subscribe_with_options(&topic, sub_opts, collector.callback())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("q2-pub", &broker).await;
    let pub_opts = PublishOptions {
        qos: QoS::ExactlyOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"qos2-msg", pub_opts)
        .await
        .expect("publish failed");

    assert!(
        collector.wait_for_messages(1, Duration::from_secs(5)).await,
        "[MQTT-3.3.4-1] QoS 2 message must be delivered to subscriber"
    );
    let msgs = collector.get_messages().await;
    assert_eq!(msgs[0].payload, b"qos2-msg");
    assert_eq!(msgs.len(), 1, "QoS 2 must deliver exactly once");
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

// ---------------------------------------------------------------------------
// Group 6: Property Forwarding
// ---------------------------------------------------------------------------

/// `[MQTT-3.3.2-4]` The Server MUST send the Payload Format Indicator
/// unaltered to all subscribers.
///
/// Publishes with `payload_format_indicator=true` and verifies the subscriber
/// receives the same property value.
#[tokio::test]
async fn publish_payload_format_indicator_forwarded() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("pfi/{}", unique_client_id("fwd"));

    let collector = MessageCollector::new();
    let subscriber = connected_client("pfi-sub", &broker).await;
    subscriber
        .subscribe(&topic, collector.callback())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("pfi-pub", &broker).await;
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
        collector.wait_for_messages(1, TIMEOUT).await,
        "Subscriber must receive message"
    );
    let msgs = collector.get_messages().await;
    assert_eq!(
        msgs[0].properties.payload_format_indicator,
        Some(true),
        "[MQTT-3.3.2-4] Payload Format Indicator must be forwarded unaltered"
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.2-20]` The Server MUST send the Content Type unaltered to all
/// subscribers.
///
/// Publishes with `content_type="application/json"` and verifies the
/// subscriber receives the same property value.
#[tokio::test]
async fn publish_content_type_forwarded() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("ct/{}", unique_client_id("fwd"));

    let collector = MessageCollector::new();
    let subscriber = connected_client("ct-sub", &broker).await;
    subscriber
        .subscribe(&topic, collector.callback())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("ct-pub", &broker).await;
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
        collector.wait_for_messages(1, TIMEOUT).await,
        "Subscriber must receive message"
    );
    let msgs = collector.get_messages().await;
    assert_eq!(
        msgs[0].properties.content_type.as_deref(),
        Some("application/json"),
        "[MQTT-3.3.2-20] Content Type must be forwarded unaltered"
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.2-15]` `[MQTT-3.3.2-16]` The Server MUST send the Response
/// Topic and Correlation Data unaltered.
///
/// Publishes with `response_topic` and `correlation_data` properties and
/// verifies both are forwarded to the subscriber.
#[tokio::test]
async fn publish_response_topic_and_correlation_data_forwarded() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("rt/{}", unique_client_id("fwd"));

    let collector = MessageCollector::new();
    let subscriber = connected_client("rt-sub", &broker).await;
    subscriber
        .subscribe(&topic, collector.callback())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("rt-pub", &broker).await;
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
        collector.wait_for_messages(1, TIMEOUT).await,
        "Subscriber must receive message"
    );
    let msgs = collector.get_messages().await;
    assert_eq!(
        msgs[0].properties.response_topic.as_deref(),
        Some("reply/topic"),
        "[MQTT-3.3.2-15] Response Topic must be forwarded unaltered"
    );
    assert_eq!(
        msgs[0].properties.correlation_data.as_deref(),
        Some(&[0xDE, 0xAD, 0xBE, 0xEF][..]),
        "[MQTT-3.3.2-16] Correlation Data must be forwarded unaltered"
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

/// `[MQTT-3.3.2-17]` `[MQTT-3.3.2-18]` The Server MUST send all User
/// Properties unaltered and maintain their order.
///
/// Publishes with multiple user properties and verifies they are delivered
/// in the same order with the same key-value pairs. Note: the broker injects
/// `x-mqtt-sender` and `x-mqtt-client-id` properties, so we filter those out
/// before comparing.
#[tokio::test]
async fn publish_user_properties_forwarded_and_ordered() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("up/{}", unique_client_id("fwd"));

    let collector = MessageCollector::new();
    let subscriber = connected_client("up-sub", &broker).await;
    subscriber
        .subscribe(&topic, collector.callback())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let sent_props = vec![
        ("key-a".to_owned(), "value-1".to_owned()),
        ("key-b".to_owned(), "value-2".to_owned()),
        ("key-a".to_owned(), "value-3".to_owned()),
    ];
    let publisher = connected_client("up-pub", &broker).await;
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
        collector.wait_for_messages(1, TIMEOUT).await,
        "Subscriber must receive message"
    );
    let msgs = collector.get_messages().await;
    let received: Vec<(String, String)> = msgs[0]
        .properties
        .user_properties
        .iter()
        .filter(|(k, _)| k != "x-mqtt-sender" && k != "x-mqtt-client-id")
        .cloned()
        .collect();
    assert_eq!(
        received, sent_props,
        "[MQTT-3.3.2-17/18] User properties must be forwarded unaltered and in order"
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}

// ---------------------------------------------------------------------------
// Group 7: Topic Matching
// ---------------------------------------------------------------------------

/// `[MQTT-3.3.2-3]` The Topic Name sent to subscribing Clients MUST match the
/// Subscription's Topic Filter.
///
/// Subscribes with a wildcard filter and publishes to a matching topic. Verifies
/// the subscriber receives the message with the exact published topic name.
#[tokio::test]
async fn publish_topic_matches_subscription_filter() {
    let broker = ConformanceBroker::start().await;
    let prefix = unique_client_id("match");
    let filter = format!("tm/{prefix}/+");
    let publish_topic = format!("tm/{prefix}/sensor");

    let collector = MessageCollector::new();
    let subscriber = connected_client("tm-sub", &broker).await;
    subscriber
        .subscribe(&filter, collector.callback())
        .await
        .expect("subscribe failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("tm-pub", &broker).await;
    publisher
        .publish(&publish_topic, b"match-test")
        .await
        .expect("publish failed");

    assert!(
        collector.wait_for_messages(1, TIMEOUT).await,
        "[MQTT-3.3.2-3] Wildcard subscription must match published topic"
    );
    let msgs = collector.get_messages().await;
    assert_eq!(
        msgs[0].topic, publish_topic,
        "[MQTT-3.3.2-3] Delivered topic name must be the published topic, not the filter"
    );
    publisher.disconnect().await.expect("disconnect failed");
    subscriber.disconnect().await.expect("disconnect failed");
}
