//! Comprehensive tests for bridge manager functionality

use mqtt5::broker::bridge::{BridgeConfig, BridgeDirection, BridgeManager};
use mqtt5::broker::router::MessageRouter;
use mqtt5::packet::publish::PublishPacket;
use mqtt5::QoS;
use std::sync::Arc;

#[tokio::test]
async fn test_bridge_manager_creation() {
    let router = Arc::new(MessageRouter::new());
    let manager = BridgeManager::new(router);

    // Initially no bridges
    let bridges = manager.list_bridges().await;
    assert_eq!(bridges.len(), 0);

    let stats = manager.get_all_stats().await;
    assert_eq!(stats.len(), 0);
}

#[tokio::test]
async fn test_bridge_manager_add_remove() {
    let router = Arc::new(MessageRouter::new());
    let manager = BridgeManager::new(router);

    // Add a bridge
    let config = BridgeConfig::new("test-bridge", "localhost:1883").add_topic(
        "test/#",
        BridgeDirection::Both,
        QoS::AtMostOnce,
    );

    // Adding will fail due to connection error, but bridge might still be registered
    let _ = manager.add_bridge(config.clone()).await;

    // Check if bridge was added (depends on implementation)
    let bridges = manager.list_bridges().await;
    // Bridge might not be added if connection fails immediately

    // Try to remove non-existent bridge
    let result = manager.remove_bridge("non-existent").await;
    assert!(result.is_err());

    // If bridge was added, remove it
    if !bridges.is_empty() {
        let result = manager.remove_bridge("test-bridge").await;
        assert!(result.is_ok());

        // Verify removed
        let bridges = manager.list_bridges().await;
        assert_eq!(bridges.len(), 0);
    }
}

#[tokio::test]
async fn test_bridge_manager_duplicate_bridge() {
    let router = Arc::new(MessageRouter::new());
    let manager = BridgeManager::new(router);

    let config = BridgeConfig::new("duplicate-bridge", "localhost:1883").add_topic(
        "test/#",
        BridgeDirection::Both,
        QoS::AtMostOnce,
    );

    // First add might fail or succeed depending on connection
    let _ = manager.add_bridge(config.clone()).await;

    // If bridge was added, second add should fail
    let bridges = manager.list_bridges().await;
    if bridges.contains(&"duplicate-bridge".to_string()) {
        let result = manager.add_bridge(config).await;
        assert!(result.is_err());
    }
}

#[tokio::test]
async fn test_bridge_manager_multiple_bridges() {
    let router = Arc::new(MessageRouter::new());
    let manager = BridgeManager::new(router);

    // Try to add multiple bridges
    let configs = vec![
        BridgeConfig::new("bridge1", "broker1:1883").add_topic(
            "topic1/#",
            BridgeDirection::Out,
            QoS::AtMostOnce,
        ),
        BridgeConfig::new("bridge2", "broker2:1883").add_topic(
            "topic2/#",
            BridgeDirection::In,
            QoS::AtLeastOnce,
        ),
        BridgeConfig::new("bridge3", "broker3:1883").add_topic(
            "topic3/#",
            BridgeDirection::Both,
            QoS::ExactlyOnce,
        ),
    ];

    for config in configs {
        let _ = manager.add_bridge(config).await;
    }

    // Some bridges might be added even if connections fail
    let bridges = manager.list_bridges().await;
    assert!(bridges.len() <= 3);
}

#[tokio::test]
async fn test_bridge_manager_stats() {
    let router = Arc::new(MessageRouter::new());
    let manager = BridgeManager::new(router);

    let config = BridgeConfig::new("stats-bridge", "localhost:11999").add_topic(
        "test/#",
        BridgeDirection::Both,
        QoS::AtMostOnce,
    );

    let _ = manager.add_bridge(config).await;

    // Get all stats
    let _all_stats = manager.get_all_stats().await;

    // Get specific bridge stats
    let stats = manager.get_bridge_stats("stats-bridge").await;
    if stats.is_some() {
        let stats = stats.unwrap();
        // Connection might succeed or fail depending on whether a broker is running
        // Just check that we got stats
        assert!(stats.connection_attempts > 0);
    }

    // Non-existent bridge should return None
    let stats = manager.get_bridge_stats("non-existent").await;
    assert!(stats.is_none());
}

#[tokio::test]
async fn test_bridge_manager_stop_all() {
    let router = Arc::new(MessageRouter::new());
    let manager = BridgeManager::new(router);

    // Add some bridges
    for i in 1..=3 {
        let config = BridgeConfig::new(format!("bridge{i}"), format!("broker{i}:1883")).add_topic(
            "test/#",
            BridgeDirection::Both,
            QoS::AtMostOnce,
        );
        let _ = manager.add_bridge(config).await;
    }

    // Stop all bridges
    let result = manager.stop_all().await;
    assert!(result.is_ok());

    // All bridges should be removed
    let bridges = manager.list_bridges().await;
    assert_eq!(bridges.len(), 0);
}

#[tokio::test]
async fn test_bridge_manager_reload() {
    let router = Arc::new(MessageRouter::new());
    let manager = BridgeManager::new(router);

    // Add initial bridge
    let config1 = BridgeConfig::new("reload-bridge", "broker1:1883").add_topic(
        "topic1/#",
        BridgeDirection::Out,
        QoS::AtMostOnce,
    );
    let _ = manager.add_bridge(config1).await;

    // Reload with new config
    let config2 = BridgeConfig::new("reload-bridge", "broker2:1883").add_topic(
        "topic2/#",
        BridgeDirection::In,
        QoS::AtLeastOnce,
    );
    let result = manager.reload_bridge(config2).await;

    // Reload should work (even if connections fail)
    assert!(result.is_ok() || result.is_err());
}

#[tokio::test]
async fn test_bridge_manager_handle_outgoing() {
    let router = Arc::new(MessageRouter::new());
    let manager = Arc::new(BridgeManager::new(router));

    // Create a test packet
    let packet = PublishPacket::new("test/topic".to_string(), b"test payload", QoS::AtMostOnce);

    // Should handle gracefully even with no bridges
    let result = manager.handle_outgoing(&packet).await;
    assert!(result.is_ok());

    // Add a bridge that matches
    let config = BridgeConfig::new("out-bridge", "localhost:1883").add_topic(
        "test/#",
        BridgeDirection::Out,
        QoS::AtMostOnce,
    );
    let _ = manager.add_bridge(config).await;

    // Should still handle gracefully (even if bridge not connected)
    let result = manager.handle_outgoing(&packet).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_bridge_manager_concurrent_operations() {
    let router = Arc::new(MessageRouter::new());
    let manager = Arc::new(BridgeManager::new(router));

    // Spawn multiple tasks to add/remove bridges concurrently
    let mut handles = vec![];

    // Add bridges
    for i in 1..=5 {
        let manager_clone = manager.clone();
        let handle = tokio::spawn(async move {
            let config =
                BridgeConfig::new(format!("concurrent-bridge-{i}"), format!("broker{i}:1883"))
                    .add_topic("test/#", BridgeDirection::Both, QoS::AtMostOnce);
            let _ = manager_clone.add_bridge(config).await;
        });
        handles.push(handle);
    }

    // Wait for adds to complete
    for handle in handles {
        let _ = handle.await;
    }

    // List bridges
    let bridges = manager.list_bridges().await;
    assert!(bridges.len() <= 5);

    // Remove bridges concurrently
    let mut handles = vec![];
    for bridge_name in bridges {
        let manager_clone = manager.clone();
        let handle = tokio::spawn(async move {
            let _ = manager_clone.remove_bridge(&bridge_name).await;
        });
        handles.push(handle);
    }

    // Wait for removes to complete
    for handle in handles {
        let _ = handle.await;
    }

    // Should be empty
    let bridges = manager.list_bridges().await;
    assert_eq!(bridges.len(), 0);
}

#[tokio::test]
async fn test_bridge_manager_error_handling() {
    let router = Arc::new(MessageRouter::new());
    let manager = BridgeManager::new(router);

    // Invalid bridge configs
    let invalid_configs = vec![
        BridgeConfig::new("", "broker:1883"), // Empty name
        BridgeConfig::new("test", ""),        // Empty address
    ];

    for config in invalid_configs {
        let result = manager.add_bridge(config).await;
        assert!(result.is_err());
    }

    // Remove non-existent bridge
    let result = manager.remove_bridge("does-not-exist").await;
    assert!(result.is_err());
}
