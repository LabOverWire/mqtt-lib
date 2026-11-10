use mqtt5::broker::storage::{InflightDirection, InflightMessage, InflightPhase, StorageBackend};
use mqtt5_protocol::error::Result;
use mqtt5_protocol::packet::puback::PubAckPacket;
use mqtt5_protocol::packet::pubcomp::PubCompPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::pubrec::PubRecPacket;
use mqtt5_protocol::packet::pubrel::PubRelPacket;
use mqtt5_protocol::packet::Packet;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use mqtt5_protocol::QoS;
use tracing::{debug, warn};

use super::SharedWriter;

use super::WasmClientHandler;

impl WasmClientHandler {
    pub(super) async fn handle_publish(
        &mut self,
        mut publish: PublishPacket,
        writer: &SharedWriter,
    ) -> Result<()> {
        let client_id = self.client_id.as_ref().unwrap();
        self.stats.publish_received(publish.payload.len());

        let authorized = self
            .auth_provider
            .authorize_publish(client_id, self.user_id.as_deref(), &publish.topic_name)
            .await;

        if !authorized {
            warn!(
                "Client {client_id} not authorized to publish to {}",
                publish.topic_name
            );
            Self::reject_publish(&publish, ReasonCode::NotAuthorized, writer)?;
            return Ok(());
        }

        let max_qos = self.config.read().map_or_else(
            |_| {
                warn!("Config read failed for max_qos, using default 2");
                2
            },
            |c| c.maximum_qos,
        );
        if (publish.qos as u8) > max_qos {
            debug!(
                "Client {client_id} sent QoS {} but max is {max_qos}",
                publish.qos as u8
            );
            Self::reject_publish(&publish, ReasonCode::QoSNotSupported, writer)?;
            return Ok(());
        }

        let payload_size = publish.payload.len();
        if !self
            .resource_monitor
            .can_send_message(client_id, payload_size)
            .await
        {
            warn!("Client {client_id} exceeded quota");
            Self::reject_publish(&publish, ReasonCode::QuotaExceeded, writer)?;
            return Ok(());
        }

        publish.properties.inject_sender(self.user_id.as_deref());
        publish
            .properties
            .inject_client_id(Some(client_id.as_str()));

        match publish.qos {
            QoS::AtMostOnce => {
                self.fire_client_publish(
                    client_id,
                    &publish.topic_name,
                    publish.qos as u8,
                    publish.retain,
                    payload_size,
                );
                self.router.route_message(&publish, Some(client_id)).await;
            }
            QoS::AtLeastOnce => {
                self.fire_client_publish(
                    client_id,
                    &publish.topic_name,
                    publish.qos as u8,
                    publish.retain,
                    payload_size,
                );
                self.router.route_message(&publish, Some(client_id)).await;
                let puback = PubAckPacket::new(publish.packet_id.unwrap());
                Self::write_packet(&Packet::PubAck(puback), writer)?;
            }
            QoS::ExactlyOnce => {
                self.fire_client_publish(
                    client_id,
                    &publish.topic_name,
                    publish.qos as u8,
                    publish.retain,
                    payload_size,
                );
                let packet_id = publish.packet_id.unwrap();
                let inflight = InflightMessage::from_publish(
                    &publish,
                    client_id.clone(),
                    InflightDirection::Inbound,
                    InflightPhase::AwaitingPubrel,
                );
                if let Err(e) = self.storage.store_inflight_message(inflight).await {
                    debug!("failed to persist inbound inflight {packet_id}: {e}");
                }
                self.inflight_publishes.insert(packet_id, publish);
                let pubrec = PubRecPacket::new(packet_id);
                Self::write_packet(&Packet::PubRec(pubrec), writer)?;
            }
        }

        Ok(())
    }

    fn reject_publish(
        publish: &PublishPacket,
        reason_code: ReasonCode,
        writer: &SharedWriter,
    ) -> Result<()> {
        if let Some(packet_id) = publish.packet_id {
            match publish.qos {
                QoS::AtLeastOnce => {
                    let mut puback = PubAckPacket::new(packet_id);
                    puback.reason_code = reason_code;
                    Self::write_packet(&Packet::PubAck(puback), writer)?;
                }
                QoS::ExactlyOnce => {
                    let mut pubrec = PubRecPacket::new(packet_id);
                    pubrec.reason_code = reason_code;
                    Self::write_packet(&Packet::PubRec(pubrec), writer)?;
                }
                QoS::AtMostOnce => {}
            }
        }
        Ok(())
    }

    pub(super) async fn handle_puback(&mut self, puback: &PubAckPacket) {
        self.finish_outbound(puback.packet_id).await;
        if let Some(client_id) = self.client_id.as_ref() {
            self.fire_message_delivered(client_id, puback.packet_id, 1);
        }
    }

    pub(super) async fn handle_pubrec(
        &self,
        pubrec: &PubRecPacket,
        writer: &SharedWriter,
    ) -> Result<()> {
        if pubrec.reason_code.is_error() {
            self.finish_outbound(pubrec.packet_id).await;
            return Ok(());
        }
        self.outbound_inflight
            .borrow_mut()
            .mark_released(pubrec.packet_id);
        let inflight = self.client_id.as_ref().and_then(|client_id| {
            self.outbound_inflight
                .borrow()
                .get(pubrec.packet_id)
                .map(|publish| {
                    InflightMessage::from_publish(
                        publish,
                        client_id.clone(),
                        InflightDirection::Outbound,
                        InflightPhase::AwaitingPubcomp,
                    )
                })
        });
        if let Some(inflight) = inflight {
            if let Err(e) = self.storage.store_inflight_message(inflight).await {
                debug!(
                    "failed to update inflight phase for {}: {e}",
                    pubrec.packet_id
                );
            }
        }
        Self::write_packet(&Packet::PubRel(PubRelPacket::new(pubrec.packet_id)), writer)?;
        Ok(())
    }

    pub(super) async fn handle_pubrel(
        &mut self,
        pubrel: PubRelPacket,
        writer: &SharedWriter,
    ) -> Result<()> {
        let reason_code = if let Some(publish) = self.inflight_publishes.remove(&pubrel.packet_id) {
            let client_id = self.client_id.as_ref().unwrap();
            let _ = self
                .storage
                .remove_inflight_message(client_id, pubrel.packet_id, InflightDirection::Inbound)
                .await;
            self.router.route_message(&publish, Some(client_id)).await;
            ReasonCode::Success
        } else {
            ReasonCode::PacketIdentifierNotFound
        };

        let pubcomp = PubCompPacket::new_with_reason(pubrel.packet_id, reason_code);
        Self::write_packet(&Packet::PubComp(pubcomp), writer)?;
        Ok(())
    }

    pub(super) async fn handle_pubcomp(&mut self, pubcomp: &PubCompPacket) {
        self.finish_outbound(pubcomp.packet_id).await;
        if let Some(client_id) = self.client_id.as_ref() {
            self.fire_message_delivered(client_id, pubcomp.packet_id, 2);
        }
    }

    async fn finish_outbound(&self, packet_id: u16) {
        self.outbound_inflight.borrow_mut().remove(packet_id);
        if let Some(client_id) = self.client_id.as_ref() {
            let _ = self
                .storage
                .remove_inflight_message(client_id, packet_id, InflightDirection::Outbound)
                .await;
        }
        self.acknowledged.notify_one();
    }

    /// Restores the exactly-once state of a resumed session: a PUBREL goes again for each
    /// outbound delivery the client has received, and inbound ones wait for their PUBREL. The
    /// forwarder resends unacknowledged deliveries once the session is handed over.
    pub(super) async fn restore_inflight_messages(&mut self, writer: &SharedWriter) -> Result<()> {
        let client_id = self.client_id.as_ref().unwrap();
        let mut inflights = match self.storage.get_inflight_messages(client_id).await {
            Ok(msgs) => msgs,
            Err(e) => {
                warn!("failed to load inflight messages for {client_id}: {e}");
                return Ok(());
            }
        };

        inflights.sort_by_key(|msg| (msg.stored_at_secs, msg.packet_id));

        for msg in inflights {
            match (msg.direction, msg.phase) {
                (InflightDirection::Outbound, InflightPhase::AwaitingPubcomp) => {
                    self.outbound_inflight
                        .borrow_mut()
                        .insert_released(msg.packet_id, msg.to_publish_packet());
                    let pubrel = PubRelPacket::new(msg.packet_id);
                    Self::write_packet(&Packet::PubRel(pubrel), writer)?;
                }
                (InflightDirection::Inbound, _) => {
                    self.inflight_publishes
                        .insert(msg.packet_id, msg.to_publish_packet());
                }
                (InflightDirection::Outbound, _) => {}
            }
        }
        Ok(())
    }
}
