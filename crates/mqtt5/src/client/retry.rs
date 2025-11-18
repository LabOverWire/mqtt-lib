#[cfg(test)]
mod tests {
    use crate::client::error_recovery::{ErrorRecoveryConfig, RecoverableError, RetryState};
    use crate::error::MqttError;
    use crate::time::Duration;

    #[tokio::test]
    async fn test_retry_state() {
        let mut state = RetryState::new();
        let config = ErrorRecoveryConfig {
            max_retries: 3,
            initial_retry_delay: Duration::from_millis(100),
            max_retry_delay: Duration::from_secs(5),
            backoff_factor: 2.0,
            ..Default::default()
        };

        // First attempt should be allowed
        assert!(state.should_retry(&config));
        state.record_attempt(
            MqttError::ConnectionError("test".to_string()),
            RecoverableError::NetworkError,
        );

        // Check delay calculation
        let delay1 = state.next_delay(&config);
        assert!(delay1 >= Duration::from_millis(100));
        assert!(delay1 <= Duration::from_millis(200)); // With jitter

        // Second attempt
        assert!(state.should_retry(&config));
        state.record_attempt(
            MqttError::ConnectionError("test".to_string()),
            RecoverableError::NetworkError,
        );

        let delay2 = state.next_delay(&config);
        assert!(delay2 > delay1); // Should increase with backoff

        // Third attempt
        assert!(state.should_retry(&config));
        state.record_attempt(
            MqttError::ConnectionError("test".to_string()),
            RecoverableError::NetworkError,
        );

        // Fourth attempt should fail (max_retries = 3)
        assert!(!state.should_retry(&config));
    }

    #[test]
    fn test_recoverable_error_classification() {
        let config = ErrorRecoveryConfig::default();

        // Connection errors are recoverable
        assert!(RecoverableError::is_recoverable(
            &MqttError::ConnectionError("Connection refused".to_string()),
            &config
        )
        .is_some());

        // Timeout errors are recoverable
        assert!(RecoverableError::is_recoverable(&MqttError::Timeout, &config).is_some());

        // Protocol errors are not recoverable
        assert!(RecoverableError::is_recoverable(
            &MqttError::ProtocolError("test".to_string()),
            &config
        )
        .is_none());

        // Invalid state errors are not recoverable
        let invalid_state = MqttError::InvalidState("test".to_string());
        assert!(RecoverableError::is_recoverable(&invalid_state, &config).is_none());
    }
}
