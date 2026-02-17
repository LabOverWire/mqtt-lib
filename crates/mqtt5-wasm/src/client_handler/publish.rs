use bytes::BytesMut;
use mqtt5::broker::storage::{InflightDirection, InflightMessage, InflightPhase, StorageBackend};
use mqtt5_protocol::error::Result;
use mqtt5_protocol::packet::puback::PubAckPacket;
use mqtt5_protocol::packet::pubcomp::PubCompPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::pubrec::PubRecPacket;
use mqtt5_protocol::packet::pubrel::PubRelPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::packet::Packet;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use mqtt5_protocol::QoS;
use tracing::{debug, warn};

use crate::transport::WasmWriter;

use super::WasmClientHandler;

impl WasmClientHandler {
    #[allow(clippy::too_many_lines)]
    pub(super) async fn handle_publish(
        &mut self,
        mut publish: PublishPacket,
        writer: &mut WasmWriter,
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
            if let Some(packet_id) = publish.packet_id {
                match publish.qos {
                    QoS::AtLeastOnce => {
                        let mut puback = PubAckPacket::new(packet_id);
                        puback.reason_code = ReasonCode::NotAuthorized;
                        self.write_packet(&Packet::PubAck(puback), writer)?;
                    }
                    QoS::ExactlyOnce => {
                        let mut pubrec = PubRecPacket::new(packet_id);
                        pubrec.reason_code = ReasonCode::NotAuthorized;
                        self.write_packet(&Packet::PubRec(pubrec), writer)?;
                    }
                    QoS::AtMostOnce => {}
                }
            }
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
            if let Some(packet_id) = publish.packet_id {
                match publish.qos {
                    QoS::AtLeastOnce => {
                        let mut puback = PubAckPacket::new(packet_id);
                        puback.reason_code = ReasonCode::QoSNotSupported;
                        self.write_packet(&Packet::PubAck(puback), writer)?;
                    }
                    QoS::ExactlyOnce => {
                        let mut pubrec = PubRecPacket::new(packet_id);
                        pubrec.reason_code = ReasonCode::QoSNotSupported;
                        self.write_packet(&Packet::PubRec(pubrec), writer)?;
                    }
                    QoS::AtMostOnce => {}
                }
            }
            return Ok(());
        }

        let payload_size = publish.payload.len();
        if !self
            .resource_monitor
            .can_send_message(client_id, payload_size)
            .await
        {
            warn!("Client {client_id} exceeded quota");
            if let Some(packet_id) = publish.packet_id {
                match publish.qos {
                    QoS::AtLeastOnce => {
                        let mut puback = PubAckPacket::new(packet_id);
                        puback.reason_code = ReasonCode::QuotaExceeded;
                        self.write_packet(&Packet::PubAck(puback), writer)?;
                    }
                    QoS::ExactlyOnce => {
                        let mut pubrec = PubRecPacket::new(packet_id);
                        pubrec.reason_code = ReasonCode::QuotaExceeded;
                        self.write_packet(&Packet::PubRec(pubrec), writer)?;
                    }
                    QoS::AtMostOnce => {}
                }
            }
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
                self.write_packet(&Packet::PubAck(puback), writer)?;
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
                self.write_packet(&Packet::PubRec(pubrec), writer)?;
            }
        }

        Ok(())
    }

    pub(super) fn handle_puback(&mut self, puback: &PubAckPacket) {
        self.outbound_inflight
            .borrow_mut()
            .remove(&puback.packet_id);
        if let Some(client_id) = self.client_id.as_ref() {
            self.fire_message_delivered(client_id, puback.packet_id, 1);
        }
    }

    #[allow(clippy::similar_names)]
    pub(super) async fn handle_pubrec(
        &self,
        pubrec: &PubRecPacket,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        let inflight = self.client_id.as_ref().and_then(|client_id| {
            self.outbound_inflight
                .borrow()
                .get(&pubrec.packet_id)
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
        let pubrel = PubRelPacket::new(pubrec.packet_id);
        self.write_packet(&Packet::PubRel(pubrel), writer)?;
        Ok(())
    }

    pub(super) async fn handle_pubrel(
        &mut self,
        pubrel: PubRelPacket,
        writer: &mut WasmWriter,
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
        self.write_packet(&Packet::PubComp(pubcomp), writer)?;
        Ok(())
    }

    pub(super) async fn handle_pubcomp(&mut self, pubcomp: &PubCompPacket) {
        self.outbound_inflight
            .borrow_mut()
            .remove(&pubcomp.packet_id);
        if let Some(client_id) = self.client_id.as_ref() {
            let _ = self
                .storage
                .remove_inflight_message(client_id, pubcomp.packet_id, InflightDirection::Outbound)
                .await;
            self.fire_message_delivered(client_id, pubcomp.packet_id, 2);
        }
    }

    pub(super) async fn resend_inflight_messages(&mut self, writer: &mut WasmWriter) -> Result<()> {
        let client_id = self.client_id.as_ref().unwrap();
        let inflights = match self.storage.get_inflight_messages(client_id).await {
            Ok(msgs) => msgs,
            Err(e) => {
                warn!("failed to load inflight messages for {client_id}: {e}");
                return Ok(());
            }
        };

        for msg in inflights {
            match msg.direction {
                InflightDirection::Outbound => match msg.phase {
                    InflightPhase::AwaitingPubrec => {
                        let mut publish = msg.to_publish_packet();
                        publish.dup = true;
                        self.outbound_inflight
                            .borrow_mut()
                            .insert(msg.packet_id, publish.clone());
                        self.write_packet(&Packet::Publish(publish), writer)?;
                    }
                    InflightPhase::AwaitingPubcomp => {
                        let publish = msg.to_publish_packet();
                        self.outbound_inflight
                            .borrow_mut()
                            .insert(msg.packet_id, publish);
                        let pubrel = PubRelPacket::new(msg.packet_id);
                        self.write_packet(&Packet::PubRel(pubrel), writer)?;
                    }
                    InflightPhase::AwaitingPubrel => {}
                },
                InflightDirection::Inbound => {
                    let publish = msg.to_publish_packet();
                    self.inflight_publishes.insert(msg.packet_id, publish);
                }
            }
        }
        Ok(())
    }

    pub(super) fn send_publish(
        &self,
        publish: PublishPacket,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        self.write_packet(&Packet::Publish(publish), writer)
    }

    pub(super) fn write_publish_packet(
        publish: &PublishPacket,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        let mut buf = BytesMut::new();
        publish.encode(&mut buf)?;
        writer.write(&buf)?;
        Ok(())
    }
}
