#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasonCode {
    // Success codes (0x00 - 0x7F)
    Success = 0x00, // Also used for NormalDisconnection and GrantedQoS0
    GrantedQoS1 = 0x01,
    GrantedQoS2 = 0x02,
    DisconnectWithWillMessage = 0x04,
    NoMatchingSubscribers = 0x10,
    NoSubscriptionExisted = 0x11,
    ContinueAuthentication = 0x18,
    ReAuthenticate = 0x19,

    // Error codes (0x80 - 0xFF)
    UnspecifiedError = 0x80,
    MalformedPacket = 0x81,
    ProtocolError = 0x82,
    ImplementationSpecificError = 0x83,
    UnsupportedProtocolVersion = 0x84,
    ClientIdentifierNotValid = 0x85,
    BadUsernameOrPassword = 0x86,
    NotAuthorized = 0x87,
    ServerUnavailable = 0x88,
    ServerBusy = 0x89,
    Banned = 0x8A,
    ServerShuttingDown = 0x8B,
    BadAuthenticationMethod = 0x8C,
    KeepAliveTimeout = 0x8D,
    SessionTakenOver = 0x8E,
    TopicFilterInvalid = 0x8F,
    TopicNameInvalid = 0x90,
    PacketIdentifierInUse = 0x91,
    PacketIdentifierNotFound = 0x92,
    ReceiveMaximumExceeded = 0x93,
    TopicAliasInvalid = 0x94,
    PacketTooLarge = 0x95,
    MessageRateTooHigh = 0x96,
    QuotaExceeded = 0x97,
    AdministrativeAction = 0x98,
    PayloadFormatInvalid = 0x99,
    RetainNotSupported = 0x9A,
    QoSNotSupported = 0x9B,
    UseAnotherServer = 0x9C,
    ServerMoved = 0x9D,
    SharedSubscriptionsNotSupported = 0x9E,
    ConnectionRateExceeded = 0x9F,
    MaximumConnectTime = 0xA0,
    SubscriptionIdentifiersNotSupported = 0xA1,
    WildcardSubscriptionsNotSupported = 0xA2,

    MqoqNoFlowState = 0xB3,
    MqoqNotFlowOwner = 0xB4,
    MqoqStreamTypeError = 0xB5,
    MqoqBadFlowId = 0xB6,
    MqoqPersistentTopic = 0xB7,
    MqoqPersistentSubError = 0xB8,
    MqoqOptionalHeader = 0xB9,
    MqoqIncompletePacket = 0xBA,
    MqoqFlowOpenIdle = 0xBB,
    MqoqFlowCancelled = 0xBC,
    MqoqFlowPacketCancelled = 0xBD,
    MqoqFlowRefused = 0xBE,
    MqoqDiscardState = 0xBF,
    MqoqServerPushNotWelcome = 0xC0,
    MqoqRecoveryFailed = 0xC1,
}

// Aliases for ReasonCode::Success (0x00) used in different contexts
pub const NORMAL_DISCONNECTION: ReasonCode = ReasonCode::Success;
pub const GRANTED_QOS_0: ReasonCode = ReasonCode::Success;

impl From<ReasonCode> for u8 {
    fn from(code: ReasonCode) -> Self {
        code as u8
    }
}

impl ReasonCode {
    #[must_use]
    pub fn is_success(&self) -> bool {
        u8::from(*self) < 0x80
    }

    #[must_use]
    pub fn is_error(&self) -> bool {
        u8::from(*self) >= 0x80
    }

    #[must_use]
    pub fn is_mqoq_error(&self) -> bool {
        matches!(
            self,
            Self::MqoqNoFlowState
                | Self::MqoqNotFlowOwner
                | Self::MqoqStreamTypeError
                | Self::MqoqBadFlowId
                | Self::MqoqPersistentTopic
                | Self::MqoqPersistentSubError
                | Self::MqoqOptionalHeader
                | Self::MqoqIncompletePacket
                | Self::MqoqFlowOpenIdle
                | Self::MqoqFlowCancelled
                | Self::MqoqFlowPacketCancelled
                | Self::MqoqFlowRefused
                | Self::MqoqDiscardState
                | Self::MqoqServerPushNotWelcome
                | Self::MqoqRecoveryFailed
        )
    }

    #[must_use]
    pub fn mqoq_error_level(&self) -> Option<u8> {
        match self {
            Self::MqoqNoFlowState | Self::MqoqNotFlowOwner => Some(0),
            Self::MqoqStreamTypeError
            | Self::MqoqBadFlowId
            | Self::MqoqPersistentTopic
            | Self::MqoqPersistentSubError
            | Self::MqoqOptionalHeader
            | Self::MqoqDiscardState
            | Self::MqoqServerPushNotWelcome
            | Self::MqoqRecoveryFailed => Some(1),
            Self::MqoqIncompletePacket
            | Self::MqoqFlowOpenIdle
            | Self::MqoqFlowCancelled
            | Self::MqoqFlowPacketCancelled
            | Self::MqoqFlowRefused => Some(2),
            _ => None,
        }
    }

    #[must_use]
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x00 => Some(Self::Success),
            0x01 => Some(Self::GrantedQoS1),
            0x02 => Some(Self::GrantedQoS2),
            0x04 => Some(Self::DisconnectWithWillMessage),
            0x10 => Some(Self::NoMatchingSubscribers),
            0x11 => Some(Self::NoSubscriptionExisted),
            0x18 => Some(Self::ContinueAuthentication),
            0x19 => Some(Self::ReAuthenticate),
            0x80 => Some(Self::UnspecifiedError),
            0x81 => Some(Self::MalformedPacket),
            0x82 => Some(Self::ProtocolError),
            0x83 => Some(Self::ImplementationSpecificError),
            0x84 => Some(Self::UnsupportedProtocolVersion),
            0x85 => Some(Self::ClientIdentifierNotValid),
            0x86 => Some(Self::BadUsernameOrPassword),
            0x87 => Some(Self::NotAuthorized),
            0x88 => Some(Self::ServerUnavailable),
            0x89 => Some(Self::ServerBusy),
            0x8A => Some(Self::Banned),
            0x8B => Some(Self::ServerShuttingDown),
            0x8C => Some(Self::BadAuthenticationMethod),
            0x8D => Some(Self::KeepAliveTimeout),
            0x8E => Some(Self::SessionTakenOver),
            0x8F => Some(Self::TopicFilterInvalid),
            0x90 => Some(Self::TopicNameInvalid),
            0x91 => Some(Self::PacketIdentifierInUse),
            0x92 => Some(Self::PacketIdentifierNotFound),
            0x93 => Some(Self::ReceiveMaximumExceeded),
            0x94 => Some(Self::TopicAliasInvalid),
            0x95 => Some(Self::PacketTooLarge),
            0x96 => Some(Self::MessageRateTooHigh),
            0x97 => Some(Self::QuotaExceeded),
            0x98 => Some(Self::AdministrativeAction),
            0x99 => Some(Self::PayloadFormatInvalid),
            0x9A => Some(Self::RetainNotSupported),
            0x9B => Some(Self::QoSNotSupported),
            0x9C => Some(Self::UseAnotherServer),
            0x9D => Some(Self::ServerMoved),
            0x9E => Some(Self::SharedSubscriptionsNotSupported),
            0x9F => Some(Self::ConnectionRateExceeded),
            0xA0 => Some(Self::MaximumConnectTime),
            0xA1 => Some(Self::SubscriptionIdentifiersNotSupported),
            0xA2 => Some(Self::WildcardSubscriptionsNotSupported),
            0xB3 => Some(Self::MqoqNoFlowState),
            0xB4 => Some(Self::MqoqNotFlowOwner),
            0xB5 => Some(Self::MqoqStreamTypeError),
            0xB6 => Some(Self::MqoqBadFlowId),
            0xB7 => Some(Self::MqoqPersistentTopic),
            0xB8 => Some(Self::MqoqPersistentSubError),
            0xB9 => Some(Self::MqoqOptionalHeader),
            0xBA => Some(Self::MqoqIncompletePacket),
            0xBB => Some(Self::MqoqFlowOpenIdle),
            0xBC => Some(Self::MqoqFlowCancelled),
            0xBD => Some(Self::MqoqFlowPacketCancelled),
            0xBE => Some(Self::MqoqFlowRefused),
            0xBF => Some(Self::MqoqDiscardState),
            0xC0 => Some(Self::MqoqServerPushNotWelcome),
            0xC1 => Some(Self::MqoqRecoveryFailed),
            _ => None,
        }
    }

    #[must_use]
    pub fn to_quic_stream_code(self) -> Option<crate::quic_error_codes::QuicStreamCode> {
        crate::quic_error_codes::QuicStreamCode::from_code(u32::from(self as u8))
    }

    #[must_use]
    pub fn from_quic_stream_code(code: crate::quic_error_codes::QuicStreamCode) -> Option<Self> {
        let value = code.code();
        if value > 255 {
            return None;
        }
        #[allow(clippy::cast_possible_truncation)]
        Self::from_u8(value as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reason_code_success_check() {
        assert!(ReasonCode::Success.is_success());
        assert!(ReasonCode::GrantedQoS1.is_success());
        assert!(ReasonCode::GrantedQoS2.is_success());
        assert!(ReasonCode::NoMatchingSubscribers.is_success());

        assert!(!ReasonCode::UnspecifiedError.is_success());
        assert!(!ReasonCode::MalformedPacket.is_success());
        assert!(!ReasonCode::NotAuthorized.is_success());
    }

    #[test]
    fn test_reason_code_error_check() {
        assert!(!ReasonCode::Success.is_error());
        assert!(!ReasonCode::GrantedQoS1.is_error());

        assert!(ReasonCode::UnspecifiedError.is_error());
        assert!(ReasonCode::MalformedPacket.is_error());
        assert!(ReasonCode::ProtocolError.is_error());
        assert!(ReasonCode::ServerBusy.is_error());
    }

    #[test]
    fn test_reason_code_from_u8() {
        assert_eq!(ReasonCode::from_u8(0x00), Some(ReasonCode::Success));
        assert_eq!(ReasonCode::from_u8(0x01), Some(ReasonCode::GrantedQoS1));
        assert_eq!(ReasonCode::from_u8(0x02), Some(ReasonCode::GrantedQoS2));
        assert_eq!(
            ReasonCode::from_u8(0x80),
            Some(ReasonCode::UnspecifiedError)
        );
        assert_eq!(ReasonCode::from_u8(0x81), Some(ReasonCode::MalformedPacket));
        assert_eq!(ReasonCode::from_u8(0x87), Some(ReasonCode::NotAuthorized));
        assert_eq!(
            ReasonCode::from_u8(0xA2),
            Some(ReasonCode::WildcardSubscriptionsNotSupported)
        );

        // Invalid codes
        assert_eq!(ReasonCode::from_u8(0x03), None);
        assert_eq!(ReasonCode::from_u8(0x05), None);
        assert_eq!(ReasonCode::from_u8(0xFF), None);
    }

    #[test]
    fn test_reason_code_aliases() {
        assert_eq!(NORMAL_DISCONNECTION, ReasonCode::Success);
        assert_eq!(GRANTED_QOS_0, ReasonCode::Success);
        assert_eq!(NORMAL_DISCONNECTION as u8, 0x00);
        assert_eq!(GRANTED_QOS_0 as u8, 0x00);
    }

    #[test]
    fn test_reason_code_values() {
        assert_eq!(ReasonCode::Success as u8, 0x00);
        assert_eq!(ReasonCode::DisconnectWithWillMessage as u8, 0x04);
        assert_eq!(ReasonCode::NoMatchingSubscribers as u8, 0x10);
        assert_eq!(ReasonCode::UnspecifiedError as u8, 0x80);
        assert_eq!(ReasonCode::ClientIdentifierNotValid as u8, 0x85);
        assert_eq!(ReasonCode::ServerBusy as u8, 0x89);
        assert_eq!(ReasonCode::QuotaExceeded as u8, 0x97);
        assert_eq!(ReasonCode::WildcardSubscriptionsNotSupported as u8, 0xA2);
    }

    #[test]
    fn test_mqoq_error_codes() {
        assert_eq!(ReasonCode::MqoqNoFlowState as u8, 0xB3);
        assert_eq!(ReasonCode::MqoqNotFlowOwner as u8, 0xB4);
        assert_eq!(ReasonCode::MqoqStreamTypeError as u8, 0xB5);
        assert_eq!(ReasonCode::MqoqBadFlowId as u8, 0xB6);
        assert_eq!(ReasonCode::MqoqPersistentTopic as u8, 0xB7);
        assert_eq!(ReasonCode::MqoqPersistentSubError as u8, 0xB8);
        assert_eq!(ReasonCode::MqoqOptionalHeader as u8, 0xB9);
        assert_eq!(ReasonCode::MqoqIncompletePacket as u8, 0xBA);
        assert_eq!(ReasonCode::MqoqFlowOpenIdle as u8, 0xBB);
        assert_eq!(ReasonCode::MqoqFlowCancelled as u8, 0xBC);
        assert_eq!(ReasonCode::MqoqFlowPacketCancelled as u8, 0xBD);
        assert_eq!(ReasonCode::MqoqFlowRefused as u8, 0xBE);
        assert_eq!(ReasonCode::MqoqDiscardState as u8, 0xBF);
        assert_eq!(ReasonCode::MqoqServerPushNotWelcome as u8, 0xC0);
        assert_eq!(ReasonCode::MqoqRecoveryFailed as u8, 0xC1);
    }

    #[test]
    fn test_mqoq_error_from_u8() {
        assert_eq!(ReasonCode::from_u8(0xB3), Some(ReasonCode::MqoqNoFlowState));
        assert_eq!(
            ReasonCode::from_u8(0xB4),
            Some(ReasonCode::MqoqNotFlowOwner)
        );
        assert_eq!(
            ReasonCode::from_u8(0xB5),
            Some(ReasonCode::MqoqStreamTypeError)
        );
        assert_eq!(ReasonCode::from_u8(0xB6), Some(ReasonCode::MqoqBadFlowId));
        assert_eq!(
            ReasonCode::from_u8(0xB7),
            Some(ReasonCode::MqoqPersistentTopic)
        );
        assert_eq!(
            ReasonCode::from_u8(0xB8),
            Some(ReasonCode::MqoqPersistentSubError)
        );
        assert_eq!(
            ReasonCode::from_u8(0xB9),
            Some(ReasonCode::MqoqOptionalHeader)
        );
        assert_eq!(
            ReasonCode::from_u8(0xBA),
            Some(ReasonCode::MqoqIncompletePacket)
        );
        assert_eq!(
            ReasonCode::from_u8(0xBB),
            Some(ReasonCode::MqoqFlowOpenIdle)
        );
        assert_eq!(
            ReasonCode::from_u8(0xBC),
            Some(ReasonCode::MqoqFlowCancelled)
        );
        assert_eq!(
            ReasonCode::from_u8(0xBD),
            Some(ReasonCode::MqoqFlowPacketCancelled)
        );
        assert_eq!(ReasonCode::from_u8(0xBE), Some(ReasonCode::MqoqFlowRefused));
        assert_eq!(
            ReasonCode::from_u8(0xBF),
            Some(ReasonCode::MqoqDiscardState)
        );
        assert_eq!(
            ReasonCode::from_u8(0xC0),
            Some(ReasonCode::MqoqServerPushNotWelcome)
        );
        assert_eq!(
            ReasonCode::from_u8(0xC1),
            Some(ReasonCode::MqoqRecoveryFailed)
        );
    }

    #[test]
    fn test_mqoq_error_detection() {
        assert!(ReasonCode::MqoqNoFlowState.is_mqoq_error());
        assert!(ReasonCode::MqoqNotFlowOwner.is_mqoq_error());
        assert!(ReasonCode::MqoqStreamTypeError.is_mqoq_error());
        assert!(ReasonCode::MqoqBadFlowId.is_mqoq_error());
        assert!(ReasonCode::MqoqPersistentTopic.is_mqoq_error());
        assert!(ReasonCode::MqoqPersistentSubError.is_mqoq_error());
        assert!(ReasonCode::MqoqOptionalHeader.is_mqoq_error());
        assert!(ReasonCode::MqoqIncompletePacket.is_mqoq_error());
        assert!(ReasonCode::MqoqFlowOpenIdle.is_mqoq_error());
        assert!(ReasonCode::MqoqFlowCancelled.is_mqoq_error());
        assert!(ReasonCode::MqoqFlowPacketCancelled.is_mqoq_error());
        assert!(ReasonCode::MqoqFlowRefused.is_mqoq_error());
        assert!(ReasonCode::MqoqDiscardState.is_mqoq_error());
        assert!(ReasonCode::MqoqServerPushNotWelcome.is_mqoq_error());
        assert!(ReasonCode::MqoqRecoveryFailed.is_mqoq_error());
        assert!(!ReasonCode::Success.is_mqoq_error());
        assert!(!ReasonCode::ProtocolError.is_mqoq_error());
    }

    #[test]
    fn test_mqoq_error_levels() {
        assert_eq!(ReasonCode::MqoqNoFlowState.mqoq_error_level(), Some(0));
        assert_eq!(ReasonCode::MqoqNotFlowOwner.mqoq_error_level(), Some(0));
        assert_eq!(ReasonCode::MqoqStreamTypeError.mqoq_error_level(), Some(1));
        assert_eq!(ReasonCode::MqoqBadFlowId.mqoq_error_level(), Some(1));
        assert_eq!(ReasonCode::MqoqPersistentTopic.mqoq_error_level(), Some(1));
        assert_eq!(
            ReasonCode::MqoqPersistentSubError.mqoq_error_level(),
            Some(1)
        );
        assert_eq!(ReasonCode::MqoqOptionalHeader.mqoq_error_level(), Some(1));
        assert_eq!(ReasonCode::MqoqDiscardState.mqoq_error_level(), Some(1));
        assert_eq!(
            ReasonCode::MqoqServerPushNotWelcome.mqoq_error_level(),
            Some(1)
        );
        assert_eq!(ReasonCode::MqoqRecoveryFailed.mqoq_error_level(), Some(1));
        assert_eq!(ReasonCode::MqoqIncompletePacket.mqoq_error_level(), Some(2));
        assert_eq!(ReasonCode::MqoqFlowOpenIdle.mqoq_error_level(), Some(2));
        assert_eq!(ReasonCode::MqoqFlowCancelled.mqoq_error_level(), Some(2));
        assert_eq!(
            ReasonCode::MqoqFlowPacketCancelled.mqoq_error_level(),
            Some(2)
        );
        assert_eq!(ReasonCode::MqoqFlowRefused.mqoq_error_level(), Some(2));
        assert_eq!(ReasonCode::Success.mqoq_error_level(), None);
        assert_eq!(ReasonCode::ProtocolError.mqoq_error_level(), None);
    }

    #[test]
    fn test_reason_to_quic_stream_code_round_trip() {
        use crate::quic_error_codes::QuicStreamCode;

        let pairs = [
            (ReasonCode::MqoqNoFlowState, QuicStreamCode::NoFlowState),
            (ReasonCode::MqoqNotFlowOwner, QuicStreamCode::NotFlowOwner),
            (ReasonCode::MqoqStreamTypeError, QuicStreamCode::StreamType),
            (ReasonCode::MqoqBadFlowId, QuicStreamCode::BadFlowId),
            (
                ReasonCode::MqoqPersistentTopic,
                QuicStreamCode::PersistentTopic,
            ),
            (
                ReasonCode::MqoqPersistentSubError,
                QuicStreamCode::PersistentSub,
            ),
            (
                ReasonCode::MqoqOptionalHeader,
                QuicStreamCode::OptionalHeader,
            ),
            (
                ReasonCode::MqoqIncompletePacket,
                QuicStreamCode::IncompletePacket,
            ),
            (ReasonCode::MqoqFlowOpenIdle, QuicStreamCode::FlowOpenIdle),
            (ReasonCode::MqoqFlowCancelled, QuicStreamCode::FlowCancelled),
            (
                ReasonCode::MqoqFlowPacketCancelled,
                QuicStreamCode::FlowPacketCancelled,
            ),
            (ReasonCode::MqoqFlowRefused, QuicStreamCode::FlowRefused),
            (ReasonCode::MqoqDiscardState, QuicStreamCode::DiscardState),
            (
                ReasonCode::MqoqServerPushNotWelcome,
                QuicStreamCode::ServerPushNotWelcome,
            ),
            (
                ReasonCode::MqoqRecoveryFailed,
                QuicStreamCode::RecoveryFailed,
            ),
        ];

        for (reason, stream) in pairs {
            assert_eq!(reason.to_quic_stream_code(), Some(stream));
            assert_eq!(ReasonCode::from_quic_stream_code(stream), Some(reason));
        }
    }
}
