use crate::packet::suback::SubAckReasonCode;
use crate::packet::unsuback::UnsubAckReasonCode;
use crate::packet::Packet;
use crate::prelude::String;
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::types::Message;

use super::state::ClientState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeoutId {
    ConnAck,
    PingResp,
    PubAck(u16),
    PubRec(u16),
    PubRel(u16),
    PubComp(u16),
    SubAck(u16),
    UnsubAck(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckType {
    SubAck,
    UnsubAck,
    PubAck,
    PubRec,
    PubRel,
    PubComp,
}

#[derive(Debug, Clone)]
pub enum ProtocolAction {
    SendPacket(Packet),
    DeliverMessage(Message),
    StateTransition(ClientState),
    TrackPendingAck {
        packet_id: u16,
        ack_type: AckType,
    },
    RemovePendingAck {
        packet_id: u16,
        ack_type: AckType,
    },
    UpdateServerLimits {
        receive_maximum: u16,
        max_packet_size: u32,
        topic_alias_maximum: u16,
    },
    ScheduleTimeout {
        timeout_id: TimeoutId,
        duration_ms: u32,
    },
    CancelTimeout {
        timeout_id: TimeoutId,
    },
    ScheduleKeepalive {
        interval_secs: u16,
    },
    ConnectionComplete {
        session_present: bool,
        server_keep_alive: Option<u16>,
    },
    SubscribeComplete {
        packet_id: u16,
        granted_qos: crate::prelude::Vec<SubAckReasonCode>,
    },
    UnsubscribeComplete {
        packet_id: u16,
        reason_codes: crate::prelude::Vec<UnsubAckReasonCode>,
    },
    PublishComplete {
        packet_id: u16,
        reason_code: ReasonCode,
    },
    Error {
        code: ReasonCode,
        message: String,
    },
    Disconnect {
        reason: ReasonCode,
    },
}

impl ProtocolAction {
    #[must_use]
    pub fn send_packet(packet: Packet) -> Self {
        Self::SendPacket(packet)
    }

    #[must_use]
    pub fn deliver_message(message: Message) -> Self {
        Self::DeliverMessage(message)
    }

    #[must_use]
    pub fn state_transition(state: ClientState) -> Self {
        Self::StateTransition(state)
    }

    #[must_use]
    pub fn schedule_timeout(timeout_id: TimeoutId, duration_ms: u32) -> Self {
        Self::ScheduleTimeout {
            timeout_id,
            duration_ms,
        }
    }

    #[must_use]
    pub fn cancel_timeout(timeout_id: TimeoutId) -> Self {
        Self::CancelTimeout { timeout_id }
    }

    #[must_use]
    pub fn error(code: ReasonCode, message: impl Into<String>) -> Self {
        Self::Error {
            code,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn disconnect(reason: ReasonCode) -> Self {
        Self::Disconnect { reason }
    }

    #[must_use]
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error { .. })
    }

    #[must_use]
    pub fn is_send_packet(&self) -> bool {
        matches!(self, Self::SendPacket(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timeout_id_equality() {
        assert_eq!(TimeoutId::ConnAck, TimeoutId::ConnAck);
        assert_eq!(TimeoutId::PubAck(1), TimeoutId::PubAck(1));
        assert_ne!(TimeoutId::PubAck(1), TimeoutId::PubAck(2));
        assert_ne!(TimeoutId::PubAck(1), TimeoutId::PubRec(1));
    }

    #[test]
    fn test_protocol_action_constructors() {
        let action = ProtocolAction::error(ReasonCode::UnspecifiedError, "test error");
        assert!(action.is_error());

        let action = ProtocolAction::schedule_timeout(TimeoutId::ConnAck, 5000);
        match action {
            ProtocolAction::ScheduleTimeout {
                timeout_id,
                duration_ms,
            } => {
                assert_eq!(timeout_id, TimeoutId::ConnAck);
                assert_eq!(duration_ms, 5000);
            }
            _ => panic!("Expected ScheduleTimeout"),
        }
    }
}
