//! Resource monitoring and limits for MQTT broker
//!
//! This module provides real-time monitoring of broker resources and enforces
//! limits to prevent resource exhaustion attacks.

use crate::time::{Duration, Instant};
use mqtt5_protocol::usize_to_f64_saturating;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Resource limits configuration
#[derive(Debug, Clone)]
pub struct ResourceLimits {
    /// Maximum number of concurrent connections
    pub max_connections: usize,

    /// Maximum connections per IP address
    pub max_connections_per_ip: usize,

    /// Maximum memory usage in bytes (0 = unlimited)
    pub max_memory_bytes: u64,

    /// Maximum message rate per client (messages/second)
    pub max_message_rate_per_client: u32,

    /// Maximum bandwidth per client (bytes/second)
    pub max_bandwidth_per_client: u64,

    /// Connection rate limit (connections/second)
    pub max_connection_rate: u32,

    /// Time window for rate limiting (seconds)
    pub rate_limit_window: Duration,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_connections: 10_000,
            max_connections_per_ip: 100,
            max_memory_bytes: 1024 * 1024 * 1024,       // 1GB
            max_message_rate_per_client: 1000,          // 1000 msg/sec
            max_bandwidth_per_client: 10 * 1024 * 1024, // 10MB/sec
            max_connection_rate: 100,                   // 100 conn/sec
            rate_limit_window: Duration::from_secs(60),
        }
    }
}

/// Per-client resource tracking
#[derive(Debug)]
struct ClientResources {
    message_count: AtomicU64,
    bytes_transferred: AtomicU64,
    window_start: Instant,
}

impl ClientResources {
    fn new() -> Self {
        Self {
            message_count: AtomicU64::new(0),
            bytes_transferred: AtomicU64::new(0),
            window_start: Instant::now(),
        }
    }

    fn reset_window(&mut self) {
        self.message_count.store(0, Ordering::Relaxed);
        self.bytes_transferred.store(0, Ordering::Relaxed);
        self.window_start = Instant::now();
    }

    fn is_window_expired(&self, window_duration: Duration) -> bool {
        self.window_start.elapsed() > window_duration
    }
}

/// Connection rate tracking
#[derive(Debug)]
struct ConnectionRateTracker {
    connections: Vec<Instant>,
}

impl ConnectionRateTracker {
    fn new() -> Self {
        Self {
            connections: Vec::new(),
        }
    }

    /// Add a new connection and check if rate limit is exceeded
    fn add_connection(&mut self, window_duration: Duration, max_rate: u32) -> bool {
        let now = Instant::now();

        // Clean old connections outside window
        self.connections
            .retain(|&timestamp| now.duration_since(timestamp) <= window_duration);

        // Check if we're under the rate limit
        if self.connections.len() < max_rate as usize {
            self.connections.push(now);
            true
        } else {
            false
        }
    }
}

/// Resource monitor for MQTT broker
pub struct ResourceMonitor {
    /// Resource limits configuration
    limits: ResourceLimits,

    /// Current connection count
    connection_count: AtomicUsize,

    /// Per-IP connection counts
    ip_connections: Arc<RwLock<HashMap<IpAddr, usize>>>,

    /// Per-client resource tracking
    client_resources: Arc<RwLock<HashMap<String, ClientResources>>>,

    /// Connection rate tracker
    connection_rate: Arc<RwLock<ConnectionRateTracker>>,

    /// Total bytes processed
    total_bytes: AtomicU64,

    /// Total messages processed
    total_messages: AtomicU64,
}

impl ResourceMonitor {
    /// Create a new resource monitor
    pub fn new(limits: ResourceLimits) -> Self {
        info!("Initializing resource monitor with limits: {:?}", limits);

        Self {
            limits,
            connection_count: AtomicUsize::new(0),
            ip_connections: Arc::new(RwLock::new(HashMap::new())),
            client_resources: Arc::new(RwLock::new(HashMap::new())),
            connection_rate: Arc::new(RwLock::new(ConnectionRateTracker::new())),
            total_bytes: AtomicU64::new(0),
            total_messages: AtomicU64::new(0),
        }
    }

    /// Check if a new connection can be accepted
    pub async fn can_accept_connection(&self, ip_addr: IpAddr) -> bool {
        // Check global connection limit
        let current_connections = self.connection_count.load(Ordering::Relaxed);
        if current_connections >= self.limits.max_connections {
            warn!(
                "Connection rejected: global limit reached ({}/{})",
                current_connections, self.limits.max_connections
            );
            return false;
        }

        // Check per-IP connection limit
        let ip_connections = self.ip_connections.read().await;
        let ip_count = ip_connections.get(&ip_addr).copied().unwrap_or(0);
        if ip_count >= self.limits.max_connections_per_ip {
            warn!(
                "Connection rejected: per-IP limit reached for {} ({}/{})",
                ip_addr, ip_count, self.limits.max_connections_per_ip
            );
            return false;
        }
        drop(ip_connections);

        // Check connection rate limit
        let mut rate_tracker = self.connection_rate.write().await;
        if !rate_tracker.add_connection(
            self.limits.rate_limit_window,
            self.limits.max_connection_rate,
        ) {
            warn!(
                "Connection rejected: rate limit exceeded ({} conn/{}s)",
                self.limits.max_connection_rate,
                self.limits.rate_limit_window.as_secs()
            );
            return false;
        }

        true
    }

    /// Register a new connection
    pub async fn register_connection(&self, client_id: String, ip_addr: IpAddr) {
        // Increment global connection count
        let new_count = self.connection_count.fetch_add(1, Ordering::Relaxed) + 1;
        debug!(
            "Connection registered: {} from {} (total: {})",
            client_id, ip_addr, new_count
        );

        // Increment per-IP connection count
        let mut ip_connections = self.ip_connections.write().await;
        *ip_connections.entry(ip_addr).or_insert(0) += 1;
        drop(ip_connections);

        let mut client_resources = self.client_resources.write().await;
        client_resources.insert(client_id, ClientResources::new());
    }

    /// Unregister a connection
    pub async fn unregister_connection(&self, client_id: &str, ip_addr: IpAddr) {
        // Decrement global connection count
        let new_count = self
            .connection_count
            .fetch_sub(1, Ordering::Relaxed)
            .saturating_sub(1);
        debug!(
            "Connection unregistered: {} from {} (total: {})",
            client_id, ip_addr, new_count
        );

        // Decrement per-IP connection count
        let mut ip_connections = self.ip_connections.write().await;
        if let Some(count) = ip_connections.get_mut(&ip_addr) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                ip_connections.remove(&ip_addr);
            }
        }
        drop(ip_connections);

        // Remove client resource tracking
        let mut client_resources = self.client_resources.write().await;
        client_resources.remove(client_id);
    }

    /// Check if client can send a message (rate limiting)
    pub async fn can_send_message(&self, client_id: &str, message_size: usize) -> bool {
        if self.limits.max_message_rate_per_client >= 1_000_000 {
            self.total_messages.fetch_add(1, Ordering::Relaxed);
            self.total_bytes
                .fetch_add(message_size as u64, Ordering::Relaxed);
            return true;
        }

        let mut client_resources = self.client_resources.write().await;

        let Some(resources) = client_resources.get_mut(client_id) else {
            return true;
        };

        if resources.is_window_expired(self.limits.rate_limit_window) {
            resources.reset_window();
        }

        let current_messages = resources.message_count.load(Ordering::Relaxed);
        if current_messages >= u64::from(self.limits.max_message_rate_per_client) {
            warn!(
                "Message rejected for {}: rate limit exceeded ({} msg/{}s)",
                client_id,
                current_messages,
                self.limits.rate_limit_window.as_secs()
            );
            return false;
        }

        let current_bytes = resources.bytes_transferred.load(Ordering::Relaxed);
        if current_bytes + message_size as u64 > self.limits.max_bandwidth_per_client {
            warn!(
                "Message rejected for {}: bandwidth limit exceeded ({} bytes/{}s)",
                client_id,
                current_bytes,
                self.limits.rate_limit_window.as_secs()
            );
            return false;
        }

        resources.message_count.fetch_add(1, Ordering::Relaxed);
        resources
            .bytes_transferred
            .fetch_add(message_size as u64, Ordering::Relaxed);

        self.total_messages.fetch_add(1, Ordering::Relaxed);
        self.total_bytes
            .fetch_add(message_size as u64, Ordering::Relaxed);

        true
    }

    /// Get current resource usage statistics
    pub async fn get_stats(&self) -> ResourceStats {
        let ip_connections = self.ip_connections.read().await;
        let client_resources = self.client_resources.read().await;

        let unique_ips = ip_connections.len();
        let active_clients = client_resources.len();

        ResourceStats {
            current_connections: self.connection_count.load(Ordering::Relaxed),
            max_connections: self.limits.max_connections,
            unique_ips,
            active_clients,
            total_messages: self.total_messages.load(Ordering::Relaxed),
            total_bytes: self.total_bytes.load(Ordering::Relaxed),
        }
    }

    /// Get memory usage estimate (simplified)
    pub fn get_memory_usage(&self) -> u64 {
        // Rough estimate based on connection count and data structures
        let connection_count = self.connection_count.load(Ordering::Relaxed) as u64;

        // Estimate memory per connection (very rough)
        let memory_per_connection = 4096; // 4KB per connection estimate

        connection_count * memory_per_connection
    }

    /// Check if memory limit is exceeded
    pub fn is_memory_limit_exceeded(&self) -> bool {
        if self.limits.max_memory_bytes == 0 {
            return false; // Unlimited
        }

        self.get_memory_usage() > self.limits.max_memory_bytes
    }

    /// Cleanup expired rate limit windows (call periodically)
    pub async fn cleanup_expired_windows(&self) {
        let mut client_resources = self.client_resources.write().await;

        for resources in client_resources.values_mut() {
            if resources.is_window_expired(self.limits.rate_limit_window) {
                resources.reset_window();
            }
        }
    }
}

/// Resource usage statistics
#[derive(Debug, Clone)]
pub struct ResourceStats {
    /// Current number of connections
    pub current_connections: usize,
    /// Maximum allowed connections
    pub max_connections: usize,
    /// Number of unique IP addresses
    pub unique_ips: usize,
    /// Number of active clients
    pub active_clients: usize,
    /// Total messages processed
    pub total_messages: u64,
    /// Total bytes processed
    pub total_bytes: u64,
}

impl ResourceStats {
    #[must_use]
    pub fn connection_utilization(&self) -> f64 {
        if self.max_connections == 0 {
            return 0.0;
        }
        let current = usize_to_f64_saturating(self.current_connections);
        let max = usize_to_f64_saturating(self.max_connections);
        (current / max) * 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[tokio::test]
    async fn test_connection_limits() {
        let limits = ResourceLimits {
            max_connections: 2,
            max_connections_per_ip: 1,
            ..Default::default()
        };

        let monitor = ResourceMonitor::new(limits);
        let ip1 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let ip2 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2));

        // First connection should be accepted
        assert!(monitor.can_accept_connection(ip1).await);
        monitor
            .register_connection("client1".to_string(), ip1)
            .await;

        // Second connection from different IP should be accepted
        assert!(monitor.can_accept_connection(ip2).await);
        monitor
            .register_connection("client2".to_string(), ip2)
            .await;

        // Third connection should be rejected (global limit)
        assert!(!monitor.can_accept_connection(ip1).await);

        // Test per-IP limit by unregistering one connection
        monitor.unregister_connection("client1", ip1).await;

        // Now connection from ip1 should be accepted again
        assert!(monitor.can_accept_connection(ip1).await);
        monitor
            .register_connection("client3".to_string(), ip1)
            .await;

        // But second connection from ip1 should be rejected (per-IP limit)
        assert!(!monitor.can_accept_connection(ip1).await);
    }

    #[tokio::test]
    async fn test_message_rate_limiting() {
        let limits = ResourceLimits {
            max_message_rate_per_client: 2,
            rate_limit_window: Duration::from_secs(1),
            ..Default::default()
        };

        let monitor = ResourceMonitor::new(limits);
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);

        monitor.register_connection("client1".to_string(), ip).await;

        // First two messages should be accepted
        assert!(monitor.can_send_message("client1", 100).await);
        assert!(monitor.can_send_message("client1", 100).await);

        // Third message should be rejected
        assert!(!monitor.can_send_message("client1", 100).await);

        // After window cleanup, should be accepted again
        tokio::time::sleep(Duration::from_millis(1100)).await;
        monitor.cleanup_expired_windows().await;
        assert!(monitor.can_send_message("client1", 100).await);
    }

    #[tokio::test]
    async fn test_stats() {
        let monitor = ResourceMonitor::new(ResourceLimits::default());
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);

        monitor.register_connection("client1".to_string(), ip).await;
        monitor.can_send_message("client1", 1000).await;

        let stats = monitor.get_stats().await;
        assert_eq!(stats.current_connections, 1);
        assert_eq!(stats.active_clients, 1);
        assert_eq!(stats.unique_ips, 1);
        assert_eq!(stats.total_messages, 1);
        assert_eq!(stats.total_bytes, 1000);
    }
}
