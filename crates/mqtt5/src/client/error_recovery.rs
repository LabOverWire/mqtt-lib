use crate::error::MqttError;
use crate::time::Duration;

pub use mqtt5_protocol::RecoverableError;

#[derive(Debug, Clone)]
pub struct ErrorRecoveryConfig {
    pub auto_retry: bool,
    pub max_retries: u32,
    pub initial_retry_delay: Duration,
    pub max_retry_delay: Duration,
    pub backoff_factor: f64,
    pub recoverable_errors: Vec<RecoverableError>,
}

impl Default for ErrorRecoveryConfig {
    fn default() -> Self {
        Self {
            auto_retry: true,
            max_retries: 3,
            initial_retry_delay: Duration::from_millis(100),
            max_retry_delay: Duration::from_secs(30),
            backoff_factor: 2.0,
            recoverable_errors: RecoverableError::default_set().to_vec(),
        }
    }
}

#[must_use]
pub fn is_recoverable(error: &MqttError, config: &ErrorRecoveryConfig) -> Option<RecoverableError> {
    if !config.auto_retry {
        return None;
    }

    let classified = error.classify()?;
    if config.recoverable_errors.contains(&classified) {
        Some(classified)
    } else {
        None
    }
}

#[must_use]
pub fn retry_delay(
    error: RecoverableError,
    attempt: u32,
    config: &ErrorRecoveryConfig,
) -> Duration {
    let base_delay = config.initial_retry_delay * error.base_delay_multiplier();

    let delay = base_delay.mul_f64(
        config
            .backoff_factor
            .powi(attempt.try_into().unwrap_or(i32::MAX)),
    );
    delay.min(config.max_retry_delay)
}

#[derive(Debug, Default)]
pub struct RetryState {
    pub attempts: u32,
    pub last_error: Option<MqttError>,
    pub error_type: Option<RecoverableError>,
}

impl RetryState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_attempt(&mut self, error: MqttError, error_type: RecoverableError) {
        self.attempts += 1;
        self.last_error = Some(error);
        self.error_type = Some(error_type);
    }

    #[must_use]
    pub fn should_retry(&self, config: &ErrorRecoveryConfig) -> bool {
        self.attempts < config.max_retries
    }

    #[must_use]
    pub fn next_delay(&self, config: &ErrorRecoveryConfig) -> Duration {
        if let Some(error_type) = self.error_type {
            retry_delay(error_type, self.attempts, config)
        } else {
            config.initial_retry_delay
        }
    }

    pub fn reset(&mut self) {
        self.attempts = 0;
        self.last_error = None;
        self.error_type = None;
    }
}

pub type ErrorCallback = Box<dyn Fn(&MqttError) + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ReasonCode;

    #[test]
    fn test_recoverable_error_classification() {
        let config = ErrorRecoveryConfig::default();

        let error = MqttError::ConnectionError("Connection refused".to_string());
        let recoverable = is_recoverable(&error, &config);
        assert_eq!(recoverable, Some(RecoverableError::NetworkError));

        let error = MqttError::ConnectionRefused(ReasonCode::QuotaExceeded);
        let recoverable = is_recoverable(&error, &config);
        assert_eq!(recoverable, Some(RecoverableError::QuotaExceeded));

        let error = MqttError::ProtocolError("Invalid packet".to_string());
        let recoverable = is_recoverable(&error, &config);
        assert_eq!(recoverable, None);
    }

    #[test]
    fn test_retry_delay_calculation() {
        let config = ErrorRecoveryConfig {
            initial_retry_delay: Duration::from_millis(100),
            max_retry_delay: Duration::from_secs(10),
            backoff_factor: 2.0,
            ..Default::default()
        };

        let error_type = RecoverableError::NetworkError;

        assert_eq!(
            retry_delay(error_type, 0, &config),
            Duration::from_millis(100)
        );

        assert_eq!(
            retry_delay(error_type, 1, &config),
            Duration::from_millis(200)
        );

        assert_eq!(
            retry_delay(error_type, 2, &config),
            Duration::from_millis(400)
        );

        assert_eq!(
            retry_delay(error_type, 10, &config),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn test_retry_state() {
        let config = ErrorRecoveryConfig {
            max_retries: 3,
            ..Default::default()
        };

        let mut retry_state = RetryState::new();
        assert!(retry_state.should_retry(&config));

        retry_state.record_attempt(
            MqttError::ConnectionError("test".to_string()),
            RecoverableError::NetworkError,
        );
        assert_eq!(retry_state.attempts, 1);
        assert!(retry_state.should_retry(&config));

        retry_state.record_attempt(
            MqttError::ConnectionError("test".to_string()),
            RecoverableError::NetworkError,
        );
        assert_eq!(retry_state.attempts, 2);
        assert!(retry_state.should_retry(&config));

        retry_state.record_attempt(
            MqttError::ConnectionError("test".to_string()),
            RecoverableError::NetworkError,
        );
        assert_eq!(retry_state.attempts, 3);
        assert!(!retry_state.should_retry(&config));

        retry_state.reset();
        assert_eq!(retry_state.attempts, 0);
        assert!(retry_state.should_retry(&config));
    }

    #[test]
    fn test_quota_exceeded_longer_delay() {
        let config = ErrorRecoveryConfig {
            initial_retry_delay: Duration::from_millis(100),
            ..Default::default()
        };

        assert_eq!(
            retry_delay(RecoverableError::NetworkError, 0, &config),
            Duration::from_millis(100)
        );
        assert_eq!(
            retry_delay(RecoverableError::QuotaExceeded, 0, &config),
            Duration::from_millis(1000)
        );
    }

    #[test]
    fn test_mqoq_error_classification() {
        let config = ErrorRecoveryConfig::default();

        let recoverable = is_recoverable(
            &MqttError::ConnectionRefused(ReasonCode::MqoqFlowCancelled),
            &config,
        );
        assert_eq!(recoverable, Some(RecoverableError::MqoqFlowRecoverable));

        let recoverable = is_recoverable(
            &MqttError::ConnectionRefused(ReasonCode::MqoqIncompletePacket),
            &config,
        );
        assert_eq!(recoverable, Some(RecoverableError::MqoqFlowRecoverable));

        let recoverable = is_recoverable(
            &MqttError::ConnectionRefused(ReasonCode::MqoqNoFlowState),
            &config,
        );
        assert_eq!(recoverable, None);
    }

    #[test]
    fn test_mqoq_error_retry_delay() {
        let config = ErrorRecoveryConfig {
            initial_retry_delay: Duration::from_millis(100),
            ..Default::default()
        };

        assert_eq!(
            retry_delay(RecoverableError::MqoqFlowRecoverable, 0, &config),
            Duration::from_millis(300)
        );
    }
}
