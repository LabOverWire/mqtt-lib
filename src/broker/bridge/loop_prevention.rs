//! Loop prevention for bridge connections
//!
//! Prevents message loops by tracking message fingerprints and detecting
//! when the same message is being forwarded multiple times.

use crate::packet::publish::PublishPacket;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use crate::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, warn};

/// Message fingerprint for loop detection
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct MessageFingerprint {
    hash: [u8; 32],
}

/// Loop prevention mechanism
#[derive(Clone)]
pub struct LoopPrevention {
    /// Cache of seen messages with their timestamps
    seen_messages: Arc<RwLock<HashMap<MessageFingerprint, Instant>>>,
    /// Time-to-live for message fingerprints
    ttl: Duration,
    /// Maximum cache size before cleanup
    max_cache_size: usize,
}

impl Default for LoopPrevention {
    fn default() -> Self {
        Self::new(Duration::from_secs(60), 10000)
    }
}

impl LoopPrevention {
    /// Creates a new loop prevention instance
    pub fn new(ttl: Duration, max_cache_size: usize) -> Self {
        Self {
            seen_messages: Arc::new(RwLock::new(HashMap::new())),
            ttl,
            max_cache_size,
        }
    }

    /// Checks if a message should be forwarded (returns false if loop detected)
    pub async fn check_message(&self, packet: &PublishPacket) -> bool {
        let fingerprint = self.calculate_fingerprint(packet);
        let mut cache = self.seen_messages.write().await;

        // Clean up old entries if cache is getting large
        if cache.len() > self.max_cache_size {
            self.cleanup_cache(&mut cache);
        }

        // Check if we've seen this message recently
        if let Some(last_seen) = cache.get(&fingerprint) {
            if last_seen.elapsed() < self.ttl {
                warn!(
                    "Message loop detected for topic: {}, elapsed: {:?}",
                    packet.topic_name,
                    last_seen.elapsed()
                );
                return false;
            }
        }

        // Record this message
        cache.insert(fingerprint, Instant::now());
        debug!(
            "Message fingerprint recorded for topic: {}, cache size: {}",
            packet.topic_name,
            cache.len()
        );

        true
    }

    /// Calculates a fingerprint for a message
    fn calculate_fingerprint(&self, packet: &PublishPacket) -> MessageFingerprint {
        let mut hasher = Sha256::new();

        // Include topic name
        hasher.update(packet.topic_name.as_bytes());

        // Include payload
        hasher.update(&packet.payload);

        // Include QoS to differentiate same content at different QoS levels
        hasher.update(&[packet.qos as u8]);

        // Include retain flag
        hasher.update(&[packet.retain as u8]);

        // If packet has properties that affect content, include them
        // For now, we're using basic fields only

        let result = hasher.finalize();
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&result);

        MessageFingerprint { hash }
    }

    /// Removes expired entries from the cache
    fn cleanup_cache(&self, cache: &mut HashMap<MessageFingerprint, Instant>) {
        let now = Instant::now();
        let ttl = self.ttl;

        cache.retain(|_, timestamp| now.duration_since(*timestamp) < ttl);

        debug!("Loop prevention cache cleaned, size: {}", cache.len());
    }

    /// Manually clears the entire cache
    pub async fn clear_cache(&self) {
        self.seen_messages.write().await.clear();
        debug!("Loop prevention cache cleared");
    }

    /// Gets the current cache size
    pub async fn cache_size(&self) -> usize {
        self.seen_messages.read().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QoS;

    #[tokio::test]
    async fn test_loop_detection() {
        let loop_prevention = LoopPrevention::new(Duration::from_secs(5), 100);

        // Create a test message
        let packet =
            PublishPacket::new("test/topic".to_string(), b"test payload", QoS::AtLeastOnce);

        // First time should pass
        assert!(loop_prevention.check_message(&packet).await);

        // Second time should fail (loop detected)
        assert!(!loop_prevention.check_message(&packet).await);

        // Different message should pass
        let packet2 =
            PublishPacket::new("test/topic2".to_string(), b"test payload", QoS::AtLeastOnce);
        assert!(loop_prevention.check_message(&packet2).await);
    }

    #[tokio::test]
    async fn test_ttl_expiration() {
        let loop_prevention = LoopPrevention::new(Duration::from_millis(100), 100);

        let packet = PublishPacket::new("test/topic".to_string(), b"test payload", QoS::AtMostOnce);

        // First time should pass
        assert!(loop_prevention.check_message(&packet).await);

        // Wait for TTL to expire
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Should pass again after TTL
        assert!(loop_prevention.check_message(&packet).await);
    }

    #[tokio::test]
    async fn test_different_qos_different_fingerprint() {
        let loop_prevention = LoopPrevention::new(Duration::from_secs(5), 100);

        // Same content but different QoS
        let packet1 =
            PublishPacket::new("test/topic".to_string(), b"test payload", QoS::AtMostOnce);

        let packet2 =
            PublishPacket::new("test/topic".to_string(), b"test payload", QoS::AtLeastOnce);

        // Both should pass as they have different fingerprints
        assert!(loop_prevention.check_message(&packet1).await);
        assert!(loop_prevention.check_message(&packet2).await);
    }
}
