#[cfg(test)]
mod tests {
    use crate::client::error_recovery::{
        is_recoverable, ErrorRecoveryConfig, RecoverableError, RetryState,
    };
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

        assert!(state.should_retry(&config));
        state.record_attempt(
            MqttError::ConnectionError("test".to_string()),
            RecoverableError::NetworkError,
        );

        let delay1 = state.next_delay(&config);
        assert!(delay1 >= Duration::from_millis(100));
        assert!(delay1 <= Duration::from_millis(200));

        assert!(state.should_retry(&config));
        state.record_attempt(
            MqttError::ConnectionError("test".to_string()),
            RecoverableError::NetworkError,
        );

        let delay2 = state.next_delay(&config);
        assert!(delay2 > delay1);

        assert!(state.should_retry(&config));
        state.record_attempt(
            MqttError::ConnectionError("test".to_string()),
            RecoverableError::NetworkError,
        );

        assert!(!state.should_retry(&config));
    }

    #[test]
    fn test_recoverable_error_classification() {
        let config = ErrorRecoveryConfig::default();

        assert!(is_recoverable(
            &MqttError::ConnectionError("Connection refused".to_string()),
            &config
        )
        .is_some());

        assert!(is_recoverable(&MqttError::Timeout, &config).is_some());

        assert!(is_recoverable(&MqttError::ProtocolError("test".to_string()), &config).is_none());

        let invalid_state = MqttError::InvalidState("test".to_string());
        assert!(is_recoverable(&invalid_state, &config).is_none());
    }
}
