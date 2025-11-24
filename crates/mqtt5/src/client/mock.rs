//! Mock MQTT Client for Testing
//!
//! This module provides a mock implementation of the MQTT client trait
//! that can be used for testing without a real broker connection.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

use crate::client::MqttClientTrait;
use crate::error::{MqttError, Result};
use crate::types::{
    ConnectOptions, ConnectResult, Message, MessageProperties, PublishOptions, PublishResult,
    SubscribeOptions,
};
use crate::QoS;

/// Type alias for subscription callbacks
type SubscriptionCallback = Box<dyn Fn(Message) + Send + Sync>;

/// Mock MQTT client for testing
#[derive(Clone)]
pub struct MockMqttClient {
    /// Mock state
    state: Arc<MockState>,
}

struct MockState {
    /// Whether the client is "connected"
    connected: AtomicBool,
    /// Client ID
    client_id: RwLock<String>,
    /// Mock packet ID generator
    packet_id_counter: AtomicU16,
    /// Whether message queuing is enabled
    queue_on_disconnect: AtomicBool,
    /// Recorded method calls for verification
    calls: Mutex<Vec<MockCall>>,
    /// Configured responses for method calls
    responses: RwLock<MockResponses>,
    /// Subscribed topics and their callbacks
    subscriptions: RwLock<HashMap<String, SubscriptionCallback>>,
}

/// Record of a method call made to the mock client
#[derive(Debug, Clone)]
pub enum MockCall {
    Connect {
        address: String,
    },
    ConnectWithOptions {
        address: String,
        options: Box<ConnectOptions>,
    },
    Disconnect,
    Publish {
        topic: String,
        payload: Vec<u8>,
    },
    PublishWithOptions {
        topic: String,
        payload: Vec<u8>,
        options: PublishOptions,
    },
    Subscribe {
        topic: String,
    },
    SubscribeWithOptions {
        topic: String,
        options: SubscribeOptions,
    },
    Unsubscribe {
        topic: String,
    },
    SetQueueOnDisconnect {
        enabled: bool,
    },
}

/// Configured responses for mock methods
#[derive(Debug, Default)]
pub struct MockResponses {
    /// Response for connect calls
    pub connect_response: Option<Result<()>>,
    /// Response for `connect_with_options` calls
    pub connect_with_options_response: Option<Result<ConnectResult>>,
    /// Response for disconnect calls
    pub disconnect_response: Option<Result<()>>,
    /// Response for publish calls
    pub publish_response: Option<Result<PublishResult>>,
    /// Response for subscribe calls
    pub subscribe_response: Option<Result<(u16, QoS)>>,
    /// Response for unsubscribe calls
    pub unsubscribe_response: Option<Result<()>>,
}

impl MockMqttClient {
    /// Creates a new mock client
    pub fn new(client_id: impl Into<String>) -> Self {
        Self {
            state: Arc::new(MockState {
                connected: AtomicBool::new(false),
                client_id: RwLock::new(client_id.into()),
                packet_id_counter: AtomicU16::new(0),
                queue_on_disconnect: AtomicBool::new(false),
                calls: Mutex::new(Vec::new()),
                responses: RwLock::new(MockResponses::default()),
                subscriptions: RwLock::new(HashMap::new()),
            }),
        }
    }

    /// Sets the mock to be "connected"
    pub fn set_connected(&self, connected: bool) {
        self.state.connected.store(connected, Ordering::SeqCst);
    }

    /// Gets all recorded method calls
    pub async fn get_calls(&self) -> Vec<MockCall> {
        self.state.calls.lock().await.clone()
    }

    /// Clears all recorded method calls
    pub async fn clear_calls(&self) {
        self.state.calls.lock().await.clear();
    }

    /// Configures the response for connect calls
    pub async fn set_connect_response(&self, response: Result<()>) {
        self.state.responses.write().await.connect_response = Some(response);
    }

    /// Configures the response for `connect_with_options` calls
    pub async fn set_connect_with_options_response(&self, response: Result<ConnectResult>) {
        self.state
            .responses
            .write()
            .await
            .connect_with_options_response = Some(response);
    }

    /// Configures the response for disconnect calls
    pub async fn set_disconnect_response(&self, response: Result<()>) {
        self.state.responses.write().await.disconnect_response = Some(response);
    }

    /// Configures the response for publish calls
    pub async fn set_publish_response(&self, response: Result<PublishResult>) {
        self.state.responses.write().await.publish_response = Some(response);
    }

    /// Configures the response for subscribe calls
    pub async fn set_subscribe_response(&self, response: Result<(u16, QoS)>) {
        self.state.responses.write().await.subscribe_response = Some(response);
    }

    /// Configures the response for unsubscribe calls
    pub async fn set_unsubscribe_response(&self, response: Result<()>) {
        self.state.responses.write().await.unsubscribe_response = Some(response);
    }

    /// Simulates receiving a message on a subscribed topic
    ///
    /// # Errors
    ///
    /// Returns an error if no callback is found for the given topic
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn simulate_message(&self, topic: &str, payload: Vec<u8>, qos: QoS) -> Result<()> {
        let subscriptions = self.state.subscriptions.read().await;

        // Find matching subscription
        for (topic_filter, callback) in subscriptions.iter() {
            if Self::topic_matches(topic_filter, topic) {
                let message = Message {
                    topic: topic.to_string(),
                    payload,
                    qos,
                    retain: false,
                    properties: MessageProperties::default(),
                };
                callback(message);
                return Ok(());
            }
        }

        Err(MqttError::ProtocolError(format!(
            "No subscription found for topic: {topic}"
        )))
    }

    /// Simple topic matching (supports + and # wildcards)
    fn topic_matches(filter: &str, topic: &str) -> bool {
        if filter == topic {
            return true;
        }

        // Simple wildcard support for testing
        if filter.contains('+') || filter.contains('#') {
            // Basic implementation - could be enhanced for full MQTT topic matching
            if filter == "#" {
                return true;
            }
            if let Some(prefix) = filter.strip_suffix("/#") {
                return topic.starts_with(prefix);
            }
            if filter.contains('+') {
                // Simple single-level wildcard matching
                let filter_parts: Vec<&str> = filter.split('/').collect();
                let topic_parts: Vec<&str> = topic.split('/').collect();

                if filter_parts.len() != topic_parts.len() {
                    return false;
                }

                for (f, t) in filter_parts.iter().zip(topic_parts.iter()) {
                    if *f != "+" && f != t {
                        return false;
                    }
                }
                return true;
            }
        }

        false
    }

    /// Records a method call
    async fn record_call(&self, call: MockCall) {
        self.state.calls.lock().await.push(call);
    }

    /// Gets the next packet ID
    fn next_packet_id(&self) -> u16 {
        self.state.packet_id_counter.fetch_add(1, Ordering::SeqCst) + 1
    }
}

#[allow(clippy::manual_async_fn)]
impl MqttClientTrait for MockMqttClient {
    fn is_connected(&self) -> impl Future<Output = bool> + Send + '_ {
        async move { self.state.connected.load(Ordering::SeqCst) }
    }

    fn client_id(&self) -> impl Future<Output = String> + Send + '_ {
        async move { self.state.client_id.read().await.clone() }
    }

    fn connect<'a>(&'a self, address: &'a str) -> impl Future<Output = Result<()>> + Send + 'a {
        async move {
            self.record_call(MockCall::Connect {
                address: address.to_string(),
            })
            .await;

            let responses = self.state.responses.read().await;
            if let Some(response) = &responses.connect_response {
                let result = response.clone();
                drop(responses);

                if result.is_ok() {
                    self.set_connected(true);
                }
                result
            } else {
                // Default behavior: succeed and set connected
                self.set_connected(true);
                Ok(())
            }
        }
    }

    fn connect_with_options<'a>(
        &'a self,
        address: &'a str,
        options: ConnectOptions,
    ) -> impl Future<Output = Result<ConnectResult>> + Send + 'a {
        async move {
            self.record_call(MockCall::ConnectWithOptions {
                address: address.to_string(),
                options: Box::new(options.clone()),
            })
            .await;

            let responses = self.state.responses.read().await;
            if let Some(response) = &responses.connect_with_options_response {
                let result = response.clone();
                drop(responses);

                if result.is_ok() {
                    self.set_connected(true);
                }
                result
            } else {
                // Default behavior: succeed and set connected
                self.set_connected(true);
                Ok(ConnectResult {
                    session_present: false,
                })
            }
        }
    }

    fn disconnect(&self) -> impl Future<Output = Result<()>> + Send + '_ {
        async move {
            self.record_call(MockCall::Disconnect).await;

            let responses = self.state.responses.read().await;
            if let Some(response) = &responses.disconnect_response {
                let result = response.clone();
                drop(responses);

                if result.is_ok() {
                    self.set_connected(false);
                }
                result
            } else {
                // Default behavior: succeed and set disconnected
                self.set_connected(false);
                Ok(())
            }
        }
    }

    fn publish<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a {
        async move {
            let topic_str = topic.into();
            let payload_vec = payload.into();

            self.record_call(MockCall::Publish {
                topic: topic_str,
                payload: payload_vec,
            })
            .await;

            let responses = self.state.responses.read().await;
            if let Some(response) = &responses.publish_response {
                response.clone()
            } else {
                // Default behavior: succeed with QoS 0
                Ok(PublishResult::QoS0)
            }
        }
    }

    fn publish_qos<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
        qos: QoS,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a {
        async move {
            let options = PublishOptions {
                qos,
                ..Default::default()
            };
            self.publish_with_options(topic, payload, options).await
        }
    }

    fn publish_with_options<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
        options: PublishOptions,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a {
        async move {
            let topic_str = topic.into();
            let payload_vec = payload.into();

            self.record_call(MockCall::PublishWithOptions {
                topic: topic_str,
                payload: payload_vec,
                options: options.clone(),
            })
            .await;

            let responses = self.state.responses.read().await;
            if let Some(response) = &responses.publish_response {
                response.clone()
            } else {
                // Default behavior based on QoS
                match options.qos {
                    QoS::AtMostOnce => Ok(PublishResult::QoS0),
                    QoS::AtLeastOnce | QoS::ExactlyOnce => {
                        let packet_id = self.next_packet_id();
                        Ok(PublishResult::QoS1Or2 { packet_id })
                    }
                }
            }
        }
    }

    fn subscribe<'a, F>(
        &'a self,
        topic_filter: impl Into<String> + Send + 'a,
        callback: F,
    ) -> impl Future<Output = Result<(u16, QoS)>> + Send + 'a
    where
        F: Fn(Message) + Send + Sync + 'static,
    {
        async move {
            let topic_str = topic_filter.into();

            self.record_call(MockCall::Subscribe {
                topic: topic_str.clone(),
            })
            .await;

            // Store the callback
            self.state
                .subscriptions
                .write()
                .await
                .insert(topic_str, Box::new(callback));

            let responses = self.state.responses.read().await;
            if let Some(response) = &responses.subscribe_response {
                response.clone()
            } else {
                // Default behavior: succeed with packet ID and QoS 0
                let packet_id = self.next_packet_id();
                Ok((packet_id, QoS::AtMostOnce))
            }
        }
    }

    fn subscribe_with_options<'a, F>(
        &'a self,
        topic_filter: impl Into<String> + Send + 'a,
        options: SubscribeOptions,
        callback: F,
    ) -> impl Future<Output = Result<(u16, QoS)>> + Send + 'a
    where
        F: Fn(Message) + Send + Sync + 'static,
    {
        async move {
            let topic_str = topic_filter.into();

            self.record_call(MockCall::SubscribeWithOptions {
                topic: topic_str.clone(),
                options: options.clone(),
            })
            .await;

            // Store the callback
            self.state
                .subscriptions
                .write()
                .await
                .insert(topic_str, Box::new(callback));

            let responses = self.state.responses.read().await;
            if let Some(response) = &responses.subscribe_response {
                response.clone()
            } else {
                // Default behavior: succeed with packet ID and requested QoS
                let packet_id = self.next_packet_id();
                Ok((packet_id, options.qos))
            }
        }
    }

    fn unsubscribe<'a>(
        &'a self,
        topic_filter: impl Into<String> + Send + 'a,
    ) -> impl Future<Output = Result<()>> + Send + 'a {
        async move {
            let topic_str = topic_filter.into();

            self.record_call(MockCall::Unsubscribe {
                topic: topic_str.clone(),
            })
            .await;

            // Remove the subscription
            self.state.subscriptions.write().await.remove(&topic_str);

            let responses = self.state.responses.read().await;
            if let Some(response) = &responses.unsubscribe_response {
                response.clone()
            } else {
                // Default behavior: succeed
                Ok(())
            }
        }
    }

    fn subscribe_many<'a, F>(
        &'a self,
        topics: Vec<(&'a str, QoS)>,
        callback: F,
    ) -> impl Future<Output = Result<Vec<(u16, QoS)>>> + Send + 'a
    where
        F: Fn(Message) + Send + Sync + 'static + Clone,
    {
        async move {
            let mut results = Vec::new();
            for (topic, qos) in topics {
                let opts = SubscribeOptions {
                    qos,
                    ..Default::default()
                };
                let result = self
                    .subscribe_with_options(topic, opts, callback.clone())
                    .await?;
                results.push(result);
            }
            Ok(results)
        }
    }

    fn unsubscribe_many<'a>(
        &'a self,
        topics: Vec<&'a str>,
    ) -> impl Future<Output = Result<Vec<(String, Result<()>)>>> + Send + 'a {
        async move {
            let mut results = Vec::with_capacity(topics.len());

            for topic in topics {
                let topic_string = topic.to_string();
                let result = self.unsubscribe(topic).await;
                results.push((topic_string, result));
            }

            Ok(results)
        }
    }

    fn publish_retain<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a {
        async move {
            let opts = PublishOptions {
                retain: true,
                ..Default::default()
            };
            self.publish_with_options(topic, payload, opts).await
        }
    }

    fn publish_qos0<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a {
        async move { self.publish_qos(topic, payload, QoS::AtMostOnce).await }
    }

    fn publish_qos1<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a {
        async move { self.publish_qos(topic, payload, QoS::AtLeastOnce).await }
    }

    fn publish_qos2<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a {
        async move { self.publish_qos(topic, payload, QoS::ExactlyOnce).await }
    }

    fn is_queue_on_disconnect(&self) -> impl Future<Output = bool> + Send + '_ {
        async move { self.state.queue_on_disconnect.load(Ordering::SeqCst) }
    }

    fn set_queue_on_disconnect(&self, enabled: bool) -> impl Future<Output = ()> + Send + '_ {
        async move {
            self.record_call(MockCall::SetQueueOnDisconnect { enabled })
                .await;
            self.state
                .queue_on_disconnect
                .store(enabled, Ordering::SeqCst);
        }
    }
}
