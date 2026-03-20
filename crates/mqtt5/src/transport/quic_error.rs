use mqtt5_protocol::quic_error_codes::{QuicConnectionCode, QuicStreamCode};
use std::fmt;

#[derive(Debug, Clone)]
pub enum QuicCloseReason {
    Normal,
    ApplicationError {
        code: QuicConnectionCode,
        reason: String,
    },
    UnknownApplicationError {
        raw_code: u32,
        reason: String,
    },
    TransportError(String),
    Timeout,
    Reset,
    LocallyClosed,
    VersionMismatch,
    CidsExhausted,
}

impl fmt::Display for QuicCloseReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Normal => write!(f, "normal closure"),
            Self::ApplicationError { code, reason } => {
                if reason.is_empty() {
                    write!(f, "application error: {code}")
                } else {
                    write!(f, "application error: {code} ({reason})")
                }
            }
            Self::UnknownApplicationError { raw_code, reason } => {
                if reason.is_empty() {
                    write!(f, "application error: unknown code 0x{raw_code:X}")
                } else {
                    write!(
                        f,
                        "application error: unknown code 0x{raw_code:X} ({reason})"
                    )
                }
            }
            Self::TransportError(msg) => write!(f, "transport error: {msg}"),
            Self::Timeout => write!(f, "connection timed out"),
            Self::Reset => write!(f, "connection reset"),
            Self::LocallyClosed => write!(f, "locally closed"),
            Self::VersionMismatch => write!(f, "version mismatch"),
            Self::CidsExhausted => write!(f, "CIDs exhausted"),
        }
    }
}

pub fn parse_connection_error(err: &quinn::ConnectionError) -> QuicCloseReason {
    match err {
        quinn::ConnectionError::ApplicationClosed(close) => {
            let raw_code = close.error_code.into_inner();
            #[allow(clippy::cast_possible_truncation)]
            let raw_u32 = raw_code as u32;
            let reason = String::from_utf8_lossy(&close.reason).into_owned();

            if raw_u32 == 0 {
                return QuicCloseReason::Normal;
            }

            match QuicConnectionCode::from_code(raw_u32) {
                Some(code) => QuicCloseReason::ApplicationError { code, reason },
                None => QuicCloseReason::UnknownApplicationError {
                    raw_code: raw_u32,
                    reason,
                },
            }
        }
        quinn::ConnectionError::ConnectionClosed(close) => {
            QuicCloseReason::TransportError(close.to_string())
        }
        quinn::ConnectionError::TransportError(te) => {
            QuicCloseReason::TransportError(te.to_string())
        }
        quinn::ConnectionError::TimedOut => QuicCloseReason::Timeout,
        quinn::ConnectionError::Reset => QuicCloseReason::Reset,
        quinn::ConnectionError::LocallyClosed => QuicCloseReason::LocallyClosed,
        quinn::ConnectionError::VersionMismatch => QuicCloseReason::VersionMismatch,
        quinn::ConnectionError::CidsExhausted => QuicCloseReason::CidsExhausted,
    }
}

#[derive(Debug, Clone)]
pub enum StreamResetReason {
    KnownCode(QuicStreamCode),
    UnknownCode(u32),
    ConnectionLost(QuicCloseReason),
    Closed,
}

impl fmt::Display for StreamResetReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KnownCode(code) => write!(f, "stream reset: {code}"),
            Self::UnknownCode(raw) => write!(f, "stream reset: unknown code 0x{raw:X}"),
            Self::ConnectionLost(reason) => write!(f, "connection lost: {reason}"),
            Self::Closed => write!(f, "stream closed"),
        }
    }
}

pub fn parse_read_error(err: &quinn::ReadError) -> StreamResetReason {
    match err {
        quinn::ReadError::Reset(code) => {
            let raw = code.into_inner();
            #[allow(clippy::cast_possible_truncation)]
            let raw_u32 = raw as u32;
            match QuicStreamCode::from_code(raw_u32) {
                Some(stream_code) => StreamResetReason::KnownCode(stream_code),
                None => StreamResetReason::UnknownCode(raw_u32),
            }
        }
        quinn::ReadError::ConnectionLost(conn_err) => {
            StreamResetReason::ConnectionLost(parse_connection_error(conn_err))
        }
        quinn::ReadError::ClosedStream
        | quinn::ReadError::IllegalOrderedRead
        | quinn::ReadError::ZeroRttRejected => StreamResetReason::Closed,
    }
}

#[derive(Debug, Clone)]
pub enum StreamStopReason {
    KnownCode(QuicStreamCode),
    UnknownCode(u32),
    ConnectionLost(QuicCloseReason),
    Closed,
    ZeroRttRejected,
}

impl fmt::Display for StreamStopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KnownCode(code) => write!(f, "write stopped: {code}"),
            Self::UnknownCode(raw) => write!(f, "write stopped: unknown code 0x{raw:X}"),
            Self::ConnectionLost(reason) => write!(f, "connection lost: {reason}"),
            Self::Closed => write!(f, "stream closed"),
            Self::ZeroRttRejected => write!(f, "0-RTT rejected"),
        }
    }
}

pub fn parse_write_error(err: &quinn::WriteError) -> StreamStopReason {
    match err {
        quinn::WriteError::Stopped(code) => {
            let raw = code.into_inner();
            #[allow(clippy::cast_possible_truncation)]
            let raw_u32 = raw as u32;
            match QuicStreamCode::from_code(raw_u32) {
                Some(stream_code) => StreamStopReason::KnownCode(stream_code),
                None => StreamStopReason::UnknownCode(raw_u32),
            }
        }
        quinn::WriteError::ConnectionLost(conn_err) => {
            StreamStopReason::ConnectionLost(parse_connection_error(conn_err))
        }
        quinn::WriteError::ClosedStream => StreamStopReason::Closed,
        quinn::WriteError::ZeroRttRejected => StreamStopReason::ZeroRttRejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quic_close_reason_display() {
        let normal = QuicCloseReason::Normal;
        assert_eq!(normal.to_string(), "normal closure");

        let app_err = QuicCloseReason::ApplicationError {
            code: QuicConnectionCode::Unspecified,
            reason: "test".to_string(),
        };
        assert_eq!(
            app_err.to_string(),
            "application error: ERROR_UNSPECIFIED (test)"
        );

        let app_err_no_reason = QuicCloseReason::ApplicationError {
            code: QuicConnectionCode::ProtocolLevel0,
            reason: String::new(),
        };
        assert_eq!(
            app_err_no_reason.to_string(),
            "application error: ERROR_PROTOCOL_L0"
        );

        let unknown = QuicCloseReason::UnknownApplicationError {
            raw_code: 0xFF,
            reason: String::new(),
        };
        assert_eq!(unknown.to_string(), "application error: unknown code 0xFF");
    }

    #[test]
    fn test_stream_reset_reason_display() {
        let known = StreamResetReason::KnownCode(QuicStreamCode::NoFlowState);
        assert_eq!(known.to_string(), "stream reset: ERROR_NO_FLOW_STATE");

        let unknown = StreamResetReason::UnknownCode(0xFF);
        assert_eq!(unknown.to_string(), "stream reset: unknown code 0xFF");
    }

    #[test]
    fn test_stream_stop_reason_display() {
        let known = StreamStopReason::KnownCode(QuicStreamCode::FlowCancelled);
        assert_eq!(known.to_string(), "write stopped: ERROR_FLOW_CANCELLED");

        let rejected = StreamStopReason::ZeroRttRejected;
        assert_eq!(rejected.to_string(), "0-RTT rejected");
    }

    #[test]
    fn test_parse_connection_error_locally_closed() {
        let err = quinn::ConnectionError::LocallyClosed;
        let reason = parse_connection_error(&err);
        assert!(matches!(reason, QuicCloseReason::LocallyClosed));
    }

    #[test]
    fn test_parse_connection_error_timeout() {
        let err = quinn::ConnectionError::TimedOut;
        let reason = parse_connection_error(&err);
        assert!(matches!(reason, QuicCloseReason::Timeout));
    }

    #[test]
    fn test_parse_connection_error_reset() {
        let err = quinn::ConnectionError::Reset;
        let reason = parse_connection_error(&err);
        assert!(matches!(reason, QuicCloseReason::Reset));
    }

    #[test]
    fn test_parse_read_error_reset() {
        let err = quinn::ReadError::Reset(quinn::VarInt::from_u32(0xB3));
        let reason = parse_read_error(&err);
        assert!(matches!(
            reason,
            StreamResetReason::KnownCode(QuicStreamCode::NoFlowState)
        ));
    }

    #[test]
    fn test_parse_read_error_unknown_code() {
        let err = quinn::ReadError::Reset(quinn::VarInt::from_u32(0xFF));
        let reason = parse_read_error(&err);
        assert!(matches!(reason, StreamResetReason::UnknownCode(0xFF)));
    }

    #[test]
    fn test_parse_read_error_closed() {
        let err = quinn::ReadError::ClosedStream;
        let reason = parse_read_error(&err);
        assert!(matches!(reason, StreamResetReason::Closed));
    }

    #[test]
    fn test_parse_write_error_stopped() {
        let err = quinn::WriteError::Stopped(quinn::VarInt::from_u32(0xBC));
        let reason = parse_write_error(&err);
        assert!(matches!(
            reason,
            StreamStopReason::KnownCode(QuicStreamCode::FlowCancelled)
        ));
    }

    #[test]
    fn test_parse_write_error_closed() {
        let err = quinn::WriteError::ClosedStream;
        let reason = parse_write_error(&err);
        assert!(matches!(reason, StreamStopReason::Closed));
    }
}
