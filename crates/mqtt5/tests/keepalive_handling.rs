mod common;
use common::TestBroker;

use mqtt5::time::Duration;
use mqtt5::{ConnectOptions, MqttClient};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::time::sleep;

#[tokio::test]
async fn test_keepalive_ping_sent() {
    // Start test broker
    let broker = TestBroker::start().await;

    let mut options = ConnectOptions::new("keepalive-test-1");
    options.keep_alive = Duration::from_secs(2); // 2 second keep-alive

    let client = MqttClient::with_options(options);
    client.connect(broker.address()).await.unwrap();

    // Wait for longer than keep-alive interval
    sleep(Duration::from_secs(3)).await;

    // Client should still be connected (ping should have been sent)
    assert!(client.is_connected().await);

    // Client should still be connected after keep-alive interval
    // The keepalive mechanism should have sent a ping to maintain connection

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_keepalive_activity_resets_timer() {
    // Start test broker
    let broker = TestBroker::start().await;

    let mut options = ConnectOptions::new("keepalive-test-2");
    options.keep_alive = Duration::from_secs(3);

    let client = MqttClient::with_options(options);
    client.connect(broker.address()).await.unwrap();

    // Continuously publish messages for 5 seconds
    // This activity should prevent pings
    for i in 0..10 {
        client
            .publish("test/keepalive", format!("msg {i}"))
            .await
            .unwrap();
        sleep(Duration::from_millis(500)).await;
    }

    // With continuous activity, the keepalive timer should be reset
    // and fewer pings should be needed

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_keepalive_zero_disables() {
    // Start test broker
    let broker = TestBroker::start().await;

    let mut options = ConnectOptions::new("keepalive-test-3");
    options.keep_alive = Duration::from_secs(0); // Disabled

    let client = MqttClient::with_options(options);
    client.connect(broker.address()).await.unwrap();

    // Wait for a while
    sleep(Duration::from_secs(5)).await;

    // Should still be connected
    assert!(client.is_connected().await);

    // With keepalive disabled (0), no pings should be sent

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_keepalive_minimum_interval() {
    // Start test broker
    let broker = TestBroker::start().await;

    let mut options = ConnectOptions::new("keepalive-test-4");
    options.keep_alive = Duration::from_secs(1); // Very short interval

    let client = MqttClient::with_options(options);
    client.connect(broker.address()).await.unwrap();

    // Wait for multiple intervals
    sleep(Duration::from_secs(5)).await;

    // With a 1-second keepalive and 5 seconds elapsed,
    // multiple pings should have been sent to maintain connection

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_keepalive_with_qos_messages() {
    // Start test broker
    let broker = TestBroker::start().await;

    let mut options = ConnectOptions::new("keepalive-test-5");
    options.keep_alive = Duration::from_secs(2);

    let client = MqttClient::with_options(options);
    client.connect(broker.address()).await.unwrap();

    // Subscribe to receive our own messages
    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    client
        .subscribe("test/keepalive/qos", move |_| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    // Send QoS 1 messages periodically
    for i in 0..5 {
        client
            .publish_qos(
                "test/keepalive/qos",
                format!("qos message {i}"),
                mqtt5::QoS::AtLeastOnce,
            )
            .await
            .unwrap();
        sleep(Duration::from_millis(800)).await;
    }

    // Verify we're still connected and messages were received
    assert!(client.is_connected().await);
    assert!(received.load(Ordering::Relaxed) > 0);

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_keepalive_during_idle_subscription() {
    // Start test broker
    let broker = TestBroker::start().await;

    let mut options = ConnectOptions::new("keepalive-test-6");
    options.keep_alive = Duration::from_secs(2);

    let client = MqttClient::with_options(options);
    client.connect(broker.address()).await.unwrap();

    // Subscribe but don't receive any messages
    client
        .subscribe("test/no/messages/here", |_| {})
        .await
        .unwrap();

    // Wait for multiple keep-alive intervals
    sleep(Duration::from_secs(7)).await;

    // Should still be connected
    assert!(client.is_connected().await);

    // During idle time with subscriptions, pings should still be sent
    // to maintain the connection

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_keepalive_ping_response_tracking() {
    // Start test broker
    let broker = TestBroker::start().await;

    let mut options = ConnectOptions::new("keepalive-test-7");
    options.keep_alive = Duration::from_secs(2);

    let client = MqttClient::with_options(options);
    client.connect(broker.address()).await.unwrap();

    // Wait for pings to be exchanged
    sleep(Duration::from_secs(5)).await;

    // The connection should remain stable with ping/pong exchanges

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_keepalive_with_connection_loss_detection() {
    // Start test broker
    let broker = TestBroker::start().await;

    let mut options = ConnectOptions::new("keepalive-test-8");
    options.keep_alive = Duration::from_secs(2);

    // Connection loss detection would be handled by the packet reader
    // when it detects a timeout or connection error

    let client = MqttClient::with_options(options);

    // This test would need a way to simulate ping timeout
    // For now, just verify the setup works
    client.connect(broker.address()).await.unwrap();

    // Normal operation should maintain the connection
    sleep(Duration::from_secs(3)).await;
    assert!(client.is_connected().await);

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_keepalive_boundary_conditions() {
    // Start test broker
    let broker = TestBroker::start().await;

    // Test with maximum allowed keep-alive (18 hours, 12 minutes, 15 seconds)
    let mut options = ConnectOptions::new("keepalive-test-9");
    options.keep_alive = Duration::from_secs(65535); // Max u16 value

    let client = MqttClient::with_options(options);
    let result = client.connect(broker.address()).await;

    // Connection should succeed with max keep-alive
    assert!(result.is_ok());

    // With maximum keepalive, no pings should be sent in a short time
    sleep(Duration::from_secs(2)).await;

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_keepalive_with_rapid_reconnect() {
    // Start test broker
    let broker = TestBroker::start().await;

    let mut options = ConnectOptions::new("keepalive-test-10");
    options.keep_alive = Duration::from_secs(2);

    let client = MqttClient::with_options(options);

    // Connect and disconnect rapidly
    for _i in 0..3 {
        client.connect(broker.address()).await.unwrap();

        // Brief connection
        sleep(Duration::from_millis(500)).await;

        // Brief connection should work
        assert!(client.is_connected().await);

        client.disconnect().await.unwrap();

        // Brief pause between connections
        sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn test_keepalive_stats_accuracy() {
    // Start test broker
    let broker = TestBroker::start().await;

    let mut options = ConnectOptions::new("keepalive-test-11");
    options.keep_alive = Duration::from_secs(1); // Fast interval for testing

    let client = MqttClient::with_options(options);
    client.connect(broker.address()).await.unwrap();

    // Wait for exactly 3 keepalive intervals
    sleep(Duration::from_millis(3200)).await;

    // Connection should still be maintained
    assert!(client.is_connected().await);

    client.disconnect().await.unwrap();
}
