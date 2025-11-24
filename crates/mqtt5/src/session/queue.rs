use crate::error::{MqttError, Result};
use crate::session::limits::{ExpiringMessage, LimitsManager};
use crate::time::{Duration, Instant};
use crate::QoS;
use std::collections::VecDeque;

/// `Result` of queueing a message
#[derive(Debug, Clone)]
pub struct QueueResult {
    /// Whether the message was successfully queued
    pub was_queued: bool,
    /// Number of messages dropped to make room (if any)
    pub messages_dropped: usize,
    /// Current queue size after the operation
    pub current_size: usize,
    /// Current number of messages in queue
    pub message_count: usize,
}

/// A queued message waiting for delivery
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    /// Topic name
    pub topic: String,
    /// Message payload
    pub payload: Vec<u8>,
    /// Quality of Service level
    pub qos: QoS,
    /// Retain flag
    pub retain: bool,
    /// Packet identifier (for `QoS` > 0)
    pub packet_id: Option<u16>,
}

impl QueuedMessage {
    /// Converts to an expiring message
    #[must_use]
    pub fn to_expiring(self, limits: &LimitsManager) -> ExpiringMessage {
        ExpiringMessage::new(
            self.topic,
            self.payload,
            self.qos,
            self.retain,
            self.packet_id,
            None, // Default expiry
            limits,
        )
    }
}

/// Internal queued message with metadata
#[derive(Debug, Clone)]
struct QueuedMessageInternal {
    /// The message with expiry tracking
    message: ExpiringMessage,
    /// When the message was queued
    queued_at: Instant,
    /// Size in bytes (topic + payload)
    size: usize,
}

/// Message queue for storing messages during disconnection
#[derive(Debug)]
pub struct MessageQueue {
    /// The queue of messages
    queue: VecDeque<QueuedMessageInternal>,
    /// Maximum number of messages
    max_messages: usize,
    /// Maximum total size in bytes
    max_size: usize,
    /// Current total size in bytes
    current_size: usize,
}

impl MessageQueue {
    /// Creates a new message queue
    #[must_use]
    pub fn new(max_messages: usize, max_size: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            max_messages,
            max_size,
            current_size: 0,
        }
    }

    /// Enqueues a message with expiry tracking
    ///
    /// Returns information about the queue operation including whether the message
    /// was queued and how many messages were dropped to make room.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub fn enqueue(&mut self, message: ExpiringMessage) -> Result<QueueResult> {
        let size = message.topic.len() + message.payload.len();

        // Check if message is too large
        if size > self.max_size {
            return Err(MqttError::MessageTooLarge);
        }

        // Don't enqueue already expired messages
        if message.is_expired() {
            return Ok(QueueResult {
                was_queued: false,
                messages_dropped: 0,
                current_size: self.current_size,
                message_count: self.queue.len(),
            });
        }

        // Make room if needed
        let mut messages_dropped = 0;
        while !self.queue.is_empty()
            && (self.queue.len() >= self.max_messages || self.current_size + size > self.max_size)
        {
            if let Some(removed) = self.queue.pop_front() {
                self.current_size -= removed.size;
                messages_dropped += 1;
            }
        }

        // Add the message
        let internal = QueuedMessageInternal {
            message,
            queued_at: Instant::now(),
            size,
        };

        self.queue.push_back(internal);
        self.current_size += size;

        Ok(QueueResult {
            was_queued: true,
            messages_dropped,
            current_size: self.current_size,
            message_count: self.queue.len(),
        })
    }

    #[must_use]
    /// Dequeues a single non-expired message
    pub fn dequeue(&mut self) -> Option<ExpiringMessage> {
        // Remove expired messages from front
        while let Some(internal) = self.queue.front() {
            if internal.message.is_expired() {
                if let Some(removed) = self.queue.pop_front() {
                    self.current_size -= removed.size;
                }
            } else {
                break;
            }
        }

        // Return the next non-expired message
        if let Some(internal) = self.queue.pop_front() {
            self.current_size -= internal.size;
            Some(internal.message)
        } else {
            None
        }
    }

    #[must_use]
    /// Dequeues up to `limit` non-expired messages
    pub fn dequeue_batch(&mut self, limit: usize) -> Vec<ExpiringMessage> {
        let mut messages = Vec::with_capacity(limit.min(self.queue.len()));

        for _ in 0..limit {
            if let Some(message) = self.dequeue() {
                messages.push(message);
            } else {
                break;
            }
        }

        messages
    }

    #[must_use]
    /// Gets the number of queued messages
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    #[must_use]
    /// Checks if the queue is empty
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    #[must_use]
    /// Gets the current size in bytes
    pub fn size(&self) -> usize {
        self.current_size
    }

    /// Clears all messages
    pub fn clear(&mut self) {
        self.queue.clear();
        self.current_size = 0;
    }

    /// Removes expired messages (both by message expiry and queue timeout)
    pub fn remove_expired(&mut self, queue_timeout: Duration) {
        let now = Instant::now();

        // Remove messages that are expired or have been in queue too long
        self.queue.retain(|internal| {
            let should_keep = !internal.message.is_expired()
                && now.duration_since(internal.queued_at) <= queue_timeout;
            if !should_keep {
                self.current_size -= internal.size;
            }
            should_keep
        });
    }

    #[must_use]
    /// Gets statistics about the queue
    pub fn stats(&self) -> QueueStats {
        let oldest_message_age = self.queue.front().map(|m| m.queued_at.elapsed());
        let newest_message_age = self.queue.back().map(|m| m.queued_at.elapsed());

        QueueStats {
            message_count: self.queue.len(),
            total_size: self.current_size,
            max_messages: self.max_messages,
            max_size: self.max_size,
            oldest_message_age,
            newest_message_age,
        }
    }
}

/// Queue statistics
#[derive(Debug, Clone)]
pub struct QueueStats {
    /// Number of messages in queue
    pub message_count: usize,
    /// Total size of messages in bytes
    pub total_size: usize,
    /// Maximum allowed messages
    pub max_messages: usize,
    /// Maximum allowed size
    pub max_size: usize,
    /// Age of oldest message
    pub oldest_message_age: Option<Duration>,
    /// Age of newest message
    pub newest_message_age: Option<Duration>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::limits::LimitsConfig;
    use crate::test_utils::{test_expiring_message, TestMessageBuilder};

    #[test]
    fn test_queue_basic_operations() {
        let mut queue = MessageQueue::new(10, 1024);
        let limits = LimitsManager::with_defaults();

        let msg1 = ExpiringMessage::new(
            "test/1".to_string(),
            vec![1, 2, 3],
            QoS::AtLeastOnce,
            false,
            Some(1),
            None,
            &limits,
        );

        let msg2 = ExpiringMessage::new(
            "test/2".to_string(),
            vec![4, 5, 6],
            QoS::AtMostOnce,
            false,
            None,
            None,
            &limits,
        );

        // Enqueue
        queue.enqueue(msg1.clone()).unwrap();
        queue.enqueue(msg2.clone()).unwrap();

        assert_eq!(queue.len(), 2);
        assert_eq!(queue.size(), 18); // "test/1"(6) + 3 bytes payload + "test/2"(6) + 3 bytes payload = 18

        // Dequeue
        let dequeued = queue.dequeue().unwrap();
        assert_eq!(dequeued.topic, "test/1");
        assert_eq!(queue.len(), 1);

        let dequeued = queue.dequeue().unwrap();
        assert_eq!(dequeued.topic, "test/2");
        assert_eq!(queue.len(), 0);
        assert!(queue.is_empty());
    }

    #[test]
    fn test_queue_max_messages() {
        let mut queue = MessageQueue::new(2, 1024);

        for i in 0u8..3 {
            let msg = test_expiring_message(i);
            let result = queue.enqueue(msg).unwrap();
            assert!(result.was_queued);
        }

        // Should only have 2 messages (oldest was dropped)
        assert_eq!(queue.len(), 2);

        let messages = queue.dequeue_batch(10);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].topic, "test/1");
        assert_eq!(messages[1].topic, "test/2");
    }

    #[test]
    fn test_queue_max_size() {
        let mut queue = MessageQueue::new(10, 20); // 20 bytes max
        let limits = LimitsManager::with_defaults();

        let msg1 = ExpiringMessage::new(
            "test".to_string(), // 4 bytes
            vec![0; 10],        // 10 bytes
            QoS::AtMostOnce,
            false,
            None,
            None,
            &limits,
        );

        let msg2 = ExpiringMessage::new(
            "test2".to_string(), // 5 bytes
            vec![0; 5],          // 5 bytes
            QoS::AtMostOnce,
            false,
            None,
            None,
            &limits,
        );

        queue.enqueue(msg1).unwrap();
        queue.enqueue(msg2).unwrap();

        // First message should have been dropped to make room
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.size(), 10);

        let dequeued = queue.dequeue().unwrap();
        assert_eq!(dequeued.topic, "test2");
    }

    #[test]
    fn test_queue_message_too_large() {
        let mut queue = MessageQueue::new(10, 20);
        let limits = LimitsManager::with_defaults();

        let msg = ExpiringMessage::new(
            "test".to_string(),
            vec![0; 50], // Too large
            QoS::AtMostOnce,
            false,
            None,
            None,
            &limits,
        );

        assert!(queue.enqueue(msg).is_err());
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn test_queue_batch_dequeue() {
        let mut queue = MessageQueue::new(10, 1024);

        // Use TestMessageBuilder to create a batch of messages
        let messages = TestMessageBuilder::new()
            .with_topic_prefix("test")
            .build_expiring_batch(5);

        for msg in messages {
            let result = queue.enqueue(msg).unwrap();
            assert!(result.was_queued);
        }

        let batch = queue.dequeue_batch(3);
        assert_eq!(batch.len(), 3);
        assert_eq!(batch[0].topic, "test/0");
        assert_eq!(batch[1].topic, "test/1");
        assert_eq!(batch[2].topic, "test/2");

        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn test_queue_clear() {
        let mut queue = MessageQueue::new(10, 1024);

        for i in 0u8..3 {
            let msg = test_expiring_message(i);
            let result = queue.enqueue(msg).unwrap();
            assert!(result.was_queued);
        }

        queue.clear();
        assert_eq!(queue.len(), 0);
        assert_eq!(queue.size(), 0);
        assert!(queue.is_empty());
    }

    #[test]
    fn test_queue_expired_messages() {
        let mut queue = MessageQueue::new(10, 1024);
        let limits = LimitsManager::with_defaults();

        // Add messages with artificial timestamps in the past
        let now = Instant::now();
        for i in 0u8..3 {
            let msg = ExpiringMessage::new(
                format!("test/{i}"),
                vec![i],
                QoS::AtMostOnce,
                false,
                None,
                None,
                &limits,
            );
            let result = queue.enqueue(msg).unwrap();
            assert!(result.was_queued);

            // Manually adjust the timestamp of the last enqueued message
            if let Some(last) = queue.queue.back_mut() {
                // Set timestamps: 30ms, 20ms, 10ms ago
                last.queued_at = now
                    .checked_sub(crate::time::Duration::from_millis((3 - u64::from(i)) * 10))
                    .unwrap();
            }
        }

        // Remove messages older than 15ms (should remove first 2)
        queue.remove_expired(crate::time::Duration::from_millis(15));

        assert_eq!(queue.len(), 1);
        let remaining = queue.dequeue().unwrap();
        assert_eq!(remaining.topic, "test/2");
    }

    #[test]
    fn test_queue_stats() {
        let mut queue = MessageQueue::new(10, 1024);
        let limits = LimitsManager::with_defaults();

        let msg = ExpiringMessage::new(
            "test".to_string(),
            vec![1, 2, 3],
            QoS::AtMostOnce,
            false,
            None,
            None,
            &limits,
        );

        queue.enqueue(msg).unwrap();

        let stats = queue.stats();
        assert_eq!(stats.message_count, 1);
        assert_eq!(stats.total_size, 7);
        assert_eq!(stats.max_messages, 10);
        assert_eq!(stats.max_size, 1024);
        assert!(stats.oldest_message_age.is_some());
        assert!(stats.newest_message_age.is_some());
    }

    #[test]
    fn test_queue_with_expiring_messages() {
        let mut queue = MessageQueue::new(10, 1024);
        let config = LimitsConfig {
            default_message_expiry: Some(crate::time::Duration::from_millis(50)),
            ..Default::default()
        };
        let limits = LimitsManager::new(config);

        // Add a message with short expiry
        let msg = ExpiringMessage::new(
            "test/expiring".to_string(),
            vec![1, 2, 3],
            QoS::AtLeastOnce,
            false,
            Some(1),
            Some(0), // Expire immediately
            &limits,
        );

        queue.enqueue(msg).unwrap();

        // Sleep to let message expire
        std::thread::sleep(crate::time::Duration::from_millis(10));

        // Dequeue should skip expired message
        let dequeued = queue.dequeue();
        assert!(dequeued.is_none());
        assert_eq!(queue.len(), 0);
    }
}
