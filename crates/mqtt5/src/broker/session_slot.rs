use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

type SlotMap = Arc<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>>;

#[derive(Default)]
pub struct SessionSlots {
    slots: SlotMap,
}

impl SessionSlots {
    pub async fn lock(&self, client_id: &str) -> SessionSlotGuard {
        let slot = Arc::clone(
            self.slots
                .lock()
                .entry(client_id.to_string())
                .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
        );
        let guard = slot.lock_owned().await;
        SessionSlotGuard {
            client_id: client_id.to_string(),
            guard: Some(guard),
            slots: Arc::clone(&self.slots),
        }
    }

    pub fn prune(&self) {
        self.slots
            .lock()
            .retain(|_, slot| Arc::strong_count(slot) > 1);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.lock().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.lock().is_empty()
    }
}

pub struct SessionSlotGuard {
    client_id: String,
    guard: Option<OwnedMutexGuard<()>>,
    slots: SlotMap,
}

impl SessionSlotGuard {
    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }
}

impl Drop for SessionSlotGuard {
    fn drop(&mut self) {
        drop(self.guard.take());
        let mut slots = self.slots.lock();
        if slots
            .get(&self.client_id)
            .is_some_and(|slot| Arc::strong_count(slot) == 1)
        {
            slots.remove(&self.client_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SessionSlots;
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn slot_is_removed_once_nobody_holds_or_waits_for_it() {
        let slots = SessionSlots::default();
        let guard = slots.lock("a").await;
        assert_eq!(slots.len(), 1);
        drop(guard);
        assert!(slots.is_empty());
    }

    #[tokio::test]
    async fn waiter_keeps_the_slot_and_is_excluded_until_release() {
        let slots = Arc::new(SessionSlots::default());
        let guard = slots.lock("a").await;
        let waiter_slots = Arc::clone(&slots);
        let waiter = tokio::spawn(async move {
            let guard = waiter_slots.lock("a").await;
            drop(guard);
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "the second holder must wait");
        drop(guard);
        assert_eq!(slots.len(), 1, "the waiter still holds a reference");
        waiter.await.expect("waiter");
        assert!(slots.is_empty());
    }

    #[tokio::test]
    async fn abandoned_waiter_is_pruned() {
        let slots = Arc::new(SessionSlots::default());
        let guard = slots.lock("a").await;
        let waiter_slots = Arc::clone(&slots);
        let waiter = tokio::spawn(async move {
            let guard = waiter_slots.lock("a").await;
            drop(guard);
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        waiter.abort();
        let aborted = waiter.await;
        assert!(aborted.is_err_and(|e| e.is_cancelled()));
        drop(guard);
        slots.prune();
        assert!(slots.is_empty());
    }
}
