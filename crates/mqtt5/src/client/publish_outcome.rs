use crate::protocol::v5::reason_codes::ReasonCode;
use crate::QoS;
use std::future::{Future, IntoFuture};
use std::pin::Pin;
use tokio::sync::watch;

/// How a publish reached the server.
///
/// `QoS` 0 has no acknowledgement, so a message that went out at `QoS` 0 (requested, or
/// downgraded to the server's Maximum `QoS` 0) is [`Delivery::Unconfirmed`]: it was
/// written to the connection, but the server never confirms receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Written at `QoS` 0. Receipt is not confirmed by the server.
    Unconfirmed,
    /// Sent at `QoS` 1 and acknowledged with a successful PUBACK.
    AtLeastOnce {
        /// Packet identifier the acknowledged PUBLISH carried.
        packet_id: u16,
    },
    /// Sent at `QoS` 2 and completed with PUBCOMP.
    ExactlyOnce {
        /// Packet identifier the completed PUBLISH carried.
        packet_id: u16,
    },
}

impl Delivery {
    /// The `QoS` the message was actually sent with. It is lower than the requested
    /// `QoS` when the server's Maximum `QoS` forced a downgrade.
    #[must_use]
    pub fn qos_used(self) -> QoS {
        match self {
            Self::Unconfirmed => QoS::AtMostOnce,
            Self::AtLeastOnce { .. } => QoS::AtLeastOnce,
            Self::ExactlyOnce { .. } => QoS::ExactlyOnce,
        }
    }

    /// Packet identifier of an acknowledged delivery.
    #[must_use]
    pub fn packet_id(self) -> Option<u16> {
        match self {
            Self::Unconfirmed => None,
            Self::AtLeastOnce { packet_id } | Self::ExactlyOnce { packet_id } => Some(packet_id),
        }
    }

    pub(crate) fn of(qos: QoS, packet_id: Option<u16>) -> Self {
        match (qos, packet_id) {
            (QoS::AtLeastOnce, Some(packet_id)) => Self::AtLeastOnce { packet_id },
            (QoS::ExactlyOnce, Some(packet_id)) => Self::ExactlyOnce { packet_id },
            _ => Self::Unconfirmed,
        }
    }
}

/// Why a publish was definitely not delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PublishRejection {
    /// The message is retained but the server reported Retain Available 0.
    RetainNotSupported,
    /// The encoded PUBLISH exceeds the server's Maximum Packet Size.
    PacketTooLarge,
    /// The server does not support the requested `QoS`.
    QoSNotSupported,
    /// The server refused the message with this reason code in PUBACK or PUBREC.
    Refused(ReasonCode),
    /// The client could not encode or record the message.
    Unsendable,
}

impl PublishRejection {
    pub(crate) fn from_error(error: &crate::error::MqttError) -> Self {
        match error {
            crate::error::MqttError::RetainNotSupported => Self::RetainNotSupported,
            crate::error::MqttError::PacketTooLarge { .. } => Self::PacketTooLarge,
            crate::error::MqttError::QoSNotSupported => Self::QoSNotSupported,
            crate::error::MqttError::PublishFailed(reason_code) => Self::Refused(*reason_code),
            _ => Self::Unsendable,
        }
    }
}

/// Why the delivery of a publish cannot be determined: it may or may not have reached
/// the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IndeterminateReason {
    /// The client asked to resume its session (Clean Start 0) but the server did not
    /// (Session Present 0) while a `QoS` 2 exchange was unacknowledged. Unacknowledged
    /// `QoS` 1 messages are re-sent instead.
    SessionLost,
    /// The client connected with Clean Start 1 and, as MQTT-3.1.2-4 requires,
    /// discarded its session state while the exchange was unacknowledged.
    SessionDiscarded,
    /// A message that had already been sent no longer conforms to the capabilities of
    /// the new connection (Retain Available, Maximum Packet Size, Maximum `QoS`), so it
    /// was not re-sent.
    ReplayNotConforming,
    /// A message that had already been sent was refused when it was re-sent.
    ResendRefused(ReasonCode),
    /// The connection failed while a message downgraded to `QoS` 0 was being written.
    ConnectionLost,
    /// The client was dropped before the publish settled.
    Abandoned,
}

/// Final outcome of a publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishOutcome {
    /// The message reached the server; see [`Delivery`] for the `QoS` used.
    Delivered(Delivery),
    /// The message was definitely not delivered.
    Rejected(PublishRejection),
    /// The message may have been delivered.
    Indeterminate(IndeterminateReason),
}

/// Pending outcome of a publish that has not settled yet: one accepted into the offline
/// queue, or one sent on a connection that ended (or did not acknowledge it in time)
/// before it was acknowledged.
///
/// Await the handle (it implements [`IntoFuture`]) or call [`PublishHandle::outcome`]
/// to wait for the single [`PublishOutcome`] of that publish. The outcome belongs to the
/// publish itself, never to a packet identifier, so reuse of identifiers cannot settle
/// the wrong publish. Clones observe the same outcome.
///
/// The handle stays pending while the client is disconnected. It settles once the client
/// reconnects and the message is acknowledged, rejected or found to be lost with the
/// session; if the client is dropped first it resolves
/// [`IndeterminateReason::Abandoned`].
#[derive(Debug, Clone)]
pub struct PublishHandle {
    outcome: watch::Receiver<Option<PublishOutcome>>,
}

impl PublishHandle {
    /// A handle that is already settled with `outcome`.
    #[must_use]
    pub fn resolved(outcome: PublishOutcome) -> Self {
        let (_, outcome) = watch::channel(Some(outcome));
        Self { outcome }
    }

    pub(crate) fn pending() -> (watch::Sender<Option<PublishOutcome>>, Self) {
        let (sender, outcome) = watch::channel(None);
        (sender, Self { outcome })
    }

    /// The outcome if the publish has already settled.
    #[must_use]
    pub fn try_outcome(&self) -> Option<PublishOutcome> {
        *self.outcome.borrow()
    }

    /// Waits until the publish settles.
    pub async fn outcome(mut self) -> PublishOutcome {
        match self.outcome.wait_for(Option::is_some).await {
            Ok(settled) => settled.unwrap_or(PublishOutcome::Indeterminate(
                IndeterminateReason::Abandoned,
            )),
            Err(_) => PublishOutcome::Indeterminate(IndeterminateReason::Abandoned),
        }
    }
}

impl IntoFuture for PublishHandle {
    type Output = PublishOutcome;
    type IntoFuture = Pin<Box<dyn Future<Output = PublishOutcome> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.outcome())
    }
}

/// Result of a publish call.
///
/// A publish on a live connection normally settles before the call returns: `QoS` 0
/// once written, `QoS` 1/2 once acknowledged ([`PublishResult::Sent`]).
///
/// [`PublishResult::Queued`] means the publish has not settled yet and its handle
/// yields the eventual outcome. That happens when a `QoS` 1/2 publish is made while
/// disconnected with offline queueing enabled, and also when a `QoS` 1/2 publish was
/// sent but the connection ended, the client disconnected, or no acknowledgement
/// arrived in time: the message stays in flight with the session and is re-sent on the
/// next connection. The handle stays pending until the client reconnects (then it
/// settles) or is dropped (then it resolves [`IndeterminateReason::Abandoned`]).
#[derive(Debug, Clone)]
pub enum PublishResult {
    /// Sent on the live connection.
    Sent(Delivery),
    /// Queued offline, or in flight across a connection loss; the handle settles later.
    Queued(PublishHandle),
}

impl PublishResult {
    /// Packet identifier of a publish acknowledged on the live connection.
    #[must_use]
    pub fn packet_id(&self) -> Option<u16> {
        match self {
            Self::Sent(delivery) => delivery.packet_id(),
            Self::Queued(_) => None,
        }
    }

    /// Waits for the final outcome of either kind of publish.
    pub async fn outcome(self) -> PublishOutcome {
        match self {
            Self::Sent(delivery) => PublishOutcome::Delivered(delivery),
            Self::Queued(handle) => handle.outcome().await,
        }
    }
}
