use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::storage::ChangeOnlyState;
use mqtt5::packet::publish::PublishPacket;
use mqtt5::time::Duration;
use mqtt5::types::ProtocolVersion;
use mqtt5::QoS;
use std::sync::Arc;
use tokio::time::timeout;

#[tokio::test]
async fn test_change_only_filters_duplicate_payloads() {
    let router = Arc::new(MessageRouter::new());

    let (tx, rx) = flume::bounded(10);
    let (dtx, _drx) = tokio::sync::oneshot::channel();

    router
        .register_client("change_only_client".to_string(), tx, dtx)
        .await;

    router
        .subscribe(
            "change_only_client".to_string(),
            "sensors/temperature".to_string(),
            QoS::AtMostOnce,
            None,
            false,
            false,
            0,
            ProtocolVersion::V5,
            true,
        )
        .await
        .unwrap();

    let packet1 = PublishPacket::new(
        "sensors/temperature".to_string(),
        &b"25.5"[..],
        QoS::AtMostOnce,
    );
    router.route_message(&packet1, None).await;

    let result1 = timeout(Duration::from_millis(100), rx.recv_async()).await;
    assert!(result1.is_ok(), "First message should be delivered");

    let packet2 = PublishPacket::new(
        "sensors/temperature".to_string(),
        &b"25.5"[..],
        QoS::AtMostOnce,
    );
    router.route_message(&packet2, None).await;

    let result2 = timeout(Duration::from_millis(100), rx.recv_async()).await;
    assert!(
        result2.is_err(),
        "Duplicate payload should not be delivered in change-only mode"
    );
}

#[tokio::test]
async fn test_change_only_allows_different_payloads() {
    let router = Arc::new(MessageRouter::new());

    let (tx, rx) = flume::bounded(10);
    let (dtx, _drx) = tokio::sync::oneshot::channel();

    router
        .register_client("change_only_client".to_string(), tx, dtx)
        .await;

    router
        .subscribe(
            "change_only_client".to_string(),
            "sensors/temperature".to_string(),
            QoS::AtMostOnce,
            None,
            false,
            false,
            0,
            ProtocolVersion::V5,
            true,
        )
        .await
        .unwrap();

    let packet1 = PublishPacket::new(
        "sensors/temperature".to_string(),
        &b"25.5"[..],
        QoS::AtMostOnce,
    );
    router.route_message(&packet1, None).await;

    let result1 = timeout(Duration::from_millis(100), rx.recv_async()).await;
    assert!(result1.is_ok(), "First message should be delivered");
    let received1 = result1.unwrap().unwrap();
    assert_eq!(&received1.payload[..], b"25.5");

    let packet2 = PublishPacket::new(
        "sensors/temperature".to_string(),
        &b"26.0"[..],
        QoS::AtMostOnce,
    );
    router.route_message(&packet2, None).await;

    let result2 = timeout(Duration::from_millis(100), rx.recv_async()).await;
    assert!(result2.is_ok(), "Different payload should be delivered");
    let received2 = result2.unwrap().unwrap();
    assert_eq!(&received2.payload[..], b"26.0");
}

#[tokio::test]
async fn test_change_only_disabled_allows_duplicates() {
    let router = Arc::new(MessageRouter::new());

    let (tx, rx) = flume::bounded(10);
    let (dtx, _drx) = tokio::sync::oneshot::channel();

    router
        .register_client("regular_client".to_string(), tx, dtx)
        .await;

    router
        .subscribe(
            "regular_client".to_string(),
            "sensors/temperature".to_string(),
            QoS::AtMostOnce,
            None,
            false,
            false,
            0,
            ProtocolVersion::V5,
            false,
        )
        .await
        .unwrap();

    let packet1 = PublishPacket::new(
        "sensors/temperature".to_string(),
        &b"25.5"[..],
        QoS::AtMostOnce,
    );
    router.route_message(&packet1, None).await;

    let result1 = timeout(Duration::from_millis(100), rx.recv_async()).await;
    assert!(result1.is_ok(), "First message should be delivered");

    let packet2 = PublishPacket::new(
        "sensors/temperature".to_string(),
        &b"25.5"[..],
        QoS::AtMostOnce,
    );
    router.route_message(&packet2, None).await;

    let result2 = timeout(Duration::from_millis(100), rx.recv_async()).await;
    assert!(
        result2.is_ok(),
        "Duplicate payload should be delivered when change_only=false"
    );
}

#[tokio::test]
async fn test_change_only_per_topic_tracking() {
    let router = Arc::new(MessageRouter::new());

    let (tx, rx) = flume::bounded(10);
    let (dtx, _drx) = tokio::sync::oneshot::channel();

    router
        .register_client("change_only_client".to_string(), tx, dtx)
        .await;

    router
        .subscribe(
            "change_only_client".to_string(),
            "sensors/+".to_string(),
            QoS::AtMostOnce,
            None,
            false,
            false,
            0,
            ProtocolVersion::V5,
            true,
        )
        .await
        .unwrap();

    let packet_temp = PublishPacket::new(
        "sensors/temperature".to_string(),
        &b"25.5"[..],
        QoS::AtMostOnce,
    );
    router.route_message(&packet_temp, None).await;

    let result1 = timeout(Duration::from_millis(100), rx.recv_async()).await;
    assert!(result1.is_ok(), "Temperature message should be delivered");

    let packet_humidity = PublishPacket::new(
        "sensors/humidity".to_string(),
        &b"25.5"[..],
        QoS::AtMostOnce,
    );
    router.route_message(&packet_humidity, None).await;

    let result2 = timeout(Duration::from_millis(100), rx.recv_async()).await;
    assert!(
        result2.is_ok(),
        "Same payload on different topic should be delivered"
    );
}

#[tokio::test]
async fn test_change_only_state_persistence() {
    let router = Arc::new(MessageRouter::new());

    let (tx, rx) = flume::bounded(10);
    let (dtx, _drx) = tokio::sync::oneshot::channel();

    router
        .register_client("persistent_client".to_string(), tx.clone(), dtx)
        .await;

    router
        .subscribe(
            "persistent_client".to_string(),
            "test/topic".to_string(),
            QoS::AtMostOnce,
            None,
            false,
            false,
            0,
            ProtocolVersion::V5,
            true,
        )
        .await
        .unwrap();

    let packet1 = PublishPacket::new("test/topic".to_string(), &b"data1"[..], QoS::AtMostOnce);
    router.route_message(&packet1, None).await;

    let _ = timeout(Duration::from_millis(100), rx.recv_async()).await;

    let state = router.get_change_only_state("persistent_client").await;
    assert!(
        state.is_some(),
        "Change-only state should exist after delivery"
    );

    let state = state.unwrap();
    assert!(
        state.last_payload_hashes.contains_key("test/topic"),
        "Change-only state should track the topic"
    );
}

#[tokio::test]
async fn test_change_only_state_load_on_reconnect() {
    let router = Arc::new(MessageRouter::new());

    let mut state = ChangeOnlyState::default();
    state.update_hash("test/topic", b"data1");

    router
        .load_change_only_state("reconnect_client", state)
        .await;

    let (tx, rx) = flume::bounded(10);
    let (dtx, _drx) = tokio::sync::oneshot::channel();

    router
        .register_client("reconnect_client".to_string(), tx, dtx)
        .await;

    router
        .subscribe(
            "reconnect_client".to_string(),
            "test/topic".to_string(),
            QoS::AtMostOnce,
            None,
            false,
            false,
            0,
            ProtocolVersion::V5,
            true,
        )
        .await
        .unwrap();

    let packet_same = PublishPacket::new("test/topic".to_string(), &b"data1"[..], QoS::AtMostOnce);
    router.route_message(&packet_same, None).await;

    let result = timeout(Duration::from_millis(100), rx.recv_async()).await;
    assert!(
        result.is_err(),
        "Same payload should be blocked after state restore"
    );

    let packet_new = PublishPacket::new("test/topic".to_string(), &b"data2"[..], QoS::AtMostOnce);
    router.route_message(&packet_new, None).await;

    let result2 = timeout(Duration::from_millis(100), rx.recv_async()).await;
    assert!(
        result2.is_ok(),
        "New payload should be delivered after state restore"
    );
}

#[tokio::test]
async fn test_change_only_state_hash_computation() {
    let hash1 = ChangeOnlyState::compute_hash(b"hello world");
    let hash2 = ChangeOnlyState::compute_hash(b"hello world");
    let hash3 = ChangeOnlyState::compute_hash(b"hello world!");

    assert_eq!(hash1, hash2, "Same payload should produce same hash");
    assert_ne!(
        hash1, hash3,
        "Different payload should produce different hash"
    );
}

#[tokio::test]
async fn test_change_only_state_should_deliver() {
    let mut state = ChangeOnlyState::default();

    assert!(
        state.should_deliver("topic", b"data"),
        "First message should be delivered"
    );

    state.update_hash("topic", b"data");

    assert!(
        !state.should_deliver("topic", b"data"),
        "Same payload should not be delivered"
    );

    assert!(
        state.should_deliver("topic", b"new_data"),
        "Different payload should be delivered"
    );

    assert!(
        state.should_deliver("other_topic", b"data"),
        "Same payload on different topic should be delivered"
    );
}

#[tokio::test]
async fn test_change_only_with_qos_levels() {
    let router = Arc::new(MessageRouter::new());

    let (tx, rx) = flume::bounded(10);
    let (dtx, _drx) = tokio::sync::oneshot::channel();

    router
        .register_client("qos_client".to_string(), tx, dtx)
        .await;

    router
        .subscribe(
            "qos_client".to_string(),
            "test/topic".to_string(),
            QoS::AtLeastOnce,
            None,
            false,
            false,
            0,
            ProtocolVersion::V5,
            true,
        )
        .await
        .unwrap();

    let packet1 = PublishPacket::new(
        "test/topic".to_string(),
        &b"qos1_data"[..],
        QoS::AtLeastOnce,
    );
    router.route_message(&packet1, None).await;

    let result1 = timeout(Duration::from_millis(100), rx.recv_async()).await;
    assert!(result1.is_ok(), "QoS1 message should be delivered");

    let packet2 = PublishPacket::new(
        "test/topic".to_string(),
        &b"qos1_data"[..],
        QoS::AtLeastOnce,
    );
    router.route_message(&packet2, None).await;

    let result2 = timeout(Duration::from_millis(100), rx.recv_async()).await;
    assert!(
        result2.is_err(),
        "Duplicate QoS1 message should be filtered in change-only mode"
    );
}

#[tokio::test]
async fn test_change_only_multiple_clients_independent() {
    let router = Arc::new(MessageRouter::new());

    let (tx1, rx1) = flume::bounded(10);
    let (tx2, rx2) = flume::bounded(10);

    let (dtx1, _drx1) = tokio::sync::oneshot::channel();
    let (dtx2, _drx2) = tokio::sync::oneshot::channel();

    router
        .register_client("client1".to_string(), tx1, dtx1)
        .await;
    router
        .register_client("client2".to_string(), tx2, dtx2)
        .await;

    router
        .subscribe(
            "client1".to_string(),
            "test/topic".to_string(),
            QoS::AtMostOnce,
            None,
            false,
            false,
            0,
            ProtocolVersion::V5,
            true,
        )
        .await
        .unwrap();

    router
        .subscribe(
            "client2".to_string(),
            "test/topic".to_string(),
            QoS::AtMostOnce,
            None,
            false,
            false,
            0,
            ProtocolVersion::V5,
            true,
        )
        .await
        .unwrap();

    let packet = PublishPacket::new("test/topic".to_string(), &b"data"[..], QoS::AtMostOnce);
    router.route_message(&packet, None).await;

    let result1 = timeout(Duration::from_millis(100), rx1.recv_async()).await;
    let result2 = timeout(Duration::from_millis(100), rx2.recv_async()).await;

    assert!(result1.is_ok(), "Client1 should receive first message");
    assert!(result2.is_ok(), "Client2 should receive first message");

    router.route_message(&packet, None).await;

    let result1_dup = timeout(Duration::from_millis(100), rx1.recv_async()).await;
    let result2_dup = timeout(Duration::from_millis(100), rx2.recv_async()).await;

    assert!(result1_dup.is_err(), "Client1 should not receive duplicate");
    assert!(result2_dup.is_err(), "Client2 should not receive duplicate");
}
