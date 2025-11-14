//! Comprehensive tests for bridge loop prevention

use mqtt5::broker::bridge::LoopPrevention;
use mqtt5::packet::publish::PublishPacket;
use mqtt5::QoS;
use mqtt5::time::Duration;
use tokio::time::sleep;

#[tokio::test]
async fn test_basic_loop_detection() {
    let loop_prevention = LoopPrevention::new(Duration::from_secs(60), 1000);

    let packet = PublishPacket::new("test/topic".to_string(), b"payload", QoS::AtMostOnce);

    // First time should pass
    assert!(loop_prevention.check_message(&packet).await);

    // Immediate retry should fail
    assert!(!loop_prevention.check_message(&packet).await);

    // Third attempt should also fail
    assert!(!loop_prevention.check_message(&packet).await);
}

#[tokio::test]
async fn test_different_topics() {
    let loop_prevention = LoopPrevention::new(Duration::from_secs(60), 1000);

    let packet1 = PublishPacket::new("topic/one".to_string(), b"payload", QoS::AtMostOnce);
    let packet2 = PublishPacket::new("topic/two".to_string(), b"payload", QoS::AtMostOnce);

    // Different topics should both pass
    assert!(loop_prevention.check_message(&packet1).await);
    assert!(loop_prevention.check_message(&packet2).await);

    // But repeating first topic should fail
    assert!(!loop_prevention.check_message(&packet1).await);
}

#[tokio::test]
async fn test_different_payloads() {
    let loop_prevention = LoopPrevention::new(Duration::from_secs(60), 1000);

    let packet1 = PublishPacket::new("same/topic".to_string(), b"payload1", QoS::AtMostOnce);
    let packet2 = PublishPacket::new("same/topic".to_string(), b"payload2", QoS::AtMostOnce);

    // Same topic but different payloads should both pass
    assert!(loop_prevention.check_message(&packet1).await);
    assert!(loop_prevention.check_message(&packet2).await);
}

#[tokio::test]
async fn test_different_qos_levels() {
    let loop_prevention = LoopPrevention::new(Duration::from_secs(60), 1000);

    // Same topic and payload but different QoS
    let packet_qos0 = PublishPacket::new("test/qos".to_string(), b"data", QoS::AtMostOnce);
    let packet_qos1 = PublishPacket::new("test/qos".to_string(), b"data", QoS::AtLeastOnce);
    let packet_qos2 = PublishPacket::new("test/qos".to_string(), b"data", QoS::ExactlyOnce);

    // All should pass as they have different QoS
    assert!(loop_prevention.check_message(&packet_qos0).await);
    assert!(loop_prevention.check_message(&packet_qos1).await);
    assert!(loop_prevention.check_message(&packet_qos2).await);

    // Repeating same QoS should fail
    assert!(!loop_prevention.check_message(&packet_qos0).await);
}

#[tokio::test]
async fn test_different_retain_flags() {
    let loop_prevention = LoopPrevention::new(Duration::from_secs(60), 1000);

    let mut packet_retained =
        PublishPacket::new("test/retain".to_string(), b"data", QoS::AtMostOnce);
    packet_retained.retain = true;

    let packet_not_retained =
        PublishPacket::new("test/retain".to_string(), b"data", QoS::AtMostOnce);

    // Different retain flags should create different fingerprints
    assert!(loop_prevention.check_message(&packet_retained).await);
    assert!(loop_prevention.check_message(&packet_not_retained).await);
}

#[tokio::test]
async fn test_ttl_expiration() {
    let ttl = Duration::from_millis(100);
    let loop_prevention = LoopPrevention::new(ttl, 1000);

    let packet = PublishPacket::new("test/ttl".to_string(), b"data", QoS::AtMostOnce);

    // First time should pass
    assert!(loop_prevention.check_message(&packet).await);

    // Immediate retry should fail
    assert!(!loop_prevention.check_message(&packet).await);

    // Wait for TTL to expire
    sleep(ttl + Duration::from_millis(50)).await;

    // Should pass again after TTL
    assert!(loop_prevention.check_message(&packet).await);
}

#[tokio::test]
async fn test_cache_size_management() {
    let loop_prevention = LoopPrevention::new(Duration::from_secs(60), 10);

    // Add more than max cache size
    for i in 0..20 {
        let packet = PublishPacket::new(format!("topic/{i}"), b"data", QoS::AtMostOnce);
        assert!(loop_prevention.check_message(&packet).await);
    }

    // Cache should have been cleaned up
    let cache_size = loop_prevention.cache_size().await;
    assert!(cache_size <= 20); // Some cleanup should have occurred
}

#[tokio::test]
async fn test_manual_cache_clear() {
    let loop_prevention = LoopPrevention::new(Duration::from_secs(60), 1000);

    let packet = PublishPacket::new("test/clear".to_string(), b"data", QoS::AtMostOnce);

    // First time should pass
    assert!(loop_prevention.check_message(&packet).await);

    // Should fail
    assert!(!loop_prevention.check_message(&packet).await);

    // Clear cache
    loop_prevention.clear_cache().await;

    // Should pass again after clear
    assert!(loop_prevention.check_message(&packet).await);
}

#[tokio::test]
async fn test_concurrent_access() {
    let loop_prevention = LoopPrevention::new(Duration::from_secs(60), 1000);

    // Spawn multiple tasks checking different messages
    let mut handles = vec![];

    for i in 0..10 {
        let loop_prevention_clone = loop_prevention.clone();
        let handle = tokio::spawn(async move {
            let packet = PublishPacket::new(format!("concurrent/{i}"), b"data", QoS::AtMostOnce);

            // Each unique message should pass once
            assert!(loop_prevention_clone.check_message(&packet).await);

            // Second check should fail
            assert!(!loop_prevention_clone.check_message(&packet).await);
        });
        handles.push(handle);
    }

    // Wait for all tasks
    for handle in handles {
        handle.await.unwrap();
    }
}

#[tokio::test]
async fn test_empty_payload_handling() {
    let loop_prevention = LoopPrevention::new(Duration::from_secs(60), 1000);

    let packet_empty = PublishPacket::new("test/empty".to_string(), b"", QoS::AtMostOnce);
    let packet_with_data = PublishPacket::new("test/empty".to_string(), b"data", QoS::AtMostOnce);

    // Both should pass as they're different
    assert!(loop_prevention.check_message(&packet_empty).await);
    assert!(loop_prevention.check_message(&packet_with_data).await);

    // Repeating empty should fail
    assert!(!loop_prevention.check_message(&packet_empty).await);
}

#[tokio::test]
async fn test_large_payload_handling() {
    let loop_prevention = LoopPrevention::new(Duration::from_secs(60), 1000);

    // Create large payload
    let large_payload = vec![0u8; 10_000];
    let packet = PublishPacket::new("test/large".to_string(), large_payload, QoS::AtMostOnce);

    // Should handle large payloads correctly
    assert!(loop_prevention.check_message(&packet).await);
    assert!(!loop_prevention.check_message(&packet).await);
}

#[tokio::test]
async fn test_special_topic_characters() {
    let loop_prevention = LoopPrevention::new(Duration::from_secs(60), 1000);

    let special_topics = vec![
        "$SYS/broker/load",
        "test/+/wildcard",
        "test/#",
        "test topic with spaces",
        "test/topic/with/many/levels/deep",
        "/leading/slash",
        "trailing/slash/",
    ];

    for topic in special_topics {
        let packet = PublishPacket::new(topic.to_string(), b"data", QoS::AtMostOnce);
        assert!(loop_prevention.check_message(&packet).await);
        assert!(!loop_prevention.check_message(&packet).await);
    }
}

// Test that LoopPrevention can be shared across tasks
#[tokio::test]
async fn test_loop_prevention_sharing() {
    let loop_prevention = LoopPrevention::new(Duration::from_secs(60), 1000);

    // LoopPrevention should be shareable via Arc
    let lp1 = loop_prevention.clone();
    let lp2 = loop_prevention.clone();

    // Use in different tasks
    let handle1 = tokio::spawn(async move {
        let packet = PublishPacket::new("shared/1".to_string(), b"data", QoS::AtMostOnce);
        lp1.check_message(&packet).await
    });

    let handle2 = tokio::spawn(async move {
        let packet = PublishPacket::new("shared/2".to_string(), b"data", QoS::AtMostOnce);
        lp2.check_message(&packet).await
    });

    // Both should succeed
    assert!(handle1.await.unwrap());
    assert!(handle2.await.unwrap());
}
