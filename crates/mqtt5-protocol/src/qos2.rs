use crate::packet::pubcomp::PubCompPacket;
use crate::packet::pubrec::PubRecPacket;
use crate::packet::pubrel::PubRelPacket;
use crate::prelude::{vec, Vec};
use crate::protocol::v5::reason_codes::ReasonCode;

#[derive(Debug, Clone, PartialEq)]
pub enum QoS2Action {
    SendPubRec {
        packet_id: u16,
        reason_code: ReasonCode,
    },
    SendPubRel {
        packet_id: u16,
    },
    SendPubComp {
        packet_id: u16,
        reason_code: ReasonCode,
    },
    TrackOutgoingPubRec {
        packet_id: u16,
    },
    TrackOutgoingPubRel {
        packet_id: u16,
    },
    RemoveOutgoingPubRel {
        packet_id: u16,
    },
    TrackIncomingPubRec {
        packet_id: u16,
    },
    RemoveIncomingPubRec {
        packet_id: u16,
    },
    DeliverMessage {
        packet_id: u16,
    },
    CompleteFlow {
        packet_id: u16,
    },
    ErrorFlow {
        packet_id: u16,
        reason_code: ReasonCode,
    },
}

impl QoS2Action {
    #[must_use]
    pub fn to_pubrec_packet(&self) -> Option<PubRecPacket> {
        match self {
            QoS2Action::SendPubRec {
                packet_id,
                reason_code,
            } => Some(PubRecPacket::new_with_reason(*packet_id, *reason_code)),
            _ => None,
        }
    }

    #[must_use]
    pub fn to_pubrel_packet(&self) -> Option<PubRelPacket> {
        match self {
            QoS2Action::SendPubRel { packet_id } => Some(PubRelPacket::new(*packet_id)),
            _ => None,
        }
    }

    #[must_use]
    pub fn to_pubcomp_packet(&self) -> Option<PubCompPacket> {
        match self {
            QoS2Action::SendPubComp {
                packet_id,
                reason_code,
            } => Some(PubCompPacket::new_with_reason(*packet_id, *reason_code)),
            _ => None,
        }
    }
}

#[must_use]
pub fn handle_outgoing_publish_qos2(packet_id: u16) -> Vec<QoS2Action> {
    vec![QoS2Action::TrackOutgoingPubRel { packet_id }]
}

#[must_use]
pub fn handle_incoming_pubrec(
    packet_id: u16,
    reason_code: ReasonCode,
    has_pending_publish: bool,
) -> Vec<QoS2Action> {
    if !has_pending_publish {
        return vec![QoS2Action::ErrorFlow {
            packet_id,
            reason_code: ReasonCode::PacketIdentifierNotFound,
        }];
    }

    if reason_code != ReasonCode::Success {
        return vec![QoS2Action::ErrorFlow {
            packet_id,
            reason_code,
        }];
    }

    vec![
        QoS2Action::SendPubRel { packet_id },
        QoS2Action::TrackOutgoingPubRel { packet_id },
    ]
}

#[must_use]
pub fn handle_incoming_pubcomp(
    packet_id: u16,
    reason_code: ReasonCode,
    has_pending_pubrel: bool,
) -> Vec<QoS2Action> {
    if !has_pending_pubrel {
        return vec![];
    }

    vec![
        QoS2Action::RemoveOutgoingPubRel { packet_id },
        if reason_code == ReasonCode::Success {
            QoS2Action::CompleteFlow { packet_id }
        } else {
            QoS2Action::ErrorFlow {
                packet_id,
                reason_code,
            }
        },
    ]
}

#[must_use]
pub fn handle_incoming_publish_qos2(packet_id: u16, is_duplicate: bool) -> Vec<QoS2Action> {
    if is_duplicate {
        vec![QoS2Action::SendPubRec {
            packet_id,
            reason_code: ReasonCode::Success,
        }]
    } else {
        vec![
            QoS2Action::DeliverMessage { packet_id },
            QoS2Action::SendPubRec {
                packet_id,
                reason_code: ReasonCode::Success,
            },
            QoS2Action::TrackIncomingPubRec { packet_id },
        ]
    }
}

#[must_use]
pub fn handle_incoming_pubrel(packet_id: u16, has_pending_pubrec: bool) -> Vec<QoS2Action> {
    if has_pending_pubrec {
        vec![
            QoS2Action::RemoveIncomingPubRec { packet_id },
            QoS2Action::SendPubComp {
                packet_id,
                reason_code: ReasonCode::Success,
            },
        ]
    } else {
        vec![QoS2Action::SendPubComp {
            packet_id,
            reason_code: ReasonCode::PacketIdentifierNotFound,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_outgoing_publish_qos2() {
        let actions = handle_outgoing_publish_qos2(123);
        assert_eq!(actions.len(), 1);
        assert_eq!(
            actions[0],
            QoS2Action::TrackOutgoingPubRel { packet_id: 123 }
        );
    }

    #[test]
    fn test_incoming_pubrec_success() {
        let actions = handle_incoming_pubrec(123, ReasonCode::Success, true);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0], QoS2Action::SendPubRel { packet_id: 123 });
        assert_eq!(
            actions[1],
            QoS2Action::TrackOutgoingPubRel { packet_id: 123 }
        );
    }

    #[test]
    fn test_incoming_pubrec_error() {
        let actions = handle_incoming_pubrec(123, ReasonCode::UnspecifiedError, true);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            QoS2Action::ErrorFlow {
                packet_id,
                reason_code,
            } => {
                assert_eq!(*packet_id, 123);
                assert_eq!(*reason_code, ReasonCode::UnspecifiedError);
            }
            _ => panic!("Expected ErrorFlow"),
        }
    }

    #[test]
    fn test_incoming_pubrec_no_pending() {
        let actions = handle_incoming_pubrec(123, ReasonCode::Success, false);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            QoS2Action::ErrorFlow {
                packet_id,
                reason_code,
            } => {
                assert_eq!(*packet_id, 123);
                assert_eq!(*reason_code, ReasonCode::PacketIdentifierNotFound);
            }
            _ => panic!("Expected ErrorFlow"),
        }
    }

    #[test]
    fn test_incoming_pubcomp_success() {
        let actions = handle_incoming_pubcomp(123, ReasonCode::Success, true);
        assert_eq!(actions.len(), 2);
        assert_eq!(
            actions[0],
            QoS2Action::RemoveOutgoingPubRel { packet_id: 123 }
        );
        assert_eq!(actions[1], QoS2Action::CompleteFlow { packet_id: 123 });
    }

    #[test]
    fn test_incoming_pubcomp_no_pending() {
        let actions = handle_incoming_pubcomp(123, ReasonCode::Success, false);
        assert!(actions.is_empty());
    }

    #[test]
    fn test_incoming_publish_qos2_new_message() {
        let actions = handle_incoming_publish_qos2(123, false);
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0], QoS2Action::DeliverMessage { packet_id: 123 });
        assert_eq!(
            actions[1],
            QoS2Action::SendPubRec {
                packet_id: 123,
                reason_code: ReasonCode::Success
            }
        );
        assert_eq!(
            actions[2],
            QoS2Action::TrackIncomingPubRec { packet_id: 123 }
        );
    }

    #[test]
    fn test_incoming_publish_qos2_duplicate() {
        let actions = handle_incoming_publish_qos2(123, true);
        assert_eq!(actions.len(), 1);
        assert_eq!(
            actions[0],
            QoS2Action::SendPubRec {
                packet_id: 123,
                reason_code: ReasonCode::Success
            }
        );
    }

    #[test]
    fn test_incoming_pubrel_with_pubrec() {
        let actions = handle_incoming_pubrel(123, true);
        assert_eq!(actions.len(), 2);
        assert_eq!(
            actions[0],
            QoS2Action::RemoveIncomingPubRec { packet_id: 123 }
        );
        assert_eq!(
            actions[1],
            QoS2Action::SendPubComp {
                packet_id: 123,
                reason_code: ReasonCode::Success
            }
        );
    }

    #[test]
    fn test_incoming_pubrel_without_pubrec() {
        let actions = handle_incoming_pubrel(123, false);
        assert_eq!(actions.len(), 1);
        assert_eq!(
            actions[0],
            QoS2Action::SendPubComp {
                packet_id: 123,
                reason_code: ReasonCode::PacketIdentifierNotFound
            }
        );
    }

    #[test]
    fn test_action_to_packet_conversions() {
        let action = QoS2Action::SendPubRec {
            packet_id: 123,
            reason_code: ReasonCode::Success,
        };
        assert!(action.to_pubrec_packet().is_some());
        assert!(action.to_pubrel_packet().is_none());
        assert!(action.to_pubcomp_packet().is_none());

        let action = QoS2Action::SendPubRel { packet_id: 123 };
        assert!(action.to_pubrec_packet().is_none());
        assert!(action.to_pubrel_packet().is_some());
        assert!(action.to_pubcomp_packet().is_none());

        let action = QoS2Action::SendPubComp {
            packet_id: 123,
            reason_code: ReasonCode::Success,
        };
        assert!(action.to_pubrec_packet().is_none());
        assert!(action.to_pubrel_packet().is_none());
        assert!(action.to_pubcomp_packet().is_some());
    }
}
