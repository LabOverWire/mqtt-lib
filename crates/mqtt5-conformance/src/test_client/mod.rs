//! High-level test client for conformance tests.
//!
//! [`TestClient`] hides the backing implementation behind a vendor-neutral
//! API. Tests construct a `TestClient` from a [`crate::sut::SutHandle`] and
//! the same test body runs against either the in-process fixture or an
//! external broker reached over raw TCP.
//!
//! The two backends are:
//!
//! * [`inprocess`] — wraps [`mqtt5::MqttClient`]; only compiled when the
//!   `inprocess-fixture` feature is enabled. This is the path mqtt-lib's own
//!   CI takes.
//! * [`raw`] — speaks MQTT v5 directly over a [`crate::transport::TcpTransport`]
//!   with its own packet loop and `QoS` state machines. This is the path the
//!   standalone conformance runner will take when validating third-party
//!   brokers via a [`crate::sut::SutDescriptor`].
//!
//! Both backends converge on the same [`ReceivedMessage`] / [`Subscription`]
//! types so tests remain agnostic of which implementation they run against.

#[cfg(feature = "inprocess-fixture")]
pub mod inprocess;
pub mod raw;

use crate::sut::{SutDescriptorError, SutHandle};
use mqtt5_protocol::types::{ConnectOptions, Message, PublishOptions, QoS, SubscribeOptions};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use ulid::Ulid;

/// Errors produced by [`TestClient`] operations.
#[derive(Debug)]
pub enum TestClientError {
    /// The SUT has no TCP address and cannot be connected to.
    NoTcpAddress,
    /// Resolving the SUT's TCP address failed.
    SutAddress(SutDescriptorError),
    /// Raw transport I/O failure.
    Io(std::io::Error),
    /// Failure encoding or decoding an MQTT packet, or an error reported by
    /// the underlying [`mqtt5::MqttClient`] (which re-exports the same
    /// `MqttError` type from `mqtt5_protocol`).
    Protocol(mqtt5_protocol::error::MqttError),
    /// The broker returned an unexpected packet or failure code.
    Unexpected(String),
    /// An operation exceeded its timeout.
    Timeout(&'static str),
    /// The background reader task exited; the connection is gone.
    Disconnected,
}

impl std::fmt::Display for TestClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTcpAddress => write!(f, "SUT has no TCP address configured"),
            Self::SutAddress(e) => write!(f, "failed to resolve SUT address: {e}"),
            Self::Io(e) => write!(f, "raw transport I/O error: {e}"),
            Self::Protocol(e) => write!(f, "MQTT protocol error: {e}"),
            Self::Unexpected(msg) => write!(f, "unexpected broker response: {msg}"),
            Self::Timeout(op) => write!(f, "operation timed out: {op}"),
            Self::Disconnected => write!(f, "connection closed by broker"),
        }
    }
}

impl std::error::Error for TestClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NoTcpAddress | Self::Unexpected(_) | Self::Timeout(_) | Self::Disconnected => {
                None
            }
            Self::SutAddress(e) => Some(e),
            Self::Io(e) => Some(e),
            Self::Protocol(e) => Some(e),
        }
    }
}

impl From<std::io::Error> for TestClientError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<mqtt5_protocol::error::MqttError> for TestClientError {
    fn from(e: mqtt5_protocol::error::MqttError) -> Self {
        Self::Protocol(e)
    }
}

impl From<SutDescriptorError> for TestClientError {
    fn from(e: SutDescriptorError) -> Self {
        Self::SutAddress(e)
    }
}

/// A message received by a [`Subscription`].
///
/// Includes the `dup` flag and `subscription_identifiers` list from
/// `MessageProperties`. Overlapping subscriptions can deliver multiple
/// identifiers per PUBLISH, so the field is a `Vec<u32>` rather than
/// `Option<u32>`.
#[derive(Debug, Clone)]
pub struct ReceivedMessage {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: QoS,
    pub retain: bool,
    pub dup: bool,
    pub subscription_identifiers: Vec<u32>,
    pub user_properties: Vec<(String, String)>,
    pub content_type: Option<String>,
    pub response_topic: Option<String>,
    pub correlation_data: Option<Vec<u8>>,
    pub message_expiry_interval: Option<u32>,
    pub payload_format_indicator: Option<bool>,
}

impl ReceivedMessage {
    pub(crate) fn from_message(msg: Message) -> Self {
        Self {
            topic: msg.topic,
            payload: msg.payload,
            qos: msg.qos,
            retain: msg.retain,
            dup: false,
            subscription_identifiers: msg.properties.subscription_identifiers,
            user_properties: msg.properties.user_properties,
            content_type: msg.properties.content_type,
            response_topic: msg.properties.response_topic,
            correlation_data: msg.properties.correlation_data,
            message_expiry_interval: msg.properties.message_expiry_interval,
            payload_format_indicator: msg.properties.payload_format_indicator,
        }
    }

    pub(crate) fn from_publish(packet: &mqtt5_protocol::packet::publish::PublishPacket) -> Self {
        let msg: Message = packet.clone().into();
        let mut out = Self::from_message(msg);
        out.dup = packet.dup;
        out
    }
}

pub(crate) type MessageQueue = Arc<Mutex<Vec<ReceivedMessage>>>;

/// Handle to an active subscription with its own message queue.
///
/// Each call to [`TestClient::subscribe`] returns a fresh `Subscription`;
/// tests pull delivered messages off it with
/// [`expect_publish`](Self::expect_publish) or inspect the snapshot via
/// [`snapshot`](Self::snapshot).
#[derive(Clone)]
pub struct Subscription {
    messages: MessageQueue,
    packet_id: u16,
    granted_qos: QoS,
}

impl Subscription {
    pub(crate) fn new(messages: MessageQueue, packet_id: u16, granted_qos: QoS) -> Self {
        Self {
            messages,
            packet_id,
            granted_qos,
        }
    }

    /// Returns the SUBACK Packet Identifier echoed by the broker.
    #[must_use]
    pub fn packet_id(&self) -> u16 {
        self.packet_id
    }

    /// Returns the granted `QoS` from the broker's SUBACK.
    #[must_use]
    pub fn granted_qos(&self) -> QoS {
        self.granted_qos
    }

    /// Waits for the next received message, up to `timeout`.
    ///
    /// Returns `None` if the timeout elapses with no message delivered. The
    /// returned message is removed from the subscription's queue so
    /// subsequent calls see later messages.
    ///
    /// # Panics
    /// Panics if the internal mutex has been poisoned by another thread
    /// panicking while holding the lock.
    pub async fn expect_publish(&self, timeout: Duration) -> Option<ReceivedMessage> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            {
                let mut queue = self.messages.lock().unwrap();
                if !queue.is_empty() {
                    return Some(queue.remove(0));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Waits until at least `count` messages have accumulated, returning
    /// `true` if reached before `timeout`.
    ///
    /// # Panics
    /// Panics if the internal mutex has been poisoned.
    pub async fn wait_for_messages(&self, count: usize, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.messages.lock().unwrap().len() >= count {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Returns a snapshot of all messages currently queued without
    /// consuming them.
    ///
    /// # Panics
    /// Panics if the internal mutex has been poisoned.
    #[must_use]
    pub fn snapshot(&self) -> Vec<ReceivedMessage> {
        self.messages.lock().unwrap().clone()
    }

    /// Returns the number of queued messages.
    ///
    /// # Panics
    /// Panics if the internal mutex has been poisoned.
    #[must_use]
    pub fn count(&self) -> usize {
        self.messages.lock().unwrap().len()
    }

    /// Drops every queued message.
    ///
    /// # Panics
    /// Panics if the internal mutex has been poisoned.
    pub fn clear(&self) {
        self.messages.lock().unwrap().clear();
    }
}

/// High-level conformance test client.
///
/// Dispatches to whichever backend is active for the [`SutHandle`] the
/// client was constructed from. The public methods mirror the vocabulary
/// used by conformance tests: `connect`, `publish`, `subscribe`,
/// `expect_publish`, `disconnect`.
pub enum TestClient {
    #[cfg(feature = "inprocess-fixture")]
    InProcess(inprocess::InProcessTestClient),
    Raw(raw::RawTestClient),
}

impl TestClient {
    /// Connects a new client to `sut` using the given `client_id` and
    /// default options.
    ///
    /// # Errors
    /// Returns an error if the SUT has no TCP address or if the underlying
    /// client fails to connect.
    pub async fn connect(sut: &SutHandle, client_id: &str) -> Result<Self, TestClientError> {
        let options = ConnectOptions::new(client_id);
        Self::connect_with_options(sut, options).await
    }

    /// Connects a new client with a random ULID-suffixed client id, using
    /// `prefix` as the leading segment.
    ///
    /// # Errors
    /// Returns an error if the SUT has no TCP address or if the underlying
    /// client fails to connect.
    pub async fn connect_with_prefix(
        sut: &SutHandle,
        prefix: &str,
    ) -> Result<Self, TestClientError> {
        let client_id = format!("conf-{prefix}-{}", Ulid::new());
        Self::connect(sut, &client_id).await
    }

    /// Connects a new client to `sut` with the given [`ConnectOptions`].
    ///
    /// # Errors
    /// Returns an error if the SUT has no TCP address or if the underlying
    /// client fails to connect.
    pub async fn connect_with_options(
        sut: &SutHandle,
        options: ConnectOptions,
    ) -> Result<Self, TestClientError> {
        match sut {
            #[cfg(feature = "inprocess-fixture")]
            SutHandle::InProcess(_) => {
                let client = inprocess::InProcessTestClient::connect(sut, options).await?;
                Ok(Self::InProcess(client))
            }
            SutHandle::External(_) => {
                let addr = sut
                    .tcp_socket_addr()?
                    .ok_or(TestClientError::NoTcpAddress)?;
                let client = raw::RawTestClient::connect(addr, options).await?;
                Ok(Self::Raw(client))
            }
        }
    }

    /// Returns the client id used to connect.
    #[must_use]
    pub fn client_id(&self) -> &str {
        match self {
            #[cfg(feature = "inprocess-fixture")]
            Self::InProcess(c) => c.client_id(),
            Self::Raw(c) => c.client_id(),
        }
    }

    /// Returns the `SessionPresent` flag from the server's CONNACK.
    #[must_use]
    pub fn session_present(&self) -> bool {
        match self {
            #[cfg(feature = "inprocess-fixture")]
            Self::InProcess(c) => c.session_present(),
            Self::Raw(c) => c.session_present(),
        }
    }

    /// Publishes `payload` to `topic` with default options (`QoS` 0, no
    /// retain).
    ///
    /// # Errors
    /// Returns an error if the broker rejects the publish or the client
    /// is disconnected.
    pub async fn publish(&self, topic: &str, payload: &[u8]) -> Result<(), TestClientError> {
        self.publish_with_options(topic, payload, PublishOptions::default())
            .await
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
        match self {
            #[cfg(feature = "inprocess-fixture")]
            Self::InProcess(c) => c.publish_with_options(topic, payload, options).await,
            Self::Raw(c) => c.publish_with_options(topic, payload, options).await,
        }
    }

    /// Subscribes to `filter` and returns a [`Subscription`] handle.
    ///
    /// # Errors
    /// Returns an error if the broker rejects the subscription.
    pub async fn subscribe(
        &self,
        filter: &str,
        options: SubscribeOptions,
    ) -> Result<Subscription, TestClientError> {
        match self {
            #[cfg(feature = "inprocess-fixture")]
            Self::InProcess(c) => c.subscribe(filter, options).await,
            Self::Raw(c) => c.subscribe(filter, options).await,
        }
    }

    /// Unsubscribes from `filter`.
    ///
    /// # Errors
    /// Returns an error if the broker rejects the unsubscribe.
    pub async fn unsubscribe(&self, filter: &str) -> Result<(), TestClientError> {
        match self {
            #[cfg(feature = "inprocess-fixture")]
            Self::InProcess(c) => c.unsubscribe(filter).await,
            Self::Raw(c) => c.unsubscribe(filter).await,
        }
    }

    /// Performs a normal DISCONNECT handshake.
    ///
    /// # Errors
    /// Returns an error if the underlying client reports one while
    /// disconnecting.
    pub async fn disconnect(&self) -> Result<(), TestClientError> {
        match self {
            #[cfg(feature = "inprocess-fixture")]
            Self::InProcess(c) => c.disconnect().await,
            Self::Raw(c) => c.disconnect().await,
        }
    }

    /// Drops the TCP connection without sending a DISCONNECT packet.
    ///
    /// # Errors
    /// Returns an error if the underlying client reports one while tearing
    /// down the transport.
    pub async fn disconnect_abnormally(&self) -> Result<(), TestClientError> {
        match self {
            #[cfg(feature = "inprocess-fixture")]
            Self::InProcess(c) => c.disconnect_abnormally().await,
            Self::Raw(c) => c.disconnect_abnormally().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sut::{InProcessFixture, SutHandle};

    #[tokio::test]
    async fn inprocess_roundtrip() {
        let fixture = InProcessFixture::start().await;
        let sut = SutHandle::InProcess(Box::new(fixture));

        let subscriber = TestClient::connect_with_prefix(&sut, "rt-sub")
            .await
            .unwrap();
        let subscription = subscriber
            .subscribe(
                "test/roundtrip",
                SubscribeOptions {
                    qos: QoS::AtLeastOnce,
                    ..SubscribeOptions::default()
                },
            )
            .await
            .unwrap();

        let publisher = TestClient::connect_with_prefix(&sut, "rt-pub")
            .await
            .unwrap();
        publisher.publish("test/roundtrip", b"hello").await.unwrap();

        let msg = subscription
            .expect_publish(Duration::from_secs(3))
            .await
            .expect("message should arrive");
        assert_eq!(msg.topic, "test/roundtrip");
        assert_eq!(msg.payload, b"hello");

        subscriber.disconnect().await.unwrap();
        publisher.disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn inprocess_expect_publish_times_out() {
        let fixture = InProcessFixture::start().await;
        let sut = SutHandle::InProcess(Box::new(fixture));

        let subscriber = TestClient::connect_with_prefix(&sut, "timeout-sub")
            .await
            .unwrap();
        let subscription = subscriber
            .subscribe("test/empty", SubscribeOptions::default())
            .await
            .unwrap();

        assert!(subscription
            .expect_publish(Duration::from_millis(100))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn raw_backend_roundtrip_against_inprocess_fixture() {
        let fixture = InProcessFixture::start().await;
        let addr = fixture.tcp_socket_addr();

        let sub_opts = ConnectOptions::new(format!("conf-raw-sub-{}", Ulid::new()));
        let subscriber = raw::RawTestClient::connect(addr, sub_opts).await.unwrap();
        let subscription = subscriber
            .subscribe(
                "raw/test/roundtrip",
                SubscribeOptions {
                    qos: QoS::AtLeastOnce,
                    ..SubscribeOptions::default()
                },
            )
            .await
            .unwrap();

        let pub_opts = ConnectOptions::new(format!("conf-raw-pub-{}", Ulid::new()));
        let publisher = raw::RawTestClient::connect(addr, pub_opts).await.unwrap();
        publisher
            .publish_with_options(
                "raw/test/roundtrip",
                b"hello-raw",
                PublishOptions {
                    qos: QoS::AtLeastOnce,
                    ..PublishOptions::default()
                },
            )
            .await
            .unwrap();

        let msg = subscription
            .expect_publish(Duration::from_secs(3))
            .await
            .expect("raw backend should receive published message");
        assert_eq!(msg.topic, "raw/test/roundtrip");
        assert_eq!(msg.payload, b"hello-raw");

        subscriber.disconnect().await.unwrap();
        publisher.disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn raw_backend_qos2_roundtrip() {
        let fixture = InProcessFixture::start().await;
        let addr = fixture.tcp_socket_addr();

        let sub_opts = ConnectOptions::new(format!("conf-raw-q2sub-{}", Ulid::new()));
        let subscriber = raw::RawTestClient::connect(addr, sub_opts).await.unwrap();
        let subscription = subscriber
            .subscribe(
                "raw/test/qos2",
                SubscribeOptions {
                    qos: QoS::ExactlyOnce,
                    ..SubscribeOptions::default()
                },
            )
            .await
            .unwrap();

        let pub_opts = ConnectOptions::new(format!("conf-raw-q2pub-{}", Ulid::new()));
        let publisher = raw::RawTestClient::connect(addr, pub_opts).await.unwrap();
        publisher
            .publish_with_options(
                "raw/test/qos2",
                b"hello-qos2",
                PublishOptions {
                    qos: QoS::ExactlyOnce,
                    ..PublishOptions::default()
                },
            )
            .await
            .unwrap();

        let msg = subscription
            .expect_publish(Duration::from_secs(3))
            .await
            .expect("raw backend should receive QoS2 message");
        assert_eq!(msg.topic, "raw/test/qos2");
        assert_eq!(msg.payload, b"hello-qos2");

        subscriber.disconnect().await.unwrap();
        publisher.disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn raw_backend_unsubscribe_stops_delivery() {
        let fixture = InProcessFixture::start().await;
        let addr = fixture.tcp_socket_addr();

        let sub_opts = ConnectOptions::new(format!("conf-raw-unsub-{}", Ulid::new()));
        let subscriber = raw::RawTestClient::connect(addr, sub_opts).await.unwrap();
        let subscription = subscriber
            .subscribe("raw/test/unsub", SubscribeOptions::default())
            .await
            .unwrap();

        let pub_opts = ConnectOptions::new(format!("conf-raw-unsub-pub-{}", Ulid::new()));
        let publisher = raw::RawTestClient::connect(addr, pub_opts).await.unwrap();
        publisher
            .publish("raw/test/unsub", b"before")
            .await
            .unwrap();

        assert!(subscription
            .expect_publish(Duration::from_secs(3))
            .await
            .is_some());

        subscriber.unsubscribe("raw/test/unsub").await.unwrap();
        publisher.publish("raw/test/unsub", b"after").await.unwrap();

        assert!(subscription
            .expect_publish(Duration::from_millis(300))
            .await
            .is_none());

        subscriber.disconnect().await.unwrap();
        publisher.disconnect().await.unwrap();
    }
}
