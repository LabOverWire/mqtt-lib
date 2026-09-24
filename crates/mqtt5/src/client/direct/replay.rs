use crate::client::publish_outcome::{
    Delivery, IndeterminateReason, PublishOutcome, PublishRejection,
};
use crate::error::{MqttError, Result};
use crate::packet::publish::PublishPacket;
use crate::packet::pubrel::PubRelPacket;
use crate::packet::{MqttPacket, Packet};
use crate::session::flow_control::FlowControlManager;
use crate::session::state::OutboundReplay;
use crate::session::SessionState;
use crate::transport::PacketWriter;
use crate::QoS;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use tokio::sync::{RwLock, Semaphore};

use super::tracking::{Completion, IdReservation, SharedIds, SharedOutcomes};
use super::unified::UnifiedWriter;

#[derive(Debug, Clone, Copy)]
pub(crate) struct PublishPolicy {
    pub(crate) maximum_qos: Option<u8>,
    pub(crate) retain_available: bool,
}

impl PublishPolicy {
    pub(crate) fn conform(self, mut publish: PublishPacket) -> Result<PublishPacket> {
        if publish.retain && !self.retain_available {
            return Err(MqttError::RetainNotSupported);
        }
        let requested = publish.qos as u8;
        if let Some(maximum) = self.maximum_qos.filter(|maximum| requested > *maximum) {
            tracing::warn!(
                "Requested QoS {requested} exceeds server maximum {maximum}, using QoS {maximum}"
            );
            publish.qos = match maximum {
                0 => QoS::AtMostOnce,
                1 => QoS::AtLeastOnce,
                _ => QoS::ExactlyOnce,
            };
            if publish.qos == QoS::AtMostOnce {
                publish.packet_id = None;
            }
        }
        Ok(publish)
    }

    pub(crate) fn admits_unchanged(self, publish: &PublishPacket) -> Result<()> {
        if publish.retain && !self.retain_available {
            return Err(MqttError::RetainNotSupported);
        }
        if self
            .maximum_qos
            .is_some_and(|maximum| publish.qos as u8 > maximum)
        {
            return Err(MqttError::QoSNotSupported);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct QueuedPublish {
    serial: u64,
    packet: PublishPacket,
    reservation: Option<IdReservation>,
    completion: Option<Completion>,
}

impl QueuedPublish {
    pub(crate) fn new(
        packet: PublishPacket,
        reservation: IdReservation,
        completion: Option<Completion>,
    ) -> Self {
        Self {
            serial: 0,
            packet,
            reservation: Some(reservation),
            completion,
        }
    }

    fn reject(self, error: &MqttError) {
        tracing::warn!(
            topic = %self.packet.topic_name,
            error = %error,
            "Queued message no longer conforms to the connection; not sent"
        );
        if let Some(completion) = self.completion {
            completion.rejected(PublishRejection::from_error(error));
        }
    }
}

#[derive(Debug, Default)]
pub struct OfflineQueue {
    messages: VecDeque<QueuedPublish>,
    next_serial: u64,
}

impl OfflineQueue {
    fn stamp(&mut self, mut queued: QueuedPublish) -> QueuedPublish {
        self.next_serial = self.next_serial.wrapping_add(1);
        queued.serial = self.next_serial;
        queued
    }

    pub(crate) fn push_back(&mut self, queued: QueuedPublish) {
        let queued = self.stamp(queued);
        self.messages.push_back(queued);
    }

    pub(crate) fn push_front_in_order(&mut self, ordered: Vec<QueuedPublish>) {
        for queued in ordered.into_iter().rev() {
            let queued = self.stamp(queued);
            self.messages.push_front(queued);
        }
    }

    fn front(&self) -> Option<(u64, PublishPacket)> {
        self.messages
            .front()
            .map(|queued| (queued.serial, queued.packet.clone()))
    }

    fn take(&mut self, serial: u64) -> Option<QueuedPublish> {
        let position = self
            .messages
            .iter()
            .position(|queued| queued.serial == serial)?;
        self.messages.remove(position)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

#[derive(Clone)]
pub(super) struct ConnectionLink {
    pub(super) epoch: u64,
    pub(super) current_epoch: Arc<AtomicU64>,
    pub(super) connected: Arc<AtomicBool>,
    pub(super) transfer: Arc<tokio::sync::Mutex<()>>,
    pub(super) alive: tokio::sync::watch::Receiver<bool>,
}

impl ConnectionLink {
    async fn ended(&self) {
        let mut alive = self.alive.clone();
        if alive.wait_for(|alive| !*alive).await.is_err() {
            tracing::trace!("Connection liveness signal dropped");
        }
    }

    fn is_current(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
            && self.current_epoch.load(Ordering::SeqCst) == self.epoch
    }
}

pub(super) struct SessionReplay {
    pub(super) items: Vec<OutboundReplay>,
    pub(super) slots: Arc<Semaphore>,
    pub(super) session: Arc<RwLock<SessionState>>,
    pub(super) writer: Weak<tokio::sync::Mutex<UnifiedWriter>>,
    pub(super) queued: Arc<Mutex<OfflineQueue>>,
    pub(super) policy: PublishPolicy,
    pub(super) outcomes: SharedOutcomes,
    pub(super) ids: SharedIds,
    pub(super) link: ConnectionLink,
}

impl SessionReplay {
    pub(super) async fn run(self) {
        let flow = Arc::clone(self.session.read().await.flow_control());
        if self.replay_session_state(&flow).await && self.flush_offline_queue(&flow).await {
            flow.read().await.finish_replay(&self.slots).await;
            tracing::debug!("Session replay complete");
        }
    }

    async fn replay_session_state(&self, flow: &Arc<RwLock<FlowControlManager>>) -> bool {
        for item in &self.items {
            let packet = match item {
                OutboundReplay::PubRel(packet_id) => Packet::PubRel(PubRelPacket::new(*packet_id)),
                OutboundReplay::Publish(publish) => {
                    let resend = without_topic_alias(publish.clone());
                    if let Err(e) = self.admits_unchanged(&resend).await {
                        if !self.abandon_replay(&resend, &e).await {
                            return false;
                        }
                        continue;
                    }
                    if !self.take_slot(flow, resend.packet_id).await {
                        return false;
                    }
                    if let Some(packet_id) = resend.packet_id {
                        self.outcomes.lock().mark_resent(packet_id);
                    }
                    Packet::Publish(PublishPacket {
                        dup: true,
                        ..resend
                    })
                }
            };
            if !self.write(packet).await {
                return false;
            }
        }
        true
    }

    async fn admits_unchanged(&self, publish: &PublishPacket) -> Result<()> {
        self.policy.admits_unchanged(publish)?;
        self.check_size(publish).await
    }

    async fn abandon_replay(&self, publish: &PublishPacket, error: &MqttError) -> bool {
        let Some(packet_id) = publish.packet_id else {
            return true;
        };
        let transfer = self.link.transfer.lock().await;
        if !self.link.is_current() {
            return false;
        }
        if publish.qos == QoS::ExactlyOnce {
            self.ids.lock().quarantine(packet_id);
        }
        self.session
            .read()
            .await
            .remove_unacked_publish(packet_id)
            .await;
        let completion = self.outcomes.lock().take(packet_id);
        drop(transfer);
        tracing::warn!(
            packet_id,
            topic = %publish.topic_name,
            error = %error,
            "Unacknowledged PUBLISH no longer conforms to the resumed connection; not re-sent"
        );
        if let Some(completion) = completion {
            completion.indeterminate(IndeterminateReason::ReplayNotConforming);
        }
        true
    }

    async fn flush_offline_queue(&self, flow: &Arc<RwLock<FlowControlManager>>) -> bool {
        loop {
            let Some((serial, queued)) = self.queued.lock().front() else {
                return true;
            };
            let publish = match self.conform_queued(queued).await {
                Ok(publish) => publish,
                Err(e) => {
                    let transfer = self.link.transfer.lock().await;
                    if !self.link.is_current() {
                        return false;
                    }
                    let rejected = self.queued.lock().take(serial);
                    drop(transfer);
                    if let Some(rejected) = rejected {
                        rejected.reject(&e);
                    }
                    continue;
                }
            };
            if !self.take_slot(flow, publish.packet_id).await {
                return false;
            }
            match self.transfer_to_session(flow, serial, &publish).await {
                Transfer::Stopped => return false,
                Transfer::Skipped => {}
                Transfer::Stored => {
                    if !matches!(self.write_publish(publish).await, Written::Complete) {
                        return false;
                    }
                }
                Transfer::Downgraded(queued, transfer) => {
                    if !self.write_downgraded(publish, queued, transfer).await {
                        return false;
                    }
                }
            }
        }
    }

    async fn write_downgraded(
        &self,
        publish: PublishPacket,
        mut queued: QueuedPublish,
        transfer: tokio::sync::MutexGuard<'_, ()>,
    ) -> bool {
        let settled = match self.write_publish(publish).await {
            Written::Complete => PublishOutcome::Delivered(Delivery::Unconfirmed),
            Written::Failed => PublishOutcome::Indeterminate(IndeterminateReason::ConnectionLost),
            Written::NotAttempted => {
                tracing::debug!(
                    "Connection ended before a downgraded message was written; it stays queued"
                );
                self.queued.lock().push_front_in_order(vec![queued]);
                drop(transfer);
                return false;
            }
        };
        drop(transfer);
        let complete = matches!(settled, PublishOutcome::Delivered(Delivery::Unconfirmed));
        if let Some(completion) = queued.completion.take() {
            completion.settle(settled);
        }
        complete
    }

    async fn transfer_to_session(
        &self,
        flow: &Arc<RwLock<FlowControlManager>>,
        serial: u64,
        publish: &PublishPacket,
    ) -> Transfer<'_> {
        let transfer = self.link.transfer.lock().await;
        if !self.link.is_current() {
            return Transfer::Stopped;
        }
        let Some(mut queued) = self.queued.lock().take(serial) else {
            drop(transfer);
            release_claim(flow, publish.packet_id).await;
            return Transfer::Skipped;
        };
        let Some(packet_id) = publish.packet_id else {
            return Transfer::Downgraded(queued, transfer);
        };
        let stored = self
            .session
            .read()
            .await
            .store_unacked_publish(publish.clone())
            .await;
        if let Err(e) = stored {
            drop(transfer);
            release_claim(flow, Some(packet_id)).await;
            queued.reject(&e);
            return Transfer::Skipped;
        }
        if let Some(completion) = queued.completion.take() {
            self.outcomes
                .lock()
                .track(packet_id, publish.qos, completion);
        }
        queued.reservation.take();
        drop(transfer);
        Transfer::Stored
    }

    async fn conform_queued(&self, queued: PublishPacket) -> Result<PublishPacket> {
        let publish = without_topic_alias(self.policy.conform(queued)?);
        self.check_size(&publish).await?;
        Ok(publish)
    }

    async fn check_size(&self, publish: &PublishPacket) -> Result<()> {
        let mut buf = bytes::BytesMut::new();
        publish.encode(&mut buf)?;
        self.session.read().await.check_packet_size(buf.len()).await
    }

    async fn take_slot(
        &self,
        flow: &Arc<RwLock<FlowControlManager>>,
        packet_id: Option<u16>,
    ) -> bool {
        let Some(packet_id) = packet_id else {
            return true;
        };
        match self.slots.acquire().await {
            Ok(permit) => {
                permit.forget();
                flow.read()
                    .await
                    .claim_send_quota(&self.slots, packet_id)
                    .await
                    .is_some()
            }
            Err(_) => false,
        }
    }

    async fn write(&self, packet: Packet) -> bool {
        matches!(self.write_packet(packet).await, Written::Complete)
    }

    async fn write_publish(&self, publish: PublishPacket) -> Written {
        self.write_packet(Packet::Publish(publish)).await
    }

    async fn write_packet(&self, packet: Packet) -> Written {
        let Some(writer) = self.writer.upgrade() else {
            return Written::NotAttempted;
        };
        let mut writer = tokio::select! {
            biased;
            () = self.link.ended() => return Written::NotAttempted,
            writer = writer.lock() => writer,
        };
        if !self.link.is_current() {
            tracing::debug!("Session replay stopped: connection replaced");
            return Written::NotAttempted;
        }
        tokio::select! {
            biased;
            () = self.link.ended() => {
                tracing::debug!("Session replay stopped: connection ended during a write");
                Written::Failed
            }
            written = writer.write_packet(packet) => match written {
                Ok(()) => Written::Complete,
                Err(e) => {
                    tracing::debug!("Session replay stopped: {e}");
                    Written::Failed
                }
            },
        }
    }
}

enum Written {
    Complete,
    NotAttempted,
    Failed,
}

enum Transfer<'a> {
    Stopped,
    Skipped,
    Stored,
    Downgraded(QueuedPublish, tokio::sync::MutexGuard<'a, ()>),
}

async fn release_claim(flow: &Arc<RwLock<FlowControlManager>>, packet_id: Option<u16>) {
    if let Some(packet_id) = packet_id {
        if let Err(e) = flow.read().await.acknowledge(packet_id).await {
            tracing::trace!(packet_id, "No send quota held: {e}");
        }
    }
}

fn without_topic_alias(mut publish: PublishPacket) -> PublishPacket {
    publish.properties.remove_topic_alias();
    publish
}
