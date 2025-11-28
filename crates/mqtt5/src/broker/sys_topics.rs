//! $SYS topics provider for broker monitoring
//!
//! Publishes broker statistics and monitoring information to special $SYS topics
//! according to MQTT specification recommendations.

use crate::broker::router::MessageRouter;
use crate::packet::publish::PublishPacket;
use crate::time::{Duration, SystemTime};
use crate::QoS;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use tokio::time::interval;
use tracing::debug;

/// Statistics tracked by the broker
#[derive(Debug)]
pub struct BrokerStats {
    /// Broker start time
    pub start_time: SystemTime,
    /// Number of currently connected clients
    pub clients_connected: AtomicUsize,
    /// Total number of clients that have connected
    pub clients_total: AtomicU64,
    /// Maximum number of concurrent clients
    pub clients_maximum: AtomicUsize,
    /// Total messages sent
    pub messages_sent: AtomicU64,
    /// Total messages received
    pub messages_received: AtomicU64,
    /// Total publish messages sent
    pub publish_sent: AtomicU64,
    /// Total publish messages received
    pub publish_received: AtomicU64,
    /// Total bytes sent
    pub bytes_sent: AtomicU64,
    /// Total bytes received
    pub bytes_received: AtomicU64,
}

impl BrokerStats {
    /// Create new broker statistics tracker
    #[must_use]
    pub fn new() -> Self {
        Self {
            start_time: SystemTime::now(),
            clients_connected: AtomicUsize::new(0),
            clients_total: AtomicU64::new(0),
            clients_maximum: AtomicUsize::new(0),
            messages_sent: AtomicU64::new(0),
            messages_received: AtomicU64::new(0),
            publish_sent: AtomicU64::new(0),
            publish_received: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
        }
    }

    /// Record client connection
    pub fn client_connected(&self) {
        let current = self.clients_connected.fetch_add(1, Ordering::Relaxed) + 1;
        self.clients_total.fetch_add(1, Ordering::Relaxed);

        // Update maximum if needed
        let mut max = self.clients_maximum.load(Ordering::Relaxed);
        while current > max {
            match self.clients_maximum.compare_exchange_weak(
                max,
                current,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => max = actual,
            }
        }
    }

    /// Record client disconnection
    pub fn client_disconnected(&self) {
        self.clients_connected.fetch_sub(1, Ordering::Relaxed);
    }

    /// Record message sent
    pub fn message_sent(&self, bytes: usize) {
        self.messages_sent.fetch_add(1, Ordering::Relaxed);
        self.bytes_sent.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Record publish message sent
    pub fn publish_sent(&self, bytes: usize) {
        self.publish_sent.fetch_add(1, Ordering::Relaxed);
        self.message_sent(bytes);
    }

    /// Record message received
    pub fn message_received(&self, bytes: usize) {
        self.messages_received.fetch_add(1, Ordering::Relaxed);
        self.bytes_received
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Record publish message received
    pub fn publish_received(&self, bytes: usize) {
        self.publish_received.fetch_add(1, Ordering::Relaxed);
        self.message_received(bytes);
    }

    /// Get broker uptime in seconds
    #[must_use]
    pub fn uptime_seconds(&self) -> u64 {
        self.start_time.elapsed().unwrap_or_default().as_secs()
    }
}

impl Default for BrokerStats {
    fn default() -> Self {
        Self::new()
    }
}

/// Provider of $SYS topics information
pub struct SysTopicsProvider {
    /// Router for publishing messages
    router: Arc<MessageRouter>,
    /// Broker statistics
    stats: Arc<BrokerStats>,
    /// Update interval
    update_interval: Duration,
    /// Broker version
    version: String,
}

impl SysTopicsProvider {
    /// Create new $SYS topics provider
    #[must_use]
    pub fn new(router: Arc<MessageRouter>, stats: Arc<BrokerStats>) -> Self {
        Self {
            router,
            stats,
            update_interval: Duration::from_secs(10),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Set update interval
    #[must_use]
    pub fn with_update_interval(mut self, interval: Duration) -> Self {
        self.update_interval = interval;
        self
    }

    /// Start publishing $SYS topics
    #[cfg(not(target_arch = "wasm32"))]
    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = interval(self.update_interval);
            interval.tick().await; // Skip first immediate tick

            // Publish static topics once
            self.publish_static_topics().await;

            loop {
                interval.tick().await;
                self.publish_dynamic_topics().await;
            }
        })
    }

    /// Publish static $SYS topics (version, etc.)
    async fn publish_static_topics(&self) {
        // Broker version
        self.publish_sys_topic("$SYS/broker/version", &self.version)
            .await;

        // Broker implementation
        self.publish_sys_topic("$SYS/broker/implementation", "mqtt-v5")
            .await;

        // Protocol version
        self.publish_sys_topic("$SYS/broker/protocol_version", "5.0")
            .await;
    }

    /// Publish dynamic $SYS topics (stats that change)
    async fn publish_dynamic_topics(&self) {
        // Uptime
        let uptime = self.stats.uptime_seconds();
        self.publish_sys_topic("$SYS/broker/uptime", &uptime.to_string())
            .await;

        // Client statistics
        let connected = self.stats.clients_connected.load(Ordering::Relaxed);
        self.publish_sys_topic("$SYS/broker/clients/connected", &connected.to_string())
            .await;

        let total = self.stats.clients_total.load(Ordering::Relaxed);
        self.publish_sys_topic("$SYS/broker/clients/total", &total.to_string())
            .await;

        let maximum = self.stats.clients_maximum.load(Ordering::Relaxed);
        self.publish_sys_topic("$SYS/broker/clients/maximum", &maximum.to_string())
            .await;

        // Message statistics
        let msg_sent = self.stats.messages_sent.load(Ordering::Relaxed);
        self.publish_sys_topic("$SYS/broker/messages/sent", &msg_sent.to_string())
            .await;

        let msg_recv = self.stats.messages_received.load(Ordering::Relaxed);
        self.publish_sys_topic("$SYS/broker/messages/received", &msg_recv.to_string())
            .await;

        let pub_sent = self.stats.publish_sent.load(Ordering::Relaxed);
        self.publish_sys_topic("$SYS/broker/messages/publish/sent", &pub_sent.to_string())
            .await;

        let pub_recv = self.stats.publish_received.load(Ordering::Relaxed);
        self.publish_sys_topic(
            "$SYS/broker/messages/publish/received",
            &pub_recv.to_string(),
        )
        .await;

        // Byte statistics
        let bytes_sent = self.stats.bytes_sent.load(Ordering::Relaxed);
        self.publish_sys_topic("$SYS/broker/bytes/sent", &bytes_sent.to_string())
            .await;

        let bytes_recv = self.stats.bytes_received.load(Ordering::Relaxed);
        self.publish_sys_topic("$SYS/broker/bytes/received", &bytes_recv.to_string())
            .await;

        // Router statistics
        let retained_count = self.router.retained_count().await;
        self.publish_sys_topic("$SYS/broker/retained/count", &retained_count.to_string())
            .await;

        let topic_count = self.router.topic_count().await;
        self.publish_sys_topic("$SYS/broker/subscriptions/count", &topic_count.to_string())
            .await;
    }

    /// Publish a single $SYS topic
    async fn publish_sys_topic(&self, topic: &str, value: &str) {
        let packet = PublishPacket::new(topic, value.as_bytes(), QoS::AtMostOnce).with_retain(true);

        debug!("Publishing $SYS topic: {} = {}", topic, value);
        self.router.route_message(&packet, None).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_broker_stats_client_tracking() {
        let stats = BrokerStats::new();

        // Connect 3 clients
        stats.client_connected();
        stats.client_connected();
        stats.client_connected();

        assert_eq!(stats.clients_connected.load(Ordering::Relaxed), 3);
        assert_eq!(stats.clients_total.load(Ordering::Relaxed), 3);
        assert_eq!(stats.clients_maximum.load(Ordering::Relaxed), 3);

        // Disconnect one
        stats.client_disconnected();
        assert_eq!(stats.clients_connected.load(Ordering::Relaxed), 2);
        assert_eq!(stats.clients_total.load(Ordering::Relaxed), 3);
        assert_eq!(stats.clients_maximum.load(Ordering::Relaxed), 3);

        // Connect two more
        stats.client_connected();
        stats.client_connected();
        assert_eq!(stats.clients_connected.load(Ordering::Relaxed), 4);
        assert_eq!(stats.clients_total.load(Ordering::Relaxed), 5);
        assert_eq!(stats.clients_maximum.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn test_broker_stats_message_tracking() {
        let stats = BrokerStats::new();

        // Send some messages
        stats.message_sent(100);
        stats.publish_sent(200);

        assert_eq!(stats.messages_sent.load(Ordering::Relaxed), 2);
        assert_eq!(stats.publish_sent.load(Ordering::Relaxed), 1);
        assert_eq!(stats.bytes_sent.load(Ordering::Relaxed), 300);

        // Receive some messages
        stats.message_received(150);
        stats.publish_received(250);

        assert_eq!(stats.messages_received.load(Ordering::Relaxed), 2);
        assert_eq!(stats.publish_received.load(Ordering::Relaxed), 1);
        assert_eq!(stats.bytes_received.load(Ordering::Relaxed), 400);
    }

    #[test]
    fn test_broker_stats_uptime() {
        let stats = BrokerStats::new();
        std::thread::sleep(Duration::from_millis(10));
        assert!(stats.uptime_seconds() < 1000); // Just verify it's reasonable
    }

    #[tokio::test]
    async fn test_sys_topics_provider() {
        let router = Arc::new(MessageRouter::new());
        let stats = Arc::new(BrokerStats::new());

        let provider = SysTopicsProvider::new(Arc::clone(&router), Arc::clone(&stats))
            .with_update_interval(Duration::from_millis(100));

        // Track some stats
        stats.client_connected();
        stats.publish_sent(100);
        stats.publish_received(200);

        // Start provider
        let handle = provider.start();

        // Wait for updates
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Check that retained messages were published
        let retained = router.get_retained_messages("$SYS/#").await;
        assert!(!retained.is_empty());

        // Should have version
        let version_msg = retained
            .iter()
            .find(|msg| msg.topic_name == "$SYS/broker/version");
        assert!(version_msg.is_some());

        // Should have client count
        let clients_msg = retained
            .iter()
            .find(|msg| msg.topic_name == "$SYS/broker/clients/connected");
        assert!(clients_msg.is_some());
        assert_eq!(clients_msg.unwrap().payload, b"1");

        // Cleanup
        handle.abort();
    }
}
