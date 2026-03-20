use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuicConnectionCode {
    NoError = 0x00,
    TlsError = 0xB1,
    Unspecified = 0xB2,
    TooManyRecoverAttempts = 0xB3,
    ProtocolLevel0 = 0xB4,
}

impl QuicConnectionCode {
    #[must_use]
    pub fn code(self) -> u32 {
        self as u32
    }

    #[must_use]
    pub fn from_code(value: u32) -> Option<Self> {
        match value {
            0x00 => Some(Self::NoError),
            0xB1 => Some(Self::TlsError),
            0xB2 => Some(Self::Unspecified),
            0xB3 => Some(Self::TooManyRecoverAttempts),
            0xB4 => Some(Self::ProtocolLevel0),
            _ => None,
        }
    }
}

impl fmt::Display for QuicConnectionCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoError => write!(f, "NO_ERROR"),
            Self::TlsError => write!(f, "ERROR_TLS_ERROR"),
            Self::Unspecified => write!(f, "ERROR_UNSPECIFIED"),
            Self::TooManyRecoverAttempts => write!(f, "ERROR_TOO_MANY_RECOVER_ATTEMPTS"),
            Self::ProtocolLevel0 => write!(f, "ERROR_PROTOCOL_L0"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuicStreamCode {
    NoError = 0x00,
    NoFlowState = 0xB3,
    NotFlowOwner = 0xB4,
    StreamType = 0xB5,
    BadFlowId = 0xB6,
    PersistentTopic = 0xB7,
    PersistentSub = 0xB8,
    OptionalHeader = 0xB9,
    IncompletePacket = 0xBA,
    FlowOpenIdle = 0xBB,
    FlowCancelled = 0xBC,
    FlowPacketCancelled = 0xBD,
    FlowRefused = 0xBE,
    DiscardState = 0xBF,
    ServerPushNotWelcome = 0xC0,
    RecoveryFailed = 0xC1,
}

impl QuicStreamCode {
    #[must_use]
    pub fn code(self) -> u32 {
        self as u32
    }

    #[must_use]
    pub fn from_code(value: u32) -> Option<Self> {
        match value {
            0x00 => Some(Self::NoError),
            0xB3 => Some(Self::NoFlowState),
            0xB4 => Some(Self::NotFlowOwner),
            0xB5 => Some(Self::StreamType),
            0xB6 => Some(Self::BadFlowId),
            0xB7 => Some(Self::PersistentTopic),
            0xB8 => Some(Self::PersistentSub),
            0xB9 => Some(Self::OptionalHeader),
            0xBA => Some(Self::IncompletePacket),
            0xBB => Some(Self::FlowOpenIdle),
            0xBC => Some(Self::FlowCancelled),
            0xBD => Some(Self::FlowPacketCancelled),
            0xBE => Some(Self::FlowRefused),
            0xBF => Some(Self::DiscardState),
            0xC0 => Some(Self::ServerPushNotWelcome),
            0xC1 => Some(Self::RecoveryFailed),
            _ => None,
        }
    }

    #[must_use]
    pub fn error_level(self) -> Option<u8> {
        match self {
            Self::NoError => None,
            Self::NoFlowState | Self::NotFlowOwner => Some(0),
            Self::StreamType
            | Self::BadFlowId
            | Self::PersistentTopic
            | Self::PersistentSub
            | Self::OptionalHeader
            | Self::DiscardState
            | Self::ServerPushNotWelcome
            | Self::RecoveryFailed => Some(1),
            Self::IncompletePacket
            | Self::FlowOpenIdle
            | Self::FlowCancelled
            | Self::FlowPacketCancelled
            | Self::FlowRefused => Some(2),
        }
    }

    #[must_use]
    pub fn should_discard_state(self) -> bool {
        matches!(
            self,
            Self::BadFlowId
                | Self::PersistentSub
                | Self::OptionalHeader
                | Self::FlowCancelled
                | Self::DiscardState
                | Self::ServerPushNotWelcome
                | Self::RecoveryFailed
        )
    }
}

impl fmt::Display for QuicStreamCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoError => write!(f, "NO_ERROR"),
            Self::NoFlowState => write!(f, "ERROR_NO_FLOW_STATE"),
            Self::NotFlowOwner => write!(f, "ERROR_NOT_FLOW_OWNER"),
            Self::StreamType => write!(f, "ERROR_STREAM_TYPE"),
            Self::BadFlowId => write!(f, "ERROR_BAD_FLOW_ID"),
            Self::PersistentTopic => write!(f, "ERROR_PERSISTENT_TOPIC"),
            Self::PersistentSub => write!(f, "ERROR_PERSISTENT_SUB"),
            Self::OptionalHeader => write!(f, "ERROR_OPTIONAL_HEADER"),
            Self::IncompletePacket => write!(f, "ERROR_INCOMPLETE_PACKET"),
            Self::FlowOpenIdle => write!(f, "ERROR_FLOW_OPEN_IDLE"),
            Self::FlowCancelled => write!(f, "ERROR_FLOW_CANCELLED"),
            Self::FlowPacketCancelled => write!(f, "ERROR_FLOW_PACKET_CANCELLED"),
            Self::FlowRefused => write!(f, "ERROR_FLOW_REFUSED"),
            Self::DiscardState => write!(f, "ERROR_DISCARD_STATE"),
            Self::ServerPushNotWelcome => write!(f, "ERROR_SERVER_PUSH_NOT_WELCOME"),
            Self::RecoveryFailed => write!(f, "ERROR_RECOVERY_FAILED"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_code_round_trip() {
        let codes = [
            QuicConnectionCode::NoError,
            QuicConnectionCode::TlsError,
            QuicConnectionCode::Unspecified,
            QuicConnectionCode::TooManyRecoverAttempts,
            QuicConnectionCode::ProtocolLevel0,
        ];
        for code in codes {
            let value = code.code();
            let parsed = QuicConnectionCode::from_code(value);
            assert_eq!(parsed, Some(code), "round-trip failed for {code}");
        }
    }

    #[test]
    fn test_connection_code_values() {
        assert_eq!(QuicConnectionCode::NoError.code(), 0x00);
        assert_eq!(QuicConnectionCode::TlsError.code(), 0xB1);
        assert_eq!(QuicConnectionCode::Unspecified.code(), 0xB2);
        assert_eq!(QuicConnectionCode::TooManyRecoverAttempts.code(), 0xB3);
        assert_eq!(QuicConnectionCode::ProtocolLevel0.code(), 0xB4);
    }

    #[test]
    fn test_connection_code_unknown() {
        assert_eq!(QuicConnectionCode::from_code(0xFF), None);
        assert_eq!(QuicConnectionCode::from_code(0xB5), None);
    }

    #[test]
    fn test_stream_code_round_trip() {
        let codes = [
            QuicStreamCode::NoError,
            QuicStreamCode::NoFlowState,
            QuicStreamCode::NotFlowOwner,
            QuicStreamCode::StreamType,
            QuicStreamCode::BadFlowId,
            QuicStreamCode::PersistentTopic,
            QuicStreamCode::PersistentSub,
            QuicStreamCode::OptionalHeader,
            QuicStreamCode::IncompletePacket,
            QuicStreamCode::FlowOpenIdle,
            QuicStreamCode::FlowCancelled,
            QuicStreamCode::FlowPacketCancelled,
            QuicStreamCode::FlowRefused,
            QuicStreamCode::DiscardState,
            QuicStreamCode::ServerPushNotWelcome,
            QuicStreamCode::RecoveryFailed,
        ];
        for code in codes {
            let value = code.code();
            let parsed = QuicStreamCode::from_code(value);
            assert_eq!(parsed, Some(code), "round-trip failed for {code}");
        }
    }

    #[test]
    fn test_stream_code_values() {
        assert_eq!(QuicStreamCode::NoError.code(), 0x00);
        assert_eq!(QuicStreamCode::NoFlowState.code(), 0xB3);
        assert_eq!(QuicStreamCode::NotFlowOwner.code(), 0xB4);
        assert_eq!(QuicStreamCode::StreamType.code(), 0xB5);
        assert_eq!(QuicStreamCode::BadFlowId.code(), 0xB6);
        assert_eq!(QuicStreamCode::PersistentTopic.code(), 0xB7);
        assert_eq!(QuicStreamCode::PersistentSub.code(), 0xB8);
        assert_eq!(QuicStreamCode::OptionalHeader.code(), 0xB9);
        assert_eq!(QuicStreamCode::IncompletePacket.code(), 0xBA);
        assert_eq!(QuicStreamCode::FlowOpenIdle.code(), 0xBB);
        assert_eq!(QuicStreamCode::FlowCancelled.code(), 0xBC);
        assert_eq!(QuicStreamCode::FlowPacketCancelled.code(), 0xBD);
        assert_eq!(QuicStreamCode::FlowRefused.code(), 0xBE);
        assert_eq!(QuicStreamCode::DiscardState.code(), 0xBF);
        assert_eq!(QuicStreamCode::ServerPushNotWelcome.code(), 0xC0);
        assert_eq!(QuicStreamCode::RecoveryFailed.code(), 0xC1);
    }

    #[test]
    fn test_stream_code_unknown() {
        assert_eq!(QuicStreamCode::from_code(0xFF), None);
        assert_eq!(QuicStreamCode::from_code(0xB1), None);
    }

    #[test]
    fn test_stream_error_levels() {
        assert_eq!(QuicStreamCode::NoError.error_level(), None);
        assert_eq!(QuicStreamCode::NoFlowState.error_level(), Some(0));
        assert_eq!(QuicStreamCode::NotFlowOwner.error_level(), Some(0));
        assert_eq!(QuicStreamCode::StreamType.error_level(), Some(1));
        assert_eq!(QuicStreamCode::BadFlowId.error_level(), Some(1));
        assert_eq!(QuicStreamCode::PersistentTopic.error_level(), Some(1));
        assert_eq!(QuicStreamCode::PersistentSub.error_level(), Some(1));
        assert_eq!(QuicStreamCode::OptionalHeader.error_level(), Some(1));
        assert_eq!(QuicStreamCode::DiscardState.error_level(), Some(1));
        assert_eq!(QuicStreamCode::ServerPushNotWelcome.error_level(), Some(1));
        assert_eq!(QuicStreamCode::RecoveryFailed.error_level(), Some(1));
        assert_eq!(QuicStreamCode::IncompletePacket.error_level(), Some(2));
        assert_eq!(QuicStreamCode::FlowOpenIdle.error_level(), Some(2));
        assert_eq!(QuicStreamCode::FlowCancelled.error_level(), Some(2));
        assert_eq!(QuicStreamCode::FlowPacketCancelled.error_level(), Some(2));
        assert_eq!(QuicStreamCode::FlowRefused.error_level(), Some(2));
    }

    #[test]
    fn test_stream_should_discard_state() {
        assert!(!QuicStreamCode::NoError.should_discard_state());
        assert!(!QuicStreamCode::NoFlowState.should_discard_state());
        assert!(!QuicStreamCode::NotFlowOwner.should_discard_state());
        assert!(!QuicStreamCode::StreamType.should_discard_state());
        assert!(QuicStreamCode::BadFlowId.should_discard_state());
        assert!(!QuicStreamCode::PersistentTopic.should_discard_state());
        assert!(QuicStreamCode::PersistentSub.should_discard_state());
        assert!(QuicStreamCode::OptionalHeader.should_discard_state());
        assert!(!QuicStreamCode::IncompletePacket.should_discard_state());
        assert!(!QuicStreamCode::FlowOpenIdle.should_discard_state());
        assert!(QuicStreamCode::FlowCancelled.should_discard_state());
        assert!(!QuicStreamCode::FlowPacketCancelled.should_discard_state());
        assert!(!QuicStreamCode::FlowRefused.should_discard_state());
        assert!(QuicStreamCode::DiscardState.should_discard_state());
        assert!(QuicStreamCode::ServerPushNotWelcome.should_discard_state());
        assert!(QuicStreamCode::RecoveryFailed.should_discard_state());
    }

    #[test]
    fn test_connection_code_display() {
        assert_eq!(QuicConnectionCode::NoError.to_string(), "NO_ERROR");
        assert_eq!(QuicConnectionCode::TlsError.to_string(), "ERROR_TLS_ERROR");
        assert_eq!(
            QuicConnectionCode::Unspecified.to_string(),
            "ERROR_UNSPECIFIED"
        );
        assert_eq!(
            QuicConnectionCode::TooManyRecoverAttempts.to_string(),
            "ERROR_TOO_MANY_RECOVER_ATTEMPTS"
        );
        assert_eq!(
            QuicConnectionCode::ProtocolLevel0.to_string(),
            "ERROR_PROTOCOL_L0"
        );
    }

    #[test]
    fn test_stream_code_display() {
        assert_eq!(
            QuicStreamCode::NoFlowState.to_string(),
            "ERROR_NO_FLOW_STATE"
        );
        assert_eq!(
            QuicStreamCode::RecoveryFailed.to_string(),
            "ERROR_RECOVERY_FAILED"
        );
    }

    #[test]
    fn test_namespace_overlap_independence() {
        assert_eq!(QuicConnectionCode::TooManyRecoverAttempts.code(), 0xB3);
        assert_eq!(QuicStreamCode::NoFlowState.code(), 0xB3);

        assert_eq!(QuicConnectionCode::ProtocolLevel0.code(), 0xB4);
        assert_eq!(QuicStreamCode::NotFlowOwner.code(), 0xB4);
    }
}
