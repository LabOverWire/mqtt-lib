//! Keepalive management and background tasks

use crate::client::connection::{ConnectionEvent, DisconnectReason};
use crate::client::ConnectionEventCallback;
use crate::error::Result;
use crate::packet::Packet;
use crate::transport::PacketWriter;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::time::Duration;

use super::unified::UnifiedWriter;
#[cfg(feature = "transport-quic")]
use crate::session::SessionState;

#[derive(Debug, Default)]
pub(crate) struct KeepaliveState {
    last_ping_sent: Option<tokio::time::Instant>,
    last_pong_received: Option<tokio::time::Instant>,
}

impl KeepaliveState {
    pub(crate) fn reset(&mut self) {
        self.last_ping_sent = None;
        self.last_pong_received = None;
    }

    pub(crate) fn has_outstanding_ping(&self) -> bool {
        match (self.last_ping_sent, self.last_pong_received) {
            (Some(sent_at), Some(received_at)) => sent_at > received_at,
            (Some(_), None) => true,
            _ => false,
        }
    }

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

pub(crate) fn owns_current_connection(
    connection_epoch: u64,
    current_connection_epoch: &AtomicU64,
) -> bool {
    current_connection_epoch.load(Ordering::SeqCst) == connection_epoch
}

#[must_use]
pub(crate) fn mark_disconnected_if_current(
    connected: &AtomicBool,
    connection_epoch: u64,
    current_connection_epoch: &AtomicU64,
) -> bool {
    owns_current_connection(connection_epoch, current_connection_epoch)
        && connected
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
}

pub(crate) async fn fire_connection_event(
    callbacks: &tokio::sync::RwLock<Vec<ConnectionEventCallback>>,
    event: ConnectionEvent,
) {
    let callbacks = callbacks.read().await.clone();
    for callback in callbacks {
        callback(event.clone());
    }
}

#[derive(Clone)]
pub(super) struct ConnectionLifecycle {
    pub(super) connected: Arc<AtomicBool>,
    pub(super) connection_epoch: u64,
    pub(super) current_connection_epoch: Arc<AtomicU64>,
    pub(super) callbacks: Arc<tokio::sync::RwLock<Vec<ConnectionEventCallback>>>,
}

impl ConnectionLifecycle {
    pub(super) fn owns_current_connection(&self) -> bool {
        owns_current_connection(self.connection_epoch, &self.current_connection_epoch)
    }

    pub(super) async fn end(&self, reason: DisconnectReason) {
        if mark_disconnected_if_current(
            &self.connected,
            self.connection_epoch,
            &self.current_connection_epoch,
        ) {
            fire_connection_event(&self.callbacks, ConnectionEvent::Disconnected { reason }).await;
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
    lifecycle: ConnectionLifecycle,
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

        let timed_out = keepalive_state.lock().is_timeout(timeout_duration);
        if timed_out {
            tracing::error!("Keepalive timeout - no PINGRESP received");
            lifecycle.end(DisconnectReason::KeepAliveTimeout).await;
            break;
        }

        let should_send_ping = {
            let mut state = keepalive_state.lock();
            let should_send_ping = !state.has_outstanding_ping();
            if should_send_ping {
                state.record_ping_sent();
            }
            should_send_ping
        };

        if !should_send_ping {
            continue;
        }

        match tokio::time::timeout(
            timeout_duration,
            send_pingreq_with_priority(&writer, &config),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::error!("Error sending PINGREQ: {e}");
                lifecycle
                    .end(DisconnectReason::NetworkError(e.to_string()))
                    .await;
                break;
            }
            Err(_) => {
                tracing::error!("PINGREQ send timed out");
                lifecycle
                    .end(DisconnectReason::NetworkError(
                        "PINGREQ send timed out".to_string(),
                    ))
                    .await;
                break;
            }
        }
    }
}

#[cfg(feature = "transport-quic")]
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

#[cfg(test)]
mod tests {
    use super::{mark_disconnected_if_current, owns_current_connection, KeepaliveState};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use tokio::time::{Duration, Instant};

    #[test]
    fn stale_epoch_is_not_current() {
        assert!(!owns_current_connection(1, &AtomicU64::new(2)));
    }

    #[test]
    fn current_epoch_matches() {
        assert!(owns_current_connection(2, &AtomicU64::new(2)));
    }

    #[test]
    fn stale_keepalive_epoch_does_not_disconnect_current_connection() {
        let connected = AtomicBool::new(true);
        let current_epoch = AtomicU64::new(2);

        assert!(!mark_disconnected_if_current(&connected, 1, &current_epoch));

        assert!(connected.load(Ordering::SeqCst));
    }

    #[test]
    fn current_keepalive_epoch_disconnects_connection() {
        let connected = AtomicBool::new(true);
        let current_epoch = AtomicU64::new(2);

        assert!(mark_disconnected_if_current(&connected, 2, &current_epoch));
        assert!(!mark_disconnected_if_current(&connected, 2, &current_epoch));

        assert!(!connected.load(Ordering::SeqCst));
    }

    #[test]
    fn reset_clears_keepalive_tracking() {
        let mut state = KeepaliveState {
            last_ping_sent: Some(Instant::now() - Duration::from_secs(2)),
            last_pong_received: Some(Instant::now() - Duration::from_secs(1)),
        };

        state.reset();

        assert_eq!(state.last_ping_sent, None);
        assert_eq!(state.last_pong_received, None);
        assert!(!state.has_outstanding_ping());
    }
}
