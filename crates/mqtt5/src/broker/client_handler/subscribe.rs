//! Subscription handling - subscribe and unsubscribe operations

use crate::broker::events::{
    ClientSubscribeEvent, ClientUnsubscribeEvent, SubAckReasonCode, SubscriptionInfo,
};
use crate::broker::storage::{StorageBackend, StoredSubscription};
use crate::error::{MqttError, Result};
use crate::packet::suback::SubAckPacket;
use crate::packet::subscribe::SubscribePacket;
use crate::packet::unsuback::UnsubAckPacket;
use crate::packet::unsubscribe::UnsubscribePacket;
use crate::packet::Packet;
use crate::transport::PacketIo;
use crate::types::ProtocolVersion;
use crate::validation::topic_matches_filter;
use crate::QoS;
use tracing::debug;

use super::ClientHandler;

impl ClientHandler {
    #[allow(clippy::too_many_lines)]
    pub(super) async fn handle_subscribe(&mut self, subscribe: SubscribePacket) -> Result<()> {
        let client_id = self.client_id.as_ref().unwrap();
        let mut reason_codes: Vec<crate::packet::suback::SubAckReasonCode> = Vec::new();

        for filter in &subscribe.filters {
            let authorized = self
                .auth_provider
                .authorize_subscribe(client_id, self.user_id.as_deref(), &filter.filter)
                .await?;

            if !authorized {
                reason_codes.push(crate::packet::suback::SubAckReasonCode::NotAuthorized);
                continue;
            }

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
                        reason_codes.push(crate::packet::suback::SubAckReasonCode::QuotaExceeded);
                        continue;
                    }
                }
            }

            let granted_qos = if filter.options.qos as u8 > self.config.maximum_qos {
                self.config.maximum_qos
            } else {
                filter.options.qos as u8
            };

            let delta_mode = self.config.delta_subscription_config.enabled
                && self
                    .config
                    .delta_subscription_config
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
                    delta_mode,
                )
                .await?;

            if let Some(ref mut session) = self.session {
                let stored = StoredSubscription {
                    qos: QoS::from(granted_qos),
                    no_local: filter.options.no_local,
                    retain_as_published: filter.options.retain_as_published,
                    retain_handling: filter.options.retain_handling as u8,
                    subscription_id: subscribe.properties.get_subscription_identifier(),
                    protocol_version: self.protocol_version,
                    delta_mode,
                };
                session.add_subscription(filter.filter.clone(), stored);
                if let Some(ref storage) = self.storage {
                    storage.store_session(session.clone()).await.ok();
                }
            }

            let should_send_retained = match filter.options.retain_handling {
                crate::packet::subscribe::RetainHandling::SendAtSubscribe => true,
                crate::packet::subscribe::RetainHandling::SendAtSubscribeIfNew => is_new,
                crate::packet::subscribe::RetainHandling::DoNotSend => false,
            };

            if should_send_retained {
                let retained = self.router.get_retained_messages(&filter.filter).await;
                for mut msg in retained {
                    msg.retain = true;
                    self.publish_tx.send_async(msg).await.map_err(|_| {
                        MqttError::InvalidState("Failed to queue retained message".to_string())
                    })?;
                }
            }

            reason_codes.push(crate::packet::suback::SubAckReasonCode::from_qos(
                QoS::from(granted_qos),
            ));
        }

        let mut suback = if self.protocol_version == 4 {
            SubAckPacket::new_v311(subscribe.packet_id)
        } else {
            SubAckPacket::new(subscribe.packet_id)
        };
        suback.reason_codes.clone_from(&reason_codes);
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
                        storage.store_session(session.clone()).await.ok();
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
