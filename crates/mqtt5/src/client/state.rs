//! Connection state management and reconnection logic

use crate::callback::CallbackId;
use crate::error::{MqttError, Result};
use crate::packet::subscribe::SubscribePacket;
use crate::packet::subscribe::SubscriptionOptions;
use crate::protocol::v5::properties::Properties;
use tokio::time::Duration;

use super::connection::{ConnectionEvent, ReconnectConfig};
use super::MqttClient;

#[derive(Debug, Clone)]
pub(crate) enum ClientTransportType {
    Tcp,
    Tls,
    WebSocket(String),
    WebSocketSecure(String),
    Quic,
    QuicSecure,
}

impl MqttClient {
    pub(crate) async fn trigger_connection_event(&self, event: ConnectionEvent) {
        let callbacks = self.connection_event_callbacks.read().await.clone();
        for callback in callbacks {
            callback(event.clone());
        }
    }

    pub(crate) async fn reset_reconnect_counter(&self) {
        let mut inner = self.inner.write().await;
        inner.reconnect_attempt = 0;
    }

    pub(crate) async fn recover_quic_flows(&self) {
        let inner = self.inner.read().await;
        match inner.recover_flows().await {
            Ok(0) => {}
            Ok(n) => tracing::info!(recovered = n, "Recovered QUIC flows after reconnect"),
            Err(e) => tracing::warn!(error = %e, "Failed to recover QUIC flows"),
        }
    }

    pub(crate) async fn restore_subscriptions_after_connect(
        &self,
        stored_subs: Vec<(String, SubscriptionOptions, CallbackId)>,
        session_present: bool,
    ) {
        if stored_subs.is_empty() {
            return;
        }

        if session_present {
            tracing::info!("Session resumed, restoring {} callbacks", stored_subs.len());
            let inner = self.inner.read().await;
            for (topic, _, callback_id) in stored_subs {
                if !inner.callback_manager.restore_callback(callback_id) {
                    tracing::warn!("Failed to restore callback for {topic}: not found in registry");
                }
            }
        } else {
            tracing::info!(
                "Session not resumed, restoring {} subscriptions",
                stored_subs.len()
            );
            for (topic, options, callback_id) in stored_subs {
                if let Err(e) = self
                    .resubscribe_internal(&topic, options, callback_id)
                    .await
                {
                    tracing::warn!("Failed to restore subscription to {}: {}", topic, e);
                }
            }
        }
    }

    pub(crate) async fn monitor_connection(&self) {
        tracing::info!("🔍 MONITOR TASK - Starting connection monitor task");

        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;

            let inner = self.inner.read().await;
            if !inner.is_connected() {
                tracing::info!(
                    "🔍 MONITOR TASK - Detected disconnection, triggering reconnection logic"
                );

                let reconnect_config = inner.options.reconnect_config.clone();
                let last_address = inner.last_address.clone();
                drop(inner);

                if !reconnect_config.enabled {
                    tracing::info!("🔍 MONITOR TASK - Reconnection disabled, exiting monitor");
                    break;
                }

                if let Some(address) = last_address {
                    tracing::info!(
                        address = %address,
                        "🔍 MONITOR TASK - Starting reconnection attempt"
                    );

                    if let Err(e) = self.attempt_reconnection(&address, &reconnect_config).await {
                        tracing::error!("🔍 MONITOR TASK - Reconnection failed: {e}");
                        break;
                    }
                } else {
                    tracing::info!("🔍 MONITOR TASK - No last address available for reconnection");
                    break;
                }
            }
        }
    }

    /// Attempt reconnection with exponential backoff
    ///
    /// # Errors
    ///
    /// Returns an error if max reconnection attempts exceeded
    pub(crate) async fn attempt_reconnection(
        &self,
        address: &str,
        config: &ReconnectConfig,
    ) -> Result<()> {
        tracing::info!(
            address = %address,
            max_attempts = ?config.max_attempts,
            initial_delay = ?config.initial_delay,
            "🔄 RECONNECTION - Starting reconnection loop"
        );

        let mut delay = config.initial_delay;

        loop {
            if self.is_connected().await {
                tracing::info!(
                    "🔄 RECONNECTION - Already connected, stopping reconnection attempts"
                );
                return Ok(());
            }

            let attempt = {
                let mut inner = self.inner.write().await;
                inner.reconnect_attempt += 1;
                inner.reconnect_attempt
            };

            tracing::info!(
                attempt = attempt,
                max_attempts = ?config.max_attempts,
                delay = ?delay,
                "🔄 RECONNECTION - Attempting reconnection #{}", attempt
            );

            if let Some(max) = config.max_attempts {
                if attempt > max {
                    tracing::error!(
                        attempt = attempt,
                        max_attempts = max,
                        "🔄 RECONNECTION - Max attempts exceeded"
                    );
                    return Err(MqttError::ConnectionError(
                        "Max reconnection attempts exceeded".to_string(),
                    ));
                }
            }

            self.trigger_connection_event(ConnectionEvent::Reconnecting { attempt })
                .await;

            tokio::time::sleep(delay).await;

            let connection_guard = self.connection_mutex.lock().await;

            if self.is_connected().await {
                tracing::info!("Connected during wait, stopping reconnection attempts");
                return Ok(());
            }

            tracing::info!(
                attempt = attempt,
                address = %address,
                "🔄 RECONNECTION - Making connection attempt #{} to {}", attempt, address
            );
            let reconnection_result = self.connect_internal(address).await;

            drop(connection_guard);

            match reconnection_result {
                Ok(_) => {
                    tracing::info!(
                        "🔄 RECONNECTION - Reconnected successfully after {} attempts",
                        attempt
                    );

                    let inner = self.inner.read().await;
                    let stored_subs = inner.stored_subscriptions.lock().clone();
                    drop(inner);

                    for (topic, options, callback_id) in stored_subs {
                        if let Err(e) = self
                            .resubscribe_internal(&topic, options, callback_id)
                            .await
                        {
                            tracing::warn!("Failed to restore subscription to {}: {}", topic, e);
                        }
                    }

                    self.send_queued_messages().await;

                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!("Reconnection attempt {} failed: {}", attempt, e);

                    #[allow(clippy::cast_possible_truncation)]
                    {
                        delay = std::cmp::min(
                            Duration::from_secs_f64(delay.as_secs_f64() * config.backoff_factor()),
                            config.max_delay,
                        );
                    }
                }
            }
        }
    }

    pub(crate) async fn send_queued_messages(&self) {
        let messages = {
            let inner = self.inner.read().await;
            let mut queued = inner.queued_messages.lock();
            std::mem::take(&mut *queued)
        };

        for mut msg in messages {
            msg.dup = true;

            if let Err(e) = self.publish_packet(msg).await {
                tracing::warn!("Failed to send queued message: {e}");
            }
        }
    }

    /// Internal method to resubscribe with stored options and callback
    ///
    /// # Errors
    ///
    /// Returns an error if resubscription fails
    pub(crate) async fn resubscribe_internal(
        &self,
        topic: &str,
        options: SubscriptionOptions,
        callback_id: CallbackId,
    ) -> Result<()> {
        let inner = self.inner.read().await;
        let _ = inner.callback_manager.restore_callback(callback_id);
        drop(inner);

        let inner = self.inner.read().await;
        let packet = SubscribePacket {
            packet_id: inner.packet_id_generator.next(),
            filters: vec![crate::packet::subscribe::TopicFilter {
                filter: topic.to_string(),
                options,
            }],
            properties: Properties::new(),
            protocol_version: inner.options.protocol_version.as_u8(),
        };
        inner.subscribe_with_callback(packet, callback_id).await?;
        Ok(())
    }

    /// Internal method to publish a packet
    ///
    /// # Errors
    ///
    /// Returns an error if publish fails
    pub(crate) async fn publish_packet(
        &self,
        packet: crate::packet::publish::PublishPacket,
    ) -> Result<()> {
        let mut inner = self.inner.write().await;
        inner
            .send_packet(crate::packet::Packet::Publish(packet))
            .await
    }
}
