#![allow(clippy::large_futures)]

mod common;

use common::{create_test_client_with_broker, test_client_id, TestBroker};
use mqtt5::time::Duration;
use mqtt5::types::{Message, PublishProperties};
use mqtt5::{MqttClient, PublishOptions, QoS};
use std::sync::Arc;
use tokio::sync::Mutex;

const PUBLISH_TOPIC: &str = "sensors/request";

async fn run_retained_v5_props_case(
    qos: QoS,
    subscribe_filter: &str,
    message_expiry_interval: Option<u32>,
    case_label: &str,
) {
    let broker = TestBroker::start().await;

    let publisher = create_test_client_with_broker(
        &format!("retained-props-pub-{case_label}"),
        broker.address(),
    )
    .await;

    let options = PublishOptions {
        qos,
        retain: true,
        properties: PublishProperties {
            response_topic: Some("sensors/response".to_string()),
            correlation_data: Some(b"corr-77".to_vec()),
            content_type: Some("text/plain".to_string()),
            payload_format_indicator: Some(true),
            user_properties: vec![("trace-id".to_string(), "issue-77".to_string())],
            message_expiry_interval,
            ..Default::default()
        },
        skip_codec: false,
    };

    publisher
        .publish_with_options(PUBLISH_TOPIC, b"25C", options)
        .await
        .expect("publish retained failed");

    tokio::time::sleep(Duration::from_millis(200)).await;

    let subscriber = MqttClient::new(test_client_id(&format!("retained-props-sub-{case_label}")));
    subscriber
        .connect(broker.address())
        .await
        .expect("subscriber connect failed");

    let received: Arc<Mutex<Option<Message>>> = Arc::new(Mutex::new(None));
    let received_clone = Arc::clone(&received);

    subscriber
        .subscribe(subscribe_filter, move |msg| {
            let slot = Arc::clone(&received_clone);
            tokio::spawn(async move {
                *slot.lock().await = Some(msg);
            });
        })
        .await
        .expect("subscribe failed");

    let start = tokio::time::Instant::now();
    let msg = loop {
        if let Some(m) = received.lock().await.clone() {
            break m;
        }
        assert!(
            start.elapsed() <= Duration::from_secs(3),
            "[{case_label}] did not receive retained message within 3s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    assert_eq!(msg.topic, PUBLISH_TOPIC, "[{case_label}] topic mismatch");
    assert_eq!(&msg.payload[..], b"25C", "[{case_label}] payload mismatch");
    assert!(msg.retain, "[{case_label}] retain flag should be set");

    assert_eq!(
        msg.properties.response_topic.as_deref(),
        Some("sensors/response"),
        "[{case_label}] response_topic was dropped"
    );
    assert_eq!(
        msg.properties.correlation_data.as_deref(),
        Some(&b"corr-77"[..]),
        "[{case_label}] correlation_data was dropped"
    );
    assert_eq!(
        msg.properties.content_type.as_deref(),
        Some("text/plain"),
        "[{case_label}] content_type was dropped"
    );
    assert_eq!(
        msg.properties.payload_format_indicator,
        Some(true),
        "[{case_label}] payload_format_indicator was dropped"
    );
    assert!(
        msg.properties
            .user_properties
            .iter()
            .any(|(k, v)| k == "trace-id" && v == "issue-77"),
        "[{case_label}] user property 'trace-id=issue-77' was dropped; got {:?}",
        msg.properties.user_properties
    );

    if let Some(expected_expiry) = message_expiry_interval {
        let actual = msg
            .properties
            .message_expiry_interval
            .expect("message_expiry_interval should still be set");
        assert!(
            actual <= expected_expiry,
            "[{case_label}] expiry should be <= original {expected_expiry}, got {actual}"
        );
    }
}

#[tokio::test]
async fn retained_v5_props_qos2_exact_topic() {
    run_retained_v5_props_case(QoS::ExactlyOnce, PUBLISH_TOPIC, None, "qos2-exact").await;
}

#[tokio::test]
async fn retained_v5_props_qos1_exact_topic() {
    run_retained_v5_props_case(QoS::AtLeastOnce, PUBLISH_TOPIC, None, "qos1-exact").await;
}

#[tokio::test]
async fn retained_v5_props_qos0_exact_topic() {
    run_retained_v5_props_case(QoS::AtMostOnce, PUBLISH_TOPIC, None, "qos0-exact").await;
}

#[tokio::test]
async fn retained_v5_props_qos1_wildcard_subscribe() {
    run_retained_v5_props_case(QoS::AtLeastOnce, "sensors/+", None, "qos1-wildcard").await;
}

#[tokio::test]
async fn retained_v5_props_qos1_multi_level_wildcard() {
    run_retained_v5_props_case(QoS::AtLeastOnce, "sensors/#", None, "qos1-multi-wildcard").await;
}

#[tokio::test]
async fn retained_v5_props_survives_message_expiry_interval() {
    run_retained_v5_props_case(QoS::AtLeastOnce, PUBLISH_TOPIC, Some(3600), "expiry").await;
}
