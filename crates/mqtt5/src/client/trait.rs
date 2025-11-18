//! MQTT Client Trait for Mockability
//!
//! This module provides a trait interface for the MQTT client to enable
//! mocking and testing without a real broker connection.

use crate::error::Result;
use crate::types::{
    ConnectOptions, ConnectResult, Message, PublishOptions, PublishResult, SubscribeOptions,
};
use crate::QoS;
use std::future::Future;

/// Trait defining the interface for an MQTT client
///
/// This trait enables mocking for testing and provides a clean interface
/// for different MQTT client implementations.
pub trait MqttClientTrait: Send + Sync {
    /// Checks if the client is connected to the broker
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    fn is_connected(&self) -> impl Future<Output = bool> + Send + '_;

    /// Gets the client ID
    fn client_id(&self) -> impl Future<Output = String> + Send + '_;

    /// Connects to the MQTT broker with default options
    fn connect<'a>(&'a self, address: &'a str) -> impl Future<Output = Result<()>> + Send + 'a;

    /// Connects to the MQTT broker with custom options
    fn connect_with_options<'a>(
        &'a self,
        address: &'a str,
        options: ConnectOptions,
    ) -> impl Future<Output = Result<ConnectResult>> + Send + 'a;

    /// Disconnects from the MQTT broker
    fn disconnect(&self) -> impl Future<Output = Result<()>> + Send + '_;

    /// Publishes a message to a topic with default options
    fn publish<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a;

    /// Publishes a message to a topic with specific `QoS`
    fn publish_qos<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
        qos: QoS,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a;

    /// Publishes a message with custom options
    fn publish_with_options<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
        options: PublishOptions,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a;

    /// Subscribes to a topic with a callback, returning (`packet_id`, `granted_qos`)
    fn subscribe<'a, F>(
        &'a self,
        topic_filter: impl Into<String> + Send + 'a,
        callback: F,
    ) -> impl Future<Output = Result<(u16, QoS)>> + Send + 'a
    where
        F: Fn(Message) + Send + Sync + 'static;

    /// Subscribes to a topic with custom options and a callback
    fn subscribe_with_options<'a, F>(
        &'a self,
        topic_filter: impl Into<String> + Send + 'a,
        options: SubscribeOptions,
        callback: F,
    ) -> impl Future<Output = Result<(u16, QoS)>> + Send + 'a
    where
        F: Fn(Message) + Send + Sync + 'static;

    /// Unsubscribes from a topic
    fn unsubscribe<'a>(
        &'a self,
        topic_filter: impl Into<String> + Send + 'a,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    /// Subscribe to multiple topics at once
    fn subscribe_many<'a, F>(
        &'a self,
        topics: Vec<(&'a str, QoS)>,
        callback: F,
    ) -> impl Future<Output = Result<Vec<(u16, QoS)>>> + Send + 'a
    where
        F: Fn(Message) + Send + Sync + 'static + Clone;

    /// Unsubscribe from multiple topics at once
    ///
    /// Returns a vector of results, one for each topic
    fn unsubscribe_many<'a>(
        &'a self,
        topics: Vec<&'a str>,
    ) -> impl Future<Output = Result<Vec<(String, Result<()>)>>> + Send + 'a;

    /// Publish a retained message
    fn publish_retain<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a;

    /// Publish with `QoS` 0 (convenience method)
    fn publish_qos0<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a;

    /// Publish with `QoS` 1 (convenience method)
    fn publish_qos1<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a;

    /// Publish with `QoS` 2 (convenience method)
    fn publish_qos2<'a>(
        &'a self,
        topic: impl Into<String> + Send + 'a,
        payload: impl Into<Vec<u8>> + Send + 'a,
    ) -> impl Future<Output = Result<PublishResult>> + Send + 'a;

    /// Check if message queuing is enabled
    fn is_queue_on_disconnect(&self) -> impl Future<Output = bool> + Send + '_;

    /// Set whether to queue messages when disconnected
    fn set_queue_on_disconnect(&self, enabled: bool) -> impl Future<Output = ()> + Send + '_;
}
