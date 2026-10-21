use crate::transport::flow::FlowFlags;
use mqtt5_protocol::{QuicConnectionCode, QuicStreamCode};
use std::collections::{HashMap, HashSet};
use tokio::sync::oneshot;
use tokio::time::{Duration, Instant};

pub(crate) const MAX_SESSION_FLOWS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowAnswer {
    Served,
    Refused(QuicStreamCode),
    Close(QuicConnectionCode),
}

#[derive(Debug)]
pub struct FlowOpening {
    pub flow_id: u64,
    pub stream_id: u64,
    pub flags: FlowFlags,
    pub expire: Duration,
    pub answer: oneshot::Sender<FlowAnswer>,
}

#[derive(Debug)]
pub enum FlowInput {
    Opened(FlowOpening),
    Ended {
        flow_id: u64,
        stream_id: u64,
        graceful: bool,
    },
    Discard {
        flow_id: u64,
        done: oneshot::Sender<()>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Opened {
    pub answer: FlowAnswer,
    pub drop_subscriptions: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ended {
    Ignored,
    Removed,
    Kept,
}

#[derive(Debug, Clone, Copy)]
enum Held {
    Open { generation: u64, stream_id: u64 },
    Kept { until: Instant },
}

#[derive(Debug, Clone, Copy)]
struct Flow {
    expire: Duration,
    held: Held,
}

impl Flow {
    fn live(&self, now: Instant) -> bool {
        match self.held {
            Held::Open { .. } => true,
            Held::Kept { until } => now < until,
        }
    }

    fn settled(mut self, now: Instant) -> Option<Self> {
        if let Held::Open { .. } = self.held {
            if self.expire.is_zero() {
                return None;
            }
            self.held = Held::Kept {
                until: now + self.expire,
            };
        }
        self.live(now).then_some(self)
    }
}

#[derive(Debug, Default)]
pub(crate) struct SessionFlows {
    owner: Option<u64>,
    flows: HashMap<u64, Flow>,
    failed: HashSet<u64>,
    expiry_epoch: u64,
}

impl SessionFlows {
    fn settle(&mut self, now: Instant) {
        self.flows = self
            .flows
            .drain()
            .filter_map(|(id, flow)| flow.settled(now).map(|flow| (id, flow)))
            .collect();
    }

    pub(crate) fn claim(&mut self, generation: u64, clean_start: bool, now: Instant) {
        self.owner = Some(generation);
        self.failed.clear();
        if clean_start {
            self.flows.clear();
        } else {
            self.settle(now);
        }
    }

    pub(crate) fn release(&mut self, generation: u64, keep: bool, now: Instant) -> bool {
        if self.owner != Some(generation) {
            return false;
        }
        self.owner = None;
        self.failed.clear();
        if keep {
            self.settle(now);
        } else {
            self.flows.clear();
        }
        true
    }

    #[must_use]
    pub(crate) fn holds(&self, flow_id: u64) -> bool {
        self.flows.contains_key(&flow_id)
    }

    #[must_use]
    pub(crate) fn is_idle(&self) -> bool {
        self.owner.is_none() && self.flows.is_empty()
    }

    pub(crate) fn open(&mut self, generation: u64, opening: &FlowOpening, now: Instant) -> Opened {
        let refuse = |code| Opened {
            answer: FlowAnswer::Refused(code),
            drop_subscriptions: false,
        };
        if self.owner != Some(generation) {
            return refuse(QuicStreamCode::FlowRefused);
        }
        let id = opening.flow_id;
        let present = self.flows.contains_key(&id);
        let live = self.flows.get(&id).is_some_and(|flow| flow.live(now));
        let open = Flow {
            expire: opening.expire,
            held: Held::Open {
                generation,
                stream_id: opening.stream_id,
            },
        };
        if opening.flags.clean == 1 {
            self.failed.remove(&id);
            self.flows.remove(&id);
            if opening.flags.abort_if_no_state == 1 {
                return Opened {
                    answer: FlowAnswer::Refused(QuicStreamCode::DiscardState),
                    drop_subscriptions: present,
                };
            }
            return self.start(id, open, present);
        }
        if live {
            self.flows.insert(id, open);
            return Opened {
                answer: FlowAnswer::Served,
                drop_subscriptions: false,
            };
        }
        self.flows.remove(&id);
        if opening.flags.abort_if_no_state == 0 {
            return self.start(id, open, present);
        }
        let answer = if self.failed.contains(&id) || self.failed.len() >= MAX_SESSION_FLOWS {
            FlowAnswer::Close(QuicConnectionCode::TooManyRecoverAttempts)
        } else {
            self.failed.insert(id);
            FlowAnswer::Refused(QuicStreamCode::NoFlowState)
        };
        Opened {
            answer,
            drop_subscriptions: present,
        }
    }

    fn start(&mut self, id: u64, flow: Flow, present: bool) -> Opened {
        if self.flows.len() >= MAX_SESSION_FLOWS {
            return Opened {
                answer: FlowAnswer::Refused(QuicStreamCode::FlowRefused),
                drop_subscriptions: present,
            };
        }
        self.flows.insert(id, flow);
        Opened {
            answer: FlowAnswer::Served,
            drop_subscriptions: present,
        }
    }

    pub(crate) fn end(
        &mut self,
        generation: u64,
        flow_id: u64,
        stream_id: u64,
        graceful: bool,
        now: Instant,
    ) -> Ended {
        if self.owner != Some(generation) {
            return Ended::Ignored;
        }
        let Some(flow) = self.flows.get_mut(&flow_id) else {
            return Ended::Ignored;
        };
        let opened_here = matches!(
            flow.held,
            Held::Open { generation: g, stream_id: s } if g == generation && s == stream_id
        );
        if !opened_here {
            return Ended::Ignored;
        }
        if graceful || flow.expire.is_zero() {
            self.flows.remove(&flow_id);
            return Ended::Removed;
        }
        flow.held = Held::Kept {
            until: now + flow.expire,
        };
        Ended::Kept
    }

    pub(crate) fn discard(&mut self, generation: u64, flow_id: u64) -> bool {
        if self.owner != Some(generation) {
            return false;
        }
        self.failed.remove(&flow_id);
        self.flows.remove(&flow_id);
        true
    }

    #[must_use]
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.flows
            .values()
            .filter_map(|flow| match flow.held {
                Held::Kept { until } => Some(until),
                Held::Open { .. } => None,
            })
            .min()
    }

    #[must_use]
    pub(crate) fn deadline_for(&self, generation: u64) -> Option<Instant> {
        if self.owner == Some(generation) {
            self.next_deadline()
        } else {
            None
        }
    }

    pub(crate) fn take_expired_for(&mut self, generation: u64, now: Instant) -> Option<u64> {
        if self.owner == Some(generation) {
            self.take_expired(now)
        } else {
            None
        }
    }

    pub(crate) fn take_expired(&mut self, now: Instant) -> Option<u64> {
        let id = self
            .flows
            .iter()
            .find(|(_, flow)| !flow.live(now))
            .map(|(id, _)| *id)?;
        self.flows.remove(&id);
        Some(id)
    }

    pub(crate) fn arm_expiry(&mut self) -> u64 {
        self.expiry_epoch += 1;
        self.expiry_epoch
    }

    #[must_use]
    pub(crate) fn expiry_armed(&self, epoch: u64) -> bool {
        self.expiry_epoch == epoch && self.owner.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GEN: u64 = 7;
    const NEXT: u64 = 8;

    fn flags(clean: u8, abort_if_no_state: u8) -> FlowFlags {
        FlowFlags {
            abort_if_no_state,
            clean,
            ..FlowFlags::default()
        }
    }

    fn opening(stream_id: u64, flags: FlowFlags, expire: u64) -> FlowOpening {
        FlowOpening {
            flow_id: 10,
            stream_id,
            flags,
            expire: Duration::from_secs(expire),
            answer: oneshot::channel().0,
        }
    }

    fn served(drop_subscriptions: bool) -> Opened {
        Opened {
            answer: FlowAnswer::Served,
            drop_subscriptions,
        }
    }

    fn refused(code: QuicStreamCode, drop_subscriptions: bool) -> Opened {
        Opened {
            answer: FlowAnswer::Refused(code),
            drop_subscriptions,
        }
    }

    fn owned(now: Instant) -> SessionFlows {
        let mut flows = SessionFlows::default();
        flows.claim(GEN, false, now);
        flows
    }

    #[test]
    fn a_flow_without_state_is_refused_only_with_abort_if_no_state() {
        let now = Instant::now();
        let mut flows = owned(now);
        assert_eq!(
            flows.open(GEN, &opening(1, flags(0, 1), 0), now),
            refused(QuicStreamCode::NoFlowState, false)
        );
        assert_eq!(
            flows.open(GEN, &opening(2, flags(0, 1), 0), now).answer,
            FlowAnswer::Close(QuicConnectionCode::TooManyRecoverAttempts)
        );
        let mut flows = owned(now);
        assert_eq!(
            flows.open(GEN, &opening(1, flags(0, 0), 0), now),
            served(false)
        );
    }

    #[test]
    fn a_clean_start_clears_a_failed_recovery() {
        let now = Instant::now();
        let mut flows = owned(now);
        flows.open(GEN, &opening(1, flags(0, 1), 0), now);
        flows.open(GEN, &opening(2, flags(1, 0), 0), now);
        assert_eq!(flows.end(GEN, 10, 2, true, now), Ended::Removed);
        assert_eq!(
            flows.open(GEN, &opening(3, flags(0, 1), 0), now),
            refused(QuicStreamCode::NoFlowState, false)
        );
    }

    #[test]
    fn an_aborted_flow_is_recovered_until_its_deadline() {
        let now = Instant::now();
        let mut flows = owned(now);
        flows.open(GEN, &opening(1, flags(1, 0), 5), now);
        assert_eq!(flows.end(GEN, 10, 1, false, now), Ended::Kept);
        assert_eq!(flows.next_deadline(), Some(now + Duration::from_secs(5)));
        assert_eq!(
            flows.open(
                GEN,
                &opening(2, flags(0, 1), 5),
                now + Duration::from_secs(4)
            ),
            served(false)
        );
        assert_eq!(flows.end(GEN, 10, 2, false, now), Ended::Kept);
        assert_eq!(
            flows.open(
                GEN,
                &opening(3, flags(0, 1), 5),
                now + Duration::from_secs(6)
            ),
            refused(QuicStreamCode::NoFlowState, true)
        );
        assert!(!flows.holds(10));
    }

    #[test]
    fn clean_discards_the_state_it_replaces() {
        let now = Instant::now();
        let mut flows = owned(now);
        flows.open(GEN, &opening(1, flags(1, 0), 5), now);
        assert_eq!(
            flows.open(GEN, &opening(2, flags(1, 0), 5), now),
            served(true)
        );
        assert_eq!(
            flows.open(GEN, &opening(3, flags(1, 1), 5), now),
            refused(QuicStreamCode::DiscardState, true)
        );
        assert!(!flows.holds(10));
    }

    #[test]
    fn an_older_stream_ending_leaves_the_stream_that_reopened_the_flow() {
        let now = Instant::now();
        let mut flows = owned(now);
        flows.open(GEN, &opening(1, flags(1, 0), 0), now);
        flows.open(GEN, &opening(2, flags(0, 0), 0), now);
        assert_eq!(flows.end(GEN, 10, 1, true, now), Ended::Ignored);
        assert!(flows.holds(10));
    }

    #[test]
    fn a_claim_keeps_open_flows_with_an_expire_interval_and_removes_the_rest() {
        let now = Instant::now();
        let mut flows = owned(now);
        flows.open(GEN, &opening(1, flags(1, 0), 5), now);
        let mut other = opening(2, flags(1, 0), 0);
        other.flow_id = 12;
        flows.open(GEN, &other, now);
        flows.claim(NEXT, false, now);
        assert!(flows.holds(10));
        assert!(!flows.holds(12));
        assert_eq!(flows.end(GEN, 10, 1, true, now), Ended::Ignored);
        assert_eq!(
            flows.open(GEN, &opening(3, flags(0, 1), 5), now),
            refused(QuicStreamCode::FlowRefused, false)
        );
        assert_eq!(
            flows.open(NEXT, &opening(3, flags(0, 1), 5), now),
            served(false)
        );
        flows.claim(GEN + 2, true, now);
        assert!(!flows.holds(10));
    }

    #[test]
    fn release_by_a_replaced_owner_changes_nothing() {
        let now = Instant::now();
        let mut flows = owned(now);
        flows.open(GEN, &opening(1, flags(1, 0), 0), now);
        flows.claim(NEXT, false, now);
        flows.open(NEXT, &opening(2, flags(1, 0), 0), now);
        assert!(!flows.release(GEN, true, now));
        assert!(flows.holds(10));
        assert!(flows.release(NEXT, true, now));
        assert!(!flows.holds(10));
        assert!(flows.is_idle());
    }

    #[test]
    fn a_session_that_ends_keeps_no_flow_state() {
        let now = Instant::now();
        let mut flows = owned(now);
        flows.open(GEN, &opening(1, flags(1, 0), 5), now);
        assert!(flows.release(GEN, false, now));
        assert!(flows.is_idle());
    }

    #[test]
    fn only_the_owner_expires_or_discards_flows() {
        let now = Instant::now();
        let later = now + Duration::from_secs(2);
        let mut flows = owned(now);
        flows.open(GEN, &opening(1, flags(1, 0), 1), now);
        flows.end(GEN, 10, 1, false, now);
        assert_eq!(flows.deadline_for(NEXT), None);
        assert_eq!(flows.take_expired_for(NEXT, later), None);
        assert!(!flows.discard(NEXT, 10));
        assert_eq!(flows.deadline_for(GEN), Some(now + Duration::from_secs(1)));
        assert_eq!(flows.take_expired_for(GEN, later), Some(10));
    }

    #[test]
    fn expired_flows_are_taken_one_at_a_time() {
        let now = Instant::now();
        let mut flows = owned(now);
        flows.open(GEN, &opening(1, flags(1, 0), 1), now);
        flows.release(GEN, true, now);
        assert_eq!(flows.take_expired(now), None);
        assert_eq!(flows.take_expired(now + Duration::from_secs(2)), Some(10));
        assert_eq!(flows.take_expired(now + Duration::from_secs(2)), None);
    }

    #[test]
    fn expiry_stays_armed_until_rearmed_or_claimed() {
        let now = Instant::now();
        let mut flows = owned(now);
        flows.open(GEN, &opening(1, flags(1, 0), 1), now);
        flows.release(GEN, true, now);
        let first = flows.arm_expiry();
        assert!(flows.expiry_armed(first));
        let second = flows.arm_expiry();
        assert!(!flows.expiry_armed(first));
        flows.claim(NEXT, false, now);
        assert!(!flows.expiry_armed(second));
    }
}
