//! Keepalive management and background tasks

use crate::error::Result;
use crate::packet::Packet;
use crate::session::SessionState;
use crate::transport::PacketWriter;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::time::Duration;

use super::unified::UnifiedWriter;

#[derive(Debug, Default)]
pub(crate) struct KeepaliveState {
    last_ping_sent: Option<tokio::time::Instant>,
    last_pong_received: Option<tokio::time::Instant>,
}

impl KeepaliveState {
    pub(crate) fn record_ping_sent(&mut self) {
        self.last_ping_sent = Some(tokio::time::Instant::now());
    }

    pub(crate) fn record_pong_received(&mut self) {
        self.last_pong_received = Some(tokio::time::Instant::now());
    }

    pub(crate) fn is_timeout(&self, timeout_duration: Duration) -> bool {
        match (self.last_ping_sent, self.last_pong_received) {
            (Some(sent_at), Some(received_at)) => {
                sent_at > received_at && sent_at.elapsed() > timeout_duration
            }
            (Some(sent_at), None) => sent_at.elapsed() > timeout_duration,
            _ => false,
        }
    }
}

pub(super) const PINGREQ_LOG_INTERVAL: u32 = 20;

pub(super) async fn send_pingreq_with_priority(
    writer: &Arc<tokio::sync::Mutex<UnifiedWriter>>,
    config: &mqtt5_protocol::KeepaliveConfig,
) -> Result<()> {
    let max_attempts = config.lock_retry_attempts;
    let retry_delay = Duration::from_millis(u64::from(config.lock_retry_delay_ms));

    for attempt in 0..max_attempts {
        if let Ok(mut guard) = writer.try_lock() {
            return guard.write_packet(Packet::PingReq).await;
        }

        if attempt > 0 && attempt % PINGREQ_LOG_INTERVAL == 0 {
            tracing::warn!(
                attempt,
                max_attempts,
                "PINGREQ waiting for writer lock - possible contention"
            );
        }

        tokio::time::sleep(retry_delay).await;
    }

    tracing::error!(
        max_attempts,
        "Failed to acquire writer lock for PINGREQ, falling back to blocking"
    );
    writer.lock().await.write_packet(Packet::PingReq).await
}

pub(super) async fn keepalive_task_with_writer(
    writer: Arc<tokio::sync::Mutex<UnifiedWriter>>,
    keepalive_interval: Duration,
    keepalive_state: Arc<Mutex<KeepaliveState>>,
    connected: Arc<AtomicBool>,
    keepalive_config: Option<mqtt5_protocol::KeepaliveConfig>,
) {
    let config = keepalive_config.unwrap_or_default();
    let ping_interval = config.ping_interval(keepalive_interval);
    let timeout_duration = config.timeout_duration(keepalive_interval);
    let mut interval = tokio::time::interval(ping_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    interval.tick().await;

    loop {
        interval.tick().await;

        {
            let state = keepalive_state.lock();
            if state.is_timeout(timeout_duration) {
                tracing::error!("Keepalive timeout - no PINGRESP received");
                connected.store(false, Ordering::SeqCst);
                break;
            }
        }

        keepalive_state.lock().record_ping_sent();

        let send_result = send_pingreq_with_priority(&writer, &config).await;
        if let Err(e) = send_result {
            tracing::error!("Error sending PINGREQ: {e}");
            break;
        }
    }
}

pub(super) async fn flow_expiration_task(session: Arc<tokio::sync::RwLock<SessionState>>) {
    let check_interval = Duration::from_secs(60);
    let mut interval = tokio::time::interval(check_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    interval.tick().await;

    loop {
        interval.tick().await;

        let expired = session.read().await.expire_flows().await;
        if !expired.is_empty() {
            tracing::debug!(count = expired.len(), "Expired {} flows", expired.len());
        }
    }
}
