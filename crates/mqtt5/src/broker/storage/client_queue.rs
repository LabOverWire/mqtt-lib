//! Per-client offline/overflow queue shared by the storage backends.
//!
//! A client is behind exactly when its queue is non-empty; the count is the queue length,
//! written at the end of every mutation under the queue lock, so it can never drift from the
//! entries. The lock never covers file I/O: the file backend persists through a write-behind
//! lane and keeps the message body in memory until the file exists.

use super::{InflightDirection, InflightMessage, QueuedMessage};
use crate::time::SystemTime;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::Notify;
use tracing::{debug, warn};

/// Sender side of the file backend's write-behind lane. Unbounded: every op corresponds to a
/// queue entry or an inflight row, both already bounded per client, and a dropped op would
/// either lose a message or resurrect one on restart.
pub type QueueWriter = mpsc::UnboundedSender<QueueOp>;

/// Sequence numbers start high so a front re-queue always has room below the oldest entry.
pub const SEQ_FLOOR: u64 = 1 << 62;

/// Bounds on one client's queue; the oldest entries are dropped when either is exceeded.
#[derive(Debug, Clone, Copy)]
pub struct QueueLimits {
    pub max_messages: usize,
    pub max_bytes: usize,
}

impl Default for QueueLimits {
    fn default() -> Self {
        Self {
            max_messages: 10_000,
            max_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Work handed to the file backend's write-behind task, in per-client FIFO order.
#[derive(Debug)]
pub enum QueueOp {
    Write {
        client_id: String,
        seq: u64,
        body: Arc<QueuedMessage>,
    },
    Delete {
        client_id: String,
        seq: u64,
    },
    StoreInflight(Box<InflightMessage>),
    RemoveInflight {
        client_id: String,
        packet_id: u16,
        direction: InflightDirection,
    },
    RemoveAllInflight {
        client_id: String,
    },
}

#[derive(Debug, Clone)]
struct QueueEntry {
    seq: u64,
    bytes: usize,
    expires_at: Option<SystemTime>,
    body: Option<Arc<QueuedMessage>>,
    path: Option<PathBuf>,
}

impl QueueEntry {
    fn is_expired(&self) -> bool {
        self.expires_at
            .is_some_and(|expiry| SystemTime::now() > expiry)
    }
}

#[derive(Debug, Default)]
struct QueueInner {
    entries: VecDeque<QueueEntry>,
    bytes: usize,
}

/// Shared handle to one client's queue.
pub type QueueHandle = Arc<ClientQueue>;

/// One client's queue: entries, the count that the router reads without locking, the
/// reconnect hand-off flag, and the Notify that wakes whichever handler serves the client.
#[derive(Debug)]
pub struct ClientQueue {
    client_id: String,
    inner: parking_lot::Mutex<QueueInner>,
    count: AtomicUsize,
    handoffs: AtomicUsize,
    draining: AtomicBool,
    notify: Notify,
    handoff_done: Notify,
    seq: Arc<AtomicU64>,
    limits: QueueLimits,
    writer: Option<QueueWriter>,
}

/// What `push` did with the message.
#[derive(Debug, PartialEq, Eq)]
pub struct PushOutcome {
    pub seq: u64,
    pub dropped_oldest: usize,
}

impl ClientQueue {
    pub(crate) fn new(
        client_id: String,
        seq: Arc<AtomicU64>,
        limits: QueueLimits,
        writer: Option<QueueWriter>,
    ) -> Self {
        Self {
            client_id,
            inner: parking_lot::Mutex::new(QueueInner::default()),
            count: AtomicUsize::new(0),
            handoffs: AtomicUsize::new(0),
            draining: AtomicBool::new(false),
            notify: Notify::new(),
            handoff_done: Notify::new(),
            seq,
            limits,
            writer,
        }
    }

    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Number of queued messages; the router's "behind" check.
    #[must_use]
    pub fn count(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count() == 0
    }

    /// True while at least one displaced handler still has deliveries to hand back.
    #[must_use]
    pub fn handoff(&self) -> bool {
        self.handoffs() > 0
    }

    /// Number of displaced handlers still to hand back their deliveries. A handler compares
    /// this against the value it saw when it bound to tell "someone displaced me" (must stop
    /// and hand off) from "an older handler is still finishing" (must keep delivering).
    #[must_use]
    pub fn handoffs(&self) -> usize {
        self.handoffs.load(Ordering::Acquire)
    }

    pub fn begin_handoff(&self) {
        self.handoffs.fetch_add(1, Ordering::AcqRel);
    }

    pub fn end_handoff(&self) {
        let previous = self
            .handoffs
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                Some(n.saturating_sub(1))
            });
        match previous {
            Ok(1) => self.handoff_done.notify_one(),
            Ok(0) => {
                debug!(client_id = %self.client_id, "Hand-off ended more often than it began");
            }
            _ => {}
        }
    }

    /// Resolves once the hand-off count reaches zero (a permit is stored if the count is
    /// already zero when a later waiter arrives). A successor waits on this before binding so
    /// it never touches the session while an older handler could still re-queue into it.
    pub async fn handoff_quiesced(&self) {
        self.handoff_done.notified().await;
    }

    /// True while a handler holds a batch it took from the queue and has not finished with.
    #[must_use]
    pub fn draining(&self) -> bool {
        self.draining.load(Ordering::Acquire)
    }

    /// Marks the batch taken by `take` as fully sent or returned, so routers may use the
    /// delivery channel again.
    pub fn finish_drain(&self) {
        self.draining.store(false, Ordering::Release);
    }

    /// Behind: the router must queue instead of using the delivery channel, because the
    /// queue holds older messages, a handler still holds a batch taken from it, or a
    /// displaced handler is about to hand its deliveries back.
    #[must_use]
    pub fn behind(&self) -> bool {
        self.count() > 0 || self.draining() || self.handoff()
    }

    /// Wakes the handler that serves this client; a permit is stored if none is waiting.
    pub fn notify(&self) {
        self.notify.notify_one();
    }

    pub async fn notified(&self) {
        self.notify.notified().await;
    }

    /// The next sequence number that will be assigned.
    #[must_use]
    pub fn next_seq(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }

    /// Appends a message. Drops the oldest entries first when a limit is exceeded.
    pub fn push(&self, message: QueuedMessage) -> PushOutcome {
        let body = Arc::new(message);
        let (seq, dropped, evicted) = {
            let mut inner = self.inner.lock();
            let seq = self.seq.fetch_add(1, Ordering::AcqRel);
            let entry = QueueEntry {
                seq,
                bytes: body.payload.len(),
                expires_at: body.expires_at,
                body: Some(Arc::clone(&body)),
                path: None,
            };
            inner.bytes += entry.bytes;
            inner.entries.push_back(entry);
            let evicted = self.enforce_limits(&mut inner);
            self.count.store(inner.entries.len(), Ordering::Release);
            (seq, evicted.len(), evicted)
        };
        let mut self_evicted = false;
        for old in evicted {
            if old.seq == seq {
                self_evicted = true;
            } else {
                self.enqueue_delete(old.seq);
            }
        }
        if !self_evicted {
            self.enqueue_write(seq, body);
        }
        if dropped > 0 {
            warn!(
                client_id = %self.client_id,
                dropped,
                "Dropped oldest queued messages: per-client queue limit reached"
            );
        }
        PushOutcome {
            seq,
            dropped_oldest: dropped,
        }
    }

    /// Removes and returns at most `limit` live messages from the front, in order.
    ///
    /// Expired and unreadable entries are discarded inside the take, so an empty result means
    /// the queue was empty at that instant.
    pub async fn take(&self, limit: usize) -> Vec<QueuedMessage> {
        let taken: Vec<QueueEntry> = {
            let mut inner = self.inner.lock();
            let mut taken = Vec::new();
            while taken.len() < limit {
                let Some(entry) = inner.entries.pop_front() else {
                    break;
                };
                inner.bytes -= entry.bytes;
                if entry.is_expired() {
                    self.enqueue_delete(entry.seq);
                    continue;
                }
                taken.push(entry);
            }
            self.count.store(inner.entries.len(), Ordering::Release);
            if !taken.is_empty() {
                self.draining.store(true, Ordering::Release);
            }
            taken
        };
        let mut messages = Vec::with_capacity(taken.len());
        for entry in taken {
            let message = match entry.body {
                Some(body) => Some(Arc::unwrap_or_clone(body)),
                None => match entry.path {
                    Some(path) => read_scanned(path).await,
                    None => None,
                },
            };
            self.enqueue_delete(entry.seq);
            if let Some(mut message) = message {
                message.recompute_expiry();
                if !message.is_expired() {
                    messages.push(message);
                }
            }
        }
        messages
    }

    /// Puts messages back at the front, in order, with sequence numbers below the current head.
    pub fn requeue_front(&self, messages: Vec<QueuedMessage>) {
        if messages.is_empty() {
            return;
        }
        let bodies: Vec<Arc<QueuedMessage>> = messages.into_iter().map(Arc::new).collect();
        let entries: Vec<(QueueEntry, Arc<QueuedMessage>)> = {
            let mut inner = self.inner.lock();
            let base = inner
                .entries
                .front()
                .map_or_else(|| self.seq.load(Ordering::Acquire), |front| front.seq);
            let len = bodies.len() as u64;
            let first = base.checked_sub(len).unwrap_or_else(|| {
                warn!(
                    client_id = %self.client_id,
                    base,
                    len,
                    "Sequence space exhausted below the queue head; re-queued order is best effort"
                );
                0
            });
            let mut entries = Vec::with_capacity(bodies.len());
            for (index, body) in bodies.iter().enumerate() {
                entries.push(QueueEntry {
                    seq: first + index as u64,
                    bytes: body.payload.len(),
                    expires_at: body.expires_at,
                    body: Some(Arc::clone(body)),
                    path: None,
                });
            }
            for entry in entries.iter().rev() {
                inner.bytes += entry.bytes;
                inner.entries.push_front(entry.clone());
            }
            let evicted = self.enforce_limits(&mut inner);
            self.count.store(inner.entries.len(), Ordering::Release);
            let evicted_seqs: Vec<u64> = evicted.iter().map(|old| old.seq).collect();
            let batch_seqs: Vec<u64> = entries.iter().map(|entry| entry.seq).collect();
            for old in evicted_seqs.iter().filter(|seq| !batch_seqs.contains(seq)) {
                self.enqueue_delete(*old);
            }
            entries
                .into_iter()
                .zip(bodies)
                .filter(|(entry, _)| !evicted_seqs.contains(&entry.seq))
                .collect::<Vec<_>>()
        };
        for (entry, body) in entries {
            self.enqueue_write(entry.seq, body);
        }
    }

    /// Removes every entry, or only those with a sequence number below `cutoff`.
    pub fn clear(&self, cutoff: Option<u64>) -> usize {
        let removed: Vec<u64> = {
            let mut inner = self.inner.lock();
            let mut removed = Vec::new();
            inner.entries.retain(|entry| {
                let drop_it = cutoff.is_none_or(|cutoff| entry.seq < cutoff);
                if drop_it {
                    removed.push(entry.seq);
                }
                !drop_it
            });
            inner.bytes = inner.entries.iter().map(|entry| entry.bytes).sum();
            self.count.store(inner.entries.len(), Ordering::Release);
            removed
        };
        for seq in &removed {
            self.enqueue_delete(*seq);
        }
        removed.len()
    }

    /// Drops expired entries whose expiry is known in memory. Returns how many were removed.
    pub fn purge_expired(&self) -> usize {
        let removed: Vec<u64> = {
            let mut inner = self.inner.lock();
            let mut removed = Vec::new();
            inner.entries.retain(|entry| {
                if entry.is_expired() {
                    removed.push(entry.seq);
                    false
                } else {
                    true
                }
            });
            inner.bytes = inner.entries.iter().map(|entry| entry.bytes).sum();
            self.count.store(inner.entries.len(), Ordering::Release);
            removed
        };
        for seq in &removed {
            self.enqueue_delete(*seq);
        }
        removed.len()
    }

    /// Non-destructive snapshot of the live messages, in order.
    pub async fn peek_all(&self) -> Vec<QueuedMessage> {
        let entries: Vec<QueueEntry> = {
            let inner = self.inner.lock();
            inner
                .entries
                .iter()
                .filter(|entry| !entry.is_expired())
                .cloned()
                .collect()
        };
        let mut messages = Vec::with_capacity(entries.len());
        for entry in entries {
            let message = match entry.body {
                Some(body) => Some((*body).clone()),
                None => match entry.path {
                    Some(path) => read_scanned(path).await,
                    None => None,
                },
            };
            if let Some(mut message) = message {
                message.recompute_expiry();
                if !message.is_expired() {
                    messages.push(message);
                }
            }
        }
        messages
    }

    /// Adds an entry discovered on disk at startup; its body is read on take, but its byte
    /// size (payload length, matching `push`) and expiry are recorded now. Only the file
    /// backend scans a directory, so this is unused on wasm.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn push_scanned(
        &self,
        seq: u64,
        path: PathBuf,
        bytes: usize,
        expires_at: Option<SystemTime>,
    ) {
        let mut inner = self.inner.lock();
        inner.bytes += bytes;
        inner.entries.push_back(QueueEntry {
            seq,
            bytes,
            expires_at,
            body: None,
            path: Some(path),
        });
        self.count.store(inner.entries.len(), Ordering::Release);
    }

    /// True when the queue is empty and only the backend map still refers to it.
    pub(crate) fn is_evictable(self: &Arc<Self>) -> bool {
        Arc::strong_count(self) == 1 && self.is_empty()
    }

    fn enforce_limits(&self, inner: &mut QueueInner) -> Vec<QueueEntry> {
        let mut evicted = Vec::new();
        while inner.entries.len() > self.limits.max_messages
            || (inner.bytes > self.limits.max_bytes && inner.entries.len() > 1)
        {
            let Some(oldest) = inner.entries.pop_front() else {
                break;
            };
            inner.bytes -= oldest.bytes;
            evicted.push(oldest);
        }
        evicted
    }

    fn enqueue_write(&self, seq: u64, body: Arc<QueuedMessage>) {
        let Some(writer) = &self.writer else {
            return;
        };
        if writer
            .send(QueueOp::Write {
                client_id: self.client_id.clone(),
                seq,
                body,
            })
            .is_err()
        {
            debug!(client_id = %self.client_id, seq, "Storage writer stopped; queued message kept in memory only");
        }
    }

    fn enqueue_delete(&self, seq: u64) {
        let Some(writer) = &self.writer else {
            return;
        };
        if writer
            .send(QueueOp::Delete {
                client_id: self.client_id.clone(),
                seq,
            })
            .is_err()
        {
            debug!(client_id = %self.client_id, seq, "Storage writer stopped; queued message file may remain");
        }
    }
}

/// Reads a queued message whose body was left on disk by the startup scan. Only the file
/// backend ever populates a scanned entry, so on wasm (no file backend) there is nothing to
/// read and the stub simply reports the entry as gone.
#[cfg(not(target_arch = "wasm32"))]
async fn read_scanned(path: PathBuf) -> Option<QueuedMessage> {
    match tokio::fs::read(&path).await {
        Ok(data) => match serde_json::from_slice::<QueuedMessage>(&data) {
            Ok(message) => Some(message),
            Err(e) => {
                warn!(
                    "Skipping unreadable queued message {} ({e}); it will be deleted",
                    path.display()
                );
                None
            }
        },
        Err(e) => {
            debug!(
                "Queued message file {} is gone before it was taken: {e}",
                path.display()
            );
            None
        }
    }
}

#[cfg(target_arch = "wasm32")]
async fn read_scanned(_path: PathBuf) -> Option<QueuedMessage> {
    std::future::ready(None).await
}

/// Registry of per-client queues owned by a backend.
#[derive(Debug)]
pub struct QueueRegistry {
    queues: parking_lot::RwLock<std::collections::HashMap<String, QueueHandle>>,
    seq: Arc<AtomicU64>,
    limits: QueueLimits,
    writer: Option<QueueWriter>,
}

impl QueueRegistry {
    pub(crate) fn new(limits: QueueLimits, writer: Option<QueueWriter>) -> Self {
        Self {
            queues: parking_lot::RwLock::new(std::collections::HashMap::new()),
            seq: Arc::new(AtomicU64::new(SEQ_FLOOR)),
            limits,
            writer,
        }
    }

    /// Returns the client's queue, creating it on first touch. Entries are never unlinked
    /// while a handle is held elsewhere.
    pub fn handle(&self, client_id: &str) -> QueueHandle {
        if let Some(handle) = self.queues.read().get(client_id) {
            return Arc::clone(handle);
        }
        let mut queues = self.queues.write();
        Arc::clone(queues.entry(client_id.to_string()).or_insert_with(|| {
            Arc::new(ClientQueue::new(
                client_id.to_string(),
                Arc::clone(&self.seq),
                self.limits,
                self.writer.clone(),
            ))
        }))
    }

    /// Every queue currently registered.
    pub fn handles(&self) -> Vec<QueueHandle> {
        self.queues.read().values().map(Arc::clone).collect()
    }

    /// Raises the sequence counter above everything found on disk. Only the file backend
    /// scans a directory, so this is unused on wasm.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn seed_seq(&self, max_seen: u64) {
        self.seq
            .fetch_max(max_seen.saturating_add(1).max(SEQ_FLOOR), Ordering::AcqRel);
    }

    /// Drops registry entries that are empty and referenced by nobody else.
    pub fn evict_idle(&self) -> usize {
        let mut queues = self.queues.write();
        let before = queues.len();
        queues.retain(|_, handle| !handle.is_evictable());
        before - queues.len()
    }

    #[must_use]
    pub fn limits(&self) -> QueueLimits {
        self.limits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::publish::PublishPacket;
    use crate::QoS;

    fn message(client: &str, tag: &str) -> QueuedMessage {
        QueuedMessage::new(
            PublishPacket::new(
                format!("t/{tag}"),
                tag.as_bytes().to_vec(),
                QoS::AtLeastOnce,
            ),
            client.to_string(),
            QoS::AtLeastOnce,
            None,
        )
    }

    fn registry(limits: QueueLimits) -> QueueRegistry {
        QueueRegistry::new(limits, None)
    }

    #[tokio::test]
    async fn take_returns_front_first_and_keeps_count_exact() {
        let registry = registry(QueueLimits::default());
        let queue = registry.handle("c");
        for tag in ["a", "b", "c"] {
            queue.push(message("c", tag));
        }
        assert_eq!(queue.count(), 3);
        assert!(queue.behind());

        let first = queue.take(2).await;
        assert_eq!(
            first.iter().map(|m| m.topic.as_str()).collect::<Vec<_>>(),
            ["t/a", "t/b"]
        );
        assert_eq!(queue.count(), 1);

        let rest = queue.take(10).await;
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].topic, "t/c");
        assert_eq!(queue.count(), 0);
        assert!(
            queue.behind(),
            "a batch taken but not yet sent keeps the client behind"
        );
        queue.finish_drain();
        assert!(!queue.behind());
        assert!(queue.take(1).await.is_empty());
        assert!(!queue.behind(), "an empty take does not start a drain");
    }

    #[tokio::test]
    async fn requeue_front_goes_ahead_of_existing_entries_in_order() {
        let registry = registry(QueueLimits::default());
        let queue = registry.handle("c");
        queue.push(message("c", "later"));
        queue.requeue_front(vec![message("c", "old1"), message("c", "old2")]);
        assert_eq!(queue.count(), 3);
        let taken = queue.take(3).await;
        assert_eq!(
            taken.iter().map(|m| m.topic.as_str()).collect::<Vec<_>>(),
            ["t/old1", "t/old2", "t/later"]
        );
    }

    #[tokio::test]
    async fn clear_with_cutoff_keeps_messages_queued_after_the_cutoff() {
        let registry = registry(QueueLimits::default());
        let queue = registry.handle("c");
        queue.push(message("c", "before1"));
        queue.push(message("c", "before2"));
        let cutoff = queue.next_seq();
        queue.push(message("c", "after"));
        assert_eq!(queue.clear(Some(cutoff)), 2);
        assert_eq!(queue.count(), 1);
        assert_eq!(queue.take(5).await[0].topic, "t/after");
    }

    #[tokio::test]
    async fn push_drops_oldest_when_the_message_limit_is_exceeded() {
        let registry = registry(QueueLimits {
            max_messages: 2,
            max_bytes: usize::MAX,
        });
        let queue = registry.handle("c");
        queue.push(message("c", "one"));
        queue.push(message("c", "two"));
        let outcome = queue.push(message("c", "three"));
        assert_eq!(outcome.dropped_oldest, 1);
        assert_eq!(queue.count(), 2);
        let taken = queue.take(5).await;
        assert_eq!(
            taken.iter().map(|m| m.topic.as_str()).collect::<Vec<_>>(),
            ["t/two", "t/three"]
        );
    }

    #[tokio::test]
    async fn purge_expired_removes_only_expired_entries() {
        let registry = registry(QueueLimits::default());
        let queue = registry.handle("c");
        let mut expired = message("c", "expired");
        expired.expires_at = Some(SystemTime::UNIX_EPOCH);
        queue.push(expired);
        queue.push(message("c", "live"));
        assert_eq!(queue.purge_expired(), 1);
        assert_eq!(queue.count(), 1);
        assert_eq!(queue.take(5).await[0].topic, "t/live");
    }

    #[tokio::test]
    async fn take_skips_expired_entries_and_reports_them_as_absent() {
        let registry = registry(QueueLimits::default());
        let queue = registry.handle("c");
        let mut expired = message("c", "expired");
        expired.expires_at = Some(SystemTime::UNIX_EPOCH);
        queue.push(expired);
        assert_eq!(queue.count(), 1);
        assert!(queue.take(1).await.is_empty());
        assert_eq!(queue.count(), 0);
    }

    #[test]
    fn registry_pins_entries_while_a_handle_is_held() {
        let registry = registry(QueueLimits::default());
        let held = registry.handle("c");
        assert_eq!(registry.evict_idle(), 0);
        let again = registry.handle("c");
        assert!(Arc::ptr_eq(&held, &again));
        drop(again);
        drop(held);
        assert_eq!(registry.evict_idle(), 1);
    }

    #[test]
    fn notify_stores_a_permit_for_a_later_waiter() {
        let registry = registry(QueueLimits::default());
        let queue = registry.handle("c");
        queue.notify();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_millis(100), queue.notified())
                .await
                .expect("stored permit must wake the first waiter");
        });
    }
}
