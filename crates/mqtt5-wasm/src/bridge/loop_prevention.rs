//! Loop prevention for WASM bridge connections.
//!
//! Prevents message loops in bidirectional bridge configurations by tracking
//! message fingerprints and blocking duplicate messages within a configurable
//! time window.
//!
//! # How It Works
//!
//! When a message is forwarded through a bridge, a SHA-256 fingerprint is
//! calculated from the topic, payload, `QoS`, and retain flag. If the same
//! fingerprint is seen again within the TTL window, the message is blocked.
//!
//! # WASM Considerations
//!
//! This implementation uses `Rc<RefCell>` instead of `Arc<RwLock>` since WASM
//! is single-threaded. Time is obtained via `js_sys::Date::now()` since
//! `std::time::Instant` is not available in WASM.

use mqtt5_protocol::numeric::{f64_to_u64_saturating, u64_to_f64_saturating};
use mqtt5_protocol::QoS;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::HashMap;

/// SHA-256 hash of a message's identifying characteristics.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct MessageFingerprint {
    hash: [u8; 32],
}

/// Loop prevention mechanism for WASM bridge connections.
///
/// Tracks message fingerprints to detect and block duplicate messages that
/// could cause infinite loops in bidirectional bridge configurations.
///
/// # Example
///
/// ```ignore
/// use mqtt5_protocol::QoS;
/// let loop_prevention = WasmLoopPrevention::new(60, 10000);
///
/// // First occurrence - allowed
/// assert!(loop_prevention.check_message("sensor/temp", b"25.5", QoS::AtLeastOnce, false));
///
/// // Duplicate within TTL - blocked
/// assert!(!loop_prevention.check_message("sensor/temp", b"25.5", QoS::AtLeastOnce, false));
/// ```
pub struct WasmLoopPrevention {
    seen_messages: RefCell<HashMap<MessageFingerprint, (f64, bool)>>,
    ttl_ms: f64,
    max_cache_size: usize,
}

impl WasmLoopPrevention {
    /// Creates a new loop prevention instance.
    ///
    /// # Arguments
    ///
    /// * `ttl_secs` - How long to remember message fingerprints (in seconds)
    /// * `max_cache_size` - Maximum number of fingerprints to cache before cleanup
    #[must_use]
    pub fn new(ttl_secs: u64, max_cache_size: usize) -> Self {
        Self {
            seen_messages: RefCell::new(HashMap::new()),
            ttl_ms: u64_to_f64_saturating(ttl_secs.saturating_mul(1000)),
            max_cache_size,
        }
    }

    #[must_use]
    pub fn ttl_secs(&self) -> u64 {
        f64_to_u64_saturating(self.ttl_ms / 1000.0)
    }

    #[must_use]
    pub fn max_cache_size(&self) -> usize {
        self.max_cache_size
    }

    /// Checks if a message should be forwarded.
    ///
    /// Returns `true` if the message should be forwarded (first occurrence),
    /// or `false` if a loop is detected (duplicate within TTL).
    pub fn check_message(&self, topic: &str, payload: &[u8], qos: QoS, retain: bool) -> bool {
        let fingerprint = Self::calculate_fingerprint(topic, payload, qos, retain);
        let now = Self::current_time_ms();
        let mut cache = self.seen_messages.borrow_mut();

        if cache.len() > self.max_cache_size {
            self.cleanup_cache(&mut cache, now);
        }

        if let Some((first_seen, already_warned)) = cache.get_mut(&fingerprint) {
            if now - *first_seen < self.ttl_ms {
                if !*already_warned {
                    tracing::debug!(topic, "loop detected, blocking message");
                    *already_warned = true;
                }
                return false;
            }
        }

        cache.insert(fingerprint, (now, false));
        true
    }

    fn calculate_fingerprint(
        topic: &str,
        payload: &[u8],
        qos: QoS,
        retain: bool,
    ) -> MessageFingerprint {
        let mut hasher = Sha256::new();
        hasher.update(topic.as_bytes());
        hasher.update(payload);
        hasher.update([qos as u8]);
        hasher.update([u8::from(retain)]);
        let result = hasher.finalize();
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&result);
        MessageFingerprint { hash }
    }

    fn cleanup_cache(&self, cache: &mut HashMap<MessageFingerprint, (f64, bool)>, now: f64) {
        let ttl = self.ttl_ms;
        cache.retain(|_, (first_seen, _)| now - *first_seen < ttl);
        tracing::debug!(size = cache.len(), "loop prevention cache cleaned");
    }

    fn current_time_ms() -> f64 {
        js_sys::Date::now()
    }

    /// Clears all cached fingerprints.
    pub fn clear_cache(&self) {
        self.seen_messages.borrow_mut().clear();
    }

    /// Returns the current number of cached fingerprints.
    #[must_use]
    pub fn cache_size(&self) -> usize {
        self.seen_messages.borrow().len()
    }
}

impl Default for WasmLoopPrevention {
    fn default() -> Self {
        Self::new(60, 10000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fingerprint_uniqueness() {
        let fp1 = WasmLoopPrevention::calculate_fingerprint("topic/a", b"payload", 1, false);
        let fp2 = WasmLoopPrevention::calculate_fingerprint("topic/b", b"payload", 1, false);
        let fp3 = WasmLoopPrevention::calculate_fingerprint("topic/a", b"payload", 2, false);
        let fp4 = WasmLoopPrevention::calculate_fingerprint("topic/a", b"payload", 1, true);

        assert_ne!(fp1, fp2);
        assert_ne!(fp1, fp3);
        assert_ne!(fp1, fp4);
    }

    #[test]
    fn test_fingerprint_same_content() {
        let fp1 = WasmLoopPrevention::calculate_fingerprint("topic/a", b"payload", 1, false);
        let fp2 = WasmLoopPrevention::calculate_fingerprint("topic/a", b"payload", 1, false);

        assert_eq!(fp1, fp2);
    }
}
