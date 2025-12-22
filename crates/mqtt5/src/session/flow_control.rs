use crate::error::{MqttError, Result};
use crate::time::{Duration, Instant};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{Notify, RwLock, Semaphore};

pub use mqtt5_protocol::session::flow_control::{FlowControlConfig, FlowControlStats};
pub use mqtt5_protocol::session::topic_alias::TopicAliasManager;

#[derive(Debug, Clone)]
pub struct FlowControlManager {
    /// Receive Maximum value (max in-flight `QoS` 1/2 messages we can send)
    receive_maximum: u16,
    /// Currently in-flight outbound messages (`packet_id` -> timestamp)
    in_flight: Arc<RwLock<HashMap<u16, Instant>>>,
    /// Semaphore for flow control quota
    quota_semaphore: Arc<Semaphore>,
    /// Notification for when quota becomes available
    quota_available: Arc<Notify>,
    /// Queue of waiting publish requests
    pending_queue: Arc<RwLock<VecDeque<PendingPublish>>>,
    /// Flow control configuration
    config: FlowControlConfig,
    /// Our receive maximum (max in-flight `QoS` 1/2 messages server can send us)
    inbound_receive_maximum: u16,
    /// Currently in-flight inbound messages from server (`packet_id` -> timestamp)
    inbound_in_flight: Arc<RwLock<HashMap<u16, Instant>>>,
}

/// A pending publish request waiting for quota
#[derive(Debug)]
pub struct PendingPublish {
    /// Packet ID of the pending publish
    pub packet_id: u16,
    /// Timestamp when the request was queued
    pub queued_at: Instant,
    /// Channel to notify when quota becomes available
    pub notify: Arc<Notify>,
}

impl FlowControlManager {
    /// Creates a new flow control manager
    #[must_use]
    pub fn new(receive_maximum: u16) -> Self {
        Self::with_config(receive_maximum, FlowControlConfig::default())
    }

    #[must_use]
    /// Creates a new flow control manager with custom configuration
    pub fn with_config(receive_maximum: u16, config: FlowControlConfig) -> Self {
        let permits = if receive_maximum == 0 {
            tokio::sync::Semaphore::MAX_PERMITS
        } else {
            usize::from(receive_maximum)
        };

        Self {
            receive_maximum,
            in_flight: Arc::new(RwLock::new(HashMap::new())),
            quota_semaphore: Arc::new(Semaphore::new(permits)),
            quota_available: Arc::new(Notify::new()),
            pending_queue: Arc::new(RwLock::new(VecDeque::new())),
            config,
            inbound_receive_maximum: 65535,
            inbound_in_flight: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn set_inbound_receive_maximum(&mut self, value: u16) {
        self.inbound_receive_maximum = value;
    }

    #[must_use]
    pub fn inbound_receive_maximum(&self) -> u16 {
        self.inbound_receive_maximum
    }

    /// # Errors
    /// Returns `ReceiveMaximumExceeded` if the inbound receive maximum is exceeded.
    pub async fn register_inbound_publish(&self, packet_id: u16) -> Result<()> {
        if self.inbound_receive_maximum == 0 {
            return Ok(());
        }

        let mut inbound = self.inbound_in_flight.write().await;
        if inbound.len() >= usize::from(self.inbound_receive_maximum) {
            return Err(MqttError::ReceiveMaximumExceeded);
        }

        inbound.insert(packet_id, Instant::now());
        Ok(())
    }

    pub async fn acknowledge_inbound(&self, packet_id: u16) {
        let mut inbound = self.inbound_in_flight.write().await;
        inbound.remove(&packet_id);
    }

    pub async fn inbound_in_flight_count(&self) -> usize {
        self.inbound_in_flight.read().await.len()
    }

    /// Checks if we can send a new `QoS` 1/2 message
    #[must_use]
    pub fn can_send(&self) -> bool {
        if self.receive_maximum == 0 {
            return true; // 0 means unlimited
        }

        // Check if we have available permits
        self.quota_semaphore.available_permits() > 0
    }

    /// Waits for quota to become available and reserves it for sending
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn acquire_send_quota(&self, packet_id: u16) -> Result<()> {
        if self.receive_maximum == 0 {
            return Ok(()); // Unlimited
        }

        // Try to acquire a permit
        let permit_result = if let Some(timeout) = self.config.backpressure_timeout {
            tokio::time::timeout(timeout, self.quota_semaphore.acquire())
                .await
                .map_err(|_| MqttError::FlowControlExceeded)?
        } else {
            self.quota_semaphore.acquire().await
        };

        let permit = permit_result.map_err(|_| MqttError::FlowControlExceeded)?;

        // Record the in-flight message
        {
            let mut in_flight = self.in_flight.write().await;
            in_flight.insert(packet_id, Instant::now());
        }

        // Forget the permit (keep it acquired)
        permit.forget();

        Ok(())
    }

    /// Tries to acquire quota immediately (non-blocking)
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn try_acquire_send_quota(&self, packet_id: u16) -> Result<()> {
        if self.receive_maximum == 0 {
            return Ok(()); // Unlimited
        }

        // Try to acquire a permit without waiting
        let permit = self
            .quota_semaphore
            .try_acquire()
            .map_err(|_| MqttError::FlowControlExceeded)?;

        // Record the in-flight message
        {
            let mut in_flight = self.in_flight.write().await;
            in_flight.insert(packet_id, Instant::now());
        }

        // Forget the permit (keep it acquired)
        permit.forget();

        Ok(())
    }

    /// Registers a new in-flight message (legacy method)
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn register_send(&self, packet_id: u16) -> Result<()> {
        self.try_acquire_send_quota(packet_id).await
    }

    /// Marks a message as acknowledged and releases quota
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn acknowledge(&self, packet_id: u16) -> Result<()> {
        if self.receive_maximum > 0 {
            let mut in_flight = self.in_flight.write().await;

            if in_flight.remove(&packet_id).is_none() {
                return Err(MqttError::PacketIdNotFound(packet_id));
            }

            // Release the quota by adding a permit back to the semaphore
            self.quota_semaphore.add_permits(1);

            // Notify waiting requests
            self.quota_available.notify_one();
        }

        Ok(())
    }

    /// Gets the current number of in-flight messages
    pub async fn in_flight_count(&self) -> usize {
        self.in_flight.read().await.len()
    }

    #[must_use]
    /// Gets the receive maximum value
    pub fn receive_maximum(&self) -> u16 {
        self.receive_maximum
    }

    /// Updates the receive maximum value and adjusts semaphore permits
    pub async fn set_receive_maximum(&mut self, value: u16) {
        let old_value = self.receive_maximum;
        self.receive_maximum = value;

        // Adjust semaphore permits based on the change
        if value == 0 {
            // Unlimited - give maximum permits
            let current_permits = self.quota_semaphore.available_permits();
            let max_permits = tokio::sync::Semaphore::MAX_PERMITS;
            if current_permits < max_permits {
                self.quota_semaphore
                    .add_permits(max_permits - current_permits);
            }
        } else if old_value == 0 {
            // Was unlimited, now limited
            // Close the semaphore and create new one with proper permits
            let in_flight_count = self.in_flight.read().await.len();
            let available_permits = if usize::from(value) > in_flight_count {
                usize::from(value) - in_flight_count
            } else {
                0
            };
            self.quota_semaphore = Arc::new(Semaphore::new(available_permits));
        } else {
            // Both were limited, adjust the difference
            let current_permits = self.quota_semaphore.available_permits();
            let in_flight_count = self.in_flight.read().await.len();
            let target_permits = if usize::from(value) > in_flight_count {
                usize::from(value) - in_flight_count
            } else {
                0
            };

            match target_permits.cmp(&current_permits) {
                std::cmp::Ordering::Greater => {
                    self.quota_semaphore
                        .add_permits(target_permits - current_permits);
                }
                std::cmp::Ordering::Less => {
                    // Need to reduce permits - acquire the difference and forget them
                    let to_remove = current_permits - target_permits;
                    for _ in 0..to_remove {
                        if let Ok(permit) = self.quota_semaphore.try_acquire() {
                            permit.forget();
                        }
                    }
                }
                std::cmp::Ordering::Equal => {
                    // No change needed
                }
            }
        }

        // Notify waiting requests about quota changes
        self.quota_available.notify_waiters();
    }

    /// Clears all in-flight tracking
    pub async fn clear(&self) {
        self.in_flight.write().await.clear();
        self.inbound_in_flight.write().await.clear();
    }

    /// Gets packet IDs that have been in-flight longer than the specified duration
    pub async fn get_expired(&self, timeout: Duration) -> Vec<u16> {
        let now = Instant::now();
        let in_flight = self.in_flight.read().await;

        in_flight
            .iter()
            .filter(|(_, timestamp)| now.duration_since(**timestamp) > timeout)
            .map(|(packet_id, _)| *packet_id)
            .collect()
    }

    /// Gets flow control statistics
    pub async fn get_stats(&self) -> FlowControlStats {
        let in_flight = self.in_flight.read().await;
        let pending_queue = self.pending_queue.read().await;

        FlowControlStats {
            receive_maximum: self.receive_maximum,
            in_flight_count: in_flight.len(),
            available_quota: self.quota_semaphore.available_permits(),
            pending_requests: pending_queue.len(),
            oldest_in_flight: in_flight.values().min().copied(),
        }
    }

    /// Processes expired in-flight messages and releases their quota
    pub async fn cleanup_expired(&self) -> Vec<u16> {
        let expired = self.get_expired(self.config.in_flight_timeout).await;

        if !expired.is_empty() {
            let mut in_flight = self.in_flight.write().await;
            let mut released_count = 0;

            for packet_id in &expired {
                if in_flight.remove(packet_id).is_some() {
                    released_count += 1;
                }
            }

            // Release quota for expired messages
            if released_count > 0 && self.receive_maximum > 0 {
                self.quota_semaphore.add_permits(released_count);
                self.quota_available.notify_waiters();
            }
        }

        expired
    }

    #[must_use]
    /// Gets the flow control configuration
    pub fn config(&self) -> &FlowControlConfig {
        &self.config
    }

    #[must_use]
    /// Gets available quota permits
    pub fn available_permits(&self) -> usize {
        self.quota_semaphore.available_permits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_flow_control_basic() {
        let fc = FlowControlManager::new(3);

        assert!(fc.can_send());

        // Register some messages
        fc.register_send(1).await.unwrap();
        fc.register_send(2).await.unwrap();
        fc.register_send(3).await.unwrap();

        assert_eq!(fc.in_flight_count().await, 3);
        assert!(!fc.can_send());

        // Try to register another
        assert!(fc.register_send(4).await.is_err());

        // Acknowledge one
        fc.acknowledge(2).await.unwrap();
        assert_eq!(fc.in_flight_count().await, 2);
        assert!(fc.can_send());
    }

    #[tokio::test]
    async fn test_flow_control_unlimited() {
        let fc = FlowControlManager::new(0); // 0 means unlimited

        // Should always be able to send
        assert!(fc.can_send());

        // Registration should be no-op
        fc.register_send(1).await.unwrap();
        fc.register_send(2).await.unwrap();

        assert_eq!(fc.in_flight_count().await, 0); // Not tracked when unlimited
    }

    #[tokio::test]
    async fn test_flow_control_expired() {
        let fc = FlowControlManager::new(5);

        fc.register_send(1).await.unwrap();
        fc.register_send(2).await.unwrap();

        // Sleep a bit
        tokio::time::sleep(crate::time::Duration::from_millis(10)).await;

        fc.register_send(3).await.unwrap();

        // Check expired with very short timeout
        let expired = fc.get_expired(crate::time::Duration::from_millis(5)).await;
        assert_eq!(expired.len(), 2);
        assert!(expired.contains(&1));
        assert!(expired.contains(&2));
        assert!(!expired.contains(&3));
    }

    #[test]
    fn test_topic_alias_basic() {
        let mut ta = TopicAliasManager::new(10);

        // Get or create alias
        let alias1 = ta.get_or_create_alias("topic/1").unwrap();
        assert_eq!(alias1, 1);

        let alias2 = ta.get_or_create_alias("topic/2").unwrap();
        assert_eq!(alias2, 2);

        // Same topic should return same alias
        let alias1_again = ta.get_or_create_alias("topic/1").unwrap();
        assert_eq!(alias1_again, 1);

        // Check lookups
        assert_eq!(ta.get_topic(1), Some("topic/1"));
        assert_eq!(ta.get_alias("topic/1"), Some(1));
    }

    #[test]
    fn test_topic_alias_register() {
        let mut ta = TopicAliasManager::new(5);

        // Register alias from peer
        ta.register_alias(3, "remote/topic").unwrap();
        assert_eq!(ta.get_topic(3), Some("remote/topic"));

        // Invalid alias
        assert!(ta.register_alias(0, "topic").is_err());
        assert!(ta.register_alias(6, "topic").is_err());

        // Overwrite existing alias
        ta.register_alias(3, "new/topic").unwrap();
        assert_eq!(ta.get_topic(3), Some("new/topic"));
        assert!(ta.get_alias("remote/topic").is_none());
    }

    #[test]
    fn test_topic_alias_limit() {
        let mut ta = TopicAliasManager::new(2);

        let alias1 = ta.get_or_create_alias("topic/1");
        let alias2 = ta.get_or_create_alias("topic/2");
        let alias3 = ta.get_or_create_alias("topic/3");

        assert!(alias1.is_some());
        assert!(alias2.is_some());
        assert!(alias3.is_none()); // Limit reached
    }

    #[test]
    fn test_topic_alias_clear() {
        let mut ta = TopicAliasManager::new(10);

        let _ = ta.get_or_create_alias("topic/1");
        let _ = ta.get_or_create_alias("topic/2");
        ta.register_alias(5, "topic/5").unwrap();

        ta.clear();

        assert!(ta.get_topic(1).is_none());
        assert!(ta.get_topic(5).is_none());
        assert!(ta.get_alias("topic/1").is_none());
    }
}
