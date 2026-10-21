use crate::broker::events::{
    ClientSubscribeEvent, ClientUnsubscribeEvent, SubAckReasonCode, SubscriptionInfo,
};
use crate::broker::storage::{QueuedMessage, StorageBackend, StoredSubscription};
use crate::error::{MqttError, Result};
use crate::packet::disconnect::DisconnectPacket;
use crate::packet::suback::SubAckPacket;
use crate::packet::subscribe::SubscribePacket;
use crate::packet::unsuback::UnsubAckPacket;
use crate::packet::unsubscribe::UnsubscribePacket;
use crate::packet::Packet;
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::validation::{parse_shared_subscription, topic_matches_filter, validate_topic_filter};
use crate::QoS;
use tracing::{debug, warn};

#[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
use crate::broker::quic_flows::{Ended, FlowInput};
#[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
use std::sync::Arc;
#[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
use tokio::time::Instant;

use crate::broker::router::{MessageRouter, RoutableMessage, Subscribed, Unsubscribed};
use crate::broker::session_slot::SessionSlotGuard;

use super::ClientHandler;

struct AcceptedFilter<'a> {
    filter: &'a crate::packet::subscribe::TopicFilter,
    stored: StoredSubscription,
    is_new: bool,
}

impl ClientHandler {
    pub(super) async fn handle_subscribe(&mut self, subscribe: SubscribePacket) -> Result<()> {
        let client_id = self.connected_client_id()?;
        let mut reason_codes = Vec::with_capacity(subscribe.filters.len());
        for filter in &subscribe.filters {
            reason_codes.push(self.validate_subscribe_filter(filter, &client_id).await?);
        }

        let slot = self.router.lock_session(&client_id).await;
        let routed = match self
            .route_subscriptions(&subscribe, &client_id, &mut reason_codes)
            .await
        {
            Ok(Some(routed)) => routed,
            Ok(None) => {
                debug!("Ignoring SUBSCRIBE from a connection whose session was taken over");
                return Ok(());
            }
            Err(e) => {
                self.restore_stored_routes(&slot).await?;
                return Err(e);
            }
        };
        if !routed.is_empty() {
            let stored: Vec<(String, StoredSubscription)> = routed
                .iter()
                .map(|accepted| (accepted.filter.filter.clone(), accepted.stored.clone()))
                .collect();
            let kept = stored.clone();
            self.persist_or_restore_routes(&slot, move |session| {
                for (filter, subscription) in stored {
                    session.add_subscription(filter, subscription);
                }
            })
            .await?;
            if let Some(session) = self.session.as_mut() {
                for (filter, subscription) in kept {
                    session.add_subscription(filter, subscription);
                }
            }
        }
        drop(slot);

        let subscription_id = subscribe.properties.get_subscription_identifier();
        for accepted in routed {
            self.deliver_retained_for_filter(
                &accepted.filter.filter,
                &accepted.filter.options,
                subscription_id,
                accepted.is_new,
            )
            .await?;
        }

        let reason_codes: Vec<crate::packet::suback::SubAckReasonCode> =
            reason_codes.into_iter().flatten().collect();
        self.build_and_send_suback(&subscribe, &reason_codes).await
    }

    fn connected_client_id(&self) -> Result<String> {
        self.client_id
            .clone()
            .ok_or_else(|| MqttError::ProtocolError("packet received before CONNECT".to_string()))
    }

    async fn route_subscriptions<'a>(
        &self,
        subscribe: &'a SubscribePacket,
        client_id: &str,
        reason_codes: &mut [Option<crate::packet::suback::SubAckReasonCode>],
    ) -> Result<Option<Vec<AcceptedFilter<'a>>>> {
        let subscription_id = subscribe.properties.get_subscription_identifier();
        let mut routed = Vec::new();
        for (filter, reason_code) in subscribe.filters.iter().zip(reason_codes.iter_mut()) {
            if reason_code.is_some() {
                continue;
            }
            if let Some(rc) = self.check_subscription_quota(filter, client_id).await {
                *reason_code = Some(rc);
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

            let stored = StoredSubscription {
                qos: QoS::from(granted_qos),
                no_local: filter.options.no_local,
                retain_as_published: filter.options.retain_as_published,
                retain_handling: filter.options.retain_handling as u8,
                subscription_id,
                protocol_version: self.protocol_version,
                change_only,
                flow_id: self.pending_external_flow_id,
            };
            let outcome = self
                .router
                .subscribe_as(
                    Some(self.generation),
                    MessageRouter::stored_subscription_request(client_id, &filter.filter, &stored),
                )
                .await?;
            let is_new = match outcome {
                Subscribed::New => true,
                Subscribed::Updated => false,
                Subscribed::Fenced => return Ok(None),
            };
            *reason_code = Some(crate::packet::suback::SubAckReasonCode::from_qos(
                stored.qos,
            ));
            routed.push(AcceptedFilter {
                filter,
                stored,
                is_new,
            });
        }
        Ok(Some(routed))
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
                self.write_to_client(Packet::Disconnect(disconnect)).await?;
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

    async fn persist_or_restore_routes<F>(
        &mut self,
        slot: &SessionSlotGuard,
        update: F,
    ) -> Result<()>
    where
        F: FnOnce(&mut crate::broker::storage::ClientSession) + Send,
    {
        let Err(e) = self.update_stored_session(update).await else {
            return Ok(());
        };
        warn!(
            client_id = slot.client_id(),
            "Failed to store a subscription change; restoring the stored subscriptions: {e}"
        );
        self.restore_stored_routes(slot).await?;
        self.disconnect_reason = Some(ReasonCode::UnspecifiedError);
        if self.protocol_version == 5 {
            let disconnect = DisconnectPacket::new(ReasonCode::UnspecifiedError);
            if let Err(write_error) = self.write_to_client(Packet::Disconnect(disconnect)).await {
                debug!("Failed to send DISCONNECT after a failed session write: {write_error}");
            }
        }
        Err(e)
    }

    async fn restore_stored_routes(&mut self, slot: &SessionSlotGuard) -> Result<()> {
        let stored = match self.storage.as_ref() {
            Some(storage) => storage.get_session(slot.client_id()).await?,
            None => None,
        };
        self.router
            .set_client_subscriptions(slot, stored.as_ref())
            .await?;
        if let (Some(session), Some(stored)) = (self.session.as_mut(), stored) {
            session.subscriptions = stored.subscriptions;
        }
        Ok(())
    }

    pub(super) async fn update_stored_session<F>(&self, update: F) -> Result<()>
    where
        F: FnOnce(&mut crate::broker::storage::ClientSession) + Send,
    {
        let (Some(storage), Some(client_id)) = (self.storage.as_ref(), self.client_id.as_ref())
        else {
            return Ok(());
        };
        if !storage
            .update_session(client_id, self.generation, update)
            .await?
        {
            debug!(client_id = %client_id, "Stored session is not this connection's");
        }
        Ok(())
    }

    async fn deliver_retained_for_filter(
        &self,
        topic_filter: &str,
        options: &crate::packet::subscribe::SubscriptionOptions,
        subscription_id: Option<u32>,
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
                msg.qos = crate::broker::router::MessageRouter::effective_qos(msg.qos, options.qos);
                if let Some(id) = subscription_id {
                    msg.properties.set_subscription_identifier(id);
                }
                let routable = RoutableMessage {
                    publish: msg,
                    target_flow: None,
                };
                self.offer_retained(routable);
            }
        }
        Ok(())
    }

    /// Hands a retained message to this client's own delivery lanes without ever awaiting
    /// them: the handler task is their only consumer, so an await here would never complete.
    fn offer_retained(&self, routable: RoutableMessage) {
        let Some(client_id) = self.client_id.as_ref() else {
            return;
        };
        if routable.publish.qos == QoS::AtMostOnce {
            if self.qos0_tx.try_send(routable).is_err() {
                debug!(client_id = %client_id, "Dropping QoS 0 retained message: lane full");
            }
            return;
        }
        let queue_behind = |publish: crate::packet::publish::PublishPacket| {
            if let Some(queue) = self.queue.as_ref() {
                let qos = publish.qos;
                let queued = QueuedMessage::new(publish, client_id.clone(), qos, None);
                if queue.push_in_epoch(queued, self.queue_epoch).is_some() {
                    queue.notify();
                } else {
                    debug!(client_id = %client_id, "Dropping retained message: session discarded");
                }
            } else {
                warn!(client_id = %client_id, "Dropping retained message: no session queue");
            }
        };
        if self.queue.as_ref().is_some_and(|queue| queue.behind()) {
            queue_behind(routable.publish);
            return;
        }
        if let Err(rejected) = self.qos1_tx.try_send(routable) {
            queue_behind(rejected.into_inner().publish);
        }
    }

    async fn build_and_send_suback(
        &mut self,
        subscribe: &SubscribePacket,
        reason_codes: &[crate::packet::suback::SubAckReasonCode],
    ) -> Result<()> {
        let client_id = self.connected_client_id()?;
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
        let result = self.write_to_client(Packet::SubAck(suback)).await;

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

    #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
    pub(super) async fn handle_flow_input(&mut self, input: FlowInput) {
        let Some(client_id) = self.client_id.clone() else {
            return;
        };
        let generation = self.generation;
        let now = Instant::now();
        match input {
            FlowInput::Opened(opening) => {
                let opened = self
                    .router
                    .with_session_flows(&client_id, |flows| flows.open(generation, &opening, now));
                if opened.drop_subscriptions {
                    self.drop_flow_subscriptions(opening.flow_id).await;
                }
                self.refresh_flow_deadline(&client_id);
                debug!(flow_id = opening.flow_id, answer = ?opened.answer, "Answered a client data flow");
                if opening.answer.send(opened.answer).is_err() {
                    debug!(
                        flow_id = opening.flow_id,
                        "Data stream ended before its flow was answered"
                    );
                }
            }
            FlowInput::Ended {
                flow_id,
                stream_id,
                graceful,
            } => match self.router.with_session_flows(&client_id, |flows| {
                flows.end(generation, flow_id, stream_id, graceful, now)
            }) {
                Ended::Removed => self.drop_flow_subscriptions(flow_id).await,
                Ended::Kept => self.refresh_flow_deadline(&client_id),
                Ended::Ignored => debug!(
                    flow_id,
                    stream_id, "Ignoring the end of a stream its flow no longer uses"
                ),
            },
            FlowInput::Discard { flow_id, done } => {
                if self
                    .router
                    .with_session_flows(&client_id, |flows| flows.discard(generation, flow_id))
                {
                    self.drop_flow_subscriptions(flow_id).await;
                    self.refresh_flow_deadline(&client_id);
                }
                if done.send(()).is_err() {
                    debug!(
                        flow_id,
                        "Discard stream ended before its flow was discarded"
                    );
                }
            }
        }
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
    fn refresh_flow_deadline(&mut self, client_id: &str) {
        let generation = self.generation;
        self.flow_deadline = self
            .router
            .with_session_flows(client_id, |flows| flows.deadline_for(generation));
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
    pub(super) async fn expire_flows(&mut self) {
        let Some(client_id) = self.client_id.clone() else {
            self.flow_deadline = None;
            return;
        };
        let generation = self.generation;
        while let Some(flow_id) = self.router.with_session_flows(&client_id, |flows| {
            flows.take_expired_for(generation, Instant::now())
        }) {
            debug!(flow_id, "Flow state expired");
            self.drop_flow_subscriptions(flow_id).await;
        }
        self.refresh_flow_deadline(&client_id);
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
    pub(super) fn claim_session_flows(
        &mut self,
        client_id: &str,
        generation: u64,
        session: &mut crate::broker::storage::ClientSession,
        fresh: bool,
    ) {
        let now = Instant::now();
        self.flow_deadline = self.router.with_session_flows(client_id, |flows| {
            flows.claim(generation, fresh, now);
            session
                .subscriptions
                .retain(|_, sub| sub.flow_id.is_none_or(|flow_id| flows.holds(flow_id)));
            flows.deadline_for(generation)
        });
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
    pub(super) fn abandon_session_flows(&mut self, client_id: &str, generation: u64) {
        self.flow_deadline = None;
        self.router.with_session_flows(client_id, |flows| {
            flows.release(generation, true, Instant::now())
        });
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
    pub(super) async fn release_session_flows(&mut self, client_id: &str) {
        let generation = self.generation;
        let keep = self
            .session
            .as_ref()
            .is_some_and(|session| session.expiry_interval != Some(0));
        let now = Instant::now();
        if !self
            .router
            .with_session_flows(client_id, |flows| flows.release(generation, keep, now))
        {
            return;
        }
        self.flow_deadline = None;
        while let Some(flow_id) = self.unheld_flow(client_id) {
            self.remove_flow_subscriptions(client_id, flow_id).await;
        }
        if let Some(epoch) = self.router.with_session_flows(client_id, |flows| {
            flows.next_deadline().map(|_| flows.arm_expiry())
        }) {
            tokio::spawn(
                Arc::clone(&self.router).expire_unowned_flows(client_id.to_string(), epoch),
            );
        }
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
    fn unheld_flow(&self, client_id: &str) -> Option<u64> {
        self.session
            .as_ref()?
            .subscriptions
            .values()
            .filter_map(|sub| sub.flow_id)
            .find(|flow_id| {
                !self
                    .router
                    .with_session_flows(client_id, |flows| flows.holds(*flow_id))
            })
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
    async fn drop_flow_subscriptions(&mut self, flow_id: u64) {
        let Some(client_id) = self.client_id.clone() else {
            return;
        };
        let slot = self.router.lock_session(&client_id).await;
        self.remove_flow_subscriptions(&client_id, flow_id).await;
        drop(slot);
    }

    #[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
    async fn remove_flow_subscriptions(&mut self, client_id: &str, flow_id: u64) {
        let removed_filters = self
            .router
            .unsubscribe_by_flow(Some(self.generation), client_id, flow_id)
            .await;
        if let Some(session) = self.session.as_mut() {
            session
                .subscriptions
                .retain(|_, sub| sub.flow_id != Some(flow_id));
        }
        if let Some(ref mut ssm) = self.server_stream_manager {
            ssm.remove_flow_stream(flow_id);
        }
        if removed_filters.is_empty() {
            return;
        }
        debug!(
            client_id = %client_id,
            flow_id = flow_id,
            count = removed_filters.len(),
            "Removed flow-bound subscriptions"
        );
        if let Err(e) = self
            .update_stored_session(move |session| {
                session
                    .subscriptions
                    .retain(|_, sub| sub.flow_id != Some(flow_id));
            })
            .await
        {
            warn!("Failed to store session after removing flow-bound subscriptions: {e}");
        }
    }

    pub(super) async fn handle_unsubscribe(
        &mut self,
        unsubscribe: UnsubscribePacket,
    ) -> Result<()> {
        let client_id = self.connected_client_id()?;
        let mut reason_codes = Vec::with_capacity(unsubscribe.filters.len());
        let mut removed_filters = Vec::new();

        let slot = self.router.lock_session(&client_id).await;
        for topic_filter in &unsubscribe.filters {
            match self
                .router
                .unsubscribe_as(
                    Some(self.generation),
                    &client_id,
                    topic_filter,
                    self.pending_external_flow_id,
                )
                .await
            {
                Unsubscribed::Removed => {
                    removed_filters.push(topic_filter.clone());
                    reason_codes.push(crate::packet::unsuback::UnsubAckReasonCode::Success);
                }
                Unsubscribed::Absent => reason_codes
                    .push(crate::packet::unsuback::UnsubAckReasonCode::NoSubscriptionExisted),
                Unsubscribed::Fenced => {
                    debug!("Ignoring UNSUBSCRIBE from a connection whose session was taken over");
                    return Ok(());
                }
            }
        }
        if !removed_filters.is_empty() {
            let removed = removed_filters.clone();
            self.persist_or_restore_routes(&slot, move |session| {
                for filter in &removed {
                    session.remove_subscription(filter);
                }
            })
            .await?;
            if let Some(session) = self.session.as_mut() {
                for filter in &removed_filters {
                    session.remove_subscription(filter);
                }
            }
        }
        drop(slot);

        let mut unsuback = if self.protocol_version == 4 {
            UnsubAckPacket::new_v311(unsubscribe.packet_id)
        } else {
            UnsubAckPacket::new(unsubscribe.packet_id)
        };
        unsuback.reason_codes = reason_codes;
        let result = self.write_to_client(Packet::UnsubAck(unsuback)).await;

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
