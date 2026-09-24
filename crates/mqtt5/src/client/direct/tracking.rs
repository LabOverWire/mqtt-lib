use crate::client::publish_outcome::{
    Delivery, IndeterminateReason, PublishHandle, PublishOutcome, PublishRejection,
};
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::QoS;
use parking_lot::Mutex;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::watch;

const PACKET_ID_WORDS: usize = (u16::MAX as usize + 1) / 64;

#[derive(Debug)]
pub(crate) struct PacketIdSet {
    words: [u64; PACKET_ID_WORDS],
}

impl Default for PacketIdSet {
    fn default() -> Self {
        Self {
            words: [0; PACKET_ID_WORDS],
        }
    }
}

impl PacketIdSet {
    fn bit(packet_id: u16) -> (usize, u64) {
        (usize::from(packet_id / 64), 1 << (packet_id % 64))
    }

    fn insert(&mut self, packet_id: u16) {
        let (word, bit) = Self::bit(packet_id);
        self.words[word] |= bit;
    }

    fn remove(&mut self, packet_id: u16) {
        let (word, bit) = Self::bit(packet_id);
        self.words[word] &= !bit;
    }

    fn contains(&self, packet_id: u16) -> bool {
        let (word, bit) = Self::bit(packet_id);
        self.words[word] & bit != 0
    }

    fn clear(&mut self) {
        self.words = [0; PACKET_ID_WORDS];
    }
}

#[derive(Debug, Default)]
pub(crate) struct OutboundIds {
    reserved: PacketIdSet,
    quarantined: PacketIdSet,
}

impl OutboundIds {
    pub(crate) fn holds(&self, packet_id: u16) -> bool {
        self.reserved.contains(packet_id) || self.quarantined.contains(packet_id)
    }

    pub(crate) fn quarantine(&mut self, packet_id: u16) {
        self.quarantined.insert(packet_id);
    }

    pub(crate) fn release_quarantine(&mut self) {
        self.quarantined.clear();
    }
}

pub(crate) type SharedIds = Arc<Mutex<OutboundIds>>;

#[derive(Debug)]
pub(crate) struct IdReservation {
    packet_id: u16,
    ids: SharedIds,
}

impl IdReservation {
    pub(crate) fn claim(ids: &SharedIds, packet_id: u16) -> Option<Self> {
        let mut held = ids.lock();
        if held.holds(packet_id) {
            return None;
        }
        held.reserved.insert(packet_id);
        Some(Self {
            packet_id,
            ids: Arc::clone(ids),
        })
    }

    pub(crate) fn packet_id(&self) -> u16 {
        self.packet_id
    }
}

impl Drop for IdReservation {
    fn drop(&mut self) {
        self.ids.lock().reserved.remove(self.packet_id);
    }
}

#[derive(Debug)]
pub(crate) struct Completion {
    outcome: watch::Sender<Option<PublishOutcome>>,
    resent: bool,
}

impl Completion {
    pub(crate) fn new() -> (Self, PublishHandle) {
        let (outcome, handle) = PublishHandle::pending();
        (
            Self {
                outcome,
                resent: false,
            },
            handle,
        )
    }

    pub(crate) fn mark_resent(&mut self) {
        self.resent = true;
    }

    pub(crate) fn settle(self, outcome: PublishOutcome) {
        tracing::debug!(?outcome, "publish settled");
        self.outcome.send_replace(Some(outcome));
    }

    pub(crate) fn delivered(self, delivery: Delivery) {
        self.settle(PublishOutcome::Delivered(delivery));
    }

    pub(crate) fn rejected(self, rejection: PublishRejection) {
        let outcome = match (self.resent, rejection) {
            (false, rejection) => PublishOutcome::Rejected(rejection),
            (true, PublishRejection::Refused(reason_code)) => {
                PublishOutcome::Indeterminate(IndeterminateReason::ResendRefused(reason_code))
            }
            (true, _) => PublishOutcome::Indeterminate(IndeterminateReason::ReplayNotConforming),
        };
        self.settle(outcome);
    }

    pub(crate) fn indeterminate(self, reason: IndeterminateReason) {
        self.settle(PublishOutcome::Indeterminate(reason));
    }
}

#[derive(Debug)]
struct Tracked {
    qos: QoS,
    completion: Completion,
}

#[derive(Debug, Default)]
pub(crate) struct OutcomeTracker {
    in_flight: HashMap<u16, Tracked>,
}

pub(crate) type SharedOutcomes = Arc<Mutex<OutcomeTracker>>;

impl OutcomeTracker {
    pub(crate) fn track(&mut self, packet_id: u16, qos: QoS, completion: Completion) {
        match self.in_flight.entry(packet_id) {
            Entry::Vacant(vacant) => {
                vacant.insert(Tracked { qos, completion });
            }
            Entry::Occupied(_) => {
                tracing::error!(
                    packet_id,
                    "packet identifier already tracks an unsettled publish; keeping the existing one"
                );
                completion.indeterminate(IndeterminateReason::Abandoned);
            }
        }
    }

    pub(crate) fn take(&mut self, packet_id: u16) -> Option<Completion> {
        self.in_flight
            .remove(&packet_id)
            .map(|tracked| tracked.completion)
    }

    pub(crate) fn mark_resent(&mut self, packet_id: u16) {
        if let Some(tracked) = self.in_flight.get_mut(&packet_id) {
            tracked.completion.mark_resent();
        }
    }

    fn take_matching(&mut self, packet_id: u16, qos: QoS) -> Option<Completion> {
        if self
            .in_flight
            .get(&packet_id)
            .is_some_and(|tracked| tracked.qos == qos)
        {
            self.take(packet_id)
        } else {
            None
        }
    }

    pub(crate) fn acknowledged(&mut self, packet_id: u16, qos: QoS) {
        if let Some(completion) = self.take_matching(packet_id, qos) {
            completion.delivered(Delivery::of(qos, Some(packet_id)));
        }
    }

    pub(crate) fn refused(&mut self, packet_id: u16, qos: QoS, reason_code: ReasonCode) {
        if let Some(completion) = self.take_matching(packet_id, qos) {
            completion.rejected(PublishRejection::Refused(reason_code));
        }
    }

    pub(crate) fn settle_puback(&mut self, packet_id: u16, reason_code: ReasonCode) {
        if reason_code.is_error() {
            self.refused(packet_id, QoS::AtLeastOnce, reason_code);
        } else {
            self.acknowledged(packet_id, QoS::AtLeastOnce);
        }
    }

    pub(crate) fn abandon_all(&mut self, reason: IndeterminateReason) {
        for (_, tracked) in self.in_flight.drain() {
            tracked.completion.indeterminate(reason);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservation_is_released_on_drop_and_blocks_duplicates() {
        let ids: SharedIds = Arc::default();
        let first = IdReservation::claim(&ids, 7);
        assert!(first.is_some());
        assert!(IdReservation::claim(&ids, 7).is_none());
        drop(first);
        assert!(IdReservation::claim(&ids, 7).is_some());
    }

    #[test]
    fn quarantined_id_cannot_be_reserved_until_released() {
        let ids: SharedIds = Arc::default();
        ids.lock().quarantine(9);
        assert!(IdReservation::claim(&ids, 9).is_none());
        ids.lock().release_quarantine();
        assert!(IdReservation::claim(&ids, 9).is_some());
    }

    #[test]
    fn acknowledgement_settles_only_the_matching_qos() {
        let mut tracker = OutcomeTracker::default();
        let (completion, handle) = Completion::new();
        tracker.track(3, QoS::ExactlyOnce, completion);
        tracker.settle_puback(3, ReasonCode::Success);
        assert_eq!(handle.try_outcome(), None);
        tracker.acknowledged(3, QoS::ExactlyOnce);
        assert_eq!(
            handle.try_outcome(),
            Some(PublishOutcome::Delivered(Delivery::ExactlyOnce {
                packet_id: 3
            }))
        );
    }

    #[test]
    fn refusal_after_resend_is_indeterminate() {
        let mut tracker = OutcomeTracker::default();
        let (completion, handle) = Completion::new();
        tracker.track(4, QoS::AtLeastOnce, completion);
        tracker.mark_resent(4);
        tracker.settle_puback(4, ReasonCode::NotAuthorized);
        assert_eq!(
            handle.try_outcome(),
            Some(PublishOutcome::Indeterminate(
                IndeterminateReason::ResendRefused(ReasonCode::NotAuthorized)
            ))
        );
    }

    #[test]
    fn tracking_an_occupied_id_keeps_the_existing_publish() {
        let mut tracker = OutcomeTracker::default();
        let (first, first_handle) = Completion::new();
        let (second, second_handle) = Completion::new();
        tracker.track(5, QoS::AtLeastOnce, first);
        tracker.track(5, QoS::AtLeastOnce, second);
        assert_eq!(
            second_handle.try_outcome(),
            Some(PublishOutcome::Indeterminate(
                IndeterminateReason::Abandoned
            ))
        );
        tracker.settle_puback(5, ReasonCode::Success);
        assert_eq!(
            first_handle.try_outcome(),
            Some(PublishOutcome::Delivered(Delivery::AtLeastOnce {
                packet_id: 5
            }))
        );
    }

    #[tokio::test]
    async fn dropped_completion_resolves_abandoned() {
        let (completion, handle) = Completion::new();
        drop(completion);
        assert_eq!(
            handle.outcome().await,
            PublishOutcome::Indeterminate(IndeterminateReason::Abandoned)
        );
    }
}
