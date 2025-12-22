use crate::error::{MqttError, Result};
use crate::prelude::{String, Vec, VecDeque};
use crate::session::limits::{ExpiringMessage, LimitsManager};
use crate::time::{Duration, Instant};
use crate::QoS;

#[derive(Debug, Clone)]
pub struct QueueResult {
    pub was_queued: bool,
    pub messages_dropped: usize,
    pub current_size: usize,
    pub message_count: usize,
}

#[derive(Debug, Clone)]
pub struct QueuedMessage {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: QoS,
    pub retain: bool,
    pub packet_id: Option<u16>,
}

impl QueuedMessage {
    #[must_use]
    pub fn to_expiring(self, limits: &LimitsManager) -> ExpiringMessage {
        ExpiringMessage::new(
            self.topic,
            self.payload,
            self.qos,
            self.retain,
            self.packet_id,
            None,
            limits,
        )
    }
}

#[derive(Debug, Clone)]
struct QueuedMessageInternal {
    message: ExpiringMessage,
    queued_at: Instant,
    size: usize,
}

#[derive(Debug)]
pub struct MessageQueue {
    queue: VecDeque<QueuedMessageInternal>,
    max_messages: usize,
    max_size: usize,
    current_size: usize,
}

impl MessageQueue {
    #[must_use]
    pub fn new(max_messages: usize, max_size: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            max_messages,
            max_size,
            current_size: 0,
        }
    }

    /// # Errors
    /// Returns `MessageTooLarge` if the message exceeds the maximum queue size.
    pub fn enqueue(&mut self, message: ExpiringMessage) -> Result<QueueResult> {
        let size = message.topic.len() + message.payload.len();

        if size > self.max_size {
            return Err(MqttError::MessageTooLarge);
        }

        if message.is_expired() {
            return Ok(QueueResult {
                was_queued: false,
                messages_dropped: 0,
                current_size: self.current_size,
                message_count: self.queue.len(),
            });
        }

        let mut messages_dropped = 0;
        while !self.queue.is_empty()
            && (self.queue.len() >= self.max_messages || self.current_size + size > self.max_size)
        {
            if let Some(removed) = self.queue.pop_front() {
                self.current_size -= removed.size;
                messages_dropped += 1;
            }
        }

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
    pub fn dequeue(&mut self) -> Option<ExpiringMessage> {
        while let Some(internal) = self.queue.front() {
            if internal.message.is_expired() {
                if let Some(removed) = self.queue.pop_front() {
                    self.current_size -= removed.size;
                }
            } else {
                break;
            }
        }

        if let Some(internal) = self.queue.pop_front() {
            self.current_size -= internal.size;
            Some(internal.message)
        } else {
            None
        }
    }

    #[must_use]
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
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    #[must_use]
    pub fn size(&self) -> usize {
        self.current_size
    }

    pub fn clear(&mut self) {
        self.queue.clear();
        self.current_size = 0;
    }

    pub fn remove_expired(&mut self, queue_timeout: Duration) {
        let now = Instant::now();
        let current_size = &mut self.current_size;

        self.queue.retain(|internal| {
            let should_keep = !internal.message.is_expired()
                && now.duration_since(internal.queued_at) <= queue_timeout;
            if !should_keep {
                *current_size -= internal.size;
            }
            should_keep
        });
    }

    #[must_use]
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

#[derive(Debug, Clone)]
pub struct QueueStats {
    pub message_count: usize,
    pub total_size: usize,
    pub max_messages: usize,
    pub max_size: usize,
    pub oldest_message_age: Option<Duration>,
    pub newest_message_age: Option<Duration>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::format;
    use crate::session::limits::LimitsConfig;

    fn test_expiring_message(idx: u8) -> ExpiringMessage {
        let limits = LimitsManager::with_defaults();
        ExpiringMessage::new(
            format!("test/{idx}"),
            vec![idx],
            QoS::AtMostOnce,
            false,
            None,
            None,
            &limits,
        )
    }

    #[test]
    fn test_queue_basic_operations() {
        let mut queue = MessageQueue::new(10, 1024);
        let limits = LimitsManager::with_defaults();

        let msg1 = ExpiringMessage::new(
            "test/1".into(),
            vec![1, 2, 3],
            QoS::AtLeastOnce,
            false,
            Some(1),
            None,
            &limits,
        );

        let msg2 = ExpiringMessage::new(
            "test/2".into(),
            vec![4, 5, 6],
            QoS::AtMostOnce,
            false,
            None,
            None,
            &limits,
        );

        queue.enqueue(msg1.clone()).unwrap();
        queue.enqueue(msg2.clone()).unwrap();

        assert_eq!(queue.len(), 2);
        assert_eq!(queue.size(), 18);

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

        assert_eq!(queue.len(), 2);

        let messages = queue.dequeue_batch(10);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].topic, "test/1");
        assert_eq!(messages[1].topic, "test/2");
    }

    #[test]
    fn test_queue_max_size() {
        let mut queue = MessageQueue::new(10, 20);
        let limits = LimitsManager::with_defaults();

        let msg1 = ExpiringMessage::new(
            "test".into(),
            vec![0; 10],
            QoS::AtMostOnce,
            false,
            None,
            None,
            &limits,
        );

        let msg2 = ExpiringMessage::new(
            "test2".into(),
            vec![0; 5],
            QoS::AtMostOnce,
            false,
            None,
            None,
            &limits,
        );

        queue.enqueue(msg1).unwrap();
        queue.enqueue(msg2).unwrap();

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
            "test".into(),
            vec![0; 50],
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

        for i in 0u8..5 {
            let msg = test_expiring_message(i);
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
    fn test_queue_stats() {
        let mut queue = MessageQueue::new(10, 1024);
        let limits = LimitsManager::with_defaults();

        let msg = ExpiringMessage::new(
            "test".into(),
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
            default_message_expiry: Some(Duration::from_millis(50)),
            ..Default::default()
        };
        let limits = LimitsManager::new(config);

        let msg = ExpiringMessage::new(
            "test/expiring".into(),
            vec![1, 2, 3],
            QoS::AtLeastOnce,
            false,
            Some(1),
            Some(0),
            &limits,
        );

        queue.enqueue(msg).unwrap();

        #[cfg(feature = "std")]
        std::thread::sleep(Duration::from_millis(10));

        let dequeued = queue.dequeue();
        assert!(dequeued.is_none());
        assert_eq!(queue.len(), 0);
    }
}
