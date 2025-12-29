use crate::error::MqttError;
use crate::prelude::*;
use crate::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Reconnecting {
        attempt: u32,
    },
}

impl ConnectionState {
    #[must_use]
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected)
    }

    #[must_use]
    pub fn is_disconnected(&self) -> bool {
        matches!(self, Self::Disconnected)
    }

    #[must_use]
    pub fn is_reconnecting(&self) -> bool {
        matches!(self, Self::Reconnecting { .. })
    }

    #[must_use]
    pub fn reconnect_attempt(&self) -> Option<u32> {
        match self {
            Self::Reconnecting { attempt } => Some(*attempt),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisconnectReason {
    ClientInitiated,
    ServerClosed,
    NetworkError(String),
    ProtocolError(String),
    KeepAliveTimeout,
    AuthFailure,
}

#[derive(Debug, Clone)]
pub enum ConnectionEvent {
    Connecting,
    Connected { session_present: bool },
    Disconnected { reason: DisconnectReason },
    Reconnecting { attempt: u32 },
    ReconnectFailed { error: MqttError },
}

#[derive(Debug, Clone, Default)]
pub struct ConnectionInfo {
    pub session_present: bool,
    pub assigned_client_id: Option<String>,
    pub server_keep_alive: Option<u16>,
}

#[derive(Debug, Clone)]
pub struct ReconnectConfig {
    pub enabled: bool,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub backoff_factor_tenths: u32,
    pub max_attempts: Option<u32>,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            backoff_factor_tenths: 20,
            max_attempts: None,
        }
    }
}

impl ReconnectConfig {
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }

    #[must_use]
    pub fn backoff_factor(&self) -> f64 {
        f64::from(self.backoff_factor_tenths) / 10.0
    }

    pub fn set_backoff_factor(&mut self, factor: f64) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            self.backoff_factor_tenths = (factor * 10.0) as u32;
        }
    }

    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap,
        clippy::cast_precision_loss
    )]
    pub fn calculate_delay(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return self.initial_delay;
        }

        let factor = self.backoff_factor();
        let multiplier = factor.powi(attempt as i32);
        let delay_ms = (self.initial_delay.as_millis() as f64 * multiplier) as u64;
        let max_ms = self.max_delay.as_millis() as u64;

        Duration::from_millis(delay_ms.min(max_ms))
    }

    #[must_use]
    pub fn should_retry(&self, attempt: u32) -> bool {
        if !self.enabled {
            return false;
        }
        match self.max_attempts {
            Some(max) => attempt < max,
            None => true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionStateMachine {
    state: ConnectionState,
    info: ConnectionInfo,
    reconnect_config: ReconnectConfig,
}

impl Default for ConnectionStateMachine {
    fn default() -> Self {
        Self {
            state: ConnectionState::Disconnected,
            info: ConnectionInfo::default(),
            reconnect_config: ReconnectConfig::default(),
        }
    }
}

impl ConnectionStateMachine {
    #[must_use]
    pub fn new(reconnect_config: ReconnectConfig) -> Self {
        Self {
            state: ConnectionState::Disconnected,
            info: ConnectionInfo::default(),
            reconnect_config,
        }
    }

    #[must_use]
    pub fn state(&self) -> ConnectionState {
        self.state
    }

    #[must_use]
    pub fn info(&self) -> &ConnectionInfo {
        &self.info
    }

    #[must_use]
    pub fn reconnect_config(&self) -> &ReconnectConfig {
        &self.reconnect_config
    }

    pub fn set_reconnect_config(&mut self, config: ReconnectConfig) {
        self.reconnect_config = config;
    }

    pub fn transition(&mut self, event: &ConnectionEvent) -> ConnectionState {
        match event {
            ConnectionEvent::Connecting => {
                self.state = ConnectionState::Connecting;
            }
            ConnectionEvent::Connected { session_present } => {
                self.state = ConnectionState::Connected;
                self.info.session_present = *session_present;
            }
            ConnectionEvent::Disconnected { .. } | ConnectionEvent::ReconnectFailed { .. } => {
                self.state = ConnectionState::Disconnected;
                self.info = ConnectionInfo::default();
            }
            ConnectionEvent::Reconnecting { attempt } => {
                self.state = ConnectionState::Reconnecting { attempt: *attempt };
            }
        }
        self.state
    }

    pub fn set_connection_info(&mut self, info: ConnectionInfo) {
        self.info = info;
    }

    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.state.is_connected()
    }

    #[must_use]
    pub fn should_reconnect(&self) -> bool {
        match self.state {
            ConnectionState::Disconnected => self.reconnect_config.enabled,
            ConnectionState::Reconnecting { attempt } => {
                self.reconnect_config.should_retry(attempt + 1)
            }
            _ => false,
        }
    }

    #[must_use]
    pub fn next_reconnect_delay(&self) -> Option<Duration> {
        match self.state {
            ConnectionState::Disconnected => {
                if self.reconnect_config.enabled {
                    Some(self.reconnect_config.calculate_delay(0))
                } else {
                    None
                }
            }
            ConnectionState::Reconnecting { attempt } => {
                if self.reconnect_config.should_retry(attempt + 1) {
                    Some(self.reconnect_config.calculate_delay(attempt))
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_state_default() {
        let state = ConnectionState::default();
        assert!(state.is_disconnected());
    }

    #[test]
    fn test_state_machine_transitions() {
        let mut sm = ConnectionStateMachine::default();

        assert!(sm.state().is_disconnected());

        sm.transition(&ConnectionEvent::Connecting);
        assert_eq!(sm.state(), ConnectionState::Connecting);

        sm.transition(&ConnectionEvent::Connected {
            session_present: true,
        });
        assert!(sm.is_connected());
        assert!(sm.info().session_present);

        sm.transition(&ConnectionEvent::Disconnected {
            reason: DisconnectReason::NetworkError("timeout".into()),
        });
        assert!(sm.state().is_disconnected());
        assert!(!sm.info().session_present);
    }

    #[test]
    fn test_reconnect_delay_calculation() {
        let config = ReconnectConfig {
            enabled: true,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            backoff_factor_tenths: 20,
            max_attempts: Some(5),
        };

        assert_eq!(config.calculate_delay(0), Duration::from_secs(1));
        assert_eq!(config.calculate_delay(1), Duration::from_secs(2));
        assert_eq!(config.calculate_delay(2), Duration::from_secs(4));
        assert_eq!(config.calculate_delay(3), Duration::from_secs(8));
        assert_eq!(config.calculate_delay(4), Duration::from_secs(16));
        assert_eq!(config.calculate_delay(5), Duration::from_secs(30));
    }

    #[test]
    fn test_should_retry() {
        let config = ReconnectConfig {
            enabled: true,
            max_attempts: Some(3),
            ..Default::default()
        };

        assert!(config.should_retry(0));
        assert!(config.should_retry(1));
        assert!(config.should_retry(2));
        assert!(!config.should_retry(3));
        assert!(!config.should_retry(4));
    }

    #[test]
    fn test_disabled_reconnect() {
        let config = ReconnectConfig::disabled();
        assert!(!config.should_retry(0));
    }

    #[test]
    fn test_reconnect_flow() {
        let mut sm = ConnectionStateMachine::new(ReconnectConfig {
            enabled: true,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(10),
            backoff_factor_tenths: 20,
            max_attempts: Some(3),
        });

        sm.transition(&ConnectionEvent::Connecting);
        sm.transition(&ConnectionEvent::Connected {
            session_present: false,
        });
        assert!(sm.is_connected());

        sm.transition(&ConnectionEvent::Disconnected {
            reason: DisconnectReason::NetworkError("connection lost".into()),
        });
        assert!(sm.should_reconnect());

        sm.transition(&ConnectionEvent::Reconnecting { attempt: 0 });
        assert!(sm.state().is_reconnecting());
        assert_eq!(sm.state().reconnect_attempt(), Some(0));
        assert!(sm.should_reconnect());

        sm.transition(&ConnectionEvent::Reconnecting { attempt: 1 });
        assert!(sm.should_reconnect());

        sm.transition(&ConnectionEvent::Reconnecting { attempt: 2 });
        assert!(!sm.should_reconnect());
    }
}
