use crate::callback::panic_message;
use std::collections::{HashMap, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::callback::CallbackId;
use crate::client::direct::handlers::ack_fits_server_maximum;
use crate::client::direct::unified::UnifiedWriter;
use crate::packet::is_valid_publish_ack_reason_code;
use crate::packet::puback::PubAckPacket;
use crate::packet::publish::PublishPacket;
use crate::packet::pubrec::PubRecPacket;
use crate::packet::Packet;
use crate::protocol::v5::properties::Properties;
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::session::state::AckResolution;
use crate::session::SessionState;
use crate::transport::PacketWriter;
use crate::validation::strip_shared_subscription_prefix;
use crate::QoS;

type WriterHandle = Arc<tokio::sync::Mutex<UnifiedWriter>>;
type WriterSlot = Arc<tokio::sync::Mutex<Option<WriterHandle>>>;

/// The reason code an unresolved token uses when it is dropped without an explicit
/// decision. Non-success so the acknowledgement records that the message was abandoned.
const DROP_REASON: ReasonCode = ReasonCode::UnspecifiedError;

pub(crate) enum AckKind {
    Ack,
    Reject(ReasonCode),
    DropAuto,
    Automatic {
        packet: Packet,
        release_inbound: bool,
    },
}

pub(crate) struct AckRequest {
    seq: Option<u64>,
    packet_id: u16,
    qos: QoS,
    kind: AckKind,
}

#[derive(Default)]
struct AckOrder {
    head: u64,
    slots: VecDeque<Option<AckRequest>>,
}

impl AckOrder {
    fn reserve(&mut self) -> u64 {
        self.slots.push_back(None);
        self.head + self.slots.len() as u64 - 1
    }

    fn discard(&mut self) {
        self.head += self.slots.len() as u64;
        self.slots.clear();
    }

    fn release(&mut self, request: AckRequest) -> Vec<AckRequest> {
        if request.seq.is_some_and(|seq| seq < self.head) {
            return Vec::new();
        }
        let index = request
            .seq
            .and_then(|seq| seq.checked_sub(self.head))
            .and_then(|offset| usize::try_from(offset).ok());
        let Some(slot) = index.and_then(|i| self.slots.get_mut(i)) else {
            return vec![request];
        };
        *slot = Some(request);
        let mut ready = Vec::new();
        while self.slots.front().is_some_and(Option::is_some) {
            if let Some(Some(next)) = self.slots.pop_front() {
                ready.push(next);
            }
            self.head += 1;
        }
        ready
    }
}

/// A capability to acknowledge exactly one inbound `QoS` > 0 message after the
/// application has durably processed it.
///
/// The token owns its message's Receive-Maximum window slot for its lifetime, so
/// holding it applies backpressure. It is move-only: [`AckToken::ack`] and
/// [`AckToken::reject`] consume it, making a double-acknowledgement a compile error.
/// Dropping it without resolving emits a non-success acknowledgement and warns, so a
/// forgotten token never wedges the window (`DeferredAckToken.tla`, obligation 7).
pub struct AckToken {
    seq: Option<u64>,
    packet_id: u16,
    qos: QoS,
    armed: bool,
    sender: mpsc::UnboundedSender<AckRequest>,
}

impl AckToken {
    /// The packet identifier of the message this token acknowledges.
    #[must_use]
    pub fn packet_id(&self) -> u16 {
        self.packet_id
    }

    /// The `QoS` of the message this token acknowledges.
    #[must_use]
    pub fn qos(&self) -> QoS {
        self.qos
    }

    /// Acknowledges the message after durable processing. Consumes the token.
    pub fn ack(mut self) {
        self.emit(AckKind::Ack);
    }

    /// Rejects the message after failing to process it, sending an error
    /// acknowledgement (an error PUBREC for `QoS` 2). Consumes the token.
    ///
    /// Rejecting is **not at-most-once**: per MQTT-5 `[MQTT-4.3.3-9]`, once the receiver
    /// has sent an error acknowledgement it must treat any later PUBLISH with the same
    /// Packet Identifier as a new message. So if the acknowledgement is lost and the
    /// broker replays the message on reconnect, your callback will see it again. Reject
    /// must therefore be idempotent, like the rest of deferred delivery.
    ///
    /// A `reason` that is not an error PUBACK/PUBREC Reason Code (e.g. `Success`, or a
    /// code such as `ServerBusy` that those packets do not allow) is normalized to
    /// [`ReasonCode::UnspecifiedError`], so `reject` can never behave as an [`ack`](Self::ack)
    /// or put a code on the wire that `[MQTT-3.4.2-1]`/`[MQTT-3.5.2-1]` forbid.
    pub fn reject(mut self, reason: ReasonCode) {
        let reason = if reason.is_error() && is_valid_publish_ack_reason_code(reason) {
            reason
        } else {
            ReasonCode::UnspecifiedError
        };
        self.emit(AckKind::Reject(reason));
    }

    fn emit(&mut self, kind: AckKind) {
        if !self.armed {
            return;
        }
        self.armed = false;
        let _ = self.sender.send(AckRequest {
            seq: self.seq,
            packet_id: self.packet_id,
            qos: self.qos,
            kind,
        });
    }
}

impl Drop for AckToken {
    fn drop(&mut self) {
        if self.armed {
            warn!(
                packet_id = self.packet_id,
                qos = ?self.qos,
                "AckToken dropped without ack/reject; auto-acknowledging with a non-success reason"
            );
            self.emit(AckKind::DropAuto);
        }
    }
}

/// Owns the single background task that writes deferred acknowledgements.
///
/// The task is connection-stable: it is spawned on the first connection (never in the
/// constructor, so building a client needs no running runtime) and outlives reconnects,
/// targeting whichever writer is current via a swappable slot. This is what lets a token
/// minted on one connection resolve on the next (the transport reconnect regime of
/// `DeferredAckQoS2Reconnect.tla`). `Drop` is synchronous and cannot await the writer, so
/// tokens only ever enqueue an `AckRequest` here.
pub(crate) struct AckDispatcher {
    tx: mpsc::UnboundedSender<AckRequest>,
    order: Arc<Mutex<AckOrder>>,
    writer_slot: WriterSlot,
    session: Arc<tokio::sync::RwLock<SessionState>>,
    pending_rx: tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<AckRequest>>>,
}

impl AckDispatcher {
    pub(crate) fn new(session: Arc<tokio::sync::RwLock<SessionState>>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel::<AckRequest>();
        Self {
            tx,
            order: Arc::new(Mutex::new(AckOrder::default())),
            writer_slot: Arc::new(tokio::sync::Mutex::new(None)),
            session,
            pending_rx: tokio::sync::Mutex::new(Some(rx)),
        }
    }

    /// Spawns the drain task on the first call, in async context. Subsequent calls are
    /// no-ops, so the task is created once (on the first connection) and outlives reconnects.
    async fn ensure_started(&self) {
        let Some(mut rx) = self.pending_rx.lock().await.take() else {
            return;
        };
        let slot = Arc::clone(&self.writer_slot);
        let session = Arc::clone(&self.session);
        let order = Arc::clone(&self.order);
        tokio::spawn(async move {
            while let Some(request) = rx.recv().await {
                let ready = order.lock().release(request);
                for next in ready {
                    Self::handle(next, &slot, &session).await;
                }
            }
        });
    }

    fn reserve(&self, qos: QoS) -> Option<u64> {
        (qos != QoS::AtMostOnce).then(|| self.order.lock().reserve())
    }

    /// Mints a token for a delivered inbound message.
    pub(crate) fn token(&self, packet_id: u16, qos: QoS) -> AckToken {
        AckToken {
            seq: self.reserve(qos),
            packet_id,
            qos,
            armed: true,
            sender: self.tx.clone(),
        }
    }

    /// Points the drain task at the writer for the current connection, starting the task
    /// on the first call.
    pub(crate) async fn set_writer(&self, writer: WriterHandle) {
        self.ensure_started().await;
        *self.writer_slot.lock().await = Some(writer);
    }

    /// Releases the current connection's writer so its socket can close on disconnect.
    ///
    /// The dispatcher outlives reconnects, but it must NOT keep the writer half alive
    /// across a teardown: a retained clone would hold the socket open and mask an
    /// abnormal disconnect from the broker. Acks enqueued while cleared are recorded
    /// as a resolution and re-sent on the next connection.
    pub(crate) async fn clear_writer(&self) {
        *self.writer_slot.lock().await = None;
    }

    pub(crate) fn discard_pending(&self) {
        self.order.lock().discard();
    }

    /// Re-sends an acknowledgement for a duplicate that was already resolved,
    /// without a token (used on a post-reconnect replay).
    pub(crate) fn enqueue(&self, packet_id: u16, qos: QoS, kind: AckKind) {
        let _ = self.tx.send(AckRequest {
            seq: self.reserve(qos),
            packet_id,
            qos,
            kind,
        });
    }

    /// Applies one acknowledgement: records the session state, then writes the ack packet.
    ///
    /// The session is updated **before** the ack reaches the wire, so a packet id the broker
    /// reuses after receiving this ack cannot race an in-flight write and be seen as a stale
    /// duplicate. The write is best-effort; if the connection is gone the recorded state drives
    /// the correct replay on reconnect. For a `QoS` 2 error acknowledgement the per-id deferred
    /// state is cleared, per `[MQTT-4.3.3-9]` (a later same-id PUBLISH is a new message).
    async fn handle(
        request: AckRequest,
        slot: &WriterSlot,
        session: &Arc<tokio::sync::RwLock<SessionState>>,
    ) {
        let reason = match request.kind {
            AckKind::Ack => ReasonCode::Success,
            AckKind::Reject(r) => r,
            AckKind::DropAuto => DROP_REASON,
            AckKind::Automatic {
                packet,
                release_inbound,
            } => {
                Self::write(request.packet_id, packet, slot, session).await;
                if release_inbound {
                    session
                        .read()
                        .await
                        .acknowledge_inbound(request.packet_id)
                        .await;
                }
                return;
            }
        };
        let packet = match request.qos {
            QoS::AtMostOnce => return,
            QoS::AtLeastOnce => Packet::PubAck(PubAckPacket {
                packet_id: request.packet_id,
                reason_code: reason,
                properties: Properties::default(),
            }),
            QoS::ExactlyOnce => Packet::PubRec(PubRecPacket {
                packet_id: request.packet_id,
                reason_code: reason,
                properties: Properties::default(),
            }),
        };

        let is_success = reason == ReasonCode::Success;
        {
            let session = session.read().await;
            match request.qos {
                QoS::AtMostOnce => {}
                QoS::ExactlyOnce if is_success => {
                    session.mark_pubrec_sent(request.packet_id).await;
                    session
                        .set_resolution(request.packet_id, AckResolution::Acked)
                        .await;
                }
                QoS::AtLeastOnce | QoS::ExactlyOnce => {
                    session.acknowledge_inbound(request.packet_id).await;
                    session.clear_inbound_state(request.packet_id).await;
                }
            }
        }

        Self::write(request.packet_id, packet, slot, session).await;
    }

    async fn write(
        packet_id: u16,
        packet: Packet,
        slot: &WriterSlot,
        session: &Arc<tokio::sync::RwLock<SessionState>>,
    ) {
        if !ack_fits_server_maximum(session, &packet).await {
            return;
        }
        let writer = slot.lock().await.clone();
        let written = match &writer {
            Some(handle) => handle.lock().await.write_packet(packet).await.is_ok(),
            None => false,
        };
        if !written {
            debug!(
                packet_id,
                "Ack not written (disconnected); resolution recorded for replay"
            );
        }
    }
}

/// A publish callback that also receives the message's [`AckToken`].
pub(crate) type AckPublishCallback = Arc<dyn Fn(PublishPacket, AckToken) + Send + Sync>;

struct AckCallbackEntry {
    callback: AckPublishCallback,
    topic_filter: String,
}

struct AckDispatchItem {
    callback: AckPublishCallback,
    message: PublishPacket,
    token: AckToken,
}

/// Registry of `subscribe_with_ack` callbacks.
///
/// Unlike [`crate::callback::CallbackManager`], a match resolves to exactly ONE
/// callback: an [`AckToken`] has a single owner and cannot be cloned or fanned out.
/// Delivery runs on a lazily spawned FIFO worker so the user callback never blocks
/// the reader task (obligation 4).
pub(crate) struct AckCallbackManager {
    exact: Mutex<HashMap<String, AckCallbackEntry>>,
    wildcard: Mutex<Vec<AckCallbackEntry>>,
    next_id: AtomicU64,
    dispatch_tx: OnceLock<mpsc::UnboundedSender<AckDispatchItem>>,
}

impl AckCallbackManager {
    pub(crate) fn new() -> Self {
        Self {
            exact: Mutex::new(HashMap::new()),
            wildcard: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            dispatch_tx: OnceLock::new(),
        }
    }

    fn dispatch_sender(&self) -> &mpsc::UnboundedSender<AckDispatchItem> {
        self.dispatch_tx.get_or_init(|| {
            let (tx, mut rx) = mpsc::unbounded_channel::<AckDispatchItem>();
            tokio::spawn(async move {
                while let Some(item) = rx.recv().await {
                    let topic = item.message.topic_name.clone();
                    let AckDispatchItem {
                        callback,
                        message,
                        token,
                    } = item;
                    if let Err(payload) =
                        catch_unwind(AssertUnwindSafe(|| callback(message, token)))
                    {
                        tracing::error!(
                            topic = %topic,
                            "ack subscription callback panicked: {}",
                            panic_message(&*payload)
                        );
                    }
                }
            });
            tx
        })
    }

    /// Registers an ack callback for a topic filter, returning its id.
    pub(crate) fn register(&self, topic_filter: &str, callback: AckPublishCallback) -> CallbackId {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let entry = AckCallbackEntry {
            callback,
            topic_filter: topic_filter.to_string(),
        };
        let actual = strip_shared_subscription_prefix(topic_filter).to_string();
        if actual.contains('+') || actual.contains('#') {
            let mut wildcard = self.wildcard.lock();
            wildcard.retain(|existing| existing.topic_filter != topic_filter);
            wildcard.push(entry);
        } else {
            self.exact.lock().insert(actual, entry);
        }
        id
    }

    /// Removes the ack callback(s) for a topic filter.
    pub(crate) fn unregister(&self, topic_filter: &str) -> bool {
        let actual = strip_shared_subscription_prefix(topic_filter);
        let removed_exact = self.exact.lock().remove(actual).is_some();
        let mut wildcard = self.wildcard.lock();
        let before = wildcard.len();
        wildcard.retain(|e| e.topic_filter != topic_filter);
        removed_exact || wildcard.len() < before
    }

    /// Finds the single best-matching callback for a topic: an exact match wins,
    /// otherwise the first matching wildcard.
    pub(crate) fn find_one(&self, topic: &str) -> Option<AckPublishCallback> {
        if let Some(entry) = self.exact.lock().get(topic) {
            return Some(Arc::clone(&entry.callback));
        }
        let wildcard = self.wildcard.lock();
        for entry in wildcard.iter() {
            let filter = strip_shared_subscription_prefix(&entry.topic_filter);
            if crate::topic_matching::matches(topic, filter) {
                return Some(Arc::clone(&entry.callback));
            }
        }
        None
    }

    /// Hands a message and its token to a callback on the FIFO worker.
    pub(crate) fn dispatch(
        &self,
        callback: AckPublishCallback,
        message: PublishPacket,
        token: AckToken,
    ) {
        if let Err(dropped) = self.dispatch_sender().send(AckDispatchItem {
            callback,
            message,
            token,
        }) {
            tracing::error!(
                topic = %dropped.0.message.topic_name,
                "ack dispatch worker is gone; message dropped"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AckCallbackManager, AckDispatcher, AckKind, AckOrder, AckPublishCallback, AckRequest,
    };
    use crate::packet::publish::PublishPacket;
    use crate::session::state::{SessionConfig, SessionState};
    use crate::QoS;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    fn request(seq: Option<u64>, packet_id: u16) -> AckRequest {
        AckRequest {
            seq,
            packet_id,
            qos: QoS::AtLeastOnce,
            kind: AckKind::Ack,
        }
    }

    fn released_ids(order: &mut AckOrder, request: AckRequest) -> Vec<u16> {
        order
            .release(request)
            .iter()
            .map(|released| released.packet_id)
            .collect()
    }

    #[test]
    fn ack_order_holds_later_acks_until_earlier_ones_resolve() {
        let mut order = AckOrder::default();
        let first = order.reserve();
        let second = order.reserve();
        let third = order.reserve();

        assert!(released_ids(&mut order, request(Some(third), 3)).is_empty());
        assert!(released_ids(&mut order, request(Some(second), 2)).is_empty());
        assert_eq!(
            released_ids(&mut order, request(Some(first), 1)),
            vec![1, 2, 3]
        );
        assert_eq!(released_ids(&mut order, request(None, 9)), vec![9]);

        let fourth = order.reserve();
        assert_eq!(released_ids(&mut order, request(Some(fourth), 4)), vec![4]);
    }

    #[test]
    fn discarded_acks_and_stale_tokens_are_never_released() {
        let mut order = AckOrder::default();
        let held = order.reserve();
        let queued = order.reserve();
        assert!(released_ids(&mut order, request(Some(queued), 2)).is_empty());
        order.discard();
        assert!(released_ids(&mut order, request(Some(held), 1)).is_empty());
        let fresh = order.reserve();
        assert_eq!(released_ids(&mut order, request(Some(fresh), 3)), vec![3]);
    }

    #[tokio::test]
    async fn panicking_ack_callback_does_not_stop_later_delivery() {
        let mgr = AckCallbackManager::new();
        let delivered = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&delivered);
        let panicking: AckPublishCallback = Arc::new(|_p, _t| panic!("callback bug"));
        let counting: AckPublishCallback = Arc::new(move |_p, _t| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        mgr.register("a/1", panicking);
        mgr.register("a/2", counting);

        let dispatcher = AckDispatcher::new(Arc::new(tokio::sync::RwLock::new(SessionState::new(
            "t".to_string(),
            SessionConfig::default(),
            true,
        ))));

        let first = mgr.find_one("a/1").expect("a/1 registered");
        mgr.dispatch(
            first,
            PublishPacket::new("a/1", b"x".to_vec(), QoS::AtLeastOnce),
            dispatcher.token(1, QoS::AtLeastOnce),
        );
        tokio::task::yield_now().await;
        let second = mgr.find_one("a/2").expect("a/2 registered");
        mgr.dispatch(
            second,
            PublishPacket::new("a/2", b"x".to_vec(), QoS::AtLeastOnce),
            dispatcher.token(2, QoS::AtLeastOnce),
        );

        let mut delivered_ok = false;
        for _ in 0..40 {
            if delivered.load(Ordering::SeqCst) == 1 {
                delivered_ok = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(delivered_ok, "ack delivery stopped after a callback panic");
    }

    #[tokio::test]
    async fn duplicate_wildcard_subscription_dispatches_to_a_single_callback() {
        let mgr = AckCallbackManager::new();
        let hits_first = Arc::new(AtomicU32::new(0));
        let hits_second = Arc::new(AtomicU32::new(0));
        let f = Arc::clone(&hits_first);
        let s = Arc::clone(&hits_second);
        let cb_first: AckPublishCallback = Arc::new(move |_p, _t| {
            f.fetch_add(1, Ordering::SeqCst);
        });
        let cb_second: AckPublishCallback = Arc::new(move |_p, _t| {
            s.fetch_add(1, Ordering::SeqCst);
        });
        mgr.register("jobs/#", cb_first);
        mgr.register("jobs/#", cb_second);

        let dispatcher = AckDispatcher::new(Arc::new(tokio::sync::RwLock::new(SessionState::new(
            "t".to_string(),
            SessionConfig::default(),
            true,
        ))));
        let callback = mgr
            .find_one("jobs/build")
            .expect("a wildcard entry matches jobs/build");
        let token = dispatcher.token(1, QoS::ExactlyOnce);
        callback(
            PublishPacket::new("jobs/build", b"x".to_vec(), QoS::ExactlyOnce),
            token,
        );

        assert_eq!(
            hits_first.load(Ordering::SeqCst) + hits_second.load(Ordering::SeqCst),
            1,
            "a matching publish invokes exactly one ack callback, never both"
        );
        assert_eq!(
            hits_first.load(Ordering::SeqCst),
            0,
            "re-registering a filter replaces the earlier callback, as it does for exact filters"
        );
    }
}
