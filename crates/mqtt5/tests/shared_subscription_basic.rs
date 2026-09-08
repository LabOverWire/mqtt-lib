#![cfg(feature = "broker")]
//! Basic test for shared subscriptions

use bytes::Bytes;
use mqtt5::broker::router::{DeliveryLanes, LaneReceivers, MessageRouter};
use mqtt5::packet::publish::PublishPacket;
use mqtt5::types::ProtocolVersion;
use mqtt5::QoS;
use std::sync::Arc;

async fn register(router: &MessageRouter, client_id: &str, filter: &str) -> LaneReceivers {
    let (lanes, rx) = DeliveryLanes::channel(100);
    let (dtx, _drx) = tokio::sync::oneshot::channel();
    router
        .register_client(
            client_id.to_string(),
            lanes,
            router.queue_handle(client_id),
            dtx,
        )
        .await;
    router
        .subscribe(
            client_id.to_string(),
            filter.to_string(),
            QoS::AtMostOnce,
            None,
            false,
            false,
            0,
            ProtocolVersion::V5,
            false,
            None,
        )
        .await
        .unwrap();
    rx
}

fn drain_count(rx: &mut LaneReceivers) -> usize {
    let mut count = 0;
    while rx.try_recv().is_ok() {
        count += 1;
    }
    count
}

#[tokio::test]
async fn test_shared_subscription_distribution() {
    let router = Arc::new(MessageRouter::new());

    let mut rx1 = register(&router, "worker1", "$share/workers/tasks/+").await;
    let mut rx2 = register(&router, "worker2", "$share/workers/tasks/+").await;
    let mut rx3 = register(&router, "worker3", "$share/workers/tasks/+").await;

    for i in 0..9 {
        let publish = PublishPacket::new(
            format!("tasks/job{}", i % 3),
            Bytes::copy_from_slice(format!("Task {i}").as_bytes()),
            QoS::AtMostOnce,
        );
        router.route_message(&publish, None).await;
    }

    let count1 = drain_count(&mut rx1);
    let count2 = drain_count(&mut rx2);
    let count3 = drain_count(&mut rx3);

    assert_eq!(count1, 3);
    assert_eq!(count2, 3);
    assert_eq!(count3, 3);
    assert_eq!(count1 + count2 + count3, 9);
}

#[tokio::test]
async fn test_mixed_shared_and_regular_subscriptions() {
    let router = Arc::new(MessageRouter::new());

    let mut rx_shared1 = register(&router, "shared1", "$share/team/alerts/+").await;
    let mut rx_shared2 = register(&router, "shared2", "$share/team/alerts/+").await;
    let mut rx_regular = register(&router, "regular", "alerts/+").await;

    for i in 0..4 {
        let publish = PublishPacket::new(
            format!("alerts/critical{i}"),
            Bytes::copy_from_slice(format!("Alert {i}").as_bytes()),
            QoS::AtMostOnce,
        );
        router.route_message(&publish, None).await;
    }

    let shared1_count = drain_count(&mut rx_shared1);
    let shared2_count = drain_count(&mut rx_shared2);
    let regular_count = drain_count(&mut rx_regular);

    assert_eq!(regular_count, 4);
    assert_eq!(shared1_count + shared2_count, 4);
    assert_eq!(shared1_count, 2);
    assert_eq!(shared2_count, 2);
}
