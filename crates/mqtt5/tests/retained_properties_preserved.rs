mod common;

use common::{create_test_client_with_broker, test_client_id, TestBroker};
use mqtt5::time::Duration;
use mqtt5::types::{Message, PublishProperties};
use mqtt5::{MqttClient, PublishOptions, QoS};
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::test]
async fn retained_message_preserves_v5_properties_for_late_subscriber() {
    let broker = TestBroker::start().await;

    let publisher = create_test_client_with_broker("retained-props-pub", broker.address()).await;

    let options = PublishOptions {
        qos: QoS::ExactlyOnce,
        retain: true,
        properties: PublishProperties {
            response_topic: Some("sensors/response".to_string()),
            correlation_data: Some(b"corr-77".to_vec()),
            content_type: Some("text/plain".to_string()),
            payload_format_indicator: Some(true),
            user_properties: vec![("trace-id".to_string(), "issue-77".to_string())],
            ..Default::default()
        },
        skip_codec: false,
    };

    publisher
        .publish_with_options("sensors/request", b"25C", options)
        .await
        .expect("publish retained failed");

    tokio::time::sleep(Duration::from_millis(200)).await;

    let subscriber = MqttClient::new(test_client_id("retained-props-sub"));
    subscriber
        .connect(broker.address())
        .await
        .expect("subscriber connect failed");

    let received: Arc<Mutex<Option<Message>>> = Arc::new(Mutex::new(None));
    let received_clone = Arc::clone(&received);

    subscriber
        .subscribe("sensors/request", move |msg| {
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
            "did not receive retained message within 3s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    assert_eq!(msg.topic, "sensors/request");
    assert_eq!(&msg.payload[..], b"25C");
    assert!(msg.retain, "retain flag should be set on retained delivery");

    assert_eq!(
        msg.properties.response_topic.as_deref(),
        Some("sensors/response"),
        "response_topic was dropped by the broker on retained delivery"
    );
    assert_eq!(
        msg.properties.correlation_data.as_deref(),
        Some(&b"corr-77"[..]),
        "correlation_data was dropped by the broker on retained delivery"
    );
    assert_eq!(
        msg.properties.content_type.as_deref(),
        Some("text/plain"),
        "content_type was dropped by the broker on retained delivery"
    );
    assert_eq!(
        msg.properties.payload_format_indicator,
        Some(true),
        "payload_format_indicator was dropped by the broker on retained delivery"
    );
    assert!(
        msg.properties
            .user_properties
            .iter()
            .any(|(k, v)| k == "trace-id" && v == "issue-77"),
        "user property 'trace-id=issue-77' was dropped by the broker on retained delivery; got {:?}",
        msg.properties.user_properties
    );
}
