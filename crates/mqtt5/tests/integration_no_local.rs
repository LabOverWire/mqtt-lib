use mqtt5::broker::router::MessageRouter;
use mqtt5::packet::publish::PublishPacket;
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::timeout;

#[tokio::test]
async fn test_no_local_true_filters_own_messages() {
    let router = Arc::new(MessageRouter::new());

    let (tx, mut rx) = mpsc::channel(10);
    let (dtx, _drx) = tokio::sync::oneshot::channel();

    router
        .register_client("test_client".to_string(), tx, dtx)
        .await;

    router
        .subscribe(
            "test_client".to_string(),
            "test/topic".to_string(),
            QoS::AtMostOnce,
            None,
            true,
            false,
        )
        .await;

    let packet = PublishPacket::new("test/topic".to_string(), b"test message", QoS::AtMostOnce);
    router.route_message(&packet, Some("test_client")).await;

    let result = timeout(Duration::from_millis(100), rx.recv()).await;
    assert!(
        result.is_err(),
        "Client should not receive its own message when no_local=true"
    );
}

#[tokio::test]
async fn test_no_local_false_allows_own_messages() {
    let router = Arc::new(MessageRouter::new());

    let (tx, mut rx) = mpsc::channel(10);
    let (dtx, _drx) = tokio::sync::oneshot::channel();

    router
        .register_client("test_client".to_string(), tx, dtx)
        .await;

    router
        .subscribe(
            "test_client".to_string(),
            "test/topic".to_string(),
            QoS::AtMostOnce,
            None,
            false,
            false,
        )
        .await;

    let packet = PublishPacket::new("test/topic".to_string(), b"test message", QoS::AtMostOnce);
    router.route_message(&packet, Some("test_client")).await;

    let result = timeout(Duration::from_millis(100), rx.recv()).await;
    assert!(
        result.is_ok(),
        "Client should receive its own message when no_local=false"
    );

    let received = result.unwrap().unwrap();
    assert_eq!(received.topic_name, "test/topic");
    assert_eq!(received.payload, b"test message");
}

#[tokio::test]
async fn test_no_local_other_clients_receive_messages() {
    let router = Arc::new(MessageRouter::new());

    let (tx1, mut rx1) = mpsc::channel(10);
    let (tx2, mut rx2) = mpsc::channel(10);

    let (dtx1, _drx1) = tokio::sync::oneshot::channel();
    router
        .register_client("publisher".to_string(), tx1, dtx1)
        .await;

    let (dtx2, _drx2) = tokio::sync::oneshot::channel();
    router
        .register_client("subscriber".to_string(), tx2, dtx2)
        .await;

    router
        .subscribe(
            "publisher".to_string(),
            "test/topic".to_string(),
            QoS::AtMostOnce,
            None,
            true,
            false,
        )
        .await;

    router
        .subscribe(
            "subscriber".to_string(),
            "test/topic".to_string(),
            QoS::AtMostOnce,
            None,
            false,
            false,
        )
        .await;

    let packet = PublishPacket::new("test/topic".to_string(), b"test message", QoS::AtMostOnce);
    router.route_message(&packet, Some("publisher")).await;

    let pub_result = timeout(Duration::from_millis(100), rx1.recv()).await;
    assert!(
        pub_result.is_err(),
        "Publisher should not receive its own message with no_local=true"
    );

    let sub_result = timeout(Duration::from_millis(100), rx2.recv()).await;
    assert!(
        sub_result.is_ok(),
        "Subscriber should receive message from publisher"
    );

    let received = sub_result.unwrap().unwrap();
    assert_eq!(received.topic_name, "test/topic");
    assert_eq!(received.payload, b"test message");
}

#[tokio::test]
async fn test_no_local_with_wildcards() {
    let router = Arc::new(MessageRouter::new());

    let (tx, mut rx) = mpsc::channel(10);
    let (dtx, _drx) = tokio::sync::oneshot::channel();

    router
        .register_client("test_client".to_string(), tx, dtx)
        .await;

    router
        .subscribe(
            "test_client".to_string(),
            "test/+".to_string(),
            QoS::AtMostOnce,
            None,
            true,
            false,
        )
        .await;

    let packet1 = PublishPacket::new("test/topic1".to_string(), b"message 1", QoS::AtMostOnce);
    router.route_message(&packet1, Some("test_client")).await;

    let packet2 = PublishPacket::new("test/topic2".to_string(), b"message 2", QoS::AtMostOnce);
    router.route_message(&packet2, Some("test_client")).await;

    let result = timeout(Duration::from_millis(100), rx.recv()).await;
    assert!(
        result.is_err(),
        "Client should not receive any messages matching wildcard from itself when no_local=true"
    );
}

#[tokio::test]
async fn test_no_local_with_multilevel_wildcard() {
    let router = Arc::new(MessageRouter::new());

    let (tx, mut rx) = mpsc::channel(10);
    let (dtx, _drx) = tokio::sync::oneshot::channel();

    router
        .register_client("test_client".to_string(), tx, dtx)
        .await;

    router
        .subscribe(
            "test_client".to_string(),
            "test/#".to_string(),
            QoS::AtMostOnce,
            None,
            true,
            false,
        )
        .await;

    let packet = PublishPacket::new(
        "test/foo/bar/baz".to_string(),
        b"nested message",
        QoS::AtMostOnce,
    );
    router.route_message(&packet, Some("test_client")).await;

    let result = timeout(Duration::from_millis(100), rx.recv()).await;
    assert!(result.is_err(), "Client should not receive messages matching multilevel wildcard from itself when no_local=true");
}

#[tokio::test]
async fn test_no_local_server_generated_messages() {
    let router = Arc::new(MessageRouter::new());

    let (tx, mut rx) = mpsc::channel(10);
    let (dtx, _drx) = tokio::sync::oneshot::channel();

    router
        .register_client("test_client".to_string(), tx, dtx)
        .await;

    router
        .subscribe(
            "test_client".to_string(),
            "test/topic".to_string(),
            QoS::AtMostOnce,
            None,
            true,
            false,
        )
        .await;

    let packet = PublishPacket::new("test/topic".to_string(), b"server message", QoS::AtMostOnce);
    router.route_message(&packet, None).await;

    let result = timeout(Duration::from_millis(100), rx.recv()).await;
    assert!(
        result.is_ok(),
        "Client should receive server-generated messages even with no_local=true"
    );

    let received = result.unwrap().unwrap();
    assert_eq!(received.topic_name, "test/topic");
    assert_eq!(received.payload, b"server message");
}

#[tokio::test]
async fn test_no_local_multiple_subscriptions_same_client() {
    let router = Arc::new(MessageRouter::new());

    let (tx, mut rx) = mpsc::channel(10);
    let (dtx, _drx) = tokio::sync::oneshot::channel();

    router
        .register_client("test_client".to_string(), tx, dtx)
        .await;

    router
        .subscribe(
            "test_client".to_string(),
            "test/topic1".to_string(),
            QoS::AtMostOnce,
            None,
            true,
            false,
        )
        .await;

    router
        .subscribe(
            "test_client".to_string(),
            "test/topic2".to_string(),
            QoS::AtMostOnce,
            None,
            false,
            false,
        )
        .await;

    let packet1 = PublishPacket::new("test/topic1".to_string(), b"message 1", QoS::AtMostOnce);
    router.route_message(&packet1, Some("test_client")).await;

    let packet2 = PublishPacket::new("test/topic2".to_string(), b"message 2", QoS::AtMostOnce);
    router.route_message(&packet2, Some("test_client")).await;

    let result1 = timeout(Duration::from_millis(100), rx.recv()).await;
    assert!(
        result1.is_ok(),
        "Client should receive message from topic2 with no_local=false"
    );

    let received = result1.unwrap().unwrap();
    assert_eq!(received.topic_name, "test/topic2");
    assert_eq!(received.payload, b"message 2");

    let result2 = timeout(Duration::from_millis(100), rx.recv()).await;
    assert!(
        result2.is_err(),
        "Client should not receive second message from topic1 with no_local=true"
    );
}

#[tokio::test]
async fn test_no_local_with_qos_levels() {
    let router = Arc::new(MessageRouter::new());

    let (tx, mut rx) = mpsc::channel(10);
    let (dtx, _drx) = tokio::sync::oneshot::channel();

    router
        .register_client("test_client".to_string(), tx, dtx)
        .await;

    router
        .subscribe(
            "test_client".to_string(),
            "test/topic".to_string(),
            QoS::AtLeastOnce,
            None,
            true,
            false,
        )
        .await;

    let packet = PublishPacket::new("test/topic".to_string(), b"qos1 message", QoS::AtLeastOnce);
    router.route_message(&packet, Some("test_client")).await;

    let result = timeout(Duration::from_millis(100), rx.recv()).await;
    assert!(
        result.is_err(),
        "Client should not receive its own QoS 1 message when no_local=true"
    );
}
