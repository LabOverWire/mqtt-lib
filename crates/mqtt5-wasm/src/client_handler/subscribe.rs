use mqtt5::broker::storage::{StorageBackend, StoredSubscription};
use mqtt5_protocol::error::{MqttError, Result};
use mqtt5_protocol::packet::disconnect::DisconnectPacket;
use mqtt5_protocol::packet::suback::{SubAckPacket, SubAckReasonCode};
use mqtt5_protocol::packet::subscribe::SubscribePacket;
use mqtt5_protocol::packet::unsuback::{UnsubAckPacket, UnsubAckReasonCode};
use mqtt5_protocol::packet::unsubscribe::UnsubscribePacket;
use mqtt5_protocol::packet::Packet;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use mqtt5_protocol::topic_matches_filter;
use mqtt5_protocol::types::ProtocolVersion;
use mqtt5_protocol::validation::{strip_shared_subscription_prefix, validate_topic_filter};
use mqtt5_protocol::QoS;
use tracing::{debug, warn};

use crate::transport::WasmWriter;

use super::WasmClientHandler;

impl WasmClientHandler {
    pub(super) async fn handle_subscribe(
        &mut self,
        subscribe: SubscribePacket,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        let client_id = self.client_id.clone().unwrap();
        let mut reason_codes = Vec::new();
        let mut successful_subscriptions = Vec::new();

        for filter in &subscribe.filters {
            if filter.options.no_local && filter.filter.starts_with("$share/") {
                warn!(
                    "Client {client_id} set NoLocal on shared subscription {}",
                    filter.filter
                );
                let disconnect = DisconnectPacket {
                    reason_code: ReasonCode::ProtocolError,
                    properties: mqtt5_protocol::protocol::v5::properties::Properties::default(),
                };
                self.write_packet(&Packet::Disconnect(disconnect), writer)?;
                return Err(MqttError::ProtocolError(
                    "NoLocal on shared subscription".to_string(),
                ));
            }

            let underlying = strip_shared_subscription_prefix(&filter.filter);
            if validate_topic_filter(underlying).is_err() {
                warn!(
                    "Client {client_id} sent invalid topic filter: {}",
                    filter.filter
                );
                reason_codes.push(SubAckReasonCode::TopicFilterInvalid);
                continue;
            }

            let authorized = self
                .auth_provider
                .authorize_subscribe(&client_id, self.user_id.as_deref(), &filter.filter)
                .await;

            if !authorized {
                reason_codes.push(SubAckReasonCode::NotAuthorized);
                continue;
            }

            let granted_qos = self.resolve_granted_qos(filter.options.qos);
            let subscription_id = subscribe.properties.get_subscription_identifier();
            let change_only = self.is_change_only_filter(&filter.filter);

            self.router
                .subscribe(
                    client_id.clone(),
                    filter.filter.clone(),
                    granted_qos,
                    subscription_id,
                    filter.options.no_local,
                    filter.options.retain_as_published,
                    filter.options.retain_handling as u8,
                    ProtocolVersion::try_from(self.protocol_version).unwrap_or_default(),
                    change_only,
                )
                .await?;

            self.persist_subscription(filter, granted_qos, subscription_id, change_only)
                .await;
            self.deliver_retained_for_filter(filter, writer).await?;

            successful_subscriptions.push((filter.filter.clone(), granted_qos as u8));
            reason_codes.push(SubAckReasonCode::from_qos(granted_qos));
        }

        let mut suback = SubAckPacket::new(subscribe.packet_id);
        suback.reason_codes = reason_codes;
        suback.protocol_version = self.protocol_version;

        self.write_packet(&Packet::SubAck(suback), writer)?;

        if !successful_subscriptions.is_empty() {
            self.fire_client_subscribe(&client_id, &successful_subscriptions);
        }

        debug!("Client {} subscribed to topics", client_id);
        Ok(())
    }

    fn resolve_granted_qos(&self, requested: QoS) -> QoS {
        let max_qos = self.config.read().map_or_else(
            |_| {
                warn!("Config read failed for max_qos, using default 2");
                2
            },
            |c| c.maximum_qos,
        );
        if requested as u8 > max_qos {
            QoS::from(max_qos)
        } else {
            requested
        }
    }

    fn is_change_only_filter(&self, topic_filter: &str) -> bool {
        self.config.read().is_ok_and(|c| {
            c.change_only_delivery_config.enabled
                && c.change_only_delivery_config
                    .topic_patterns
                    .iter()
                    .any(|pattern| topic_matches_filter(topic_filter, pattern))
        })
    }

    async fn persist_subscription(
        &mut self,
        filter: &mqtt5_protocol::packet::subscribe::TopicFilter,
        granted_qos: QoS,
        subscription_id: Option<u32>,
        change_only: bool,
    ) {
        if let Some(ref mut session) = self.session {
            let stored = StoredSubscription {
                qos: granted_qos,
                no_local: filter.options.no_local,
                retain_as_published: filter.options.retain_as_published,
                retain_handling: filter.options.retain_handling as u8,
                subscription_id,
                protocol_version: self.protocol_version,
                change_only,
            };
            session.add_subscription(filter.filter.clone(), stored);
            self.storage.store_session(session.clone()).await.ok();
        }
    }

    async fn deliver_retained_for_filter(
        &mut self,
        filter: &mqtt5_protocol::packet::subscribe::TopicFilter,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        if filter.options.retain_handling
            != mqtt5_protocol::packet::subscribe::RetainHandling::DoNotSend
        {
            let retained = self.router.get_retained_messages(&filter.filter).await;
            for mut msg in retained {
                msg.retain = true;
                self.send_publish(msg, writer)?;
            }
        }
        Ok(())
    }

    pub(super) async fn handle_unsubscribe(
        &mut self,
        unsubscribe: UnsubscribePacket,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        let client_id = self.client_id.as_ref().unwrap();
        let mut reason_codes = Vec::new();

        for filter in &unsubscribe.filters {
            let removed = self.router.unsubscribe(client_id, filter).await;

            if removed {
                if let Some(ref mut session) = self.session {
                    session.remove_subscription(filter);
                    self.storage.store_session(session.clone()).await.ok();
                }
            }

            reason_codes.push(if removed {
                UnsubAckReasonCode::Success
            } else {
                UnsubAckReasonCode::NoSubscriptionExisted
            });
        }

        let mut unsuback = UnsubAckPacket::new(unsubscribe.packet_id);
        unsuback.reason_codes = reason_codes;
        unsuback.protocol_version = self.protocol_version;

        self.write_packet(&Packet::UnsubAck(unsuback), writer)?;

        if !unsubscribe.filters.is_empty() {
            self.fire_client_unsubscribe(client_id, &unsubscribe.filters);
        }

        Ok(())
    }

    pub(super) async fn deliver_queued_messages(
        &mut self,
        client_id: &str,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        let queued_messages = self.storage.get_queued_messages(client_id).await?;
        self.storage.remove_queued_messages(client_id).await?;

        if !queued_messages.is_empty() {
            debug!(
                "Delivering {} queued messages to {}",
                queued_messages.len(),
                client_id
            );
            for msg in queued_messages {
                let publish = msg.to_publish_packet();
                self.send_publish(publish, writer)?;
            }
        }
        Ok(())
    }
}
