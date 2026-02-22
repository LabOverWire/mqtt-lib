use crate::broker::events::{
    ClientSubscribeEvent, ClientUnsubscribeEvent, SubAckReasonCode, SubscriptionInfo,
};
use crate::broker::storage::{StorageBackend, StoredSubscription};
use crate::error::{MqttError, Result};
use crate::packet::disconnect::DisconnectPacket;
use crate::packet::suback::SubAckPacket;
use crate::packet::subscribe::SubscribePacket;
use crate::packet::unsuback::UnsubAckPacket;
use crate::packet::unsubscribe::UnsubscribePacket;
use crate::packet::Packet;
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::transport::PacketIo;
use crate::types::ProtocolVersion;
use crate::validation::{parse_shared_subscription, topic_matches_filter, validate_topic_filter};
use crate::QoS;
use tracing::{debug, warn};

use super::ClientHandler;

impl ClientHandler {
    pub(super) async fn handle_subscribe(&mut self, subscribe: SubscribePacket) -> Result<()> {
        let client_id = self.client_id.clone().unwrap();
        let mut reason_codes: Vec<crate::packet::suback::SubAckReasonCode> = Vec::new();

        for filter in &subscribe.filters {
            if let Some(rc) = self.validate_subscribe_filter(filter, &client_id).await? {
                reason_codes.push(rc);
                continue;
            }

            if let Some(rc) = self.check_subscription_quota(filter, &client_id).await {
                reason_codes.push(rc);
                continue;
            }

            let granted_qos = if filter.options.qos as u8 > self.config.maximum_qos {
                self.config.maximum_qos
            } else {
                filter.options.qos as u8
            };

            let change_only = self.config.change_only_delivery_config.enabled
                && self
                    .config
                    .change_only_delivery_config
                    .topic_patterns
                    .iter()
                    .any(|pattern| topic_matches_filter(&filter.filter, pattern));

            let is_new = self
                .router
                .subscribe(
                    client_id.clone(),
                    filter.filter.clone(),
                    QoS::from(granted_qos),
                    subscribe.properties.get_subscription_identifier(),
                    filter.options.no_local,
                    filter.options.retain_as_published,
                    filter.options.retain_handling as u8,
                    ProtocolVersion::try_from(self.protocol_version).unwrap_or_default(),
                    change_only,
                )
                .await?;

            self.persist_subscription(
                &filter.filter,
                &filter.options,
                granted_qos,
                subscribe.properties.get_subscription_identifier(),
                change_only,
            )
            .await;

            self.deliver_retained_for_filter(&filter.filter, &filter.options, is_new)
                .await?;

            reason_codes.push(crate::packet::suback::SubAckReasonCode::from_qos(
                QoS::from(granted_qos),
            ));
        }

        self.build_and_send_suback(&subscribe, &reason_codes).await
    }

    async fn validate_subscribe_filter(
        &mut self,
        filter: &crate::packet::subscribe::TopicFilter,
        client_id: &str,
    ) -> Result<Option<crate::packet::suback::SubAckReasonCode>> {
        if filter.options.no_local && filter.filter.starts_with("$share/") {
            warn!(
                "Client {client_id} set NoLocal on shared subscription {}",
                filter.filter
            );
            if self.protocol_version == 5 {
                let disconnect = DisconnectPacket::new(ReasonCode::ProtocolError);
                self.transport
                    .write_packet(Packet::Disconnect(disconnect))
                    .await?;
            }
            return Err(MqttError::ProtocolError(
                "NoLocal on shared subscription".to_string(),
            ));
        }

        let (underlying, share_group) = parse_shared_subscription(&filter.filter);
        if validate_topic_filter(underlying).is_err() {
            warn!(
                "Client {client_id} sent invalid topic filter: {}",
                filter.filter
            );
            return Ok(Some(
                crate::packet::suback::SubAckReasonCode::TopicFilterInvalid,
            ));
        }

        if filter.filter.starts_with("$share/") {
            match share_group {
                None => {
                    warn!(
                        "Client {client_id} sent malformed shared subscription: {}",
                        filter.filter
                    );
                    return Ok(Some(
                        crate::packet::suback::SubAckReasonCode::TopicFilterInvalid,
                    ));
                }
                Some(group) if group.contains('+') || group.contains('#') => {
                    warn!(
                        "Client {client_id} sent shared subscription with invalid ShareName: {}",
                        filter.filter
                    );
                    return Ok(Some(
                        crate::packet::suback::SubAckReasonCode::TopicFilterInvalid,
                    ));
                }
                _ => {}
            }
        }

        let authorized = self
            .auth_provider
            .authorize_subscribe(client_id, self.user_id.as_deref(), &filter.filter)
            .await;

        if !authorized {
            return Ok(Some(crate::packet::suback::SubAckReasonCode::NotAuthorized));
        }

        Ok(None)
    }

    async fn check_subscription_quota(
        &self,
        filter: &crate::packet::subscribe::TopicFilter,
        client_id: &str,
    ) -> Option<crate::packet::suback::SubAckReasonCode> {
        if self.config.max_subscriptions_per_client > 0 {
            let is_existing = self
                .router
                .has_subscription(client_id, &filter.filter)
                .await;
            if !is_existing {
                let current_count = self.router.subscription_count_for_client(client_id).await;
                if current_count >= self.config.max_subscriptions_per_client {
                    debug!(
                        "Client {} subscription quota exceeded ({}/{})",
                        client_id, current_count, self.config.max_subscriptions_per_client
                    );
                    return Some(crate::packet::suback::SubAckReasonCode::QuotaExceeded);
                }
            }
        }
        None
    }

    async fn persist_subscription(
        &mut self,
        topic_filter: &str,
        options: &crate::packet::subscribe::SubscriptionOptions,
        granted_qos: u8,
        subscription_id: Option<u32>,
        change_only: bool,
    ) {
        if let Some(ref mut session) = self.session {
            let stored = StoredSubscription {
                qos: QoS::from(granted_qos),
                no_local: options.no_local,
                retain_as_published: options.retain_as_published,
                retain_handling: options.retain_handling as u8,
                subscription_id,
                protocol_version: self.protocol_version,
                change_only,
            };
            session.add_subscription(topic_filter.to_string(), stored);
            if let Some(ref storage) = self.storage {
                if let Err(e) = storage.store_session(session.clone()).await {
                    warn!("Failed to store session: {e}");
                }
            }
        }
    }

    async fn deliver_retained_for_filter(
        &self,
        topic_filter: &str,
        options: &crate::packet::subscribe::SubscriptionOptions,
        is_new: bool,
    ) -> Result<()> {
        let should_send = match options.retain_handling {
            crate::packet::subscribe::RetainHandling::SendAtSubscribe => true,
            crate::packet::subscribe::RetainHandling::SendAtSubscribeIfNew => is_new,
            crate::packet::subscribe::RetainHandling::DoNotSend => false,
        };

        if should_send {
            let retained = self.router.get_retained_messages(topic_filter).await;
            debug!(
                topic = %topic_filter,
                count = retained.len(),
                "Retained messages found for subscription"
            );
            for mut msg in retained {
                debug!(
                    topic = %msg.topic_name,
                    qos = ?msg.qos,
                    payload_len = msg.payload.len(),
                    retain = msg.retain,
                    "Queuing retained message for delivery"
                );
                msg.retain = true;
                self.publish_tx.send_async(msg).await.map_err(|_| {
                    MqttError::InvalidState("Failed to queue retained message".to_string())
                })?;
            }
        }
        Ok(())
    }

    async fn build_and_send_suback(
        &mut self,
        subscribe: &SubscribePacket,
        reason_codes: &[crate::packet::suback::SubAckReasonCode],
    ) -> Result<()> {
        let client_id = self.client_id.as_ref().unwrap();
        let mut suback = if self.protocol_version == 4 {
            SubAckPacket::new_v311(subscribe.packet_id)
        } else {
            SubAckPacket::new(subscribe.packet_id)
        };
        suback.reason_codes = reason_codes.to_vec();
        if self.protocol_version == 5 && self.request_problem_information {
            if reason_codes.contains(&crate::packet::suback::SubAckReasonCode::NotAuthorized) {
                suback
                    .properties
                    .set_reason_string("One or more subscriptions not authorized".to_string());
            } else if reason_codes.contains(&crate::packet::suback::SubAckReasonCode::QuotaExceeded)
            {
                suback
                    .properties
                    .set_reason_string("Subscription quota exceeded".to_string());
            }
        }
        let result = self.transport.write_packet(Packet::SubAck(suback)).await;

        if let Some(ref handler) = self.config.event_handler {
            let subscriptions: Vec<SubscriptionInfo> = reason_codes
                .iter()
                .zip(subscribe.filters.iter())
                .map(|(rc, filter)| SubscriptionInfo {
                    topic_filter: filter.filter.clone().into(),
                    qos: filter.options.qos,
                    result: SubAckReasonCode::from(*rc),
                })
                .collect();
            let event = ClientSubscribeEvent {
                client_id: client_id.clone().into(),
                subscriptions,
            };
            handler.on_client_subscribe(event).await;
        }

        result
    }

    pub(super) async fn handle_unsubscribe(
        &mut self,
        unsubscribe: UnsubscribePacket,
    ) -> Result<()> {
        let client_id = self.client_id.as_ref().unwrap();
        let mut reason_codes = Vec::new();

        for topic_filter in &unsubscribe.filters {
            let removed = self.router.unsubscribe(client_id, topic_filter).await;

            if removed {
                if let Some(ref mut session) = self.session {
                    session.remove_subscription(topic_filter);
                    if let Some(ref storage) = self.storage {
                        if let Err(e) = storage.store_session(session.clone()).await {
                            warn!("Failed to store session: {e}");
                        }
                    }
                }
            }

            reason_codes.push(if removed {
                crate::packet::unsuback::UnsubAckReasonCode::Success
            } else {
                crate::packet::unsuback::UnsubAckReasonCode::NoSubscriptionExisted
            });
        }

        let mut unsuback = if self.protocol_version == 4 {
            UnsubAckPacket::new_v311(unsubscribe.packet_id)
        } else {
            UnsubAckPacket::new(unsubscribe.packet_id)
        };
        unsuback.reason_codes = reason_codes;
        let result = self
            .transport
            .write_packet(Packet::UnsubAck(unsuback))
            .await;

        if let Some(ref handler) = self.config.event_handler {
            let event = ClientUnsubscribeEvent {
                client_id: client_id.clone().into(),
                topic_filters: unsubscribe
                    .filters
                    .iter()
                    .map(|f| f.clone().into())
                    .collect(),
            };
            handler.on_client_unsubscribe(event).await;
        }

        result
    }
}
