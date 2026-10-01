use super::QueuedMessage;
#[cfg(not(target_arch = "wasm32"))]
use super::{InflightDirection, InflightMessage};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Notify;

pub(crate) type QueueKey = (String, u64);
#[cfg(not(target_arch = "wasm32"))]
pub(crate) type InflightKey = (String, u16, InflightDirection);

const WRITE_OUT_THRESHOLD: usize = 8192;

#[derive(Default)]
struct Pending {
    queue: HashMap<QueueKey, Option<Arc<QueuedMessage>>>,
    queue_on_disk: HashSet<QueueKey>,
    #[cfg(not(target_arch = "wasm32"))]
    inflight: HashMap<InflightKey, Option<Box<InflightMessage>>>,
    #[cfg(not(target_arch = "wasm32"))]
    inflight_on_disk: HashSet<InflightKey>,
    #[cfg(not(target_arch = "wasm32"))]
    cleared_inflight: Vec<String>,
}

impl Pending {
    #[cfg(not(target_arch = "wasm32"))]
    fn len(&self) -> usize {
        self.queue.len() + self.inflight.len()
    }

    #[cfg(target_arch = "wasm32")]
    fn len(&self) -> usize {
        self.queue.len()
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
pub(crate) struct WriteBatch {
    pub(crate) cleared_inflight: Vec<String>,
    pub(crate) queue: HashMap<QueueKey, Option<Arc<QueuedMessage>>>,
    pub(crate) inflight: HashMap<InflightKey, Option<Box<InflightMessage>>>,
}

#[derive(Default)]
pub(crate) struct WriteBehind {
    pending: parking_lot::Mutex<Pending>,
    full: Notify,
}

impl std::fmt::Debug for WriteBehind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteBehind").finish_non_exhaustive()
    }
}

impl WriteBehind {
    pub(crate) fn store_queued(&self, client_id: &str, seq: u64, body: Arc<QueuedMessage>) {
        let mut pending = self.pending.lock();
        pending
            .queue
            .insert((client_id.to_string(), seq), Some(body));
        self.signal_if_full(&pending);
    }

    pub(crate) fn remove_queued(&self, client_id: &str, seq: u64) {
        let mut pending = self.pending.lock();
        let key = (client_id.to_string(), seq);
        let unwritten = matches!(pending.queue.get(&key), Some(Some(_)))
            && !pending.queue_on_disk.contains(&key);
        if unwritten {
            pending.queue.remove(&key);
        } else {
            pending.queue.insert(key, None);
            self.signal_if_full(&pending);
        }
    }

    fn signal_if_full(&self, pending: &Pending) {
        if pending.len() >= WRITE_OUT_THRESHOLD {
            self.full.notify_one();
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl WriteBehind {
    pub(crate) fn store_inflight(&self, message: InflightMessage) {
        let mut pending = self.pending.lock();
        let key = (
            message.client_id.clone(),
            message.packet_id,
            message.direction,
        );
        pending.inflight.insert(key, Some(Box::new(message)));
        self.signal_if_full(&pending);
    }

    pub(crate) fn remove_inflight(
        &self,
        client_id: &str,
        packet_id: u16,
        direction: InflightDirection,
    ) {
        let mut pending = self.pending.lock();
        let key = (client_id.to_string(), packet_id, direction);
        let unwritten = matches!(pending.inflight.get(&key), Some(Some(_)))
            && !pending.inflight_on_disk.contains(&key);
        if unwritten {
            pending.inflight.remove(&key);
        } else {
            pending.inflight.insert(key, None);
            self.signal_if_full(&pending);
        }
    }

    pub(crate) fn clear_inflight(&self, client_id: &str) {
        let mut pending = self.pending.lock();
        pending.inflight.retain(|key, _| key.0 != client_id);
        pending.inflight_on_disk.retain(|key| key.0 != client_id);
        pending.cleared_inflight.push(client_id.to_string());
    }

    pub(crate) fn take_batch(&self) -> WriteBatch {
        let mut pending = self.pending.lock();
        let batch = WriteBatch {
            cleared_inflight: std::mem::take(&mut pending.cleared_inflight),
            queue: std::mem::take(&mut pending.queue),
            inflight: std::mem::take(&mut pending.inflight),
        };
        for (key, entry) in &batch.queue {
            if entry.is_some() {
                pending.queue_on_disk.insert(key.clone());
            } else {
                pending.queue_on_disk.remove(key);
            }
        }
        for (key, entry) in &batch.inflight {
            if entry.is_some() {
                pending.inflight_on_disk.insert(key.clone());
            } else {
                pending.inflight_on_disk.remove(key);
            }
        }
        batch
    }

    pub(crate) async fn filled(&self) {
        self.full.notified().await;
    }
}
