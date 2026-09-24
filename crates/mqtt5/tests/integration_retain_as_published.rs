#![cfg(feature = "broker")]
use mqtt5::broker::router::{MessageRouter, SubscriptionRequest};
use mqtt5::packet::publish::PublishPacket;
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::sync::Arc;

use tokio::time::timeout;

#[tokio::test]
async fn test_retain_as_published_false_clears_retain_flag() {
    let router = Arc::new(MessageRouter::new());

    let (qos1_tx, _qos1_rx) = tokio::sync::mpsc::channel(10);
    let (qos0_tx, mut rx) = tokio::sync::mpsc::channel(10);
    let (dtx, _drx) = tokio::sync::oneshot::channel();

    router
        .register_client(
            "subscriber".to_string(),
            mqtt5::broker::router::DeliveryLanes { qos1_tx, qos0_tx },
            router.queue_handle("subscriber"),
            dtx,
        )
        .await;

    router
        .subscribe(SubscriptionRequest::new(
            "subscriber",
            "test/topic",
            QoS::AtMostOnce,
        ))
        .await
        .unwrap();

    let mut packet = PublishPacket::new(
        "test/topic".to_string(),
        &b"test message"[..],
        QoS::AtMostOnce,
    );
    packet.retain = true;
    router.route_message(&packet, Some("publisher")).await;

    let received = timeout(Duration::from_millis(100), rx.recv())
        .await
        .expect("timeout")
        .expect("message");
    assert!(!received.publish.retain, "retain flag should be cleared");
}

#[tokio::test]
async fn test_retain_as_published_true_preserves_retain_flag() {
    let router = Arc::new(MessageRouter::new());

    let (qos1_tx, _qos1_rx) = tokio::sync::mpsc::channel(10);
    let (qos0_tx, mut rx) = tokio::sync::mpsc::channel(10);
    let (dtx, _drx) = tokio::sync::oneshot::channel();

    router
        .register_client(
            "subscriber".to_string(),
            mqtt5::broker::router::DeliveryLanes { qos1_tx, qos0_tx },
            router.queue_handle("subscriber"),
            dtx,
        )
        .await;

    router
        .subscribe(
            SubscriptionRequest::new("subscriber", "test/topic", QoS::AtMostOnce)
                .with_retain_as_published(true),
        )
        .await
        .unwrap();

    let mut packet = PublishPacket::new(
        "test/topic".to_string(),
        &b"test message"[..],
        QoS::AtMostOnce,
    );
    packet.retain = true;
    router.route_message(&packet, Some("publisher")).await;

    let received = timeout(Duration::from_millis(100), rx.recv())
        .await
        .expect("timeout")
        .expect("message");
    assert!(received.publish.retain, "retain flag should be preserved");
}
