#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::{BrokerConfig, MqttBroker, StorageConfig};
use mqtt5::time::Duration;
use mqtt5::{MqttClient, QoS};
use mqtt5_protocol::packet::publish::PublishPacket;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

const TOTAL: usize = 400;

async fn remote_broker() -> SocketAddr {
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_storage(StorageConfig::new().with_persistence(false));
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let addr = broker.local_addr().unwrap();
    let mut ready = broker.ready_receiver();
    tokio::spawn(async move {
        if let Err(e) = broker.run().await {
            panic!("remote broker stopped: {e}");
        }
    });
    ready.wait_for(|&is_ready| is_ready).await.unwrap();
    addr
}

async fn arrival_order(name: &str, qos: QoS, queue_capacity: usize) -> Vec<usize> {
    let remote = remote_broker().await;
    let received = Arc::new(Mutex::new(Vec::new()));
    let subscriber = MqttClient::new(format!("{name}-sub"));
    subscriber
        .connect(&format!("mqtt://{remote}"))
        .await
        .unwrap();
    let seen = Arc::clone(&received);
    subscriber
        .subscribe_with_options(
            "out/#",
            mqtt5::SubscribeOptions {
                qos,
                ..Default::default()
            },
            move |msg| {
                let index = String::from_utf8_lossy(&msg.payload).parse().unwrap();
                seen.lock().unwrap().push(index);
            },
        )
        .await
        .unwrap();

    let mut config =
        BridgeConfig::new(name, remote.to_string()).add_topic("out/#", BridgeDirection::Out, qos);
    config.retry_jitter = false;
    config.outbound_queue_capacity = queue_capacity;
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move { running.run().await });
    for _ in 0..200 {
        if bridge.get_stats().await.connected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(bridge.get_stats().await.connected);

    for i in 0..TOTAL {
        let packet = PublishPacket::new("out/x", i.to_string().into_bytes(), qos);
        bridge.forward_message(&packet);
    }
    let started = tokio::time::Instant::now();
    while started.elapsed() < Duration::from_secs(20)
        && received.lock().unwrap().last() != Some(&(TOTAL - 1))
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let _ = bridge.stop().await;
    task.abort();
    let order = received.lock().unwrap().clone();
    order
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bridged_qos_1_messages_keep_their_order() {
    let order = arrival_order("order-qos1", QoS::AtLeastOnce, 1000).await;
    assert_eq!(order, (0..TOTAL).collect::<Vec<_>>());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bridged_qos_0_messages_keep_their_order() {
    let order = arrival_order("order-qos0", QoS::AtMostOnce, 1000).await;
    assert_eq!(order, (0..TOTAL).collect::<Vec<_>>());
}

#[tokio::test]
async fn bridged_messages_keep_their_order_on_one_thread() {
    let order = arrival_order("order-current", QoS::AtLeastOnce, 1000).await;
    assert_eq!(order, (0..TOTAL).collect::<Vec<_>>());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_outbound_queue_drops_the_oldest_and_keeps_order() {
    let order = arrival_order("order-full", QoS::AtLeastOnce, 1).await;
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");
    assert_eq!(order.last(), Some(&(TOTAL - 1)), "{order:?}");
}

#[test]
fn an_empty_outbound_queue_is_refused() {
    let mut config = BridgeConfig::new("order-empty", "127.0.0.1:1").add_topic(
        "out/#",
        BridgeDirection::Out,
        QoS::AtLeastOnce,
    );
    config.outbound_queue_capacity = 0;
    assert!(BridgeConnection::new(config, Arc::new(MessageRouter::new())).is_err());
}
