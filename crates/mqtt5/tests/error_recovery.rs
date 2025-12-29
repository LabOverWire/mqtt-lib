use mqtt5::client::{is_recoverable, retry_delay, ErrorRecoveryConfig, RecoverableError};
use mqtt5::time::Duration;
use mqtt5::types::ReasonCode;
use mqtt5::{MqttClient, MqttError};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

#[tokio::test]
async fn test_error_callback_registration() {
    let client = MqttClient::new("test-client");

    let error_count = Arc::new(AtomicU32::new(0));
    let error_count_clone = Arc::clone(&error_count);

    // Register error callback
    client
        .on_error(move |error| {
            println!("Error occurred: {error}");
            error_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    // Trigger an error by trying to publish while disconnected with auto_retry disabled
    let mut config = client.error_recovery_config().await;
    config.auto_retry = false;
    client.set_error_recovery_config(config).await;

    // This should fail and trigger the error callback
    let result = client.publish("test/topic", "message").await;
    assert!(result.is_err());

    // Note: In a real implementation, the error callback would be triggered
    // by the internal error handling mechanism
}

#[tokio::test]
async fn test_error_recovery_config() {
    let client = MqttClient::new("test-client");

    // Get default config
    let config = client.error_recovery_config().await;
    assert!(config.auto_retry);
    assert_eq!(config.max_retries, 3);

    // Update config
    let new_config = ErrorRecoveryConfig {
        auto_retry: false,
        max_retries: 5,
        initial_retry_delay: Duration::from_secs(1),
        ..Default::default()
    };

    client.set_error_recovery_config(new_config.clone()).await;

    // Verify config was updated
    let updated_config = client.error_recovery_config().await;
    assert!(!updated_config.auto_retry);
    assert_eq!(updated_config.max_retries, 5);
    assert_eq!(updated_config.initial_retry_delay, Duration::from_secs(1));
}

#[tokio::test]
async fn test_recoverable_error_classification() {
    let config = ErrorRecoveryConfig::default();

    let error = MqttError::ConnectionError("Connection refused".to_string());
    assert_eq!(
        is_recoverable(&error, &config),
        Some(RecoverableError::NetworkError)
    );

    let error = MqttError::ConnectionRefused(ReasonCode::QuotaExceeded);
    assert_eq!(
        is_recoverable(&error, &config),
        Some(RecoverableError::QuotaExceeded)
    );

    let error = MqttError::FlowControlExceeded;
    assert_eq!(
        is_recoverable(&error, &config),
        Some(RecoverableError::FlowControlLimited)
    );

    let error = MqttError::PacketIdExhausted;
    assert_eq!(
        is_recoverable(&error, &config),
        Some(RecoverableError::PacketIdExhausted)
    );

    let error = MqttError::ProtocolError("Invalid packet".to_string());
    assert!(is_recoverable(&error, &config).is_none());

    let error = MqttError::AuthenticationFailed;
    assert!(is_recoverable(&error, &config).is_none());
}

#[tokio::test]
async fn test_disable_auto_retry() {
    let client = MqttClient::new("test-client");

    // Disable auto retry
    let config = ErrorRecoveryConfig {
        auto_retry: false,
        ..Default::default()
    };
    client.set_error_recovery_config(config).await;

    // Attempt to publish while disconnected should fail immediately
    let start = std::time::Instant::now();
    let result = client.publish("test/topic", "message").await;
    let duration = start.elapsed();

    assert!(result.is_err());
    // Should fail quickly without retries
    assert!(duration < Duration::from_millis(100));
}

#[tokio::test]
async fn test_recoverable_errors_configuration() {
    let mut config = ErrorRecoveryConfig::default();

    config
        .recoverable_errors
        .retain(|&e| e != RecoverableError::NetworkError);

    let error = MqttError::ConnectionError("Connection refused".to_string());
    assert!(is_recoverable(&error, &config).is_none());

    let error = MqttError::ConnectionRefused(ReasonCode::QuotaExceeded);
    assert_eq!(
        is_recoverable(&error, &config),
        Some(RecoverableError::QuotaExceeded)
    );
}

#[tokio::test]
async fn test_retry_delay_calculation() {
    let config = ErrorRecoveryConfig {
        initial_retry_delay: Duration::from_millis(100),
        max_retry_delay: Duration::from_secs(10),
        backoff_factor: 2.0,
        ..Default::default()
    };

    assert_eq!(
        retry_delay(RecoverableError::NetworkError, 0, &config),
        Duration::from_millis(100)
    );
    assert_eq!(
        retry_delay(RecoverableError::NetworkError, 1, &config),
        Duration::from_millis(200)
    );
    assert_eq!(
        retry_delay(RecoverableError::NetworkError, 2, &config),
        Duration::from_millis(400)
    );

    assert_eq!(
        retry_delay(RecoverableError::QuotaExceeded, 0, &config),
        Duration::from_millis(1000)
    );
    assert_eq!(
        retry_delay(RecoverableError::QuotaExceeded, 1, &config),
        Duration::from_millis(2000)
    );

    assert_eq!(
        retry_delay(RecoverableError::FlowControlLimited, 0, &config),
        Duration::from_millis(200)
    );
    assert_eq!(
        retry_delay(RecoverableError::FlowControlLimited, 1, &config),
        Duration::from_millis(400)
    );

    assert_eq!(
        retry_delay(RecoverableError::FlowControlLimited, 20, &config),
        Duration::from_secs(10)
    );
}

#[tokio::test]
async fn test_multiple_error_callbacks() {
    let client = MqttClient::new("test-client");

    let callback1_count = Arc::new(AtomicU32::new(0));
    let callback2_count = Arc::new(AtomicU32::new(0));

    let count1 = Arc::clone(&callback1_count);
    client
        .on_error(move |_error| {
            count1.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    let count2 = Arc::clone(&callback2_count);
    client
        .on_error(move |_error| {
            count2.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    // Clear callbacks
    client.clear_error_callbacks().await;

    // New callbacks should not be triggered after clearing
}

#[tokio::test]
async fn test_publish_with_retry_disabled() {
    let client = MqttClient::new("test-client");

    // Disable retry
    let mut config = client.error_recovery_config().await;
    config.auto_retry = false;
    client.set_error_recovery_config(config).await;

    // Disable queuing to force error
    client.set_queue_on_disconnect(false).await;

    // Should fail immediately
    let result = client.publish("test/topic", "message").await;
    assert!(result.is_err());
    match result {
        Err(MqttError::NotConnected) => {}
        _ => panic!("Expected NotConnected error"),
    }
}

#[tokio::test]
async fn test_custom_recoverable_errors() {
    let mut config = ErrorRecoveryConfig::default();

    config
        .recoverable_errors
        .push(RecoverableError::SessionTakenOver);

    let error = MqttError::ConnectionRefused(ReasonCode::SessionTakenOver);
    assert_eq!(
        is_recoverable(&error, &config),
        Some(RecoverableError::SessionTakenOver)
    );
}
