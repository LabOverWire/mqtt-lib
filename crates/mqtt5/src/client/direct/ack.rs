use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::callback::CallbackId;
use crate::client::direct::unified::UnifiedWriter;
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
}

pub(crate) struct AckRequest {
    packet_id: u16,
    qos: QoS,
    kind: AckKind,
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
    /// A non-error `reason` (Reason Code below `0x80`, e.g. `Success`) is normalized to
    /// [`ReasonCode::UnspecifiedError`], so `reject` can never behave as an [`ack`](Self::ack).
    pub fn reject(mut self, reason: ReasonCode) {
        let reason = if reason.is_error() {
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
    writer_slot: WriterSlot,
    session: Arc<tokio::sync::RwLock<SessionState>>,
    pending_rx: tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<AckRequest>>>,
}

impl AckDispatcher {
    pub(crate) fn new(session: Arc<tokio::sync::RwLock<SessionState>>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel::<AckRequest>();
        Self {
            tx,
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
        tokio::spawn(async move {
            while let Some(request) = rx.recv().await {
                Self::handle(&request, &slot, &session).await;
            }
        });
    }

    /// Mints a token for a delivered inbound message.
    pub(crate) fn token(&self, packet_id: u16, qos: QoS) -> AckToken {
        AckToken {
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

    /// Re-sends an acknowledgement for a duplicate that was already resolved,
    /// without a token (used on a post-reconnect replay).
    pub(crate) fn enqueue(&self, packet_id: u16, qos: QoS, kind: AckKind) {
        let _ = self.tx.send(AckRequest {
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
        request: &AckRequest,
        slot: &WriterSlot,
        session: &Arc<tokio::sync::RwLock<SessionState>>,
    ) {
        let reason = match request.kind {
            AckKind::Ack => ReasonCode::Success,
            AckKind::Reject(r) => r,
            AckKind::DropAuto => DROP_REASON,
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

        let writer = slot.lock().await.clone();
        let written = match &writer {
            Some(handle) => handle.lock().await.write_packet(packet).await.is_ok(),
            None => false,
        };
        if !written {
            debug!(
                packet_id = request.packet_id,
                "Deferred ack not written (disconnected); resolution recorded for replay"
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
                    (item.callback)(item.message, item.token);
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
            self.wildcard.lock().push(entry);
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
        let _ = self.dispatch_sender().send(AckDispatchItem {
            callback,
            message,
            token,
        });
    }
}
