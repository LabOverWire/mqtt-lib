use crate::error::MqttError;
use crate::protocol::v5::reason_codes::ReasonCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecoverableError {
    NetworkError,
    ServerUnavailable,
    QuotaExceeded,
    PacketIdExhausted,
    FlowControlLimited,
    SessionTakenOver,
    ServerShuttingDown,
    MqoqFlowRecoverable,
}

impl RecoverableError {
    #[must_use]
    pub fn base_delay_multiplier(&self) -> u32 {
        match self {
            Self::QuotaExceeded => 10,
            Self::MqoqFlowRecoverable => 3,
            Self::FlowControlLimited => 2,
            _ => 1,
        }
    }

    #[must_use]
    pub fn default_set() -> [Self; 6] {
        [
            Self::NetworkError,
            Self::ServerUnavailable,
            Self::QuotaExceeded,
            Self::PacketIdExhausted,
            Self::FlowControlLimited,
            Self::MqoqFlowRecoverable,
        ]
    }
}

impl MqttError {
    #[must_use]
    pub fn classify(&self) -> Option<RecoverableError> {
        match self {
            Self::ConnectionError(msg) => classify_connection_error(msg),
            Self::ConnectionRefused(reason) => classify_connection_refused(*reason),
            Self::PacketIdExhausted => Some(RecoverableError::PacketIdExhausted),
            Self::FlowControlExceeded => Some(RecoverableError::FlowControlLimited),
            Self::Timeout => Some(RecoverableError::NetworkError),
            Self::ServerUnavailable | Self::ServerBusy => Some(RecoverableError::ServerUnavailable),
            Self::QuotaExceeded => Some(RecoverableError::QuotaExceeded),
            Self::ServerShuttingDown => Some(RecoverableError::ServerShuttingDown),
            Self::SessionExpired => Some(RecoverableError::SessionTakenOver),
            _ => None,
        }
    }

    #[must_use]
    pub fn is_aws_iot_connection_limit(&self) -> bool {
        match self {
            Self::ConnectionError(msg) => is_aws_iot_limit_error(msg),
            _ => false,
        }
    }
}

fn classify_connection_error(msg: &str) -> Option<RecoverableError> {
    if is_aws_iot_limit_error(msg) {
        return None;
    }

    if msg.contains("temporarily unavailable")
        || msg.contains("Connection refused")
        || msg.contains("Network is unreachable")
        || msg.contains("connection reset")
        || msg.contains("broken pipe")
        || msg.contains("timed out")
    {
        return Some(RecoverableError::NetworkError);
    }

    None
}

fn is_aws_iot_limit_error(msg: &str) -> bool {
    msg.contains("Connection reset by peer")
        || msg.contains("RST")
        || msg.contains("TCP RST")
        || msg.contains("reset by peer")
        || msg.contains("connection limit")
        || msg.contains("client limit")
}

fn classify_connection_refused(reason: ReasonCode) -> Option<RecoverableError> {
    match reason {
        ReasonCode::ServerUnavailable | ReasonCode::ServerBusy => {
            Some(RecoverableError::ServerUnavailable)
        }
        ReasonCode::QuotaExceeded => Some(RecoverableError::QuotaExceeded),
        ReasonCode::SessionTakenOver => Some(RecoverableError::SessionTakenOver),
        ReasonCode::ServerShuttingDown => Some(RecoverableError::ServerShuttingDown),
        ReasonCode::MqoqIncompletePacket
        | ReasonCode::MqoqFlowOpenIdle
        | ReasonCode::MqoqFlowCancelled => Some(RecoverableError::MqoqFlowRecoverable),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_error_classification() {
        let error = MqttError::ConnectionError("Connection refused".to_string());
        assert_eq!(error.classify(), Some(RecoverableError::NetworkError));

        let error = MqttError::ConnectionError("Network is unreachable".to_string());
        assert_eq!(error.classify(), Some(RecoverableError::NetworkError));

        let error = MqttError::ConnectionError("temporarily unavailable".to_string());
        assert_eq!(error.classify(), Some(RecoverableError::NetworkError));
    }

    #[test]
    fn test_aws_iot_limit_not_recoverable() {
        let error = MqttError::ConnectionError("Connection reset by peer".to_string());
        assert_eq!(error.classify(), None);
        assert!(error.is_aws_iot_connection_limit());

        let error = MqttError::ConnectionError("TCP RST received".to_string());
        assert_eq!(error.classify(), None);
        assert!(error.is_aws_iot_connection_limit());

        let error = MqttError::ConnectionError("client limit exceeded".to_string());
        assert_eq!(error.classify(), None);
        assert!(error.is_aws_iot_connection_limit());
    }

    #[test]
    fn test_connection_refused_classification() {
        let error = MqttError::ConnectionRefused(ReasonCode::ServerUnavailable);
        assert_eq!(error.classify(), Some(RecoverableError::ServerUnavailable));

        let error = MqttError::ConnectionRefused(ReasonCode::QuotaExceeded);
        assert_eq!(error.classify(), Some(RecoverableError::QuotaExceeded));

        let error = MqttError::ConnectionRefused(ReasonCode::SessionTakenOver);
        assert_eq!(error.classify(), Some(RecoverableError::SessionTakenOver));

        let error = MqttError::ConnectionRefused(ReasonCode::BadUsernameOrPassword);
        assert_eq!(error.classify(), None);
    }

    #[test]
    fn test_mqoq_classification() {
        let error = MqttError::ConnectionRefused(ReasonCode::MqoqIncompletePacket);
        assert_eq!(
            error.classify(),
            Some(RecoverableError::MqoqFlowRecoverable)
        );

        let error = MqttError::ConnectionRefused(ReasonCode::MqoqFlowOpenIdle);
        assert_eq!(
            error.classify(),
            Some(RecoverableError::MqoqFlowRecoverable)
        );

        let error = MqttError::ConnectionRefused(ReasonCode::MqoqFlowCancelled);
        assert_eq!(
            error.classify(),
            Some(RecoverableError::MqoqFlowRecoverable)
        );

        let error = MqttError::ConnectionRefused(ReasonCode::MqoqNoFlowState);
        assert_eq!(error.classify(), None);
    }

    #[test]
    fn test_direct_error_classification() {
        assert_eq!(
            MqttError::PacketIdExhausted.classify(),
            Some(RecoverableError::PacketIdExhausted)
        );
        assert_eq!(
            MqttError::FlowControlExceeded.classify(),
            Some(RecoverableError::FlowControlLimited)
        );
        assert_eq!(
            MqttError::Timeout.classify(),
            Some(RecoverableError::NetworkError)
        );
        assert_eq!(
            MqttError::ServerUnavailable.classify(),
            Some(RecoverableError::ServerUnavailable)
        );
        assert_eq!(
            MqttError::ServerBusy.classify(),
            Some(RecoverableError::ServerUnavailable)
        );
        assert_eq!(
            MqttError::QuotaExceeded.classify(),
            Some(RecoverableError::QuotaExceeded)
        );
        assert_eq!(
            MqttError::ServerShuttingDown.classify(),
            Some(RecoverableError::ServerShuttingDown)
        );
    }

    #[test]
    fn test_non_recoverable_errors() {
        assert_eq!(MqttError::NotConnected.classify(), None);
        assert_eq!(MqttError::AlreadyConnected.classify(), None);
        assert_eq!(MqttError::AuthenticationFailed.classify(), None);
        assert_eq!(MqttError::NotAuthorized.classify(), None);
        assert_eq!(MqttError::BadUsernameOrPassword.classify(), None);
        assert_eq!(
            MqttError::ProtocolError("test".to_string()).classify(),
            None
        );
    }

    #[test]
    fn test_base_delay_multiplier() {
        assert_eq!(RecoverableError::NetworkError.base_delay_multiplier(), 1);
        assert_eq!(
            RecoverableError::ServerUnavailable.base_delay_multiplier(),
            1
        );
        assert_eq!(RecoverableError::QuotaExceeded.base_delay_multiplier(), 10);
        assert_eq!(
            RecoverableError::FlowControlLimited.base_delay_multiplier(),
            2
        );
        assert_eq!(
            RecoverableError::MqoqFlowRecoverable.base_delay_multiplier(),
            3
        );
    }

    #[test]
    fn test_default_set() {
        let defaults = RecoverableError::default_set();
        assert_eq!(defaults.len(), 6);
        assert!(defaults.contains(&RecoverableError::NetworkError));
        assert!(defaults.contains(&RecoverableError::ServerUnavailable));
        assert!(defaults.contains(&RecoverableError::QuotaExceeded));
        assert!(defaults.contains(&RecoverableError::PacketIdExhausted));
        assert!(defaults.contains(&RecoverableError::FlowControlLimited));
        assert!(defaults.contains(&RecoverableError::MqoqFlowRecoverable));
        assert!(!defaults.contains(&RecoverableError::SessionTakenOver));
        assert!(!defaults.contains(&RecoverableError::ServerShuttingDown));
    }
}
