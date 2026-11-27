//! Keepalive management and background tasks

use crate::client::connection::{ConnectionEvent, DisconnectReason};
use crate::client::{fire_connection_event, ConnectionEventCallback};
use crate::packet::Packet;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::time::Duration;

use super::unified::UnifiedWriter;
use crate::session::flow_control::FlowControlManager;
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

#[derive(Clone)]
pub(super) struct ConnectionLifecycle {
    pub(super) connected: Arc<AtomicBool>,
    pub(super) last_disconnect_reason: Arc<Mutex<Option<DisconnectReason>>>,
    pub(super) connection_epoch: u64,
    pub(super) current_connection_epoch: Arc<AtomicU64>,
    pub(super) callbacks: Arc<tokio::sync::RwLock<Vec<ConnectionEventCallback>>>,
    pub(super) closing: Arc<AtomicBool>,
    pub(super) alive: Arc<tokio::sync::watch::Sender<bool>>,
    pub(super) flow: Arc<tokio::sync::RwLock<FlowControlManager>>,
    pub(super) reader_task: Arc<Mutex<Option<tokio::task::AbortHandle>>>,
}

impl ConnectionLifecycle {
    pub(super) fn owns_current_connection(&self) -> bool {
        owns_current_connection(self.connection_epoch, &self.current_connection_epoch)
    }

    pub(super) fn begin_close(&self) {
        self.closing.store(true, Ordering::SeqCst);
    }

    #[cfg(feature = "transport-quic")]
    pub(super) fn is_closing(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
            || !self.connected.load(Ordering::SeqCst)
            || !self.owns_current_connection()
    }

    pub(super) async fn end(&self, reason: DisconnectReason) {
        self.alive.send_replace(false);
        let ended = {
            let mut last_disconnect_reason = self.last_disconnect_reason.lock();
            let ended = mark_disconnected_if_current(
                &self.connected,
                self.connection_epoch,
                &self.current_connection_epoch,
            );
            if ended {
                *last_disconnect_reason = Some(reason.clone());
            }
            ended
        };
        if ended {
            self.flow.read().await.close_send_quota();
            fire_connection_event(&self.callbacks, ConnectionEvent::Disconnected { reason }).await;
        }
    }

    fn stop_reader(&self) {
        if let Some(reader) = self.reader_task.lock().take() {
            reader.abort();
        }
    }
}

pub(super) async fn keepalive_task_with_writer(
    writer: Arc<UnifiedWriter>,
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
            super::reader::close_connection(
                &writer,
                &lifecycle,
                &crate::error::MqttError::KeepAliveTimeout,
            )
            .await;
            lifecycle.end(DisconnectReason::KeepAliveTimeout).await;
            lifecycle.stop_reader();
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

        match tokio::time::timeout(timeout_duration, writer.send(Packet::PingReq)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::error!("Error sending PINGREQ: {e}");
                lifecycle
                    .end(DisconnectReason::NetworkError(e.to_string()))
                    .await;
                lifecycle.stop_reader();
                break;
            }
            Err(_) => {
                tracing::error!("PINGREQ send timed out");
                lifecycle
                    .end(DisconnectReason::NetworkError(
                        "PINGREQ send timed out".to_string(),
                    ))
                    .await;
                lifecycle.stop_reader();
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
