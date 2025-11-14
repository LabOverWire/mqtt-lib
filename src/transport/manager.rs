use crate::error::{MqttError, Result};
use crate::time::{Duration, Instant};
use crate::transport::Transport;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio::time::interval;

/// Transport connection statistics
#[derive(Debug, Clone, Default)]
pub struct ConnectionStats {
    /// Total number of connections established
    pub connections_established: u64,
    /// Total number of connection failures
    pub connection_failures: u64,
    /// Total bytes sent
    pub bytes_sent: u64,
    /// Total bytes received
    pub bytes_received: u64,
    /// Time of last successful connection
    pub last_connected: Option<Instant>,
    /// Time of last disconnection
    pub last_disconnected: Option<Instant>,
    /// Current connection uptime
    pub uptime: Option<Duration>,
}

/// Transport connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Not connected
    Disconnected,
    /// Currently connecting
    Connecting,
    /// Connected and ready
    Connected,
    /// Connection is closing
    Closing,
}

/// Transport manager configuration
#[derive(Debug, Clone)]
pub struct ManagerConfig {
    /// Enable automatic reconnection
    pub auto_reconnect: bool,
    /// Initial reconnect delay
    pub reconnect_delay: Duration,
    /// Maximum reconnect delay
    pub max_reconnect_delay: Duration,
    /// Reconnect delay multiplier (for exponential backoff)
    pub reconnect_multiplier: f64,
    /// Connection keep-alive interval
    pub keep_alive_interval: Option<Duration>,
    /// Connection idle timeout (disconnect after no activity)
    pub idle_timeout: Option<Duration>,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            auto_reconnect: true,
            reconnect_delay: Duration::from_secs(1),
            max_reconnect_delay: Duration::from_secs(60),
            reconnect_multiplier: 2.0,
            keep_alive_interval: Some(Duration::from_secs(30)),
            idle_timeout: Some(Duration::from_secs(300)),
        }
    }
}

/// Transport connection manager
pub struct TransportManager<T: Transport> {
    transport: Arc<Mutex<T>>,
    config: ManagerConfig,
    state: Arc<RwLock<ConnectionState>>,
    stats: Arc<RwLock<ConnectionStats>>,
    last_activity: Arc<RwLock<Instant>>,
    reconnect_delay: Arc<RwLock<Duration>>,
}

impl<T: Transport + 'static> TransportManager<T> {
    /// Creates a new transport manager
    pub fn new(transport: T, config: ManagerConfig) -> Self {
        let initial_delay = config.reconnect_delay;
        Self {
            transport: Arc::new(Mutex::new(transport)),
            config,
            state: Arc::new(RwLock::new(ConnectionState::Disconnected)),
            stats: Arc::new(RwLock::new(ConnectionStats::default())),
            last_activity: Arc::new(RwLock::new(Instant::now())),
            reconnect_delay: Arc::new(RwLock::new(initial_delay)),
        }
    }

    /// Gets the current connection state
    pub async fn state(&self) -> ConnectionState {
        *self.state.read().await
    }

    /// Gets connection statistics
    pub async fn stats(&self) -> ConnectionStats {
        let mut stats = self.stats.read().await.clone();

        // Update uptime if connected
        if *self.state.read().await == ConnectionState::Connected {
            if let Some(connected_at) = stats.last_connected {
                stats.uptime = Some(connected_at.elapsed());
            }
        }

        stats
    }

    /// Connects the transport with automatic retry
    ///
    /// # Errors
    ///
    /// Returns an error if connection fails or is already in progress
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn connect(&self) -> Result<()> {
        // Check current state
        let current_state = *self.state.read().await;
        match current_state {
            ConnectionState::Connected => return Ok(()),
            ConnectionState::Connecting => {
                return Err(MqttError::ConnectionError(
                    "Connection already in progress".to_string(),
                ))
            }
            _ => {}
        }

        // Set state to connecting
        *self.state.write().await = ConnectionState::Connecting;

        // Try to connect
        let mut transport = self.transport.lock().await;
        match transport.connect().await {
            Ok(()) => {
                // Update state and stats
                *self.state.write().await = ConnectionState::Connected;
                let mut stats = self.stats.write().await;
                stats.connections_established += 1;
                stats.last_connected = Some(Instant::now());

                // Reset reconnect delay
                *self.reconnect_delay.write().await = self.config.reconnect_delay;

                // Update last activity
                *self.last_activity.write().await = Instant::now();

                Ok(())
            }
            Err(e) => {
                // Update state and stats
                *self.state.write().await = ConnectionState::Disconnected;
                let mut stats = self.stats.write().await;
                stats.connection_failures += 1;

                Err(e)
            }
        }
    }

    /// Disconnects the transport
    ///
    /// # Errors
    ///
    /// Returns an error if the transport fails to close
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn disconnect(&self) -> Result<()> {
        // Set state to closing
        *self.state.write().await = ConnectionState::Closing;

        // Close transport
        let mut transport = self.transport.lock().await;
        let result = transport.close().await;

        // Update state and stats
        *self.state.write().await = ConnectionState::Disconnected;
        let mut stats = self.stats.write().await;
        stats.last_disconnected = Some(Instant::now());
        stats.uptime = None;

        result
    }

    /// Reads data from the transport
    ///
    /// # Errors
    ///
    /// Returns an error if not connected or read fails
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn read(&self, buf: &mut [u8]) -> Result<usize> {
        // Check state
        if *self.state.read().await != ConnectionState::Connected {
            return Err(MqttError::NotConnected);
        }

        // Read from transport
        let mut transport = self.transport.lock().await;
        let result = transport.read(buf).await;

        // Update activity and stats on success
        if let Ok(n) = result {
            *self.last_activity.write().await = Instant::now();
            self.stats.write().await.bytes_received = self
                .stats
                .read()
                .await
                .bytes_received
                .saturating_add(n as u64);
        }

        result
    }

    /// Writes data to the transport
    ///
    /// # Errors
    ///
    /// Returns an error if not connected or write fails
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub async fn write(&self, buf: &[u8]) -> Result<()> {
        // Check state
        if *self.state.read().await != ConnectionState::Connected {
            return Err(MqttError::NotConnected);
        }

        // Write to transport
        let mut transport = self.transport.lock().await;
        let result = transport.write(buf).await;

        // Update activity and stats on success
        if result.is_ok() {
            *self.last_activity.write().await = Instant::now();
            self.stats.write().await.bytes_sent = self
                .stats
                .read()
                .await
                .bytes_sent
                .saturating_add(buf.len() as u64);
        }

        result
    }

    /// Starts the connection manager background tasks
    pub fn start_background_tasks(self: Arc<Self>) {
        // Start keep-alive task
        if let Some(keep_alive_interval) = self.config.keep_alive_interval {
            let manager = Arc::clone(&self);
            tokio::spawn(async move {
                let mut ticker = interval(keep_alive_interval);
                ticker.tick().await; // Skip first immediate tick

                loop {
                    ticker.tick().await;

                    // Only send keep-alive if connected
                    if *manager.state.read().await == ConnectionState::Connected {
                        // In a real implementation, this would send MQTT PINGREQ
                        // For now, just update activity time
                        *manager.last_activity.write().await = Instant::now();
                    }
                }
            });
        }

        // Start idle timeout task
        if let Some(idle_timeout) = self.config.idle_timeout {
            let manager = Arc::clone(&self);
            tokio::spawn(async move {
                let mut ticker = interval(Duration::from_secs(10)); // Check every 10 seconds
                ticker.tick().await;

                loop {
                    ticker.tick().await;

                    if *manager.state.read().await == ConnectionState::Connected {
                        let last_activity = *manager.last_activity.read().await;
                        if last_activity.elapsed() > idle_timeout {
                            // Disconnect due to idle timeout
                            if let Err(e) = manager.disconnect().await {
                                tracing::warn!("Failed to disconnect on idle timeout: {}", e);
                            }
                        }
                    }
                }
            });
        }

        // Start auto-reconnect task
        if self.config.auto_reconnect {
            let manager = Arc::clone(&self);
            tokio::spawn(async move {
                let mut ticker = interval(Duration::from_secs(1));
                ticker.tick().await;

                loop {
                    ticker.tick().await;

                    if *manager.state.read().await == ConnectionState::Disconnected {
                        // Wait for reconnect delay
                        let delay = *manager.reconnect_delay.read().await;
                        tokio::time::sleep(delay).await;

                        // Try to reconnect
                        if manager.connect().await.is_err() {
                            // Increase delay with exponential backoff
                            let mut new_delay = manager.reconnect_delay.write().await;
                            *new_delay = Duration::from_secs_f64(
                                (new_delay.as_secs_f64() * manager.config.reconnect_multiplier)
                                    .min(manager.config.max_reconnect_delay.as_secs_f64()),
                            );
                        }
                    }
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::tcp::{TcpConfig, TcpTransport};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    #[tokio::test]
    async fn test_manager_creation() {
        let transport =
            TcpTransport::from_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1883));
        let manager = TransportManager::new(transport, ManagerConfig::default());

        assert_eq!(manager.state().await, ConnectionState::Disconnected);

        let stats = manager.stats().await;
        assert_eq!(stats.connections_established, 0);
        assert_eq!(stats.connection_failures, 0);
    }

    #[tokio::test]
    async fn test_manager_connect_not_available() {
        // Use a non-routable address
        let transport = TcpTransport::new(
            TcpConfig::new(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), // TEST-NET-1 address
                1883,
            ))
            .with_connect_timeout(Duration::from_millis(100)),
        );

        let manager = TransportManager::new(transport, ManagerConfig::default());
        let result = manager.connect().await;

        assert!(result.is_err());
        assert_eq!(manager.state().await, ConnectionState::Disconnected);

        let stats = manager.stats().await;
        assert_eq!(stats.connection_failures, 1);
    }

    #[tokio::test]
    async fn test_manager_read_write_not_connected() {
        let transport =
            TcpTransport::from_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1883));
        let manager = TransportManager::new(transport, ManagerConfig::default());

        let mut buf = [0u8; 10];
        assert!(manager.read(&mut buf).await.is_err());
        assert!(manager.write(b"test").await.is_err());
    }

    #[test]
    fn test_manager_config_default() {
        let config = ManagerConfig::default();
        assert!(config.auto_reconnect);
        assert_eq!(config.reconnect_delay, Duration::from_secs(1));
        assert_eq!(config.max_reconnect_delay, Duration::from_secs(60));
        assert!((config.reconnect_multiplier - 2.0).abs() < f64::EPSILON);
        assert_eq!(config.keep_alive_interval, Some(Duration::from_secs(30)));
        assert_eq!(config.idle_timeout, Some(Duration::from_secs(300)));
    }
}
