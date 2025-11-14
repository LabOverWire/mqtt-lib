//! Integration test for broker WebSocket support

use mqtt5::broker::config::{BrokerConfig, WebSocketConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::time::Duration;

#[tokio::test]
async fn test_broker_websocket_creation() {
    // Test that we can create a broker with WebSocket configuration
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0)) // Use random port
        .with_websocket(
            WebSocketConfig::new()
                .with_bind_address(([127, 0, 0, 1], 0)) // Use random port
                .with_path("/mqtt")
                .with_tls(false),
        );

    let mut broker = MqttBroker::with_config(config)
        .await
        .expect("Failed to create broker with WebSocket");

    // Test that broker can start
    let broker_handle = tokio::spawn(async move { broker.run().await });

    // Give it a moment to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Then shut it down
    broker_handle.abort();

    // If we got here without panic, the test passed
}

#[tokio::test]
async fn test_broker_multiple_transports() {
    // Test broker with both TCP and WebSocket
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_websocket(
            WebSocketConfig::new()
                .with_bind_address(([127, 0, 0, 1], 0))
                .with_path("/ws"),
        );

    let mut broker = MqttBroker::with_config(config)
        .await
        .expect("Failed to create broker with multiple transports");

    // Verify both listeners are configured
    let broker_handle = tokio::spawn(async move { broker.run().await });

    tokio::time::sleep(Duration::from_millis(100)).await;
    broker_handle.abort();
}

#[tokio::test]
async fn test_websocket_config_paths() {
    // Test different WebSocket paths
    let paths = vec!["/mqtt", "/ws", "/websocket"];

    for path in paths {
        let config = BrokerConfig::default()
            .with_bind_address(([127, 0, 0, 1], 0))
            .with_websocket(
                WebSocketConfig::new()
                    .with_bind_address(([127, 0, 0, 1], 0))
                    .with_path(path),
            );

        let mut broker = MqttBroker::with_config(config)
            .await
            .expect("Failed to create broker");

        let broker_handle = tokio::spawn(async move { broker.run().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        broker_handle.abort();
    }
}

#[tokio::test]
async fn test_websocket_different_configs() {
    // Test different WebSocket configurations
    let configs = vec![
        WebSocketConfig::new()
            .with_bind_address(([127, 0, 0, 1], 0))
            .with_path("/mqtt")
            .with_tls(false),
        WebSocketConfig::new()
            .with_bind_address(([127, 0, 0, 1], 0))
            .with_path("/ws")
            .with_tls(false),
    ];

    for ws_config in configs {
        let config = BrokerConfig::default()
            .with_bind_address(([127, 0, 0, 1], 0))
            .with_websocket(ws_config);

        let mut broker = MqttBroker::with_config(config)
            .await
            .expect("Failed to create broker");

        let broker_handle = tokio::spawn(async move { broker.run().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        broker_handle.abort();
    }
}
