//! In-process backend for [`crate::test_client::TestClient`].
//!
//! Wraps an [`mqtt5::MqttClient`] running against an in-process broker
//! fixture. Only compiled when the `inprocess-fixture` feature is enabled.

use super::{MessageQueue, ReceivedMessage, Subscription, TestClientError};
use crate::sut::SutHandle;
use mqtt5::MqttClient;
use mqtt5_protocol::types::{ConnectOptions, PublishOptions, SubscribeOptions};
use std::sync::{Arc, Mutex};

/// In-process backing for [`crate::test_client::TestClient`].
pub struct InProcessTestClient {
    client: MqttClient,
    client_id: String,
    session_present: bool,
}

impl InProcessTestClient {
    /// Connects an in-process client against `sut` using the given
    /// protocol-level connect options.
    ///
    /// # Errors
    /// Returns an error if the SUT has no TCP address or if the underlying
    /// `MqttClient` fails to connect.
    pub async fn connect(
        sut: &SutHandle,
        options: ConnectOptions,
    ) -> Result<Self, TestClientError> {
        let addr = sut
            .tcp_socket_addr()?
            .ok_or(TestClientError::NoTcpAddress)?;
        let address = format!("mqtt://{addr}");
        let client_id = options.client_id.clone();

        let wrapper = mqtt5::ConnectOptions {
            protocol_options: options.clone(),
            ..mqtt5::ConnectOptions::default()
        };
        let client = MqttClient::with_options(wrapper.clone());
        let result = Box::pin(client.connect_with_options(&address, wrapper)).await?;
        Ok(Self {
            client,
            client_id,
            session_present: result.session_present,
        })
    }

    /// Returns the client id used to connect.
    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Returns the `SessionPresent` flag from the server's CONNACK.
    #[must_use]
    pub fn session_present(&self) -> bool {
        self.session_present
    }

    /// Publishes with the given [`PublishOptions`].
    ///
    /// # Errors
    /// Returns an error if the broker rejects the publish or the client
    /// is disconnected.
    pub async fn publish_with_options(
        &self,
        topic: &str,
        payload: &[u8],
        options: PublishOptions,
    ) -> Result<(), TestClientError> {
        self.client
            .publish_with_options(topic, payload.to_vec(), options)
            .await?;
        Ok(())
    }

    /// Subscribes to `filter` and returns a [`Subscription`] handle.
    ///
    /// # Errors
    /// Returns an error if the broker rejects the subscription.
    ///
    /// # Panics
    /// Panics from the delivery callback if the internal mutex has been
    /// poisoned.
    pub async fn subscribe(
        &self,
        filter: &str,
        options: SubscribeOptions,
    ) -> Result<Subscription, TestClientError> {
        let messages: MessageQueue = Arc::new(Mutex::new(Vec::new()));
        let messages_cb = Arc::clone(&messages);
        let (packet_id, granted_qos) = self
            .client
            .subscribe_with_options(filter, options, move |msg| {
                messages_cb
                    .lock()
                    .unwrap()
                    .push(ReceivedMessage::from_message(msg));
            })
            .await?;
        Ok(Subscription::new(messages, packet_id, granted_qos))
    }

    /// Unsubscribes from `filter`.
    ///
    /// # Errors
    /// Returns an error if the broker rejects the unsubscribe.
    pub async fn unsubscribe(&self, filter: &str) -> Result<(), TestClientError> {
        self.client.unsubscribe(filter).await?;
        Ok(())
    }

    /// Performs a normal DISCONNECT handshake.
    ///
    /// # Errors
    /// Returns an error if the underlying client reports one while
    /// disconnecting.
    pub async fn disconnect(&self) -> Result<(), TestClientError> {
        self.client.disconnect().await?;
        Ok(())
    }

    /// Drops the TCP connection without sending a DISCONNECT packet.
    ///
    /// # Errors
    /// Returns an error if the underlying client reports one while tearing
    /// down the transport.
    pub async fn disconnect_abnormally(&self) -> Result<(), TestClientError> {
        self.client.disconnect_abnormally().await?;
        Ok(())
    }
}
