use mqtt5::error::MqttError;
use mqtt5::session::flow_control::{FlowControlConfig, FlowControlManager};
use mqtt5::time::Duration;
use tokio::time::timeout;

#[tokio::test]
async fn test_flow_control_basic_functionality() {
    let fc = FlowControlManager::new(2);

    // Should be able to send initially
    assert!(fc.can_send());
    assert_eq!(fc.available_permits(), 2);

    // Acquire first quota
    fc.acquire_send_quota(1).await.unwrap();
    assert_eq!(fc.in_flight_count().await, 1);
    assert_eq!(fc.available_permits(), 1);

    // Acquire second quota
    fc.acquire_send_quota(2).await.unwrap();
    assert_eq!(fc.in_flight_count().await, 2);
    assert_eq!(fc.available_permits(), 0);
    assert!(!fc.can_send());

    // Third attempt should fail immediately
    let result = fc.try_acquire_send_quota(3).await;
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        MqttError::FlowControlExceeded
    ));
}

#[tokio::test]
async fn test_flow_control_acknowledgment() {
    let fc = FlowControlManager::new(2);

    // Fill up the quota
    fc.acquire_send_quota(1).await.unwrap();
    fc.acquire_send_quota(2).await.unwrap();
    assert!(!fc.can_send());

    // Acknowledge one message
    fc.acknowledge(1).await.unwrap();
    assert_eq!(fc.in_flight_count().await, 1);
    assert_eq!(fc.available_permits(), 1);
    assert!(fc.can_send());

    // Should be able to acquire quota again
    fc.acquire_send_quota(3).await.unwrap();
    assert!(!fc.can_send());

    // Acknowledge non-existent packet should return error
    let result = fc.acknowledge(999).await;
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        MqttError::PacketIdNotFound(999)
    ));
}

#[tokio::test]
async fn test_flow_control_unlimited() {
    let fc = FlowControlManager::new(0); // 0 means unlimited

    // Should always be able to send
    assert!(fc.can_send());

    // Can acquire many quotas
    for i in 1..=1000 {
        fc.acquire_send_quota(i).await.unwrap();
    }

    // Still unlimited
    assert!(fc.can_send());
    assert_eq!(fc.in_flight_count().await, 0); // Not tracked when unlimited
}

#[tokio::test]
async fn test_flow_control_backpressure() {
    let config = FlowControlConfig {
        enable_backpressure: true,
        backpressure_timeout: Some(Duration::from_millis(100)),
        max_pending_queue_size: 10,
        in_flight_timeout: Duration::from_secs(60),
    };

    let fc = FlowControlManager::with_config(1, config);

    // Fill up the quota
    fc.acquire_send_quota(1).await.unwrap();
    assert!(!fc.can_send());

    // Trying to acquire another quota should timeout
    let start = std::time::Instant::now();
    let result = fc.acquire_send_quota(2).await;
    let elapsed = start.elapsed();

    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        MqttError::FlowControlExceeded
    ));
    assert!(elapsed >= Duration::from_millis(100));
    assert!(elapsed < Duration::from_millis(200)); // Should be close to timeout
}

#[tokio::test]
async fn test_flow_control_backpressure_release() {
    let config = FlowControlConfig {
        enable_backpressure: true,
        backpressure_timeout: Some(Duration::from_millis(500)),
        max_pending_queue_size: 10,
        in_flight_timeout: Duration::from_secs(60),
    };

    let fc = FlowControlManager::with_config(1, config);

    // Fill up the quota
    fc.acquire_send_quota(1).await.unwrap();

    // Start a task to release quota after delay
    let fc_clone = fc.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        fc_clone.acknowledge(1).await.unwrap();
    });

    // This should succeed after the quota is released
    let start = std::time::Instant::now();
    fc.acquire_send_quota(2).await.unwrap();
    let elapsed = start.elapsed();

    assert!(elapsed >= Duration::from_millis(100));
    assert!(elapsed < Duration::from_millis(200));
}

#[tokio::test]
async fn test_flow_control_receive_maximum_adjustment() {
    let mut fc = FlowControlManager::new(2);

    // Fill up the quota
    fc.acquire_send_quota(1).await.unwrap();
    fc.acquire_send_quota(2).await.unwrap();
    assert!(!fc.can_send());

    // Increase receive maximum
    fc.set_receive_maximum(3).await;
    assert!(fc.can_send());
    assert_eq!(fc.available_permits(), 1);

    // Decrease receive maximum
    fc.set_receive_maximum(1).await;
    assert!(!fc.can_send());
    assert_eq!(fc.available_permits(), 0);

    // Set to unlimited
    fc.set_receive_maximum(0).await;
    assert!(fc.can_send());
}

#[tokio::test]
async fn test_flow_control_expired_cleanup() {
    let config = FlowControlConfig {
        enable_backpressure: true,
        backpressure_timeout: Some(Duration::from_secs(30)),
        max_pending_queue_size: 1000,
        in_flight_timeout: Duration::from_millis(100),
    };

    let fc = FlowControlManager::with_config(3, config);

    // Register some messages
    fc.acquire_send_quota(1).await.unwrap();
    fc.acquire_send_quota(2).await.unwrap();

    // Wait for them to expire
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Add a newer message
    fc.acquire_send_quota(3).await.unwrap();

    // Clean up expired messages
    let expired = fc.cleanup_expired().await;

    // Should have cleaned up the first two
    assert_eq!(expired.len(), 2);
    assert!(expired.contains(&1));
    assert!(expired.contains(&2));
    assert!(!expired.contains(&3));

    // Should have more available quota now
    assert_eq!(fc.in_flight_count().await, 1);
    assert_eq!(fc.available_permits(), 2);
}

#[tokio::test]
async fn test_flow_control_statistics() {
    let fc = FlowControlManager::new(5);

    // Register some messages
    fc.acquire_send_quota(1).await.unwrap();
    fc.acquire_send_quota(2).await.unwrap();
    fc.acquire_send_quota(3).await.unwrap();

    let stats = fc.get_stats().await;

    assert_eq!(stats.receive_maximum, 5);
    assert_eq!(stats.in_flight_count, 3);
    assert_eq!(stats.available_quota, 2);
    assert_eq!(stats.pending_requests, 0);
    assert!(stats.oldest_in_flight.is_some());
}

#[tokio::test]
async fn test_flow_control_clear() {
    let fc = FlowControlManager::new(3);

    // Register some messages
    fc.acquire_send_quota(1).await.unwrap();
    fc.acquire_send_quota(2).await.unwrap();
    assert_eq!(fc.in_flight_count().await, 2);

    // Clear all
    fc.clear().await;
    assert_eq!(fc.in_flight_count().await, 0);

    // Should still respect receive maximum
    assert_eq!(fc.available_permits(), 1); // 2 permits were consumed and not released
}

#[tokio::test]
async fn test_flow_control_concurrent_access() {
    let fc = FlowControlManager::new(10);
    let fc = std::sync::Arc::new(fc);

    // Spawn multiple tasks trying to acquire quota
    let mut handles = Vec::new();
    for i in 1..=20 {
        let fc_clone = fc.clone();
        let handle = tokio::spawn(async move { fc_clone.try_acquire_send_quota(i).await });
        handles.push(handle);
    }

    // Collect results
    let mut successes = 0;
    let mut failures = 0;

    for handle in handles {
        match handle.await.unwrap() {
            Ok(()) => successes += 1,
            Err(_) => failures += 1,
        }
    }

    // Should have exactly 10 successes (the receive maximum) and 10 failures
    assert_eq!(successes, 10);
    assert_eq!(failures, 10);
    assert_eq!(fc.in_flight_count().await, 10);
    assert_eq!(fc.available_permits(), 0);
}

#[tokio::test]
async fn test_flow_control_legacy_register_send() {
    let fc = FlowControlManager::new(2);

    // Test legacy register_send method
    fc.register_send(1).await.unwrap();
    fc.register_send(2).await.unwrap();

    // Should be at capacity
    assert!(!fc.can_send());
    assert_eq!(fc.in_flight_count().await, 2);

    // Third attempt should fail
    let result = fc.register_send(3).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_flow_control_config_variations() {
    // Test with backpressure disabled
    let config = FlowControlConfig {
        enable_backpressure: false,
        backpressure_timeout: None,
        max_pending_queue_size: 100,
        in_flight_timeout: Duration::from_secs(30),
    };

    let fc = FlowControlManager::with_config(1, config);

    // Fill quota
    fc.acquire_send_quota(1).await.unwrap();

    // Should still timeout with disabled backpressure due to semaphore behavior
    let result = timeout(Duration::from_millis(100), fc.acquire_send_quota(2)).await;
    assert!(result.is_err()); // Should timeout
}

#[tokio::test]
async fn test_flow_control_edge_cases() {
    // Test with receive_maximum = 1
    let fc = FlowControlManager::new(1);

    fc.acquire_send_quota(1).await.unwrap();
    assert!(!fc.can_send());

    // Acknowledge and immediately try to acquire again
    fc.acknowledge(1).await.unwrap();
    assert!(fc.can_send());
    fc.acquire_send_quota(2).await.unwrap();
    assert!(!fc.can_send());

    // Test maximum packet ID
    let fc = FlowControlManager::new(1);
    fc.acquire_send_quota(u16::MAX).await.unwrap();
    fc.acknowledge(u16::MAX).await.unwrap();

    // Should work fine with maximum packet ID
    assert!(fc.can_send());
}

#[tokio::test]
async fn test_flow_control_get_expired() {
    let fc = FlowControlManager::new(5);

    // Register messages
    fc.acquire_send_quota(1).await.unwrap();
    fc.acquire_send_quota(2).await.unwrap();
    fc.acquire_send_quota(3).await.unwrap();

    // Wait enough time to expire all messages
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Check what's expired with 100ms timeout (all should be expired)
    let expired = fc.get_expired(Duration::from_millis(100)).await;
    assert_eq!(expired.len(), 3);
    assert!(expired.contains(&1));
    assert!(expired.contains(&2));
    assert!(expired.contains(&3));

    // Check what's expired with very long timeout (nothing should be expired yet)
    let expired = fc.get_expired(Duration::from_secs(10)).await;
    assert_eq!(expired.len(), 0);
}
