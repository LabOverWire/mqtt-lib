use mqtt5::broker::storage::{StorageBackend, StoredSubscription};
use mqtt5_protocol::error::Result;
use mqtt5_protocol::packet::suback::{SubAckPacket, SubAckReasonCode};
use mqtt5_protocol::packet::subscribe::SubscribePacket;
use mqtt5_protocol::packet::unsuback::{UnsubAckPacket, UnsubAckReasonCode};
use mqtt5_protocol::packet::unsubscribe::UnsubscribePacket;
use mqtt5_protocol::packet::Packet;
use mqtt5_protocol::topic_matches_filter;
use mqtt5_protocol::types::ProtocolVersion;
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
            let authorized = self
                .auth_provider
                .authorize_subscribe(&client_id, self.user_id.as_deref(), &filter.filter)
                .await;

            if !authorized {
                reason_codes.push(SubAckReasonCode::NotAuthorized);
                continue;
            }

            let max_qos = self.config.read().map_or_else(
                |_| {
                    warn!("Config read failed for max_qos, using default 2");
                    2
                },
                |c| c.maximum_qos,
            );
            let granted_qos = if filter.options.qos as u8 > max_qos {
                QoS::from(max_qos)
            } else {
                filter.options.qos
            };

            let subscription_id = subscribe.properties.get_subscription_identifier();

            let change_only = self.config.read().is_ok_and(|c| {
                c.change_only_delivery_config.enabled
                    && c.change_only_delivery_config
                        .topic_patterns
                        .iter()
                        .any(|pattern| topic_matches_filter(&filter.filter, pattern))
            });

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

            if filter.options.retain_handling
                != mqtt5_protocol::packet::subscribe::RetainHandling::DoNotSend
            {
                let retained = self.router.get_retained_messages(&filter.filter).await;
                for mut msg in retained {
                    msg.retain = true;
                    self.send_publish(msg, writer)?;
                }
            }

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

    pub(super) async fn handle_unsubscribe(
        &mut self,
        unsubscribe: UnsubscribePacket,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        let client_id = self.client_id.as_ref().unwrap();
        let mut reason_codes = Vec::new();

        for filter in &unsubscribe.filters {
            self.router.unsubscribe(client_id, filter).await;

            if let Some(ref mut session) = self.session {
                session.remove_subscription(filter);
                self.storage.store_session(session.clone()).await.ok();
            }

            reason_codes.push(UnsubAckReasonCode::Success);
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
