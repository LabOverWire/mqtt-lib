use crate::error::MqttError;
use crate::time::Duration;
use crate::types::ReasonCode;

/// Error recovery configuration
#[derive(Debug, Clone)]
pub struct ErrorRecoveryConfig {
    /// Enable automatic retry for recoverable errors
    pub auto_retry: bool,
    /// Maximum number of retry attempts
    pub max_retries: u32,
    /// Initial retry delay
    pub initial_retry_delay: Duration,
    /// Maximum retry delay
    pub max_retry_delay: Duration,
    /// Exponential backoff factor
    pub backoff_factor: f64,
    /// Errors that should trigger automatic retry
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
            recoverable_errors: RecoverableError::default_set(),
        }
    }
}

/// Types of errors that can be automatically recovered
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverableError {
    /// Temporary network issues
    NetworkError,
    /// Server is temporarily unavailable
    ServerUnavailable,
    /// Quota exceeded (can retry after backoff)
    QuotaExceeded,
    /// Packet ID exhausted (can retry after some IDs are freed)
    PacketIdExhausted,
    /// Flow control limit reached
    FlowControlLimited,
    /// Session taken over by another client
    SessionTakenOver,
    /// Server is shutting down
    ServerShuttingDown,
    /// [MQoQ§Error] `MQoQ` flow recoverable error (Level 2)
    MqoqFlowRecoverable,
}

impl RecoverableError {
    /// Returns the default set of recoverable errors
    #[must_use]
    pub fn default_set() -> Vec<Self> {
        vec![
            Self::NetworkError,
            Self::ServerUnavailable,
            Self::QuotaExceeded,
            Self::PacketIdExhausted,
            Self::FlowControlLimited,
            Self::MqoqFlowRecoverable,
        ]
    }

    /// Checks if an error is recoverable based on the configuration
    #[must_use]
    pub fn is_recoverable(error: &MqttError, config: &ErrorRecoveryConfig) -> Option<Self> {
        if !config.auto_retry {
            return None;
        }

        match error {
            MqttError::ConnectionError(msg) => {
                // AWS IoT-specific errors that should NOT be retried
                if msg.contains("Connection reset by peer")
                    || msg.contains("RST")
                    || msg.contains("TCP RST")
                    || msg.contains("reset by peer")
                    || msg.contains("connection limit")
                    || msg.contains("client limit")
                {
                    // These are AWS IoT's way of saying "you already have a connection"
                    return None;
                }

                // Other connection errors that may be recoverable
                if (msg.contains("temporarily unavailable")
                    || msg.contains("Connection refused")
                    || msg.contains("Network is unreachable"))
                    && config.recoverable_errors.contains(&Self::NetworkError)
                {
                    return Some(Self::NetworkError);
                }
            }
            MqttError::ConnectionRefused(reason) => match reason {
                ReasonCode::ServerUnavailable => {
                    if config.recoverable_errors.contains(&Self::ServerUnavailable) {
                        return Some(Self::ServerUnavailable);
                    }
                }
                ReasonCode::QuotaExceeded => {
                    if config.recoverable_errors.contains(&Self::QuotaExceeded) {
                        return Some(Self::QuotaExceeded);
                    }
                }
                ReasonCode::SessionTakenOver => {
                    if config.recoverable_errors.contains(&Self::SessionTakenOver) {
                        return Some(Self::SessionTakenOver);
                    }
                }
                ReasonCode::ServerShuttingDown => {
                    if config
                        .recoverable_errors
                        .contains(&Self::ServerShuttingDown)
                    {
                        return Some(Self::ServerShuttingDown);
                    }
                }
                ReasonCode::MqoqIncompletePacket
                | ReasonCode::MqoqFlowOpenIdle
                | ReasonCode::MqoqFlowCancelled => {
                    if config
                        .recoverable_errors
                        .contains(&Self::MqoqFlowRecoverable)
                    {
                        return Some(Self::MqoqFlowRecoverable);
                    }
                }
                _ => {}
            },
            MqttError::PacketIdExhausted => {
                if config.recoverable_errors.contains(&Self::PacketIdExhausted) {
                    return Some(Self::PacketIdExhausted);
                }
            }
            MqttError::FlowControlExceeded => {
                if config
                    .recoverable_errors
                    .contains(&Self::FlowControlLimited)
                {
                    return Some(Self::FlowControlLimited);
                }
            }
            MqttError::Timeout => {
                if config.recoverable_errors.contains(&Self::NetworkError) {
                    return Some(Self::NetworkError);
                }
            }
            _ => {}
        }

        None
    }

    /// Gets the recommended retry delay for this error type
    #[must_use]
    pub fn retry_delay(&self, attempt: u32, config: &ErrorRecoveryConfig) -> Duration {
        let base_delay = match self {
            Self::QuotaExceeded => config.initial_retry_delay * 10,
            Self::FlowControlLimited => config.initial_retry_delay * 2,
            Self::MqoqFlowRecoverable => config.initial_retry_delay * 3,
            _ => config.initial_retry_delay,
        };

        let delay = base_delay.mul_f64(
            config
                .backoff_factor
                .powi(attempt.try_into().unwrap_or(i32::MAX)),
        );
        delay.min(config.max_retry_delay)
    }
}

/// Retry state for tracking retry attempts
#[derive(Debug, Default)]
pub struct RetryState {
    /// Number of attempts made
    pub attempts: u32,
    /// Last error encountered
    pub last_error: Option<MqttError>,
    /// Type of recoverable error
    pub error_type: Option<RecoverableError>,
}

impl RetryState {
    /// Creates a new retry state
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a retry attempt
    pub fn record_attempt(&mut self, error: MqttError, error_type: RecoverableError) {
        self.attempts += 1;
        self.last_error = Some(error);
        self.error_type = Some(error_type);
    }

    /// Checks if retry should be attempted
    #[must_use]
    pub fn should_retry(&self, config: &ErrorRecoveryConfig) -> bool {
        self.attempts < config.max_retries
    }

    /// Gets the next retry delay
    #[must_use]
    pub fn next_delay(&self, config: &ErrorRecoveryConfig) -> Duration {
        if let Some(error_type) = self.error_type {
            error_type.retry_delay(self.attempts, config)
        } else {
            config.initial_retry_delay
        }
    }

    /// Resets the retry state
    pub fn reset(&mut self) {
        self.attempts = 0;
        self.last_error = None;
        self.error_type = None;
    }
}

/// Error callback type
pub type ErrorCallback = Box<dyn Fn(&MqttError) + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recoverable_error_classification() {
        let config = ErrorRecoveryConfig::default();

        // Network error should be recoverable
        let error = MqttError::ConnectionError("Connection refused".to_string());
        let recoverable = RecoverableError::is_recoverable(&error, &config);
        assert_eq!(recoverable, Some(RecoverableError::NetworkError));

        // Quota exceeded should be recoverable
        let error = MqttError::ConnectionRefused(ReasonCode::QuotaExceeded);
        let recoverable = RecoverableError::is_recoverable(&error, &config);
        assert_eq!(recoverable, Some(RecoverableError::QuotaExceeded));

        // Protocol error should not be recoverable
        let error = MqttError::ProtocolError("Invalid packet".to_string());
        let recoverable = RecoverableError::is_recoverable(&error, &config);
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

        // First attempt: 100ms
        assert_eq!(
            error_type.retry_delay(0, &config),
            Duration::from_millis(100)
        );

        // Second attempt: 200ms
        assert_eq!(
            error_type.retry_delay(1, &config),
            Duration::from_millis(200)
        );

        // Third attempt: 400ms
        assert_eq!(
            error_type.retry_delay(2, &config),
            Duration::from_millis(400)
        );

        // Should be capped at max_retry_delay
        assert_eq!(error_type.retry_delay(10, &config), Duration::from_secs(10));
    }

    #[test]
    fn test_retry_state() {
        let config = ErrorRecoveryConfig {
            max_retries: 3,
            ..Default::default()
        };

        let mut retry_state = RetryState::new();
        assert!(retry_state.should_retry(&config));

        // Record attempts
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
        assert!(!retry_state.should_retry(&config)); // Max retries reached

        // Reset
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

        let network_error = RecoverableError::NetworkError;
        let quota_error = RecoverableError::QuotaExceeded;

        assert_eq!(
            network_error.retry_delay(0, &config),
            Duration::from_millis(100)
        );
        assert_eq!(
            quota_error.retry_delay(0, &config),
            Duration::from_millis(1000)
        );
    }

    #[test]
    fn test_mqoq_error_classification() {
        let config = ErrorRecoveryConfig::default();

        let recoverable = RecoverableError::is_recoverable(
            &MqttError::ConnectionRefused(ReasonCode::MqoqFlowCancelled),
            &config,
        );
        assert_eq!(recoverable, Some(RecoverableError::MqoqFlowRecoverable));

        let recoverable = RecoverableError::is_recoverable(
            &MqttError::ConnectionRefused(ReasonCode::MqoqIncompletePacket),
            &config,
        );
        assert_eq!(recoverable, Some(RecoverableError::MqoqFlowRecoverable));

        let recoverable = RecoverableError::is_recoverable(
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

        let mqoq_error = RecoverableError::MqoqFlowRecoverable;
        assert_eq!(
            mqoq_error.retry_delay(0, &config),
            Duration::from_millis(300)
        );
    }
}
