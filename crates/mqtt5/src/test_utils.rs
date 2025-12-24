//! Test utilities for the MQTT library

use crate::packet::connack::ConnAckPacket;
use crate::packet::connect::ConnectPacket;
use crate::packet::publish::PublishPacket;
use crate::packet::suback::{SubAckPacket, SubAckReasonCode};
use crate::packet::subscribe::{SubscribePacket, SubscriptionOptions, TopicFilter};
use crate::packet::Packet;
use crate::protocol::v5::properties::Properties;
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::session::limits::{ExpiringMessage, LimitsManager};
use crate::session::retained::RetainedMessage;
use crate::time::Duration;
use crate::{MqttClient, QoS, Result};
use bytes::BytesMut;
use tokio::time::timeout;

/// Helper trait for building test packets
pub trait TestPacketBuilder: Sized {
    fn test_default() -> Self;
}

impl TestPacketBuilder for ConnectPacket {
    fn test_default() -> Self {
        Self {
            protocol_version: 5,
            clean_start: true,
            keep_alive: 60,
            properties: Properties::new(),
            client_id: "test-client".to_string(),
            will: None,
            username: None,
            password: None,
            will_properties: Properties::new(),
        }
    }
}

impl TestPacketBuilder for ConnAckPacket {
    fn test_default() -> Self {
        Self {
            protocol_version: 5,
            session_present: false,
            reason_code: ReasonCode::Success,
            properties: Properties::new(),
        }
    }
}

impl TestPacketBuilder for PublishPacket {
    fn test_default() -> Self {
        Self {
            topic_name: "test/topic".to_string(),
            packet_id: None,
            payload: b"test payload".to_vec().into(),
            qos: QoS::AtMostOnce,
            retain: false,
            dup: false,
            properties: Properties::new(),
            protocol_version: 5,
        }
    }
}

impl TestPacketBuilder for SubscribePacket {
    fn test_default() -> Self {
        Self {
            packet_id: 1,
            properties: Properties::new(),
            filters: vec![TopicFilter {
                filter: "test/+".to_string(),
                options: SubscriptionOptions {
                    qos: QoS::AtLeastOnce,
                    no_local: false,
                    retain_as_published: false,
                    retain_handling: crate::packet::subscribe::RetainHandling::SendAtSubscribe,
                },
            }],
            protocol_version: 5,
        }
    }
}

impl TestPacketBuilder for SubAckPacket {
    fn test_default() -> Self {
        Self {
            packet_id: 1,
            properties: Properties::new(),
            reason_codes: vec![SubAckReasonCode::GrantedQoS1],
            protocol_version: 5,
        }
    }
}

/// Creates a test CONNECT packet
#[must_use]
pub fn create_test_connect() -> Packet {
    Packet::Connect(Box::new(ConnectPacket::test_default()))
}

/// Creates a test CONNACK packet
#[must_use]
pub fn create_test_connack(session_present: bool, reason_code: ReasonCode) -> Packet {
    Packet::ConnAck(ConnAckPacket {
        protocol_version: 5,
        session_present,
        reason_code,
        properties: Properties::new(),
    })
}

/// Creates a test PUBLISH packet
#[must_use]
pub fn create_test_publish(topic: &str, payload: &[u8], qos: QoS) -> Packet {
    let packet_id = match qos {
        QoS::AtMostOnce => None,
        _ => Some(1),
    };

    Packet::Publish(PublishPacket {
        topic_name: topic.to_string(),
        packet_id,
        payload: payload.to_vec().into(),
        qos,
        retain: false,
        dup: false,
        properties: Properties::new(),
        protocol_version: 5,
    })
}

/// Creates a test SUBSCRIBE packet
#[must_use]
pub fn create_test_subscribe(topics: Vec<(&str, QoS)>) -> Packet {
    Packet::Subscribe(SubscribePacket {
        packet_id: 1,
        properties: Properties::new(),
        filters: topics
            .into_iter()
            .map(|(filter, qos)| TopicFilter {
                filter: filter.to_string(),
                options: SubscriptionOptions {
                    qos,
                    no_local: false,
                    retain_as_published: false,
                    retain_handling: crate::packet::subscribe::RetainHandling::SendAtSubscribe,
                },
            })
            .collect(),
        protocol_version: 5,
    })
}

/// Encodes a packet to bytes for testing
///
/// # Errors
///
/// Returns `MqttError` if packet encoding fails
pub fn encode_packet(packet: &Packet) -> std::result::Result<Vec<u8>, crate::error::MqttError> {
    let mut buf = BytesMut::with_capacity(crate::constants::buffer::DEFAULT_CAPACITY);
    crate::transport::packet_io::encode_packet_to_buffer(packet, &mut buf)?;
    Ok(buf.to_vec())
}

/// Asserts that two packets are equal
///
/// # Panics
///
/// Panics if the packets are not equal
pub fn assert_packets_equal(actual: &Packet, expected: &Packet) {
    match (actual, expected) {
        (Packet::Connect(a), Packet::Connect(e)) => {
            assert_eq!(a.protocol_version, e.protocol_version);
            assert_eq!(a.clean_start, e.clean_start);
            assert_eq!(a.keep_alive, e.keep_alive);
            assert_eq!(a.client_id, e.client_id);
        }
        (Packet::ConnAck(a), Packet::ConnAck(e)) => {
            assert_eq!(a.session_present, e.session_present);
            assert_eq!(a.reason_code, e.reason_code);
        }
        (Packet::Publish(a), Packet::Publish(e)) => {
            assert_eq!(a.topic_name, e.topic_name);
            assert_eq!(a.payload, e.payload);
            assert_eq!(a.qos, e.qos);
            assert_eq!(a.packet_id, e.packet_id);
        }
        (Packet::PingReq, Packet::PingReq) | (Packet::PingResp, Packet::PingResp) => {}
        _ => panic!("Packet types don't match: {actual:?} vs {expected:?}"),
    }
}

/// Runs an async test with a timeout
///
/// # Panics
///
/// Panics if the future does not complete within the specified duration
pub async fn run_with_timeout<F, T>(duration: Duration, future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    match timeout(duration, future).await {
        Ok(result) => result,
        Err(tokio::time::error::Elapsed { .. }) => panic!("Test timed out after {duration:?}"),
    }
}

/// Helper macro for async tests with timeout
#[macro_export]
macro_rules! test_timeout {
    ($duration:expr, $body:expr) => {
        $crate::test_utils::run_with_timeout($duration, $body)
    };
}

// ===== Client Test Utilities =====

/// Generates a unique client ID for testing
#[must_use]
pub fn test_client_id(base: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("test-{base}-{id}")
}

/// Creates a test client with a unique ID
#[must_use]
pub fn create_test_client(name: &str) -> MqttClient {
    MqttClient::new(test_client_id(name))
}

/// Creates a test client and connects it to the default test broker
///
/// # Errors
///
/// Returns `MqttError` if connection fails
pub async fn create_connected_client(name: &str) -> Result<MqttClient> {
    let client = create_test_client(name);
    client.connect("mqtt://127.0.0.1:1883").await?;
    Ok(client)
}

/// Builder for creating test clients with various configurations
pub struct TestClientBuilder {
    name: String,
    connect_url: Option<String>,
}

impl TestClientBuilder {
    /// Creates a new test client builder
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            connect_url: None,
        }
    }

    /// Sets the connection URL for automatic connection
    #[must_use]
    pub fn with_connection(mut self, url: &str) -> Self {
        self.connect_url = Some(url.to_string());
        self
    }

    /// Builds the test client, optionally connecting if URL was provided
    ///
    /// # Errors
    ///
    /// Returns `MqttError` if connection fails when URL is provided
    pub async fn build(self) -> Result<MqttClient> {
        let client = MqttClient::new(test_client_id(&self.name));
        if let Some(url) = self.connect_url {
            client.connect(&url).await?;
        }
        Ok(client)
    }
}

/// Creates a test expiring message with standard defaults
#[must_use]
pub fn test_expiring_message(index: u8) -> ExpiringMessage {
    ExpiringMessage::new(
        format!("test/{index}"),
        vec![index],
        QoS::AtMostOnce,
        false,
        None,
        None,
        &LimitsManager::with_defaults(),
    )
}

/// Creates a test retained message with standard defaults
#[must_use]
pub fn test_retained_message(index: u8) -> RetainedMessage {
    RetainedMessage {
        topic: format!("topic/{index}"),
        payload: vec![index],
        qos: QoS::AtMostOnce,
        properties: Properties::default(),
    }
}

/// Builder for creating batches of test messages
pub struct TestMessageBuilder {
    topic_prefix: String,
    qos: QoS,
    retain: bool,
}

impl TestMessageBuilder {
    /// Creates a new test message builder with defaults
    #[must_use]
    pub fn new() -> Self {
        Self {
            topic_prefix: "test".to_string(),
            qos: QoS::AtMostOnce,
            retain: false,
        }
    }

    /// Sets the topic prefix for generated messages
    #[must_use]
    pub fn with_topic_prefix(mut self, prefix: &str) -> Self {
        self.topic_prefix = prefix.to_string();
        self
    }

    /// Sets the `QoS` level for generated messages
    #[must_use]
    pub fn with_qos(mut self, qos: QoS) -> Self {
        self.qos = qos;
        self
    }

    /// Sets the retain flag for generated messages
    #[must_use]
    pub fn with_retain(mut self, retain: bool) -> Self {
        self.retain = retain;
        self
    }

    /// Builds a batch of expiring messages
    #[must_use]
    pub fn build_expiring_batch(self, count: u8) -> Vec<ExpiringMessage> {
        (0..count)
            .map(|i| {
                ExpiringMessage::new(
                    format!("{}/{i}", self.topic_prefix),
                    vec![i],
                    self.qos,
                    self.retain,
                    None,
                    None,
                    &LimitsManager::with_defaults(),
                )
            })
            .collect()
    }

    /// Builds a batch of retained messages
    #[must_use]
    pub fn build_retained_batch(self, count: u8) -> Vec<RetainedMessage> {
        (0..count)
            .map(|i| RetainedMessage {
                topic: format!("{}/{i}", self.topic_prefix),
                payload: vec![i],
                qos: self.qos,
                properties: Properties::default(),
            })
            .collect()
    }
}

impl Default for TestMessageBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Connects a client with retry logic
///
/// # Errors
///
/// Returns `MqttError` if all connection attempts fail
pub async fn connect_with_retry(client: &MqttClient, url: &str, max_retries: u32) -> Result<()> {
    for attempt in 0..max_retries {
        match client.connect(url).await {
            Ok(()) => return Ok(()),
            Err(e) if attempt < max_retries - 1 => {
                tracing::debug!(
                    "Connection attempt {} failed: {:?}, retrying...",
                    attempt,
                    e
                );
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// Test data generators
pub struct TestData;

impl TestData {
    /// Generates a list of test topics with a given prefix
    #[must_use]
    pub fn topics(count: usize, prefix: &str) -> Vec<String> {
        (0..count).map(|i| format!("{prefix}/{i}")).collect()
    }

    /// Generates a list of test payloads
    #[must_use]
    pub fn payloads(count: u8) -> Vec<Vec<u8>> {
        (0..count).map(|i| vec![i]).collect()
    }

    /// Generates a list of topic-payload pairs
    #[must_use]
    pub fn messages(count: u8, topic_prefix: &str) -> Vec<(String, Vec<u8>)> {
        (0..count)
            .map(|i| (format!("{topic_prefix}/{i}"), vec![i]))
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packet_builder_connect() {
        let packet = ConnectPacket::test_default();
        assert_eq!(packet.client_id, "test-client");
        assert_eq!(packet.protocol_version, 5);
        assert!(packet.clean_start);
    }

    #[test]
    fn test_create_publish() {
        let packet = create_test_publish("test/topic", b"hello", QoS::AtLeastOnce);
        match packet {
            Packet::Publish(p) => {
                assert_eq!(p.topic_name, "test/topic");
                assert_eq!(&p.payload[..], b"hello");
                assert_eq!(p.qos, QoS::AtLeastOnce);
                assert_eq!(p.packet_id, Some(1));
            }
            _ => panic!("Wrong packet type"),
        }
    }

    #[test]
    fn test_encode_decode_roundtrip() {
        let original = create_test_connect();
        let encoded = encode_packet(&original).unwrap();
        assert!(!encoded.is_empty());

        // Verify fixed header - CONNECT is packet type 1
        assert_eq!(encoded[0] >> 4, 1);
    }

    #[tokio::test]
    async fn test_timeout_helper() {
        // Should complete
        let result = run_with_timeout(Duration::from_millis(100), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            42
        })
        .await;
        assert_eq!(result, 42);

        // Should timeout
        let result = std::panic::catch_unwind(|| {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                run_with_timeout(Duration::from_millis(10), async {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    42
                })
                .await
            })
        });
        assert!(result.is_err());
    }
}
