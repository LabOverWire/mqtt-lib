use super::outbound::OutboundInflight;
use super::{DeliveryCheck, RunState, SharedWriter};
use bytes::BytesMut;
use futures::future::{select, Either};
use mqtt5::broker::router::{RoutableMessage, TakeoverNotice};
use mqtt5::broker::storage::{
    DynamicStorage, InflightDirection, InflightMessage, InflightPhase, QueueHandle, QueuedMessage,
    StorageBackend,
};
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::QoS;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Notify};
use tracing::{debug, error, warn};

const QUEUE_BATCH: usize = 64;
const HANDOFF_BOUND_MS: u32 = 30_000;

enum Wake {
    Lane(Option<RoutableMessage>),
    Queue,
    Acknowledged,
    Stopped,
}

/// Sends a connection what the router hands it, one message at a time, while the client's Receive
/// Maximum leaves room. It starts once a connection it displaced has handed its messages back, and
/// when the connection ends it hands back its own: unacknowledged messages in the order they were
/// sent, then what it had not sent yet, unless it is `QoS` 0.
pub(super) struct Forwarder {
    pub(super) run_state: Rc<RunState>,
    pub(super) writer: SharedWriter,
    pub(super) outbound_inflight: Rc<RefCell<OutboundInflight>>,
    pub(super) next_pid: Rc<Cell<u16>>,
    pub(super) storage: Arc<DynamicStorage>,
    pub(super) client_id: String,
    pub(super) handler_id: u32,
    pub(super) delivery_check: DeliveryCheck,
    pub(super) protocol_version: u8,
    pub(super) queue: QueueHandle,
    pub(super) queue_epoch: u64,
    pub(super) generation: u64,
    pub(super) resumed: bool,
    pub(super) released: Option<oneshot::Receiver<()>>,
    pub(super) notice_rx: oneshot::Receiver<TakeoverNotice>,
    pub(super) done_tx: oneshot::Sender<()>,
    pub(super) qos1_rx: mpsc::Receiver<RoutableMessage>,
    pub(super) qos0_rx: mpsc::Receiver<RoutableMessage>,
    pub(super) receive_maximum: usize,
    pub(super) max_packet_size: Option<usize>,
    pub(super) acknowledged: Rc<Notify>,
    pub(super) reserved_pids: HashSet<u16>,
}

impl Forwarder {
    pub(super) async fn run(mut self) {
        let taken = if self.bind().await {
            self.forward_until_stopped().await
        } else {
            None
        };
        self.hand_back(taken);
        let Self { done_tx, .. } = self;
        if done_tx.send(()).is_err() {
            debug!("Connection ended before its forwarder finished");
        }
    }

    /// Returns false when the connection stopped before it could start delivering.
    /// A delivery stays in storage until the client acknowledges it, so storage and the resends
    /// waiting in the queue name every packet identifier the session still owes the client; those
    /// this connection does not hold are kept free for their resends. Once the hand-off has
    /// drained, a stored delivery that is not waiting in the queue is resent from storage. When it
    /// does not drain in time, what the displaced connection holds is still its own to hand back.
    async fn bind(&mut self) -> bool {
        let run_state = Rc::clone(&self.run_state);
        let drained = match select(
            Box::pin(self.await_handoff()),
            Box::pin(run_state.stopped()),
        )
        .await
        {
            Either::Left((drained, _)) => drained,
            Either::Right(_) => return false,
        };
        if self.resumed {
            let stored = self.stored_outbound().await;
            let queued = self.queued_resend_ids().await;
            if drained {
                self.requeue_stored_resends(&stored, &queued);
            }
            let owed: Vec<u16> = {
                let held = self.outbound_inflight.borrow();
                stored
                    .iter()
                    .map(|msg| msg.packet_id)
                    .chain(queued)
                    .filter(|pid| !held.contains(*pid))
                    .collect()
            };
            self.reserved_pids.extend(owed);
        }
        self.queue.mark_bound(self.generation);
        self.queue.notify();
        true
    }

    /// Returns false when the hand-off did not drain within the bound.
    async fn await_handoff(&mut self) -> bool {
        let released = self.released.take();
        let queue = Arc::clone(&self.queue);
        let drained = Box::pin(async move {
            if let Some(released) = released {
                if released.await.is_err() {
                    debug!("Displaced connection ended without signalling its hand-off");
                }
            }
            while queue.handoffs() != 0 {
                queue.handoff_quiesced().await;
            }
        });
        let deadline = Box::pin(gloo_timers::future::TimeoutFuture::new(HANDOFF_BOUND_MS));
        if let Either::Right(_) = select(drained, deadline).await {
            warn!(client_id = %self.client_id, "Hand-off did not drain in time; proceeding without it");
            return false;
        }
        true
    }

    async fn stored_outbound(&self) -> Vec<InflightMessage> {
        let mut inflights = match self.storage.get_inflight_messages(&self.client_id).await {
            Ok(inflights) => inflights,
            Err(e) => {
                warn!(
                    "failed to load inflight messages for {}: {e}",
                    self.client_id
                );
                return Vec::new();
            }
        };
        inflights.retain(|msg| msg.direction == InflightDirection::Outbound);
        inflights.sort_by_key(|msg| (msg.stored_at_secs, msg.packet_id));
        inflights
    }

    async fn queued_resend_ids(&self) -> HashSet<u16> {
        self.queue
            .peek_all()
            .await
            .into_iter()
            .filter(|message| message.dup)
            .filter_map(|message| message.packet_id)
            .collect()
    }

    fn requeue_stored_resends(&self, stored: &[InflightMessage], queued: &HashSet<u16>) {
        let resends: Vec<QueuedMessage> = stored
            .iter()
            .filter(|msg| {
                msg.phase == InflightPhase::AwaitingPubrec && !queued.contains(&msg.packet_id)
            })
            .map(|msg| self.to_resent(msg.packet_id, msg.to_publish_packet()))
            .collect();
        self.return_to_queue(resends);
    }

    /// Room left under the client's Receive Maximum, with at least one packet identifier free of
    /// unacknowledged deliveries and reserved resends.
    fn window_left(&self) -> usize {
        let unacknowledged = self.outbound_inflight.borrow().len();
        let free_ids =
            usize::from(u16::MAX).saturating_sub(unacknowledged + self.reserved_pids.len());
        self.receive_maximum
            .saturating_sub(unacknowledged)
            .min(free_ids)
    }

    /// Returns a message it took from a lane and did not send because the connection ended.
    async fn forward_until_stopped(&mut self) -> Option<RoutableMessage> {
        loop {
            if !self.run_state.is_running() {
                return None;
            }
            match self.next_wake().await {
                Wake::Lane(None) | Wake::Stopped => return None,
                Wake::Lane(Some(routable)) => {
                    let RoutableMessage {
                        publish,
                        subscription_instance,
                        ..
                    } = &routable;
                    if !self
                        .delivery_check
                        .allows(publish, *subscription_instance)
                        .await
                    {
                        continue;
                    }
                    if !self.run_state.is_running() {
                        return Some(routable);
                    }
                    if !self.forward_one(routable.publish).await {
                        return None;
                    }
                }
                Wake::Queue => {
                    if !self.drain_queue().await {
                        return None;
                    }
                }
                Wake::Acknowledged => {}
            }
        }
    }

    async fn next_wake(&mut self) -> Wake {
        let window_open = self.window_left() > 0;
        let queue = Arc::clone(&self.queue);
        let acknowledged = Rc::clone(&self.acknowledged);
        let run_state = Rc::clone(&self.run_state);
        tokio::select! {
            biased;
            () = run_state.stopped() => Wake::Stopped,
            lane = self.qos1_rx.recv(), if window_open => Wake::Lane(lane),
            lane = self.qos0_rx.recv() => Wake::Lane(lane),
            () = queue.notified(), if window_open => Wake::Queue,
            () = acknowledged.notified(), if !window_open => Wake::Acknowledged,
        }
    }

    /// Returns false when the connection can't be sent any more. A message whose write failed is
    /// already in flight, so it is handed back with the other unacknowledged ones.
    async fn drain_queue(&mut self) -> bool {
        let queue = Arc::clone(&self.queue);
        let limit = QUEUE_BATCH.min(self.window_left());
        let mut pending = queue.take(limit).await.into_iter();
        while let Some(message) = pending.next() {
            let queued = message.to_publish_packet();
            if !self
                .delivery_check
                .allows(&queued, message.subscription_instance)
                .await
            {
                continue;
            }
            let stopped = !self.run_state.is_running();
            if stopped || !self.forward_one(queued).await {
                let rest = stopped
                    .then_some(message)
                    .into_iter()
                    .chain(pending)
                    .collect();
                queue.requeue_front_in_epoch(rest, self.queue_epoch);
                queue.finish_drain();
                return false;
            }
        }
        queue.finish_drain();
        if queue.count() > 0 {
            queue.notify();
        }
        true
    }

    /// A message resent with DUP keeps its packet identifier while no other delivery holds it.
    /// Otherwise it gets the next one after the last that no unacknowledged delivery holds and no
    /// resend is due under; the window leaves one free.
    fn assign_packet_id(&mut self, publish: &PublishPacket) -> u16 {
        let outbound = self.outbound_inflight.borrow();
        if let Some(pid) = publish
            .packet_id
            .filter(|pid| publish.dup && *pid != 0 && !outbound.contains(*pid))
        {
            self.reserved_pids.remove(&pid);
            return pid;
        }
        let following = |pid: u16| if pid == u16::MAX { 1 } else { pid + 1 };
        let mut pid = self.next_pid.get();
        for _ in 0..u16::MAX {
            if !outbound.contains(pid) && !self.reserved_pids.contains(&pid) {
                break;
            }
            pid = following(pid);
        }
        self.next_pid.set(following(pid));
        pid
    }

    /// Returns false when the connection can't be sent any more. A message over the client's
    /// Maximum Packet Size is discarded.
    async fn forward_one(&mut self, mut publish: PublishPacket) -> bool {
        publish.protocol_version = self.protocol_version;
        let packet_id = (publish.qos != QoS::AtMostOnce).then(|| self.assign_packet_id(&publish));
        publish.packet_id = packet_id;
        publish.dup &= packet_id.is_some();

        let mut encoded = BytesMut::new();
        if let Err(e) = publish.encode(&mut encoded) {
            error!("Handler #{} error encoding publish: {e}", self.handler_id);
            return false;
        }
        if self.max_packet_size.is_some_and(|max| encoded.len() > max) {
            debug!(
                packet_size = encoded.len(),
                "Discarding PUBLISH exceeding client Maximum Packet Size"
            );
            return true;
        }

        if let Some(pid) = packet_id {
            self.outbound_inflight
                .borrow_mut()
                .insert(pid, publish.clone());
            let inflight = InflightMessage::from_publish(
                &publish,
                self.client_id.clone(),
                InflightDirection::Outbound,
                InflightPhase::AwaitingPubrec,
            );
            if let Err(e) = self.storage.store_inflight_message(inflight).await {
                debug!("failed to persist outbound inflight {pid}: {e}");
            }
        }

        if let Err(e) = self.writer.write(&encoded) {
            error!("Handler #{} error forwarding publish: {e}", self.handler_id);
            return false;
        }
        true
    }

    /// Puts what the connection still holds for the session at the front of its queue before the
    /// connection gives the session up, so the connection that takes it over, or the next one,
    /// resumes in order. After a clean start took the session over the queue is another epoch, so
    /// nothing goes back. The takeover notice is completed only once this is done.
    fn hand_back(&mut self, taken: Option<RoutableMessage>) {
        self.qos1_rx.close();
        self.qos0_rx.close();
        while self.qos0_rx.try_recv().is_ok() {}
        let unacknowledged = self.outbound_inflight.borrow_mut().take_unreleased();
        let mut unsent: Vec<QueuedMessage> = unacknowledged
            .into_iter()
            .map(|(pid, publish)| self.to_resent(pid, publish))
            .collect();
        unsent.extend(
            taken
                .into_iter()
                .filter(|routable| routable.publish.qos != QoS::AtMostOnce)
                .map(|routable| self.to_queued(routable)),
        );
        while let Ok(routable) = self.qos1_rx.try_recv() {
            unsent.push(self.to_queued(routable));
        }
        self.return_to_queue(unsent);
        if let Ok(notice) = self.notice_rx.try_recv() {
            let TakeoverNotice {
                released, guard, ..
            } = notice;
            drop(guard);
            if released.send(()).is_err() {
                debug!("New connection went away before the hand-off completed");
            }
            self.queue.notify();
        }
    }

    fn return_to_queue(&self, messages: Vec<QueuedMessage>) {
        if messages.is_empty() {
            return;
        }
        if self.queue.epoch() != self.queue_epoch {
            debug!(
                count = messages.len(),
                "Dropping messages handed back for a session that has since been discarded"
            );
            return;
        }
        self.queue
            .requeue_front_in_epoch(messages, self.queue_epoch);
    }

    fn to_resent(&self, packet_id: u16, publish: PublishPacket) -> QueuedMessage {
        let qos = publish.qos;
        let mut queued = QueuedMessage::new(publish, self.client_id.clone(), qos, Some(packet_id));
        queued.dup = true;
        queued
    }

    fn to_queued(&self, routable: RoutableMessage) -> QueuedMessage {
        let qos = routable.publish.qos;
        let mut queued = QueuedMessage::new(routable.publish, self.client_id.clone(), qos, None);
        queued.subscription_instance = routable.subscription_instance;
        queued
    }
}
