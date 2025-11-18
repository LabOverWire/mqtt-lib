//! Packet ID generation for MQTT

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;

/// Generates unique packet IDs for MQTT messages
#[derive(Debug, Clone)]
pub struct PacketIdGenerator {
    next_id: Arc<AtomicU16>,
}

impl PacketIdGenerator {
    /// Creates a new packet ID generator
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_id: Arc::new(AtomicU16::new(1)),
        }
    }

    #[must_use]
    /// Gets the next available packet ID
    ///
    /// Packet IDs are in the range 1..=65535 (0 is invalid)
    pub fn next(&self) -> u16 {
        loop {
            let current = self.next_id.load(Ordering::SeqCst);
            let next = if current == u16::MAX { 1 } else { current + 1 };

            if self
                .next_id
                .compare_exchange(current, next, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return current;
            }
        }
    }
}

impl Default for PacketIdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packet_id_generation() {
        let gen = PacketIdGenerator::new();

        assert_eq!(gen.next(), 1);
        assert_eq!(gen.next(), 2);
        assert_eq!(gen.next(), 3);
    }

    #[test]
    fn test_packet_id_wraparound() {
        let gen = PacketIdGenerator::new();
        gen.next_id.store(u16::MAX, Ordering::SeqCst);

        assert_eq!(gen.next(), u16::MAX);
        assert_eq!(gen.next(), 1); // Wraps back to 1
        assert_eq!(gen.next(), 2);
    }

    #[test]
    fn test_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        let gen = Arc::new(PacketIdGenerator::new());
        let mut handles = vec![];

        for _ in 0..10 {
            let gen_clone = gen.clone();
            let handle = thread::spawn(move || {
                let mut ids = vec![];
                for _ in 0..100 {
                    ids.push(gen_clone.next());
                }
                ids
            });
            handles.push(handle);
        }

        let mut all_ids = vec![];
        for handle in handles {
            all_ids.extend(handle.join().unwrap());
        }

        // All IDs should be unique
        all_ids.sort_unstable();
        all_ids.dedup();
        assert_eq!(all_ids.len(), 1000);
    }
}
