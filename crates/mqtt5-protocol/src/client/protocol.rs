use crate::packet::connack::ConnAckPacket;
use crate::packet::connect::ConnectPacket;
use crate::packet::disconnect::DisconnectPacket;
use crate::packet::puback::PubAckPacket;
use crate::packet::pubcomp::PubCompPacket;
use crate::packet::publish::PublishPacket;
use crate::packet::pubrec::PubRecPacket;
use crate::packet::pubrel::PubRelPacket;
use crate::packet::suback::{SubAckPacket, SubAckReasonCode};
use crate::packet::subscribe::{SubscribePacket, SubscriptionOptions, TopicFilter};
use crate::packet::unsuback::{UnsubAckPacket, UnsubAckReasonCode};
use crate::packet::unsubscribe::UnsubscribePacket;
use crate::packet::Packet;
use crate::prelude::{vec, Box, String, ToString, Vec};
use crate::protocol::v5::properties::{PropertyId, PropertyValue};
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::qos2;
use crate::session::Subscription;
use crate::types::{ConnectOptions, Message, QoS};

use super::actions::{AckType, ProtocolAction, TimeoutId};
use super::state::{
    ClientSession, ClientState, PendingPubRel, PendingPublish, PendingSubscribe, PendingUnsubscribe,
};

const DEFAULT_ACK_TIMEOUT_MS: u32 = 30_000;

#[derive(Debug)]
pub struct ClientProtocol {
    session: ClientSession,
    ack_timeout_ms: u32,
}

impl ClientProtocol {
    #[must_use]
    pub fn new(client_id: &str) -> Self {
        Self {
            session: ClientSession::new(client_id),
            ack_timeout_ms: DEFAULT_ACK_TIMEOUT_MS,
        }
    }

    #[must_use]
    pub fn with_ack_timeout(mut self, timeout_ms: u32) -> Self {
        self.ack_timeout_ms = timeout_ms;
        self
    }

    #[must_use]
    pub fn state(&self) -> &ClientState {
        self.session.state()
    }

    #[must_use]
    pub fn is_connected(&self) -> bool {
        matches!(self.session.state(), ClientState::Connected { .. })
    }

    #[must_use]
    pub fn client_id(&self) -> &str {
        self.session.client_id()
    }

    #[must_use]
    pub fn connect(&mut self, options: &ConnectOptions) -> Vec<ProtocolAction> {
        let mut actions = Vec::new();

        if !matches!(self.session.state(), ClientState::Disconnected) {
            actions.push(ProtocolAction::error(
                ReasonCode::ProtocolError,
                "Cannot connect: not in disconnected state",
            ));
            return actions;
        }

        self.session.set_state(ClientState::Connecting);
        actions.push(ProtocolAction::state_transition(ClientState::Connecting));

        let connect_packet = ConnectPacket::new(options.clone());
        actions.push(ProtocolAction::send_packet(Packet::Connect(Box::new(
            connect_packet,
        ))));

        actions.push(ProtocolAction::schedule_timeout(
            TimeoutId::ConnAck,
            self.ack_timeout_ms,
        ));

        actions
    }

    #[must_use]
    pub fn handle_connack(&mut self, packet: &ConnAckPacket) -> Vec<ProtocolAction> {
        let mut actions = Vec::new();

        if !matches!(
            self.session.state(),
            ClientState::Connecting | ClientState::AwaitingAuth { .. }
        ) {
            actions.push(ProtocolAction::error(
                ReasonCode::ProtocolError,
                "Received CONNACK in unexpected state",
            ));
            return actions;
        }

        actions.push(ProtocolAction::cancel_timeout(TimeoutId::ConnAck));

        if packet.reason_code != ReasonCode::Success {
            self.session.set_state(ClientState::Disconnected);
            actions.push(ProtocolAction::state_transition(ClientState::Disconnected));
            actions.push(ProtocolAction::error(
                packet.reason_code,
                "Connection refused",
            ));
            return actions;
        }

        let mut receive_maximum = 65535u16;
        let mut max_packet_size = 268_435_455u32;
        let mut topic_alias_maximum = 0u16;
        let mut server_keep_alive = None;

        for (id, value) in packet.properties.iter() {
            match (id, value) {
                (PropertyId::ReceiveMaximum, PropertyValue::TwoByteInteger(v)) => {
                    receive_maximum = *v;
                }
                (PropertyId::MaximumPacketSize, PropertyValue::FourByteInteger(v)) => {
                    max_packet_size = *v;
                }
                (PropertyId::TopicAliasMaximum, PropertyValue::TwoByteInteger(v)) => {
                    topic_alias_maximum = *v;
                }
                (PropertyId::ServerKeepAlive, PropertyValue::TwoByteInteger(v)) => {
                    server_keep_alive = Some(*v);
                }
                _ => {}
            }
        }

        self.session.set_receive_maximum(receive_maximum);
        self.session.set_max_packet_size(max_packet_size);
        self.session
            .update_topic_alias_maximum(topic_alias_maximum, 0);
        self.session.set_server_keep_alive(server_keep_alive);

        if !packet.session_present {
            self.session.reset_for_clean_session();
        }

        let new_state = ClientState::Connected {
            session_present: packet.session_present,
        };
        self.session.set_state(new_state.clone());
        actions.push(ProtocolAction::state_transition(new_state));

        actions.push(ProtocolAction::UpdateServerLimits {
            receive_maximum,
            max_packet_size,
            topic_alias_maximum,
        });

        actions.push(ProtocolAction::ConnectionComplete {
            session_present: packet.session_present,
            server_keep_alive,
        });

        if let Some(keep_alive) = server_keep_alive {
            if keep_alive > 0 {
                actions.push(ProtocolAction::ScheduleKeepalive {
                    interval_secs: keep_alive,
                });
            }
        }

        actions
    }

    #[must_use]
    pub fn subscribe(&mut self, filters: &[(String, SubscriptionOptions)]) -> Vec<ProtocolAction> {
        let mut actions = Vec::new();

        if !self.is_connected() {
            actions.push(ProtocolAction::error(
                ReasonCode::ProtocolError,
                "Cannot subscribe: not connected",
            ));
            return actions;
        }

        if filters.is_empty() {
            actions.push(ProtocolAction::error(
                ReasonCode::ProtocolError,
                "Cannot subscribe: no filters provided",
            ));
            return actions;
        }

        let packet_id = self.session.next_packet_id();
        let mut subscribe_packet = SubscribePacket::new(packet_id);

        let mut topic_filters = Vec::new();
        let mut qos_levels = Vec::new();

        for (filter, options) in filters {
            let topic_filter = TopicFilter::with_options(filter.clone(), *options);
            subscribe_packet = subscribe_packet.add_filter_with_options(topic_filter);
            topic_filters.push(filter.clone());
            qos_levels.push(options.qos);
        }

        self.session.track_pending_suback(
            packet_id,
            PendingSubscribe {
                topic_filters,
                qos_levels,
            },
        );

        actions.push(ProtocolAction::send_packet(Packet::Subscribe(
            subscribe_packet,
        )));
        actions.push(ProtocolAction::TrackPendingAck {
            packet_id,
            ack_type: AckType::SubAck,
        });
        actions.push(ProtocolAction::schedule_timeout(
            TimeoutId::SubAck(packet_id),
            self.ack_timeout_ms,
        ));

        actions
    }

    #[must_use]
    pub fn handle_suback(&mut self, packet: &SubAckPacket) -> Vec<ProtocolAction> {
        let mut actions = Vec::new();

        let Some(pending) = self.session.remove_pending_suback(packet.packet_id) else {
            actions.push(ProtocolAction::error(
                ReasonCode::PacketIdentifierNotFound,
                "Received SUBACK for unknown packet ID",
            ));
            return actions;
        };

        actions.push(ProtocolAction::cancel_timeout(TimeoutId::SubAck(
            packet.packet_id,
        )));
        actions.push(ProtocolAction::RemovePendingAck {
            packet_id: packet.packet_id,
            ack_type: AckType::SubAck,
        });

        for (i, reason_code) in packet.reason_codes.iter().enumerate() {
            if reason_code.is_success() {
                if let Some(filter) = pending.topic_filters.get(i) {
                    let granted_qos = match *reason_code {
                        SubAckReasonCode::GrantedQoS1 => QoS::AtLeastOnce,
                        SubAckReasonCode::GrantedQoS2 => QoS::ExactlyOnce,
                        _ => QoS::AtMostOnce,
                    };
                    let subscription = Subscription {
                        topic_filter: filter.clone(),
                        options: SubscriptionOptions::default().with_qos(granted_qos),
                    };
                    if let Err(e) = self
                        .session
                        .subscriptions_mut()
                        .add(filter.clone(), subscription)
                    {
                        actions.push(ProtocolAction::error(
                            ReasonCode::UnspecifiedError,
                            format!("Failed to track subscription: {e}"),
                        ));
                    }
                }
            }
        }

        actions.push(ProtocolAction::SubscribeComplete {
            packet_id: packet.packet_id,
            granted_qos: packet.reason_codes.clone(),
        });

        actions
    }

    #[must_use]
    pub fn unsubscribe(&mut self, filters: &[String]) -> Vec<ProtocolAction> {
        let mut actions = Vec::new();

        if !self.is_connected() {
            actions.push(ProtocolAction::error(
                ReasonCode::ProtocolError,
                "Cannot unsubscribe: not connected",
            ));
            return actions;
        }

        if filters.is_empty() {
            actions.push(ProtocolAction::error(
                ReasonCode::ProtocolError,
                "Cannot unsubscribe: no filters provided",
            ));
            return actions;
        }

        let packet_id = self.session.next_packet_id();
        let mut unsubscribe_packet = UnsubscribePacket::new(packet_id);
        for filter in filters {
            unsubscribe_packet = unsubscribe_packet.add_filter(filter.clone());
        }

        self.session.track_pending_unsuback(
            packet_id,
            PendingUnsubscribe {
                topic_filters: filters.to_vec(),
            },
        );

        actions.push(ProtocolAction::send_packet(Packet::Unsubscribe(
            unsubscribe_packet,
        )));
        actions.push(ProtocolAction::TrackPendingAck {
            packet_id,
            ack_type: AckType::UnsubAck,
        });
        actions.push(ProtocolAction::schedule_timeout(
            TimeoutId::UnsubAck(packet_id),
            self.ack_timeout_ms,
        ));

        actions
    }

    #[must_use]
    pub fn handle_unsuback(&mut self, packet: &UnsubAckPacket) -> Vec<ProtocolAction> {
        let mut actions = Vec::new();

        let Some(pending) = self.session.remove_pending_unsuback(packet.packet_id) else {
            actions.push(ProtocolAction::error(
                ReasonCode::PacketIdentifierNotFound,
                "Received UNSUBACK for unknown packet ID",
            ));
            return actions;
        };

        actions.push(ProtocolAction::cancel_timeout(TimeoutId::UnsubAck(
            packet.packet_id,
        )));
        actions.push(ProtocolAction::RemovePendingAck {
            packet_id: packet.packet_id,
            ack_type: AckType::UnsubAck,
        });

        for (i, reason_code) in packet.reason_codes.iter().enumerate() {
            if *reason_code == UnsubAckReasonCode::Success {
                if let Some(filter) = pending.topic_filters.get(i) {
                    if let Err(e) = self.session.subscriptions_mut().remove(filter) {
                        actions.push(ProtocolAction::error(
                            ReasonCode::UnspecifiedError,
                            format!("Failed to remove subscription: {e}"),
                        ));
                    }
                }
            }
        }

        actions.push(ProtocolAction::UnsubscribeComplete {
            packet_id: packet.packet_id,
            reason_codes: packet.reason_codes.clone(),
        });

        actions
    }

    #[must_use]
    pub fn publish(
        &mut self,
        topic: &str,
        payload: &[u8],
        qos: QoS,
        retain: bool,
    ) -> Vec<ProtocolAction> {
        let mut actions = Vec::new();

        if !self.is_connected() {
            actions.push(ProtocolAction::error(
                ReasonCode::ProtocolError,
                "Cannot publish: not connected",
            ));
            return actions;
        }

        let packet_id = if qos == QoS::AtMostOnce {
            None
        } else {
            Some(self.session.next_packet_id())
        };

        let mut publish_packet =
            PublishPacket::new(topic.to_string(), payload.to_vec(), qos).with_retain(retain);

        if let Some(pid) = packet_id {
            publish_packet = publish_packet.with_packet_id(pid);
        }

        actions.push(ProtocolAction::send_packet(Packet::Publish(publish_packet)));

        if let Some(pid) = packet_id {
            let pending = PendingPublish {
                topic: topic.to_string(),
                qos,
                retain,
            };

            match qos {
                QoS::AtLeastOnce => {
                    self.session.track_pending_puback(pid, pending);
                    actions.push(ProtocolAction::TrackPendingAck {
                        packet_id: pid,
                        ack_type: AckType::PubAck,
                    });
                    actions.push(ProtocolAction::schedule_timeout(
                        TimeoutId::PubAck(pid),
                        self.ack_timeout_ms,
                    ));
                }
                QoS::ExactlyOnce => {
                    self.session.track_pending_pubrec(pid, pending);
                    actions.push(ProtocolAction::TrackPendingAck {
                        packet_id: pid,
                        ack_type: AckType::PubRec,
                    });
                    actions.push(ProtocolAction::schedule_timeout(
                        TimeoutId::PubRec(pid),
                        self.ack_timeout_ms,
                    ));
                }
                QoS::AtMostOnce => {}
            }
        }

        actions
    }

    #[must_use]
    pub fn handle_puback(&mut self, packet: &PubAckPacket) -> Vec<ProtocolAction> {
        let mut actions = Vec::new();

        if self
            .session
            .remove_pending_puback(packet.packet_id)
            .is_none()
        {
            return actions;
        }

        actions.push(ProtocolAction::cancel_timeout(TimeoutId::PubAck(
            packet.packet_id,
        )));
        actions.push(ProtocolAction::RemovePendingAck {
            packet_id: packet.packet_id,
            ack_type: AckType::PubAck,
        });
        actions.push(ProtocolAction::PublishComplete {
            packet_id: packet.packet_id,
            reason_code: packet.reason_code,
        });

        actions
    }

    #[must_use]
    pub fn handle_pubrec(&mut self, packet: &PubRecPacket) -> Vec<ProtocolAction> {
        let mut actions = Vec::new();

        let has_pending = self.session.has_pending_pubrec(packet.packet_id);
        let qos2_actions =
            qos2::handle_incoming_pubrec(packet.packet_id, packet.reason_code, has_pending);

        for qos2_action in qos2_actions {
            match qos2_action {
                qos2::QoS2Action::SendPubRel { packet_id } => {
                    self.session.remove_pending_pubrec(packet_id);
                    actions.push(ProtocolAction::cancel_timeout(TimeoutId::PubRec(packet_id)));
                    actions.push(ProtocolAction::RemovePendingAck {
                        packet_id,
                        ack_type: AckType::PubRec,
                    });

                    let pubrel = PubRelPacket::new(packet_id);
                    actions.push(ProtocolAction::send_packet(Packet::PubRel(pubrel)));

                    self.session
                        .track_pending_pubcomp(packet_id, PendingPubRel { packet_id });
                    actions.push(ProtocolAction::TrackPendingAck {
                        packet_id,
                        ack_type: AckType::PubComp,
                    });
                    actions.push(ProtocolAction::schedule_timeout(
                        TimeoutId::PubComp(packet_id),
                        self.ack_timeout_ms,
                    ));
                }
                qos2::QoS2Action::ErrorFlow {
                    packet_id,
                    reason_code,
                } => {
                    actions.push(ProtocolAction::cancel_timeout(TimeoutId::PubRec(packet_id)));
                    actions.push(ProtocolAction::PublishComplete {
                        packet_id,
                        reason_code,
                    });
                }
                _ => {}
            }
        }

        actions
    }

    #[must_use]
    pub fn handle_pubcomp(&mut self, packet: &PubCompPacket) -> Vec<ProtocolAction> {
        let mut actions = Vec::new();

        let has_pending = self.session.has_pending_pubcomp(packet.packet_id);
        let qos2_actions =
            qos2::handle_incoming_pubcomp(packet.packet_id, packet.reason_code, has_pending);

        for qos2_action in qos2_actions {
            match qos2_action {
                qos2::QoS2Action::RemoveOutgoingPubRel { packet_id } => {
                    self.session.remove_pending_pubcomp(packet_id);
                    actions.push(ProtocolAction::cancel_timeout(TimeoutId::PubComp(
                        packet_id,
                    )));
                    actions.push(ProtocolAction::RemovePendingAck {
                        packet_id,
                        ack_type: AckType::PubComp,
                    });
                }
                qos2::QoS2Action::CompleteFlow { packet_id } => {
                    actions.push(ProtocolAction::PublishComplete {
                        packet_id,
                        reason_code: ReasonCode::Success,
                    });
                }
                qos2::QoS2Action::ErrorFlow {
                    packet_id,
                    reason_code,
                } => {
                    actions.push(ProtocolAction::PublishComplete {
                        packet_id,
                        reason_code,
                    });
                }
                _ => {}
            }
        }

        actions
    }

    #[must_use]
    pub fn handle_publish(&mut self, packet: &PublishPacket) -> Vec<ProtocolAction> {
        let mut actions = Vec::new();

        match packet.qos {
            QoS::AtMostOnce => {
                let message = Message::from(packet.clone());
                actions.push(ProtocolAction::deliver_message(message));
            }
            QoS::AtLeastOnce => {
                if let Some(packet_id) = packet.packet_id {
                    let message = Message::from(packet.clone());
                    actions.push(ProtocolAction::deliver_message(message));

                    let puback = PubAckPacket::new(packet_id);
                    actions.push(ProtocolAction::send_packet(Packet::PubAck(puback)));
                }
            }
            QoS::ExactlyOnce => {
                if let Some(packet_id) = packet.packet_id {
                    let is_dup = packet.dup;
                    let qos2_actions = qos2::handle_incoming_publish_qos2(packet_id, is_dup);

                    for qos2_action in qos2_actions {
                        match qos2_action {
                            qos2::QoS2Action::DeliverMessage { .. } => {
                                let message = Message::from(packet.clone());
                                actions.push(ProtocolAction::deliver_message(message));
                            }
                            qos2::QoS2Action::SendPubRec {
                                packet_id,
                                reason_code,
                            } => {
                                let pubrec = PubRecPacket::new_with_reason(packet_id, reason_code);
                                actions.push(ProtocolAction::send_packet(Packet::PubRec(pubrec)));
                            }
                            qos2::QoS2Action::TrackIncomingPubRec { packet_id } => {
                                self.session
                                    .track_pending_pubrel(packet_id, PendingPubRel { packet_id });
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        actions
    }

    #[must_use]
    pub fn handle_pubrel(&mut self, packet: &PubRelPacket) -> Vec<ProtocolAction> {
        let mut actions = Vec::new();

        let has_pending = self.session.has_pending_pubrel(packet.packet_id);
        let qos2_actions = qos2::handle_incoming_pubrel(packet.packet_id, has_pending);

        for qos2_action in qos2_actions {
            match qos2_action {
                qos2::QoS2Action::RemoveIncomingPubRec { packet_id } => {
                    self.session.remove_pending_pubrel(packet_id);
                }
                qos2::QoS2Action::SendPubComp {
                    packet_id,
                    reason_code,
                } => {
                    let pubcomp = PubCompPacket::new_with_reason(packet_id, reason_code);
                    actions.push(ProtocolAction::send_packet(Packet::PubComp(pubcomp)));
                }
                _ => {}
            }
        }

        actions
    }

    #[must_use]
    pub fn ping(&mut self) -> Vec<ProtocolAction> {
        let mut actions = Vec::new();

        if !self.is_connected() {
            return actions;
        }

        actions.push(ProtocolAction::send_packet(Packet::PingReq));
        actions.push(ProtocolAction::schedule_timeout(
            TimeoutId::PingResp,
            self.ack_timeout_ms,
        ));

        actions
    }

    #[must_use]
    pub fn handle_pingresp(&mut self) -> Vec<ProtocolAction> {
        vec![ProtocolAction::cancel_timeout(TimeoutId::PingResp)]
    }

    #[must_use]
    pub fn disconnect(&mut self, reason: ReasonCode) -> Vec<ProtocolAction> {
        let mut actions = Vec::new();

        if !self.is_connected() {
            return actions;
        }

        self.session.set_state(ClientState::Disconnecting);
        actions.push(ProtocolAction::state_transition(ClientState::Disconnecting));

        let disconnect_packet = DisconnectPacket::new(reason);
        actions.push(ProtocolAction::send_packet(Packet::Disconnect(
            disconnect_packet,
        )));

        self.session.set_state(ClientState::Disconnected);
        actions.push(ProtocolAction::state_transition(ClientState::Disconnected));
        actions.push(ProtocolAction::disconnect(reason));

        actions
    }

    #[must_use]
    pub fn handle_disconnect(&mut self, packet: &DisconnectPacket) -> Vec<ProtocolAction> {
        let mut actions = Vec::new();

        self.session.set_state(ClientState::Disconnected);
        actions.push(ProtocolAction::state_transition(ClientState::Disconnected));
        actions.push(ProtocolAction::disconnect(packet.reason_code));

        actions
    }

    #[must_use]
    pub fn handle_timeout(&mut self, timeout_id: TimeoutId) -> Vec<ProtocolAction> {
        let mut actions = Vec::new();

        match timeout_id {
            TimeoutId::ConnAck => {
                self.session.set_state(ClientState::Disconnected);
                actions.push(ProtocolAction::state_transition(ClientState::Disconnected));
                actions.push(ProtocolAction::error(
                    ReasonCode::UnspecifiedError,
                    "Connection timeout: no CONNACK received",
                ));
            }
            TimeoutId::PingResp => {
                self.session.set_state(ClientState::Disconnected);
                actions.push(ProtocolAction::state_transition(ClientState::Disconnected));
                actions.push(ProtocolAction::error(
                    ReasonCode::KeepAliveTimeout,
                    "Keepalive timeout: no PINGRESP received",
                ));
            }
            TimeoutId::PubAck(packet_id) => {
                actions.push(ProtocolAction::error(
                    ReasonCode::UnspecifiedError,
                    "Publish timeout: no PUBACK received",
                ));
                actions.push(ProtocolAction::PublishComplete {
                    packet_id,
                    reason_code: ReasonCode::UnspecifiedError,
                });
            }
            TimeoutId::PubRec(packet_id) => {
                actions.push(ProtocolAction::error(
                    ReasonCode::UnspecifiedError,
                    "Publish timeout: no PUBREC received",
                ));
                actions.push(ProtocolAction::PublishComplete {
                    packet_id,
                    reason_code: ReasonCode::UnspecifiedError,
                });
            }
            TimeoutId::PubRel(_packet_id) => {}
            TimeoutId::PubComp(packet_id) => {
                actions.push(ProtocolAction::error(
                    ReasonCode::UnspecifiedError,
                    "Publish timeout: no PUBCOMP received",
                ));
                actions.push(ProtocolAction::PublishComplete {
                    packet_id,
                    reason_code: ReasonCode::UnspecifiedError,
                });
            }
            TimeoutId::SubAck(packet_id) => {
                self.session.remove_pending_suback(packet_id);
                actions.push(ProtocolAction::error(
                    ReasonCode::UnspecifiedError,
                    "Subscribe timeout: no SUBACK received",
                ));
            }
            TimeoutId::UnsubAck(packet_id) => {
                self.session.remove_pending_unsuback(packet_id);
                actions.push(ProtocolAction::error(
                    ReasonCode::UnspecifiedError,
                    "Unsubscribe timeout: no UNSUBACK received",
                ));
            }
        }

        actions
    }

    pub fn reset(&mut self) {
        self.session.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_connected_protocol(client_id: &str) -> ClientProtocol {
        let mut protocol = ClientProtocol::new(client_id);
        let options = ConnectOptions::new(client_id);
        let connect_actions = protocol.connect(&options);
        assert!(!connect_actions.is_empty());

        let connack = ConnAckPacket::new(false, ReasonCode::Success);
        let connack_actions = protocol.handle_connack(&connack);
        assert!(!connack_actions.is_empty());
        assert!(protocol.is_connected());

        protocol
    }

    #[test]
    fn test_connect_flow() {
        let mut protocol = ClientProtocol::new("test-client");

        let options = ConnectOptions::new("test-client");
        let actions = protocol.connect(&options);

        assert!(matches!(protocol.state(), ClientState::Connecting));

        let has_connect_packet = actions
            .iter()
            .any(|a| matches!(a, ProtocolAction::SendPacket(Packet::Connect(_))));
        assert!(has_connect_packet);

        let has_timeout = actions.iter().any(|a| {
            matches!(
                a,
                ProtocolAction::ScheduleTimeout {
                    timeout_id: TimeoutId::ConnAck,
                    ..
                }
            )
        });
        assert!(has_timeout);
    }

    #[test]
    fn test_connack_success() {
        let mut protocol = ClientProtocol::new("test-client");

        let options = ConnectOptions::new("test-client");
        let connect_actions = protocol.connect(&options);
        assert!(!connect_actions.is_empty());

        let connack = ConnAckPacket::new(false, ReasonCode::Success);
        let actions = protocol.handle_connack(&connack);

        assert!(protocol.is_connected());

        let has_connection_complete = actions
            .iter()
            .any(|a| matches!(a, ProtocolAction::ConnectionComplete { .. }));
        assert!(has_connection_complete);
    }

    #[test]
    fn test_connack_failure() {
        let mut protocol = ClientProtocol::new("test-client");

        let options = ConnectOptions::new("test-client");
        let connect_actions = protocol.connect(&options);
        assert!(!connect_actions.is_empty());

        let connack = ConnAckPacket::new(false, ReasonCode::NotAuthorized);
        let actions = protocol.handle_connack(&connack);

        assert!(!protocol.is_connected());

        let has_error = actions.iter().any(ProtocolAction::is_error);
        assert!(has_error);
    }

    #[test]
    fn test_publish_qos0() {
        let mut protocol = setup_connected_protocol("test-client");

        let actions = protocol.publish("test/topic", b"hello", QoS::AtMostOnce, false);

        let has_publish = actions
            .iter()
            .any(|a| matches!(a, ProtocolAction::SendPacket(Packet::Publish(_))));
        assert!(has_publish);

        let has_no_timeout = !actions
            .iter()
            .any(|a| matches!(a, ProtocolAction::ScheduleTimeout { .. }));
        assert!(has_no_timeout);
    }

    #[test]
    fn test_publish_qos1() {
        let mut protocol = setup_connected_protocol("test-client");

        let actions = protocol.publish("test/topic", b"hello", QoS::AtLeastOnce, false);

        let has_publish = actions
            .iter()
            .any(|a| matches!(a, ProtocolAction::SendPacket(Packet::Publish(_))));
        assert!(has_publish);

        let has_timeout = actions.iter().any(|a| {
            matches!(
                a,
                ProtocolAction::ScheduleTimeout {
                    timeout_id: TimeoutId::PubAck(_),
                    ..
                }
            )
        });
        assert!(has_timeout);
    }

    #[test]
    fn test_subscribe_flow() {
        let mut protocol = setup_connected_protocol("test-client");

        let filters = vec![("test/#".to_string(), SubscriptionOptions::default())];
        let actions = protocol.subscribe(&filters);

        let has_subscribe = actions
            .iter()
            .any(|a| matches!(a, ProtocolAction::SendPacket(Packet::Subscribe(_))));
        assert!(has_subscribe);
    }

    #[test]
    fn test_ping_flow() {
        let mut protocol = setup_connected_protocol("test-client");

        let actions = protocol.ping();

        let has_pingreq = actions
            .iter()
            .any(|a| matches!(a, ProtocolAction::SendPacket(Packet::PingReq)));
        assert!(has_pingreq);

        let has_timeout = actions.iter().any(|a| {
            matches!(
                a,
                ProtocolAction::ScheduleTimeout {
                    timeout_id: TimeoutId::PingResp,
                    ..
                }
            )
        });
        assert!(has_timeout);
    }

    #[test]
    fn test_disconnect_flow() {
        let mut protocol = setup_connected_protocol("test-client");

        let actions = protocol.disconnect(ReasonCode::Success);

        assert!(!protocol.is_connected());

        let has_disconnect = actions
            .iter()
            .any(|a| matches!(a, ProtocolAction::SendPacket(Packet::Disconnect(_))));
        assert!(has_disconnect);
    }
}
