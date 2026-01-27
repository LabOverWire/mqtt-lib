//! Publish handling and `QoS` flow control

use crate::broker::events::{ClientPublishEvent, MessageDeliveredEvent};
use crate::broker::storage::{QueuedMessage, StorageBackend};
use crate::error::{MqttError, Result};
use crate::packet::puback::PubAckPacket;
use crate::packet::pubcomp::PubCompPacket;
use crate::packet::publish::PublishPacket;
use crate::packet::pubrec::PubRecPacket;
use crate::packet::pubrel::PubRelPacket;
use crate::packet::Packet;
use crate::protocol::v5::properties::{PropertyId, PropertyValue};
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::transport::packet_io::encode_packet_to_buffer;
use crate::transport::PacketIo;
use crate::QoS;
use crate::Transport;
use std::sync::Arc;
use tracing::{debug, warn};

#[cfg(feature = "opentelemetry")]
use crate::telemetry::propagation;

use super::ClientHandler;

impl ClientHandler {
    #[cfg(feature = "opentelemetry")]
    pub(super) async fn route_with_trace_context(
        &self,
        publish: &PublishPacket,
        client_id: &str,
    ) -> Result<()> {
        let user_props = propagation::extract_user_properties(&publish.properties);
        if let Some(span_context) = propagation::extract_trace_context(&user_props) {
            use opentelemetry::trace::TraceContextExt;
            use tracing::Instrument;
            use tracing_opentelemetry::OpenTelemetrySpanExt;

            let parent_cx =
                opentelemetry::Context::current().with_remote_span_context(span_context);
            let span = tracing::info_span!("broker_publish");
            let _ = span.set_parent(parent_cx);

            self.route_publish(publish, Some(client_id))
                .instrument(span)
                .await;
        } else {
            self.route_publish(publish, Some(client_id)).await;
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(super) async fn handle_publish(&mut self, mut publish: PublishPacket) -> Result<()> {
        let client_id = self.client_id.clone().unwrap();

        if let Some(alias) = publish.properties.get_topic_alias() {
            if alias == 0 || alias > self.config.topic_alias_maximum {
                return Err(MqttError::ProtocolError(format!(
                    "Invalid topic alias: {} (maximum: {})",
                    alias, self.config.topic_alias_maximum
                )));
            }

            if publish.topic_name.is_empty() {
                if let Some(topic) = self.topic_aliases.get(&alias) {
                    publish.topic_name.clone_from(topic);
                } else {
                    return Err(MqttError::ProtocolError(format!(
                        "Topic alias {alias} not found"
                    )));
                }
            } else {
                self.topic_aliases.insert(alias, publish.topic_name.clone());
            }
        } else if publish.topic_name.is_empty() {
            return Err(MqttError::ProtocolError(
                "Empty topic name without topic alias".to_string(),
            ));
        }

        let payload_size = publish.payload.len();
        self.stats.publish_received(payload_size);

        let authorized = self
            .auth_provider
            .authorize_publish(&client_id, self.user_id.as_deref(), &publish.topic_name)
            .await?;

        if !authorized {
            warn!(
                "Client {} (user: {:?}) not authorized to publish to topic: {}",
                client_id, self.user_id, publish.topic_name
            );
            return self.send_not_authorized_response(&publish).await;
        }

        if (publish.qos as u8) > self.config.maximum_qos {
            debug!(
                "Client {} sent QoS {} but max is {}",
                client_id, publish.qos as u8, self.config.maximum_qos
            );
            return self.send_qos_not_supported_response(&publish).await;
        }

        if publish.retain && !publish.payload.is_empty() {
            if let Some(err) = self.check_retained_limits(&publish, &client_id).await? {
                return err;
            }
        }

        if !self
            .resource_monitor
            .can_send_message(&client_id, payload_size)
            .await
        {
            warn!("Message from {} dropped due to rate limit", client_id);
            return self.send_rate_limit_response(&publish).await;
        }

        self.fire_publish_event(&publish, &client_id).await;

        match publish.qos {
            QoS::AtMostOnce => {
                #[cfg(feature = "opentelemetry")]
                self.route_with_trace_context(&publish, &client_id).await?;
                #[cfg(not(feature = "opentelemetry"))]
                self.route_publish(&publish, Some(&client_id)).await;
            }
            QoS::AtLeastOnce => {
                #[cfg(feature = "opentelemetry")]
                self.route_with_trace_context(&publish, &client_id).await?;
                #[cfg(not(feature = "opentelemetry"))]
                self.route_publish(&publish, Some(&client_id)).await;

                let mut puback = PubAckPacket::new(publish.packet_id.unwrap());
                puback.reason_code = ReasonCode::Success;
                self.transport.write_packet(Packet::PubAck(puback)).await?;
            }
            QoS::ExactlyOnce => {
                let packet_id = publish.packet_id.unwrap();
                self.inflight_publishes.insert(packet_id, publish);
                let mut pubrec = PubRecPacket::new(packet_id);
                pubrec.reason_code = ReasonCode::Success;
                self.transport.write_packet(Packet::PubRec(pubrec)).await?;
            }
        }

        Ok(())
    }

    async fn send_not_authorized_response(&mut self, publish: &PublishPacket) -> Result<()> {
        if publish.qos == QoS::AtMostOnce {
            return Ok(());
        }
        match publish.qos {
            QoS::AtLeastOnce => {
                let mut puback = PubAckPacket::new(publish.packet_id.unwrap());
                puback.reason_code = ReasonCode::NotAuthorized;
                if self.request_problem_information {
                    puback.properties.set_reason_string(format!(
                        "Not authorized to publish to topic: {}",
                        publish.topic_name
                    ));
                }
                debug!("Sending PUBACK with NotAuthorized");
                self.transport.write_packet(Packet::PubAck(puback)).await?;
            }
            QoS::ExactlyOnce => {
                let mut pubrec = PubRecPacket::new(publish.packet_id.unwrap());
                pubrec.reason_code = ReasonCode::NotAuthorized;
                if self.request_problem_information {
                    pubrec.properties.set_reason_string(format!(
                        "Not authorized to publish to topic: {}",
                        publish.topic_name
                    ));
                }
                debug!("Sending PUBREC with NotAuthorized");
                self.transport.write_packet(Packet::PubRec(pubrec)).await?;
            }
            QoS::AtMostOnce => {}
        }
        Ok(())
    }

    async fn send_qos_not_supported_response(&mut self, publish: &PublishPacket) -> Result<()> {
        if let Some(packet_id) = publish.packet_id {
            match publish.qos {
                QoS::AtLeastOnce => {
                    let mut puback = PubAckPacket::new(packet_id);
                    puback.reason_code = ReasonCode::QoSNotSupported;
                    if self.request_problem_information {
                        puback.properties.set_reason_string(format!(
                            "QoS {} not supported (maximum: {})",
                            publish.qos as u8, self.config.maximum_qos
                        ));
                    }
                    self.transport.write_packet(Packet::PubAck(puback)).await?;
                }
                QoS::ExactlyOnce => {
                    let mut pubrec = PubRecPacket::new(packet_id);
                    pubrec.reason_code = ReasonCode::QoSNotSupported;
                    if self.request_problem_information {
                        pubrec.properties.set_reason_string(format!(
                            "QoS {} not supported (maximum: {})",
                            publish.qos as u8, self.config.maximum_qos
                        ));
                    }
                    self.transport.write_packet(Packet::PubRec(pubrec)).await?;
                }
                QoS::AtMostOnce => {}
            }
        }
        Ok(())
    }

    async fn check_retained_limits(
        &mut self,
        publish: &PublishPacket,
        client_id: &str,
    ) -> Result<Option<Result<()>>> {
        if self.config.max_retained_message_size > 0
            && publish.payload.len() > self.config.max_retained_message_size
        {
            debug!(
                "Client {} retained message too large ({} > {})",
                client_id,
                publish.payload.len(),
                self.config.max_retained_message_size
            );
            return Ok(Some(
                self.send_quota_exceeded_response(publish, "Retained message too large")
                    .await,
            ));
        }

        if self.config.max_retained_messages > 0 {
            let is_update = self.router.has_retained_message(&publish.topic_name).await;
            if !is_update {
                let current_count = self.router.retained_count().await;
                if current_count >= self.config.max_retained_messages {
                    debug!(
                        "Client {} retained message limit exceeded ({}/{})",
                        client_id, current_count, self.config.max_retained_messages
                    );
                    return Ok(Some(
                        self.send_quota_exceeded_response(
                            publish,
                            "Retained message limit exceeded",
                        )
                        .await,
                    ));
                }
            }
        }
        Ok(None)
    }

    async fn send_quota_exceeded_response(
        &mut self,
        publish: &PublishPacket,
        reason: &str,
    ) -> Result<()> {
        if publish.qos == QoS::AtMostOnce {
            return Ok(());
        }
        match publish.qos {
            QoS::AtLeastOnce => {
                let mut puback = PubAckPacket::new(publish.packet_id.unwrap());
                puback.reason_code = ReasonCode::QuotaExceeded;
                if self.request_problem_information {
                    puback.properties.set_reason_string(reason.to_string());
                }
                self.transport.write_packet(Packet::PubAck(puback)).await?;
            }
            QoS::ExactlyOnce => {
                let mut pubrec = PubRecPacket::new(publish.packet_id.unwrap());
                pubrec.reason_code = ReasonCode::QuotaExceeded;
                if self.request_problem_information {
                    pubrec.properties.set_reason_string(reason.to_string());
                }
                self.transport.write_packet(Packet::PubRec(pubrec)).await?;
            }
            QoS::AtMostOnce => {}
        }
        Ok(())
    }

    async fn send_rate_limit_response(&mut self, publish: &PublishPacket) -> Result<()> {
        if publish.qos == QoS::AtMostOnce {
            return Ok(());
        }
        match publish.qos {
            QoS::AtLeastOnce => {
                let mut puback = PubAckPacket::new(publish.packet_id.unwrap());
                puback.reason_code = ReasonCode::QuotaExceeded;
                if self.request_problem_information {
                    puback
                        .properties
                        .set_reason_string("Rate limit exceeded".to_string());
                }
                self.transport.write_packet(Packet::PubAck(puback)).await?;
            }
            QoS::ExactlyOnce => {
                let mut pubrec = PubRecPacket::new(publish.packet_id.unwrap());
                pubrec.reason_code = ReasonCode::QuotaExceeded;
                if self.request_problem_information {
                    pubrec
                        .properties
                        .set_reason_string("Rate limit exceeded".to_string());
                }
                self.transport.write_packet(Packet::PubRec(pubrec)).await?;
            }
            QoS::AtMostOnce => {}
        }
        Ok(())
    }

    async fn fire_publish_event(&self, publish: &PublishPacket, client_id: &str) {
        if let Some(ref handler) = self.config.event_handler {
            let response_topic = publish
                .properties
                .get(PropertyId::ResponseTopic)
                .and_then(|v| {
                    if let PropertyValue::Utf8String(s) = v {
                        Some(Arc::from(s.as_str()))
                    } else {
                        None
                    }
                });

            let correlation_data = publish
                .properties
                .get(PropertyId::CorrelationData)
                .and_then(|v| {
                    if let PropertyValue::BinaryData(b) = v {
                        Some(b.clone())
                    } else {
                        None
                    }
                });

            let event = ClientPublishEvent {
                client_id: client_id.to_string().into(),
                topic: publish.topic_name.clone().into(),
                payload: publish.payload.clone(),
                qos: publish.qos,
                retain: publish.retain,
                packet_id: publish.packet_id,
                response_topic,
                correlation_data,
            };
            handler.on_client_publish(event).await;
        }
    }

    pub(super) async fn handle_puback(&mut self, puback: &PubAckPacket) {
        if self.outbound_inflight.remove(&puback.packet_id) {
            tracing::trace!(
                packet_id = puback.packet_id,
                inflight = self.outbound_inflight.len(),
                "Released outbound flow control quota on PUBACK"
            );

            if let Some(ref handler) = self.config.event_handler {
                if let Some(ref client_id) = self.client_id {
                    let event = MessageDeliveredEvent {
                        client_id: Arc::from(client_id.as_str()),
                        packet_id: puback.packet_id,
                        qos: QoS::AtLeastOnce,
                    };
                    handler.on_message_delivered(event).await;
                }
            }
        }
    }

    pub(super) async fn handle_pubrec(&mut self, pubrec: PubRecPacket) -> Result<()> {
        let mut pub_rel = PubRelPacket::new(pubrec.packet_id);
        pub_rel.reason_code = ReasonCode::Success;
        self.transport.write_packet(Packet::PubRel(pub_rel)).await
    }

    pub(super) async fn handle_pubrel(&mut self, pubrel: PubRelPacket) -> Result<()> {
        if let Some(publish) = self.inflight_publishes.remove(&pubrel.packet_id) {
            let client_id = self.client_id.as_ref().unwrap();

            #[cfg(feature = "opentelemetry")]
            self.route_with_trace_context(&publish, client_id).await?;
            #[cfg(not(feature = "opentelemetry"))]
            self.route_publish(&publish, Some(client_id)).await;
        }

        let mut pubcomp = PubCompPacket::new(pubrel.packet_id);
        pubcomp.reason_code = ReasonCode::Success;
        self.transport.write_packet(Packet::PubComp(pubcomp)).await
    }

    pub(super) async fn handle_pubcomp(&mut self, pubcomp: &PubCompPacket) {
        if self.outbound_inflight.remove(&pubcomp.packet_id) {
            tracing::trace!(
                packet_id = pubcomp.packet_id,
                inflight = self.outbound_inflight.len(),
                "Released outbound flow control quota on PUBCOMP"
            );

            if let Some(ref handler) = self.config.event_handler {
                if let Some(ref client_id) = self.client_id {
                    let event = MessageDeliveredEvent {
                        client_id: Arc::from(client_id.as_str()),
                        packet_id: pubcomp.packet_id,
                        qos: QoS::ExactlyOnce,
                    };
                    handler.on_message_delivered(event).await;
                }
            }
        }
    }

    pub(super) async fn send_publish(&mut self, mut publish: PublishPacket) -> Result<()> {
        if publish.qos != QoS::AtMostOnce {
            if self.outbound_inflight.len() >= usize::from(self.client_receive_maximum) {
                if let Some(ref storage) = self.storage {
                    let client_id = self.client_id.as_ref().unwrap();
                    let queued_msg =
                        QueuedMessage::new(publish.clone(), client_id.clone(), publish.qos, None);
                    storage.queue_message(queued_msg).await?;
                    debug!(
                        client_id = %client_id,
                        inflight = self.outbound_inflight.len(),
                        max = self.client_receive_maximum,
                        "Message queued due to receive maximum limit"
                    );
                }
                return Ok(());
            }

            if publish.packet_id.is_none() {
                publish.packet_id = Some(self.next_packet_id());
            }

            if let Some(packet_id) = publish.packet_id {
                self.outbound_inflight.insert(packet_id);
            }
        }

        let payload_size = publish.payload.len();
        tracing::debug!(
            topic = %publish.topic_name,
            qos = ?publish.qos,
            retain = publish.retain,
            payload_len = payload_size,
            packet_id = ?publish.packet_id,
            "Sending PUBLISH to client"
        );
        self.write_buffer.clear();
        encode_packet_to_buffer(&Packet::Publish(publish), &mut self.write_buffer)?;
        self.transport.write(&self.write_buffer).await?;
        self.stats.publish_sent(payload_size);
        Ok(())
    }
}
