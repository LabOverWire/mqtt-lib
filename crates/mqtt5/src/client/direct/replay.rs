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
use std::sync::{Arc, Weak};
use tokio::sync::{RwLock, Semaphore};

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
}

pub(super) struct SessionReplay {
    pub(super) items: Vec<OutboundReplay>,
    pub(super) slots: Arc<Semaphore>,
    pub(super) session: Arc<RwLock<SessionState>>,
    pub(super) writer: Weak<tokio::sync::Mutex<UnifiedWriter>>,
    pub(super) queued: Arc<Mutex<VecDeque<PublishPacket>>>,
    pub(super) policy: PublishPolicy,
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
                    if !self.take_slot(flow, publish.packet_id).await {
                        return false;
                    }
                    let mut resend = without_topic_alias(publish.clone());
                    resend.dup = true;
                    Packet::Publish(resend)
                }
            };
            if !self.write(packet).await {
                return false;
            }
        }
        true
    }

    async fn flush_offline_queue(&self, flow: &Arc<RwLock<FlowControlManager>>) -> bool {
        loop {
            let Some(queued) = self.queued.lock().front().cloned() else {
                return true;
            };
            let publish = match self.conform_queued(queued).await {
                Ok(publish) => publish,
                Err(e) => {
                    tracing::warn!("Dropping queued message: {e}");
                    self.queued.lock().pop_front();
                    continue;
                }
            };
            if !self.take_slot(flow, publish.packet_id).await {
                return false;
            }
            self.queued.lock().pop_front();
            if publish.qos != QoS::AtMostOnce {
                if let Err(e) = self
                    .session
                    .read()
                    .await
                    .store_unacked_publish(publish.clone())
                    .await
                {
                    tracing::warn!("Dropping queued message: {e}");
                    continue;
                }
            }
            if !self.write(Packet::Publish(publish)).await {
                return false;
            }
        }
    }

    async fn conform_queued(&self, queued: PublishPacket) -> Result<PublishPacket> {
        let publish = without_topic_alias(self.policy.conform(queued)?);
        let mut buf = bytes::BytesMut::new();
        publish.encode(&mut buf)?;
        self.session
            .read()
            .await
            .check_packet_size(buf.len())
            .await?;
        Ok(publish)
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
            }
            Err(_) => false,
        }
    }

    async fn write(&self, packet: Packet) -> bool {
        let Some(writer) = self.writer.upgrade() else {
            return false;
        };
        let written = writer.lock().await.write_packet(packet).await;
        if let Err(e) = &written {
            tracing::debug!("Session replay stopped: {e}");
        }
        written.is_ok()
    }
}

fn without_topic_alias(mut publish: PublishPacket) -> PublishPacket {
    publish.properties.remove_topic_alias();
    publish
}
