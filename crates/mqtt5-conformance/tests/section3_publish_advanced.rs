use mqtt5_conformance::harness::unique_client_id;
use mqtt5_conformance::sut::inprocess_sut;
use mqtt5_conformance::test_client::TestClient;
use mqtt5_protocol::types::{PublishOptions, PublishProperties, QoS, SubscribeOptions};
use std::time::Duration;

#[tokio::test]
async fn message_expiry_drops_expired_retained() {
    let sut = inprocess_sut().await;
    let tag = unique_client_id("expiry");
    let topic = format!("expiry/{tag}");

    let publisher = TestClient::connect_with_prefix(&sut, "pub-expiry")
        .await
        .unwrap();
    let pub_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        retain: true,
        properties: PublishProperties {
            message_expiry_interval: Some(1),
            ..Default::default()
        },
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, b"ephemeral", pub_opts)
        .await
        .expect("publish retained failed");
    publisher.disconnect().await.expect("disconnect failed");

    tokio::time::sleep(Duration::from_secs(2)).await;

    let subscriber = TestClient::connect_with_prefix(&sut, "sub-expiry")
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
        "expired retained message must not be delivered to new subscriber"
    );

    subscriber.disconnect().await.expect("disconnect failed");
}
