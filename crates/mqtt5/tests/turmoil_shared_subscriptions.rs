#![cfg(all(feature = "broker", feature = "turmoil-testing"))]
//! Shared subscription tests using Turmoil
//!
//! These tests verify the shared subscription functionality using
//! the existing `MessageRouter` directly, which we know works.

use mqtt5::broker::router::{DeliveryLanes, LaneReceivers, MessageRouter};
use mqtt5::packet::publish::PublishPacket;
use mqtt5::time::Duration;
use mqtt5::types::ProtocolVersion;
use mqtt5::QoS;
use std::sync::Arc;

async fn register_worker(router: &MessageRouter, client_id: &str) -> LaneReceivers {
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
            "$share/workers/tasks/+".to_string(),
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

#[test]
fn test_shared_subscriptions_in_turmoil() {
    let mut sim = turmoil::Builder::new()
        .simulation_duration(Duration::from_secs(10))
        .build();

    sim.host("test", || async {
        let router = Arc::new(MessageRouter::new());

        let mut rx1 = register_worker(&router, "worker1").await;
        let mut rx2 = register_worker(&router, "worker2").await;
        let mut rx3 = register_worker(&router, "worker3").await;

        for i in 0..9 {
            let publish = PublishPacket::new(
                format!("tasks/job{}", i % 3),
                format!("Task {i}").into_bytes(),
                QoS::AtMostOnce,
            );
            router.route_message(&publish, None).await;
        }

        let count1 = drain_count(&mut rx1);
        let count2 = drain_count(&mut rx2);
        let count3 = drain_count(&mut rx3);

        assert_eq!(count1 + count2 + count3, 9);
        assert!((2..=4).contains(&count1));
        assert!((2..=4).contains(&count2));
        assert!((2..=4).contains(&count3));

        Ok::<(), Box<dyn std::error::Error>>(())
    });

    sim.run().unwrap();
}
