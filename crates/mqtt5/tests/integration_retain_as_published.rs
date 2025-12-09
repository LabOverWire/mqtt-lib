use mqtt5::broker::router::MessageRouter;
use mqtt5::packet::publish::PublishPacket;
use mqtt5::time::Duration;
use mqtt5::types::ProtocolVersion;
use mqtt5::QoS;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::timeout;

#[tokio::test]
async fn test_retain_as_published_false_clears_retain_flag() {
    let router = Arc::new(MessageRouter::new());

    let (tx, mut rx) = mpsc::channel(10);
    let (dtx, _drx) = tokio::sync::oneshot::channel();

    router
        .register_client("subscriber".to_string(), tx, dtx)
        .await;

    router
        .subscribe(
            "subscriber".to_string(),
            "test/topic".to_string(),
            QoS::AtMostOnce,
            None,
            false,
            false,
            ProtocolVersion::V5,
        )
        .await
        .unwrap();

    let mut packet = PublishPacket::new("test/topic".to_string(), b"test message", QoS::AtMostOnce);
    packet.retain = true;
    router.route_message(&packet, Some("publisher")).await;

    let received = timeout(Duration::from_millis(100), rx.recv())
        .await
        .expect("timeout")
        .expect("message");
    assert!(!received.retain, "retain flag should be cleared");
}

#[tokio::test]
async fn test_retain_as_published_true_preserves_retain_flag() {
    let router = Arc::new(MessageRouter::new());

    let (tx, mut rx) = mpsc::channel(10);
    let (dtx, _drx) = tokio::sync::oneshot::channel();

    router
        .register_client("subscriber".to_string(), tx, dtx)
        .await;

    router
        .subscribe(
            "subscriber".to_string(),
            "test/topic".to_string(),
            QoS::AtMostOnce,
            None,
            false,
            true,
            ProtocolVersion::V5,
        )
        .await
        .unwrap();

    let mut packet = PublishPacket::new("test/topic".to_string(), b"test message", QoS::AtMostOnce);
    packet.retain = true;
    router.route_message(&packet, Some("publisher")).await;

    let received = timeout(Duration::from_millis(100), rx.recv())
        .await
        .expect("timeout")
        .expect("message");
    assert!(received.retain, "retain flag should be preserved");
}
