use crate::error::MqttError;
use crate::protocol::v5::reason_codes::ReasonCode;

/// A transient fault for which retrying the *same* operation after a delay can
/// succeed on its own, with no state change or user intervention.
///
/// This is the retry/backoff machinery's notion of "recoverable", not a general
/// judgement about whether the failure is the caller's fault. An error is
/// recoverable here only if the fix is "wait, then try again": the operation's
/// preconditions still hold and something outside the caller's control is
/// expected to clear. Faults whose remedy is a state transition (re-establishing
/// a connection), a different request (smaller payload), or human action
/// (fixing credentials) are deliberately *not* recoverable, because a naive
/// backoff loop on them would spin forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecoverableError {
    /// Transient network fault (timeout, reset, unreachable). Retry after backoff.
    NetworkError,
    /// Server temporarily cannot serve the request. Retry after backoff.
    ServerUnavailable,
    /// Server-side quota hit. Retry after a longer backoff (see `base_delay_multiplier`).
    QuotaExceeded,
    /// No packet identifiers currently free; one frees as in-flight messages ack.
    PacketIdExhausted,
    /// Receive-maximum flow-control limit reached; capacity returns as acks arrive.
    FlowControlLimited,
    /// Session was taken over by another client using the same client id.
    SessionTakenOver,
    /// Server is shutting down; reconnect will land on a fresh instance.
    ServerShuttingDown,
    /// Recoverable `MQoQ` flow condition (idle/cancelled/refused flow).
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
    /// Classify this error for the retry/backoff layer.
    ///
    /// Returns `Some(kind)` when retrying the *same* operation after a delay can
    /// succeed on its own (see [`RecoverableError`]), and `None` when it cannot.
    ///
    /// `None` covers three distinct cases, all of which a blind backoff loop
    /// would handle wrong:
    ///
    /// - **Precondition errors** such as [`MqttError::NotConnected`]. These are
    ///   returned whenever there is no active connection, which covers both a
    ///   dropped link and calling `publish`/`subscribe` before `connect` or
    ///   after `disconnect`. The remedy is re-establishing the connection, a
    ///   state transition owned by the reconnect layer, not re-issuing the same
    ///   packet. For a `QoS` 1 publish that failed because the link dropped, the
    ///   intended recovery is reconnect plus resend of unacked messages on the
    ///   next session, not treating the raw `NotConnected` as retryable here.
    /// - **Caller/permanent errors** such as authentication failures, bad
    ///   credentials, authorization denials, and protocol errors. Retrying is
    ///   futile until the caller changes the request or their configuration.
    /// - **Connection-limit signals** carried in [`MqttError::ConnectionError`]
    ///   strings (see [`MqttError::is_aws_iot_connection_limit`]). These look
    ///   like network resets but indicate the account/client limit was hit;
    ///   immediate retry makes it worse, so they are excluded on purpose.
    ///
    /// This is intentionally conservative: a variant only classifies as
    /// recoverable when re-issuing the identical operation is the correct
    /// response. Anything requiring a reconnect, a different request, or human
    /// intervention returns `None`.
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
        | ReasonCode::MqoqFlowCancelled
        | ReasonCode::MqoqFlowPacketCancelled
        | ReasonCode::MqoqFlowRefused => Some(RecoverableError::MqoqFlowRecoverable),
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
