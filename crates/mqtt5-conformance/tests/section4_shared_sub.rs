use mqtt5::{PublishOptions, QoS, SubscribeOptions};
use mqtt5_conformance::harness::{
    connected_client, unique_client_id, ConformanceBroker, MessageCollector,
};
use mqtt5_conformance::raw_client::{RawMqttClient, RawPacketBuilder};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------
// Group 1: Shared Subscription Format Validation — Section 4.8.2
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shared_sub_valid_format_accepted() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("shared-valid");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "$share/mygroup/sensor/+",
        0,
        1,
    ))
    .await
    .unwrap();

    let (_, reason_codes) = raw
        .expect_suback(TIMEOUT)
        .await
        .expect("[MQTT-4.8.2-1] must receive SUBACK");
    assert_eq!(reason_codes.len(), 1);
    assert_eq!(
        reason_codes[0], 0x00,
        "[MQTT-4.8.2-1] valid shared subscription must be granted QoS 0"
    );
}

#[tokio::test]
async fn shared_sub_share_name_with_wildcard_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("shared-badname");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_multiple(
        &[("$share/gr+oup/topic", 0), ("$share/gr#oup/topic", 0)],
        1,
    ))
    .await
    .unwrap();

    let (_, reason_codes) = raw
        .expect_suback(TIMEOUT)
        .await
        .expect("[MQTT-4.8.2-2] must receive SUBACK");
    assert_eq!(reason_codes.len(), 2);
    assert_eq!(
        reason_codes[0], 0x8F,
        "[MQTT-4.8.2-2] ShareName with + must return TopicFilterInvalid (0x8F)"
    );
    assert_eq!(
        reason_codes[1], 0x8F,
        "[MQTT-4.8.2-2] ShareName with # must return TopicFilterInvalid (0x8F)"
    );
}

#[tokio::test]
async fn shared_sub_incomplete_format_rejected() {
    let broker = ConformanceBroker::start().await;
    let mut raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let client_id = unique_client_id("shared-incomplete");
    raw.connect_and_establish(&client_id, TIMEOUT).await;

    raw.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        "$share/grouponly",
        0,
        1,
    ))
    .await
    .unwrap();

    let (_, reason_codes) = raw
        .expect_suback(TIMEOUT)
        .await
        .expect("[MQTT-4.8.2-1] must receive SUBACK");
    assert_eq!(reason_codes.len(), 1);
    assert_eq!(
        reason_codes[0], 0x8F,
        "[MQTT-4.8.2-1] incomplete $share/grouponly (no second /) must return TopicFilterInvalid"
    );
}

// ---------------------------------------------------------------------------
// Group 2: Shared Subscription Message Distribution — Section 4.8.2
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shared_sub_round_robin_delivery() {
    let broker = ConformanceBroker::start().await;

    let worker1 = connected_client("shared-rr-w1", &broker).await;
    let collector1 = MessageCollector::new();
    worker1
        .subscribe("$share/workers/tasks", collector1.callback())
        .await
        .unwrap();

    let worker2 = connected_client("shared-rr-w2", &broker).await;
    let collector2 = MessageCollector::new();
    worker2
        .subscribe("$share/workers/tasks", collector2.callback())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("shared-rr-pub", &broker).await;
    for i in 0..6 {
        publisher
            .publish("tasks", format!("msg-{i}").as_bytes())
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let count1 = collector1.count().await;
    let count2 = collector2.count().await;
    assert_eq!(
        count1 + count2,
        6,
        "all 6 messages must be delivered across shared group"
    );
    assert!(
        count1 >= 2 && count2 >= 2,
        "round-robin should distribute roughly evenly: w1={count1}, w2={count2}"
    );
}

#[tokio::test]
async fn shared_sub_mixed_with_regular() {
    let broker = ConformanceBroker::start().await;

    let shared = connected_client("shared-mix-s", &broker).await;
    let collector_shared = MessageCollector::new();
    shared
        .subscribe("$share/workers/tasks", collector_shared.callback())
        .await
        .unwrap();

    let regular = connected_client("shared-mix-r", &broker).await;
    let collector_regular = MessageCollector::new();
    regular
        .subscribe("tasks", collector_regular.callback())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("shared-mix-pub", &broker).await;
    for i in 0..4 {
        publisher
            .publish("tasks", format!("msg-{i}").as_bytes())
            .await
            .unwrap();
    }

    collector_regular.wait_for_messages(4, TIMEOUT).await;
    collector_shared.wait_for_messages(4, TIMEOUT).await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        collector_regular.count().await,
        4,
        "regular subscriber must receive all 4 messages"
    );
    assert_eq!(
        collector_shared.count().await,
        4,
        "sole shared group member must receive all 4 messages"
    );
}

// ---------------------------------------------------------------------------
// Group 3: Shared Subscription Retained Messages — Section 4.8
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shared_sub_no_retained_on_subscribe() {
    let broker = ConformanceBroker::start().await;

    let publisher = connected_client("shared-ret-pub", &broker).await;
    publisher
        .publish_with_options(
            "sensor/temp",
            b"25.5",
            PublishOptions {
                retain: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let shared_sub = connected_client("shared-ret-s", &broker).await;
    let collector_shared = MessageCollector::new();
    shared_sub
        .subscribe("$share/readers/sensor/temp", collector_shared.callback())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        collector_shared.count().await,
        0,
        "shared subscription must not receive retained messages on subscribe"
    );

    let regular_sub = connected_client("shared-ret-r", &broker).await;
    let collector_regular = MessageCollector::new();
    regular_sub
        .subscribe("sensor/temp", collector_regular.callback())
        .await
        .unwrap();

    collector_regular.wait_for_messages(1, TIMEOUT).await;
    let msgs = collector_regular.get_messages().await;
    assert_eq!(
        msgs.len(),
        1,
        "regular subscription must receive retained message"
    );
    assert_eq!(msgs[0].payload, b"25.5");
}

// ---------------------------------------------------------------------------
// Group 4: Shared Subscription Unsubscribe and Multiple Groups — Section 4.8
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shared_sub_unsubscribe_stops_delivery() {
    let broker = ConformanceBroker::start().await;

    let subscriber = connected_client("shared-unsub-s", &broker).await;
    let collector = MessageCollector::new();
    subscriber
        .subscribe("$share/workers/tasks", collector.callback())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("shared-unsub-pub", &broker).await;
    publisher.publish("tasks", b"before").await.unwrap();

    collector.wait_for_messages(1, TIMEOUT).await;
    assert_eq!(
        collector.count().await,
        1,
        "must receive message before unsubscribe"
    );

    subscriber
        .unsubscribe("$share/workers/tasks")
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    publisher.publish("tasks", b"after").await.unwrap();

    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        collector.count().await,
        1,
        "must not receive messages after unsubscribe from shared subscription"
    );
}

#[tokio::test]
async fn shared_sub_multiple_groups_independent() {
    let broker = ConformanceBroker::start().await;

    let client_a = connected_client("shared-grpA", &broker).await;
    let collector_a = MessageCollector::new();
    client_a
        .subscribe("$share/groupA/topic", collector_a.callback())
        .await
        .unwrap();

    let client_b = connected_client("shared-grpB", &broker).await;
    let collector_b = MessageCollector::new();
    client_b
        .subscribe("$share/groupB/topic", collector_b.callback())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("shared-grp-pub", &broker).await;
    publisher.publish("topic", b"payload").await.unwrap();

    collector_a.wait_for_messages(1, TIMEOUT).await;
    collector_b.wait_for_messages(1, TIMEOUT).await;

    assert_eq!(
        collector_a.count().await,
        1,
        "groupA must receive the message independently"
    );
    assert_eq!(
        collector_b.count().await,
        1,
        "groupB must receive the message independently"
    );
}

#[tokio::test]
async fn shared_sub_respects_granted_qos() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("shared-qos/{}", unique_client_id("t"));
    let shared_filter = format!("$share/qgrp/{topic}");

    let mut sub = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let sub_id = unique_client_id("sub-sqos");
    sub.connect_and_establish(&sub_id, TIMEOUT).await;

    sub.send_raw(&RawPacketBuilder::subscribe_with_packet_id(
        &shared_filter,
        0,
        1,
    ))
    .await
    .unwrap();
    let (_, reason_codes) = sub
        .expect_suback(TIMEOUT)
        .await
        .expect("must receive SUBACK");
    assert_eq!(
        reason_codes[0], 0x00,
        "shared subscription must be granted at QoS 0"
    );

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut pub_raw = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let pub_id = unique_client_id("pub-sqos");
    pub_raw.connect_and_establish(&pub_id, TIMEOUT).await;

    pub_raw
        .send_raw(&RawPacketBuilder::publish_qos1(&topic, b"qos1-data", 1))
        .await
        .unwrap();
    let _ = pub_raw.expect_puback(TIMEOUT).await;

    let (qos, recv_topic, payload) = sub
        .expect_publish(TIMEOUT)
        .await
        .expect("[MQTT-4.8.2-3] subscriber must receive message");

    assert_eq!(recv_topic, topic);
    assert_eq!(payload, b"qos1-data");
    assert_eq!(
        qos, 0,
        "[MQTT-4.8.2-3] message published at QoS 1 must be downgraded to granted QoS 0"
    );
}

#[tokio::test]
async fn shared_sub_puback_error_discards() {
    let broker = ConformanceBroker::start().await;
    let topic = format!("shared-err/{}", unique_client_id("t"));
    let shared_filter = format!("$share/errgrp/{topic}");

    let sub_opts = SubscribeOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };

    let worker1 = connected_client("shared-err-w1", &broker).await;
    let collector1 = MessageCollector::new();
    worker1
        .subscribe_with_options(&shared_filter, sub_opts.clone(), collector1.callback())
        .await
        .unwrap();

    let mut worker2 = RawMqttClient::connect_tcp(broker.socket_addr())
        .await
        .unwrap();
    let w2_id = unique_client_id("shared-err-w2");
    worker2.connect_and_establish(&w2_id, TIMEOUT).await;
    worker2
        .send_raw(&RawPacketBuilder::subscribe_with_packet_id(
            &shared_filter,
            1,
            1,
        ))
        .await
        .unwrap();
    let _ = worker2.expect_suback(TIMEOUT).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let publisher = connected_client("shared-err-pub", &broker).await;
    let pub_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"reject-me", pub_opts)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    let w1_count = collector1.count().await;

    let w2_received = worker2
        .expect_publish_with_id(Duration::from_millis(500))
        .await;

    if let Some((_, packet_id, _, _, _)) = w2_received {
        worker2
            .send_raw(&RawPacketBuilder::puback_with_reason(packet_id, 0x80))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(500)).await;

        let redistributed = worker2.read_packet_bytes(Duration::from_secs(1)).await;
        assert!(
            redistributed.is_none(),
            "[MQTT-4.8.2-6] message rejected with PUBACK error must not be redistributed"
        );
    } else {
        assert_eq!(
            w1_count, 1,
            "one of the two shared subscribers must receive the message"
        );
    }
}
