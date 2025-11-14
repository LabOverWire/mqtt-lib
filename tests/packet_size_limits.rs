use mqtt5::error::MqttError;
use mqtt5::session::limits::{ExpiringMessage, LimitsConfig, LimitsManager};
use mqtt5::session::queue::MessageQueue;
use mqtt5::QoS;
use std::thread;
use mqtt5::time::Duration;

#[test]
fn test_packet_size_validation() {
    let config = LimitsConfig {
        client_maximum_packet_size: 1024,      // 1KB limit
        server_maximum_packet_size: Some(512), // Server limit lower
        ..Default::default()
    };
    let limits = LimitsManager::new(config);

    // Should use the lower limit (server's)
    assert_eq!(limits.effective_maximum_packet_size(), 512);

    // Within limits
    assert!(limits.check_packet_size(256).is_ok());
    assert!(limits.check_packet_size(512).is_ok());

    // Exceeds limits
    let result = limits.check_packet_size(1024);
    assert!(result.is_err());
    if let Err(MqttError::PacketTooLarge { size, max }) = result {
        assert_eq!(size, 1024);
        assert_eq!(max, 512);
    } else {
        panic!("Expected PacketTooLarge error");
    }
}

#[test]
fn test_message_expiry_calculation() {
    let config = LimitsConfig {
        default_message_expiry: Some(Duration::from_secs(60)),
        max_message_expiry: Some(Duration::from_secs(300)), // 5 min max
        ..Default::default()
    };
    let limits = LimitsManager::new(config);

    // Explicit expiry within max
    let expiry_time = limits.calculate_message_expiry(Some(120));
    assert!(expiry_time.is_some());

    // Explicit expiry exceeding max (should be capped)
    let expiry_time = limits.calculate_message_expiry(Some(600)); // 10 min
    assert!(expiry_time.is_some());
    let remaining = limits.get_remaining_expiry(expiry_time);
    assert!(remaining.unwrap() <= 300);

    // Default expiry
    let expiry_time = limits.calculate_message_expiry(None);
    assert!(expiry_time.is_some());
    let remaining = limits.get_remaining_expiry(expiry_time);
    assert!(remaining.unwrap() > 55 && remaining.unwrap() <= 60);
}

#[test]
fn test_expiring_message_in_queue() {
    let mut queue = MessageQueue::new(10, 1024);
    let config = LimitsConfig {
        default_message_expiry: Some(Duration::from_millis(100)),
        ..Default::default()
    };
    let limits = LimitsManager::new(config);

    // Add messages with different expiry times
    let msg1 = ExpiringMessage::new(
        "test/1".to_string(),
        vec![1, 2, 3],
        QoS::AtLeastOnce,
        false,
        Some(1),
        Some(1), // 1 second expiry
        &limits,
    );

    let msg2 = ExpiringMessage::new(
        "test/2".to_string(),
        vec![4, 5, 6],
        QoS::AtLeastOnce,
        false,
        Some(2),
        None, // Use default (100ms)
        &limits,
    );

    queue.enqueue(msg1).unwrap();
    queue.enqueue(msg2).unwrap();
    assert_eq!(queue.len(), 2);

    // Wait for second message to expire
    thread::sleep(Duration::from_millis(150));

    // Dequeue should return first message (still valid)
    let dequeued = queue.dequeue().unwrap();
    assert_eq!(dequeued.topic, "test/1"); // First message still valid

    // Try to dequeue again - should return None as second message is expired
    let dequeued2 = queue.dequeue();
    assert!(dequeued2.is_none());
    assert_eq!(queue.len(), 0); // Both messages removed
}

#[test]
fn test_queue_with_mixed_expiry() {
    let mut queue = MessageQueue::new(10, 1024);
    let limits = LimitsManager::with_defaults();

    // Add non-expiring message
    let msg1 = ExpiringMessage::new(
        "test/permanent".to_string(),
        vec![1, 2, 3],
        QoS::AtMostOnce,
        false,
        None,
        None, // No expiry
        &limits,
    );

    // Add expiring message
    let config = LimitsConfig {
        default_message_expiry: Some(Duration::from_millis(50)),
        ..Default::default()
    };
    let limits_with_expiry = LimitsManager::new(config);

    let msg2 = ExpiringMessage::new(
        "test/expiring".to_string(),
        vec![4, 5, 6],
        QoS::AtMostOnce,
        false,
        None,
        Some(0), // Expire immediately
        &limits_with_expiry,
    );

    queue.enqueue(msg1).unwrap();
    queue.enqueue(msg2).unwrap();

    // Sleep to ensure expiry
    thread::sleep(Duration::from_millis(10));

    // Should only get non-expiring message
    let messages = queue.dequeue_batch(10);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].topic, "test/permanent");
}

#[test]
fn test_remaining_expiry_interval() {
    let limits = LimitsManager::with_defaults();

    let mut msg = ExpiringMessage::new(
        "test".to_string(),
        vec![1, 2, 3],
        QoS::AtLeastOnce,
        false,
        Some(1),
        Some(60), // 60 seconds
        &limits,
    );

    // Check remaining interval is close to original
    let remaining = msg.remaining_expiry_interval();
    assert!(remaining.is_some());
    assert!(remaining.unwrap() > 55 && remaining.unwrap() <= 60);

    // Test expired message
    msg.expiry_time = Some(
        std::time::Instant::now()
            .checked_sub(Duration::from_secs(10))
            .unwrap(),
    );
    assert!(msg.is_expired());
    assert_eq!(msg.remaining_expiry_interval(), Some(0));
}

#[test]
fn test_zero_packet_size_limit() {
    let config = LimitsConfig {
        client_maximum_packet_size: 0,       // No limit
        server_maximum_packet_size: Some(0), // No limit
        ..Default::default()
    };
    let limits = LimitsManager::new(config);

    // Should allow any size when limit is 0
    assert!(limits.check_packet_size(1_000_000).is_ok());
    assert!(limits.check_packet_size(10_000_000).is_ok());
}
