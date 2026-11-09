//! Bridge connection implementation
//!
//! Manages a single bridge connection to a remote broker using our existing
//! `MqttClient` implementation.

use crate::broker::bridge::{BridgeConfig, BridgeError, BridgeProtocol, BridgeStats, Result};
use crate::broker::router::MessageRouter;
use crate::client::MqttClient;
use crate::error::MqttError;
use crate::packet::publish::PublishPacket;
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::time::{Duration, Instant};
use crate::transport::tls::TlsConfig;
use crate::types::ConnectOptions;
use crate::types::PublishOptions;
use crate::validation::topic_matches_filter;
use crate::{AckToken, IndeterminateReason, PublishOutcome, QoS, SubscribeOptions};
use bytes::Bytes;
use mqtt5_protocol::bridge::{evaluate_forwarding, TopicMappingCore};
use rand::RngExt;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio::sync::{broadcast, mpsc, Notify, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Bound on remote deliveries a bridge holds unacknowledged; also its Receive Maximum.
const INGRESS_LANE_CAPACITY: usize = 1024;
/// How long the remote keeps the bridge's session (and its unacknowledged messages)
/// across a reconnect.
const BRIDGE_SESSION_EXPIRY_SECS: u32 = 3600;

/// One incoming delivery from the remote: every local copy it maps to, plus the token
/// that acknowledges the remote once all of them are routed.
type IngressItem = (Vec<PublishPacket>, AckToken);

/// Delivery lanes from the remote-facing client into the local router, mirroring the
/// client handler's gated and ungated lanes.
struct Ingress {
    qos1_tx: mpsc::Sender<IngressItem>,
    qos0_tx: mpsc::Sender<Vec<PublishPacket>>,
}

/// All incoming mappings that share one remote filter, subscribed once with a
/// subscription identifier so overlapping filters route each copy exactly once.
struct IngressFilter {
    remote_filter: String,
    qos: QoS,
    mappings: Vec<TopicMappingCore>,
}

/// Connect options for the remote-facing client: a persistent deferred-ack session whose
/// receive maximum equals the ingress lane, so the remote never sends more than the bridge
/// can hold unacknowledged. The bridge reconnects by its own loop, so the client doesn't.
fn connect_options(config: &BridgeConfig) -> ConnectOptions {
    let receive_maximum = u16::try_from(INGRESS_LANE_CAPACITY).unwrap_or(u16::MAX);
    let mut options = ConnectOptions::new(&config.client_id)
        .with_automatic_reconnect(false)
        .with_deferred_ack(true)
        .with_clean_start(false)
        .with_resume_existing_session(true)
        .with_session_expiry_interval(BRIDGE_SESSION_EXPIRY_SECS)
        .with_receive_maximum(receive_maximum);
    options.keep_alive = Duration::from_secs(u64::from(config.keepalive));

    if let Some(ref username) = config.username {
        options.username = Some(username.clone());
    }
    if let Some(ref password) = config.password {
        options.password = Some(password.clone().into_bytes());
    }

    if config.try_private {
        options
            .properties
            .user_properties
            .push(("bridge".to_string(), config.name.clone()));
    }

    options
}

fn apply_jitter(delay: Duration, enable: bool) -> Duration {
    if !enable {
        return delay;
    }
    let secs = delay.as_secs_f64();
    let jitter_range = secs * 0.25;
    let jitter = (rand::rng().random::<f64>() - 0.5) * 2.0 * jitter_range;
    Duration::try_from_secs_f64((secs + jitter).max(0.1)).unwrap_or(delay)
}

fn fallback_protocols(config: &BridgeConfig) -> Vec<BridgeProtocol> {
    let tcp_fallback =
        config.fallback_tcp && !config.fallback_protocols.contains(&BridgeProtocol::Tcp);
    config
        .fallback_protocols
        .iter()
        .copied()
        .chain(tcp_fallback.then_some(BridgeProtocol::Tcp))
        .collect()
}

/// Which broker the bridge is currently connected to
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectedBroker {
    /// Connected to the primary broker
    Primary,
    /// Connected to a backup broker (index in `backup_brokers` list)
    Backup(usize),
}

/// A bridge connection to a remote broker
pub struct BridgeConnection {
    /// Bridge configuration
    config: BridgeConfig,
    /// MQTT client for remote connection
    client: Arc<MqttClient>,
    /// Local message router
    router: Arc<MessageRouter>,
    /// Bridge statistics
    stats: Arc<RwLock<BridgeStats>>,
    /// Whether the bridge is running
    running: Arc<AtomicBool>,
    /// Shutdown signal
    shutdown_tx: broadcast::Sender<()>,
    /// Raised by the health check when the primary broker is back, to leave the backup
    failback: Arc<Notify>,
    /// Whether the connection being made follows a failback, so it should reach the primary
    failing_back: AtomicBool,
    /// Failbacks in a row that ended on a backup again, each doubling the wait before the next
    failed_failbacks: AtomicU32,
    /// Message counters for stats
    messages_sent: Arc<AtomicU64>,
    messages_received: Arc<AtomicU64>,
    bytes_sent: Arc<AtomicU64>,
    bytes_received: Arc<AtomicU64>,
    /// Which broker we're currently connected to
    current_broker: Arc<RwLock<Option<ConnectedBroker>>>,
    /// Handle to the health check task (runs when on backup)
    health_check_handle: Arc<RwLock<Option<JoinHandle<()>>>>,
    /// Pending messages to forward when connection is established
    pending_messages: Arc<Mutex<VecDeque<PublishPacket>>>,
    /// Lanes from the remote-facing client into the local router, created on first use
    ingress: OnceLock<Ingress>,
    /// Whether the remote subscriptions have been registered for this connection's lifetime
    subscribed: AtomicBool,
}

impl BridgeConnection {
    /// Creates a new bridge connection.
    ///
    /// # Errors
    /// Returns an error if the configuration is invalid.
    pub fn new(config: BridgeConfig, router: Arc<MessageRouter>) -> Result<Self> {
        config
            .validate()
            .map_err(|e| BridgeError::ConfigurationError(e.to_string()))?;

        let client = Arc::new(MqttClient::with_options(connect_options(&config)));

        let (shutdown_tx, _) = broadcast::channel(1);

        Ok(Self {
            config,
            client,
            router,
            stats: Arc::new(RwLock::new(BridgeStats::default())),
            running: Arc::new(AtomicBool::new(false)),
            shutdown_tx,
            failback: Arc::new(Notify::new()),
            failing_back: AtomicBool::new(false),
            failed_failbacks: AtomicU32::new(0),
            messages_sent: Arc::new(AtomicU64::new(0)),
            messages_received: Arc::new(AtomicU64::new(0)),
            bytes_sent: Arc::new(AtomicU64::new(0)),
            bytes_received: Arc::new(AtomicU64::new(0)),
            current_broker: Arc::new(RwLock::new(None)),
            health_check_handle: Arc::new(RwLock::new(None)),
            pending_messages: Arc::new(Mutex::new(VecDeque::new())),
            ingress: OnceLock::new(),
            subscribed: AtomicBool::new(false),
        })
    }

    /// Starts the bridge connection.
    pub fn start(&self) {
        if self.running.load(Ordering::Relaxed) {
            return;
        }

        self.failing_back.store(false, Ordering::Relaxed);
        self.failed_failbacks.store(0, Ordering::Relaxed);
        self.running.store(true, Ordering::Relaxed);
        info!("Starting bridge '{}'", self.config.name);
    }

    /// Stops the bridge connection.
    ///
    /// # Errors
    /// Returns an error if the stats update fails.
    pub async fn stop(&self) -> Result<()> {
        if !self.running.load(Ordering::Relaxed) {
            return Ok(());
        }

        info!("Stopping bridge '{}'", self.config.name);
        self.running.store(false, Ordering::Relaxed);

        self.stop_health_check().await;

        let _ = self.shutdown_tx.send(());

        if let Err(e) = self.client.disconnect().await {
            debug!(bridge = %self.config.name, error = %e, "bridge client disconnect failed during stop");
        }

        let mut stats = self.stats.write().await;
        stats.connected = false;
        stats.connected_since = None;
        *self.current_broker.write().await = None;

        Ok(())
    }

    /// Builds TLS configuration for a broker address
    fn build_tls_config(&self, address: &str, addr: std::net::SocketAddr) -> Result<TlsConfig> {
        let hostname = if let Some(ref server_name) = self.config.tls_server_name {
            server_name.clone()
        } else {
            let host = address.rsplit_once(':').map_or(address, |(host, _)| host);
            host.strip_prefix('[')
                .and_then(|address| address.strip_suffix(']'))
                .unwrap_or(host)
                .to_string()
        };

        let mut tls_config = TlsConfig::new(addr, hostname);

        if let Some(ref ca_file) = self.config.ca_file {
            tls_config.load_ca_cert_pem(ca_file).map_err(|e| {
                BridgeError::ConfigurationError(format!("Failed to load CA cert: {e}"))
            })?;
        }

        if let Some(ref cert_file) = self.config.client_cert_file {
            tls_config.load_client_cert_pem(cert_file).map_err(|e| {
                BridgeError::ConfigurationError(format!("Failed to load client cert: {e}"))
            })?;
        }

        if let Some(ref key_file) = self.config.client_key_file {
            tls_config.load_client_key_pem(key_file).map_err(|e| {
                BridgeError::ConfigurationError(format!("Failed to load client key: {e}"))
            })?;
        }

        if let Some(insecure) = self.config.insecure {
            tls_config = tls_config.with_verify_server_cert(!insecure);
        }

        if let Some(ref alpn_protocols) = self.config.alpn_protocols {
            let protocols: Vec<&str> = alpn_protocols.iter().map(String::as_str).collect();
            tls_config = tls_config.with_alpn_protocols(&protocols);
        }

        Ok(tls_config)
    }

    async fn configure_quic_tls(&self) -> Result<()> {
        let cert_pem = self
            .config
            .client_cert_file
            .as_ref()
            .map(|cert_file| {
                std::fs::read(cert_file).map_err(|e| {
                    BridgeError::ConfigurationError(format!(
                        "Failed to read client cert file {cert_file}: {e}"
                    ))
                })
            })
            .transpose()?;

        let key_pem = self
            .config
            .client_key_file
            .as_ref()
            .map(|key_file| {
                std::fs::read(key_file).map_err(|e| {
                    BridgeError::ConfigurationError(format!(
                        "Failed to read client key file {key_file}: {e}"
                    ))
                })
            })
            .transpose()?;

        let ca_pem = self
            .config
            .ca_file
            .as_ref()
            .map(|ca_file| {
                std::fs::read(ca_file).map_err(|e| {
                    BridgeError::ConfigurationError(format!(
                        "Failed to read CA cert file {ca_file}: {e}"
                    ))
                })
            })
            .transpose()?;

        if cert_pem.is_some() || key_pem.is_some() || ca_pem.is_some() {
            self.client.set_tls_config(cert_pem, key_pem, ca_pem).await;
        }

        Ok(())
    }

    /// Builds connection options from config
    fn build_connect_options(&self) -> ConnectOptions {
        connect_options(&self.config)
    }

    /// Attempts to connect using TLS to primary and backup brokers
    async fn connect_tls(&self, options: &ConnectOptions) -> Result<ConnectedBroker> {
        match self
            .connect_tls_to(&self.config.remote_address, options)
            .await
        {
            Ok(()) => {
                info!(
                    "Bridge '{}' connected to primary broker: {} (TLS)",
                    self.config.name, self.config.remote_address
                );
                self.update_connected_stats(ConnectedBroker::Primary, &self.config.remote_address)
                    .await;
                return Ok(ConnectedBroker::Primary);
            }
            Err(e) => {
                warn!("Failed to connect to primary broker: {e}");
                self.update_error_stats(e).await;
            }
        }

        for (idx, backup) in self.config.backup_brokers.iter().enumerate() {
            match self.connect_tls_to(backup, options).await {
                Ok(()) => {
                    info!(
                        "Bridge '{}' connected to backup broker: {} (TLS)",
                        self.config.name, backup
                    );
                    self.update_connected_stats(ConnectedBroker::Backup(idx), backup)
                        .await;
                    return Ok(ConnectedBroker::Backup(idx));
                }
                Err(e) => {
                    warn!("Failed to connect to backup broker {}: {}", backup, e);
                    self.update_error_stats(e).await;
                }
            }
        }

        Err(BridgeError::ConnectionFailed(
            "Failed to connect to any broker".to_string(),
        ))
    }

    async fn connect_tls_to(
        &self,
        address: &str,
        options: &ConnectOptions,
    ) -> std::result::Result<(), String> {
        let addrs = tokio::net::lookup_host(address)
            .await
            .map_err(|e| format!("Invalid address: {e}"))?;
        let mut last_error = "Could not resolve address".to_string();
        for addr in addrs {
            let tls_config = self
                .build_tls_config(address, addr)
                .map_err(|e| e.to_string())?;
            match self
                .client
                .connect_with_tls_and_options(tls_config, options.clone())
                .await
            {
                Ok(_) => return Ok(()),
                Err(e) => last_error = e.to_string(),
            }
        }
        Err(last_error)
    }

    /// Attempts to connect without TLS to primary and backup brokers
    async fn connect_plain(&self, options: &ConnectOptions) -> Result<ConnectedBroker> {
        let connection_string = format!("mqtt://{}", self.config.remote_address);
        match Box::pin(
            self.client
                .connect_with_options(&connection_string, options.clone()),
        )
        .await
        {
            Ok(_) => {
                info!(
                    "Bridge '{}' connected to primary broker: {}",
                    self.config.name, self.config.remote_address
                );
                self.update_connected_stats(ConnectedBroker::Primary, &self.config.remote_address)
                    .await;
                return Ok(ConnectedBroker::Primary);
            }
            Err(e) => {
                warn!("Failed to connect to primary broker: {e}");
                self.update_error_stats(e.to_string()).await;
            }
        }

        for (idx, backup) in self.config.backup_brokers.iter().enumerate() {
            let backup_connection_string = format!("mqtt://{backup}");
            match Box::pin(
                self.client
                    .connect_with_options(&backup_connection_string, options.clone()),
            )
            .await
            {
                Ok(_) => {
                    info!(
                        "Bridge '{}' connected to backup broker: {}",
                        self.config.name, backup
                    );
                    self.update_connected_stats(ConnectedBroker::Backup(idx), backup)
                        .await;
                    return Ok(ConnectedBroker::Backup(idx));
                }
                Err(e) => {
                    warn!("Failed to connect to backup broker {}: {}", backup, e);
                    self.update_error_stats(e.to_string()).await;
                }
            }
        }

        Err(BridgeError::ConnectionFailed(
            "Failed to connect to any broker".to_string(),
        ))
    }

    /// Attempts to connect using QUIC to primary and backup brokers
    async fn connect_quic(
        &self,
        options: &ConnectOptions,
        secure: bool,
    ) -> Result<ConnectedBroker> {
        let scheme = if secure { "quics" } else { "quic" };
        let connection_string = format!("{scheme}://{}", self.config.remote_address);

        if !secure || self.config.insecure == Some(true) {
            self.client.set_insecure_tls(true).await;
        }

        if let Some(strategy) = self.config.quic_stream_strategy {
            self.client.set_quic_stream_strategy(strategy).await;
        }
        if let Some(enable) = self.config.quic_flow_headers {
            self.client.set_quic_flow_headers(enable).await;
        }
        if let Some(enable) = self.config.quic_datagrams {
            self.client.set_quic_datagrams(enable).await;
        }
        if let Some(server_name) = &self.config.tls_server_name {
            let host = self
                .config
                .remote_address
                .rsplit_once(':')
                .map_or(self.config.remote_address.as_str(), |(host, _)| host);
            self.client.set_quic_server_name(host, server_name).await;
        }
        if let Some(max) = self.config.quic_max_streams {
            self.client.set_quic_max_streams(Some(max)).await;
        }

        self.configure_quic_tls().await?;

        match Box::pin(
            self.client
                .connect_with_options(&connection_string, options.clone()),
        )
        .await
        {
            Ok(_) => {
                info!(
                    "Bridge '{}' connected to primary broker: {} (QUIC{})",
                    self.config.name,
                    self.config.remote_address,
                    if secure { "S" } else { "" }
                );
                self.update_connected_stats(ConnectedBroker::Primary, &self.config.remote_address)
                    .await;
                return Ok(ConnectedBroker::Primary);
            }
            Err(e) => {
                warn!("Failed to connect to primary broker via QUIC: {e}");
                self.update_error_stats(e.to_string()).await;
            }
        }

        for (idx, backup) in self.config.backup_brokers.iter().enumerate() {
            let backup_connection_string = format!("{scheme}://{backup}");
            match Box::pin(
                self.client
                    .connect_with_options(&backup_connection_string, options.clone()),
            )
            .await
            {
                Ok(_) => {
                    info!(
                        "Bridge '{}' connected to backup broker: {} (QUIC{})",
                        self.config.name,
                        backup,
                        if secure { "S" } else { "" }
                    );
                    self.update_connected_stats(ConnectedBroker::Backup(idx), backup)
                        .await;
                    return Ok(ConnectedBroker::Backup(idx));
                }
                Err(e) => {
                    warn!(
                        "Failed to connect to backup broker {} via QUIC: {}",
                        backup, e
                    );
                    self.update_error_stats(e.to_string()).await;
                }
            }
        }

        Err(BridgeError::ConnectionFailed(
            "Failed to connect to any broker via QUIC".to_string(),
        ))
    }

    /// Attempts connection with a specific protocol
    async fn connect_with_protocol(
        &self,
        protocol: BridgeProtocol,
        options: &ConnectOptions,
    ) -> Result<ConnectedBroker> {
        match protocol {
            BridgeProtocol::Tcp => {
                if self.config.use_tls {
                    Box::pin(self.connect_tls(options)).await
                } else {
                    Box::pin(self.connect_plain(options)).await
                }
            }
            BridgeProtocol::Tls => Box::pin(self.connect_tls(options)).await,
            BridgeProtocol::Quic | BridgeProtocol::QuicSecure => {
                Box::pin(self.connect_quic(options, true)).await
            }
        }
    }

    async fn try_all_protocols(
        &self,
        options: &ConnectOptions,
    ) -> Result<(ConnectedBroker, Vec<BridgeProtocol>)> {
        let max_retries = self.config.connection_retries.max(1);

        for attempt in 1..=max_retries {
            if let Ok(broker) = self
                .connect_with_protocol(self.config.protocol, options)
                .await
            {
                return Ok((broker, vec![self.config.protocol]));
            }

            if attempt < max_retries {
                let delay = apply_jitter(self.config.first_retry_delay, self.config.retry_jitter);
                debug!(
                    "Bridge '{}' {:?} attempt {}/{} failed, retrying in {:?}",
                    self.config.name, self.config.protocol, attempt, max_retries, delay
                );
                tokio::time::sleep(delay).await;
            }
        }

        let fallbacks = fallback_protocols(&self.config);
        if !fallbacks.is_empty() {
            warn!(
                "Bridge '{}' primary protocol {:?} failed after {} attempts, trying fallbacks",
                self.config.name, self.config.protocol, max_retries
            );
        }

        for (index, &fallback) in fallbacks.iter().enumerate() {
            if let Ok(broker) = self.connect_with_protocol(fallback, options).await {
                info!(
                    "Bridge '{}' connected via fallback protocol {:?}",
                    self.config.name, fallback
                );
                let tried = std::iter::once(self.config.protocol)
                    .chain(fallbacks[..=index].iter().copied())
                    .collect();
                return Ok((broker, tried));
            }
        }

        Err(BridgeError::ConnectionFailed(format!(
            "All protocols failed for bridge '{}'",
            self.config.name
        )))
    }

    async fn connect(&self) -> Result<ConnectedBroker> {
        let options = self.build_connect_options();
        let failing_back = self.failing_back.swap(false, Ordering::Relaxed);
        let mut attempt = 0u32;

        loop {
            attempt += 1;

            {
                let mut stats = self.stats.write().await;
                stats.connection_attempts += 1;
            }

            match self.try_all_protocols(&options).await {
                Ok((broker, tried)) => {
                    if matches!(broker, ConnectedBroker::Primary) {
                        self.failed_failbacks.store(0, Ordering::Relaxed);
                    } else if failing_back {
                        self.failed_failbacks.fetch_add(1, Ordering::Relaxed);
                    }
                    if matches!(broker, ConnectedBroker::Backup(_)) && self.config.enable_failback {
                        self.start_health_check(tried).await;
                    }
                    return Ok(broker);
                }
                Err(e) => {
                    if let Some(max) = self.config.max_reconnect_attempts {
                        if attempt >= max {
                            error!(
                                "Bridge '{}' exceeded max connection attempts ({})",
                                self.config.name, max
                            );
                            return Err(e);
                        }
                    }

                    let base_delay = if attempt == 1 {
                        self.config.first_retry_delay
                    } else {
                        let exponent = attempt.saturating_sub(2).min(30);
                        let delay_secs = self.config.initial_reconnect_delay.as_secs_f64()
                            * self.config.backoff_multiplier.powf(f64::from(exponent));
                        Duration::try_from_secs_f64(delay_secs)
                            .unwrap_or(self.config.max_reconnect_delay)
                            .max(self.config.initial_reconnect_delay)
                            .min(self.config.max_reconnect_delay)
                    };

                    let delay = apply_jitter(base_delay, self.config.retry_jitter);
                    warn!(
                        "Bridge '{}' connection attempt {} failed: {}, retrying in {:?}",
                        self.config.name, attempt, e, delay
                    );
                    tokio::time::sleep(delay).await;

                    if !self.running.load(Ordering::Relaxed) {
                        return Err(BridgeError::ConnectionFailed("Bridge stopped".to_string()));
                    }
                }
            }
        }
    }

    async fn start_health_check(&self, protocols: Vec<BridgeProtocol>) {
        self.stop_health_check().await;

        let config = self.config.clone();
        let running = self.running.clone();
        let failback = Arc::clone(&self.failback);
        let current_broker = self.current_broker.clone();
        let stats = self.stats.clone();

        let period = self.health_check_period();
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(period);
            interval.tick().await;

            while running.load(Ordering::Relaxed) {
                interval.tick().await;

                if !running.load(Ordering::Relaxed) {
                    break;
                }

                if let Some(ConnectedBroker::Primary) = *current_broker.read().await {
                    debug!("Health check: already on primary, stopping");
                    break;
                }

                if Self::probe_broker(&config.remote_address, &config.name, &protocols).await {
                    info!(
                        "Bridge '{}': primary broker {} is available, triggering failback",
                        config.name, config.remote_address
                    );

                    {
                        let mut stats = stats.write().await;
                        stats.failback_attempts += 1;
                    }

                    failback.notify_one();
                    break;
                }
            }
        });

        *self.health_check_handle.write().await = Some(handle);
    }

    /// The wait between probes of the primary: the configured interval, doubled for each
    /// failback in a row that ended on a backup again, up to `max_reconnect_delay` but never
    /// below the configured interval.
    fn health_check_period(&self) -> Duration {
        let base = self.config.primary_health_check_interval;
        let cap = self.config.max_reconnect_delay.max(base);
        (0..self.failed_failbacks.load(Ordering::Relaxed))
            .try_fold(base, |period, _| {
                (period < cap).then(|| period.saturating_mul(2).min(cap))
            })
            .unwrap_or(cap)
    }

    async fn stop_health_check(&self) {
        if let Some(handle) = self.health_check_handle.write().await.take() {
            handle.abort();
        }
    }

    /// Whether the broker at `address` accepts a connection over one of `protocols`, the
    /// protocols the bridge tried up to and including the one that reached its backup, at
    /// each address the name resolves to. A TCP or TLS protocol is probed with a TCP
    /// connection, a QUIC one with a QUIC handshake that verifies no certificate, since it
    /// sends nothing; the failback connection that follows does.
    async fn probe_broker(address: &str, name: &str, protocols: &[BridgeProtocol]) -> bool {
        let Ok(addrs) = tokio::net::lookup_host(address).await else {
            return false;
        };
        for addr in addrs {
            for &protocol in protocols {
                if Self::accepts(protocol, addr).await {
                    debug!(
                        "Bridge '{}': primary broker {} responded to probe",
                        name, address
                    );
                    return true;
                }
            }
        }
        false
    }

    async fn accepts(protocol: BridgeProtocol, addr: std::net::SocketAddr) -> bool {
        match protocol {
            BridgeProtocol::Tcp | BridgeProtocol::Tls => Self::accepts_tcp(addr).await,
            #[cfg(feature = "transport-quic")]
            BridgeProtocol::Quic | BridgeProtocol::QuicSecure => Self::accepts_quic(addr).await,
            #[cfg(not(feature = "transport-quic"))]
            BridgeProtocol::Quic | BridgeProtocol::QuicSecure => false,
        }
    }

    async fn accepts_tcp(addr: std::net::SocketAddr) -> bool {
        matches!(
            tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect(addr)).await,
            Ok(Ok(_))
        )
    }

    #[cfg(feature = "transport-quic")]
    async fn accepts_quic(addr: std::net::SocketAddr) -> bool {
        use crate::transport::{QuicConfig, QuicTransport};
        use crate::Transport;

        let mut transport = QuicTransport::new(
            QuicConfig::new(addr, "localhost")
                .with_verify_server_cert(false)
                .with_connect_timeout(PROBE_TIMEOUT),
        );
        let accepted = matches!(
            tokio::time::timeout(PROBE_TIMEOUT, transport.connect()).await,
            Ok(Ok(()))
        );
        let _ = transport.close().await;
        accepted
    }

    #[must_use]
    pub fn current_broker(&self) -> Arc<RwLock<Option<ConnectedBroker>>> {
        self.current_broker.clone()
    }

    pub async fn current_broker_address(&self) -> Option<String> {
        let broker = self.current_broker.read().await;
        match *broker {
            Some(ConnectedBroker::Primary) => Some(self.config.remote_address.clone()),
            Some(ConnectedBroker::Backup(idx)) => self.config.backup_brokers.get(idx).cloned(),
            None => None,
        }
    }

    /// Groups the incoming mappings by remote filter, in configuration order.
    fn ingress_filters(&self) -> Vec<IngressFilter> {
        let mut filters: Vec<IngressFilter> = Vec::new();
        for mapping in &self.config.topics {
            let core = TopicMappingCore::from(mapping);
            if !core.direction.allows_incoming() {
                continue;
            }
            let remote_filter = core.apply_remote_prefix(&core.pattern);
            match filters
                .iter_mut()
                .find(|filter| filter.remote_filter == remote_filter)
            {
                Some(filter) => {
                    if core.qos as u8 > filter.qos as u8 {
                        filter.qos = core.qos;
                    }
                    filter.mappings.push(core);
                }
                None => filters.push(IngressFilter {
                    remote_filter,
                    qos: core.qos,
                    mappings: vec![core],
                }),
            }
        }
        filters
    }

    fn ingress(&self) -> &Ingress {
        self.ingress.get_or_init(|| {
            let (qos1_tx, mut qos1_rx) = mpsc::channel::<IngressItem>(INGRESS_LANE_CAPACITY);
            let (qos0_tx, mut qos0_rx) = mpsc::channel::<Vec<PublishPacket>>(INGRESS_LANE_CAPACITY);
            let router = Arc::clone(&self.router);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        biased;
                        item = qos1_rx.recv() => {
                            let Some((packets, token)) = item else { break };
                            for packet in &packets {
                                router.route_message_local_only(packet, None).await;
                            }
                            token.ack();
                        }
                        item = qos0_rx.recv() => {
                            let Some(packets) = item else { break };
                            for packet in &packets {
                                router.route_message_local_only(packet, None).await;
                            }
                        }
                    }
                }
            });
            Ingress { qos1_tx, qos0_tx }
        })
    }

    /// Registers the ingress callback for every remote filter before the first connect, so a
    /// backlog the remote flushes right after CONNACK (it may still hold this bridge's
    /// session) is routed instead of being auto-acknowledged and lost.
    async fn register_ingress_callbacks(&self) -> Result<()> {
        let filters = Arc::new(self.ingress_filters());
        let ingress = self.ingress();
        for filter in filters.iter() {
            let callback = self.ingress_callback(
                Arc::clone(&filters),
                ingress.qos1_tx.clone(),
                ingress.qos0_tx.clone(),
            );
            self.client
                .register_ack_callback(filter.remote_filter.clone(), callback)
                .await?;
        }
        Ok(())
    }

    /// Subscribes once per distinct remote filter for the bridge's lifetime; the client
    /// restores them itself after a lost session.
    async fn setup_subscriptions(&self) -> Result<()> {
        if self.subscribed.load(Ordering::Acquire) {
            return Ok(());
        }
        let filters = Arc::new(self.ingress_filters());
        let ingress = self.ingress();

        for (index, filter) in filters.iter().enumerate() {
            let subscription_id = u32::try_from(index + 1).unwrap_or(u32::MAX);
            let callback = self.ingress_callback(
                Arc::clone(&filters),
                ingress.qos1_tx.clone(),
                ingress.qos0_tx.clone(),
            );
            let options = SubscribeOptions {
                qos: filter.qos,
                subscription_identifier: Some(subscription_id),
                ..SubscribeOptions::default()
            };
            self.client
                .subscribe_with_ack(filter.remote_filter.clone(), options, callback)
                .await?;
            info!(
                "Bridge '{}' subscribed to remote topic: {} (QoS: {:?}, {} mapping(s))",
                self.config.name,
                filter.remote_filter,
                filter.qos,
                filter.mappings.len()
            );
        }
        self.subscribed.store(true, Ordering::Release);

        Ok(())
    }

    /// The callback every remote filter shares: maps one remote delivery to its local
    /// copies, then hands them (with the token) to the routing task.
    fn ingress_callback(
        &self,
        filters: Arc<Vec<IngressFilter>>,
        qos1_tx: mpsc::Sender<IngressItem>,
        qos0_tx: mpsc::Sender<Vec<PublishPacket>>,
    ) -> impl Fn(PublishPacket, AckToken) + Send + Sync + 'static {
        let bridge_name = self.config.name.clone();
        let stats_received = Arc::clone(&self.messages_received);
        let stats_bytes = Arc::clone(&self.bytes_received);
        let warned_missing_ids = Arc::new(AtomicBool::new(false));

        move |mut publish, token| {
            stats_received.fetch_add(1, Ordering::Relaxed);
            stats_bytes.fetch_add(publish.payload.len() as u64, Ordering::Relaxed);

            let ids = publish.properties.subscription_identifiers();
            publish.properties.remove_subscription_identifiers();
            publish.properties.inject_sender(None);
            publish.properties.inject_client_id(None);

            let mappings: Vec<&TopicMappingCore> = if ids.is_empty() {
                if !warned_missing_ids.swap(true, Ordering::Relaxed) {
                    warn!(
                        bridge = %bridge_name,
                        "Remote sends no subscription identifiers; overlapping mappings may deliver copies"
                    );
                }
                filters
                    .iter()
                    .filter(|filter| {
                        topic_matches_filter(&publish.topic_name, &filter.remote_filter)
                    })
                    .flat_map(|filter| filter.mappings.iter())
                    .collect()
            } else {
                ids.iter()
                    .filter_map(|id| usize::try_from(*id).ok())
                    .filter_map(|id| id.checked_sub(1))
                    .filter_map(|index| filters.get(index))
                    .flat_map(|filter| filter.mappings.iter())
                    .collect()
            };

            let packets: Vec<PublishPacket> = mappings
                .into_iter()
                .map(|mapping| {
                    let mut packet = publish.clone();
                    packet.topic_name = mapping.apply_local_prefix(&publish.topic_name);
                    packet
                })
                .collect();

            if publish.qos == QoS::AtMostOnce {
                if qos0_tx.try_send(packets).is_err() {
                    debug!(bridge = %bridge_name, "Dropping QoS 0 remote delivery: ingress lane full");
                }
                token.ack();
                return;
            }
            match qos1_tx.try_send((packets, token)) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full((_, token))) => {
                    warn!(bridge = %bridge_name, "Ingress lane full; rejecting remote delivery");
                    token.reject(ReasonCode::QuotaExceeded);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    debug!(bridge = %bridge_name, "Bridge stopping; remote delivery dropped");
                }
            }
        }
    }

    /// Forwards a message to the remote broker.
    ///
    /// # Errors
    /// Returns an error if the message forwarding fails.
    pub async fn forward_message(&self, packet: &PublishPacket) -> Result<()> {
        let bridge_name = &self.config.name;
        let is_running = self.running.load(Ordering::Relaxed);
        let is_connected = self.client.is_connected().await;

        debug!(
            bridge = %bridge_name,
            topic = %packet.topic_name,
            running = is_running,
            connected = is_connected,
            "forward_message called"
        );

        if !is_running {
            return Ok(());
        }

        if !is_connected {
            self.queue_pending_message(packet.clone());
            return Ok(());
        }

        let mappings: Vec<TopicMappingCore> = self
            .config
            .topics
            .iter()
            .map(TopicMappingCore::from)
            .collect();
        let Some(decision) = evaluate_forwarding(&packet.topic_name, &mappings, true) else {
            debug!(
                bridge = %bridge_name,
                topic = %packet.topic_name,
                "no matching topic pattern found"
            );
            return Ok(());
        };

        let remote_topic = decision.transformed_topic;
        debug!(
            bridge = %bridge_name,
            local_topic = %packet.topic_name,
            remote_topic = %remote_topic,
            "topic matched, spawning publish task"
        );

        let msg_props: crate::types::MessageProperties = packet.properties.clone().into();
        let options = PublishOptions {
            qos: decision.qos,
            retain: packet.retain,
            properties: msg_props.into(),
            skip_codec: false,
        };

        let client = Arc::downgrade(&self.client);
        let payload = packet.payload.clone();
        let messages_sent = self.messages_sent.clone();
        let bytes_sent = self.bytes_sent.clone();
        let payload_len = payload.len();
        let bridge_name_clone = bridge_name.clone();

        tokio::spawn(async move {
            debug!(
                bridge = %bridge_name_clone,
                topic = %remote_topic,
                "publish task started"
            );
            match Self::publish_to_remote(&client, &remote_topic, &payload, &options).await {
                Ok(PublishOutcome::Delivered(_)) => {
                    debug!(
                        bridge = %bridge_name_clone,
                        topic = %remote_topic,
                        "publish delivered"
                    );
                    messages_sent.fetch_add(1, Ordering::Relaxed);
                    bytes_sent.fetch_add(payload_len as u64, Ordering::Relaxed);
                }
                Ok(outcome) => {
                    warn!(
                        bridge = %bridge_name_clone,
                        topic = %remote_topic,
                        outcome = ?outcome,
                        "publish not delivered"
                    );
                }
                Err(e) => {
                    error!(
                        bridge = %bridge_name_clone,
                        topic = %remote_topic,
                        error = %e,
                        "publish failed"
                    );
                }
            }
        });

        Ok(())
    }

    /// Publishes to the remote broker until the message is settled there. A publish cut
    /// off by a disconnect goes to the client's offline queue, and a `QoS` 2 message that
    /// the new connection's server has no session for is published again.
    async fn publish_to_remote(
        client: &Weak<MqttClient>,
        topic: &str,
        payload: &Bytes,
        options: &PublishOptions,
    ) -> std::result::Result<PublishOutcome, MqttError> {
        loop {
            let Some(live) = client.upgrade() else {
                return Ok(PublishOutcome::Indeterminate(
                    IndeterminateReason::Abandoned,
                ));
            };
            let published = live
                .publish_with_options(topic, payload.to_vec(), options.clone())
                .await;
            drop(live);
            match published {
                Err(MqttError::NotConnected | MqttError::FlowControlExceeded)
                    if options.qos != QoS::AtMostOnce => {}
                Err(e) => return Err(e),
                Ok(published) => match published.outcome().await {
                    PublishOutcome::Indeterminate(IndeterminateReason::SessionLost) => {}
                    outcome => return Ok(outcome),
                },
            }
        }
    }

    /// Updates stats when connected
    async fn update_connected_stats(&self, broker: ConnectedBroker, address: &str) {
        let mut stats = self.stats.write().await;
        stats.connected = true;
        stats.connected_since = Some(Instant::now());
        stats.last_error = None;
        stats.current_broker = Some(address.to_string());
        stats.on_primary = matches!(broker, ConnectedBroker::Primary);

        *self.current_broker.write().await = Some(broker);

        self.flush_pending_messages().await;
    }

    fn queue_pending_message(&self, packet: PublishPacket) {
        const MAX_PENDING: usize = 1000;
        let Ok(mut queue) = self.pending_messages.lock() else {
            return;
        };
        if queue.len() < MAX_PENDING {
            queue.push_back(packet);
        } else {
            warn!(
                bridge = %self.config.name,
                "pending message queue full, dropping oldest message"
            );
            queue.pop_front();
            queue.push_back(packet);
        }
    }

    /// Flushes pending messages that were queued while disconnected
    async fn flush_pending_messages(&self) {
        let pending: Vec<PublishPacket> = {
            let Ok(mut queue) = self.pending_messages.lock() else {
                return;
            };
            queue.drain(..).collect()
        };

        if pending.is_empty() {
            return;
        }

        info!(
            bridge = %self.config.name,
            count = pending.len(),
            "flushing pending messages after connection established"
        );

        for packet in pending {
            if let Err(e) = self.forward_message(&packet).await {
                warn!(
                    bridge = %self.config.name,
                    topic = %packet.topic_name,
                    error = %e,
                    "failed to forward pending message"
                );
            }
        }
    }

    /// Updates stats when an error occurs
    async fn update_error_stats(&self, error: String) {
        let mut stats = self.stats.write().await;
        stats.connected = false;
        stats.connected_since = None;
        stats.last_error = Some(error);
    }

    /// Gets current statistics
    pub async fn get_stats(&self) -> BridgeStats {
        let mut stats = self.stats.read().await.clone();
        stats.messages_sent = self.messages_sent.load(Ordering::Relaxed);
        stats.messages_received = self.messages_received.load(Ordering::Relaxed);
        stats.bytes_sent = self.bytes_sent.load(Ordering::Relaxed);
        stats.bytes_received = self.bytes_received.load(Ordering::Relaxed);
        stats
    }

    /// Runs the bridge connection with automatic reconnection.
    ///
    /// # Errors
    /// Returns an error if the maximum reconnect attempts is exceeded.
    pub async fn run(&self) -> Result<()> {
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        while self.running.load(Ordering::Relaxed) {
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    info!("Bridge '{}' received shutdown signal", self.config.name);
                    break;
                }
                () = self.failback.notified() => {
                    if matches!(*self.current_broker.read().await, Some(ConnectedBroker::Backup(_))) {
                        self.leave_backup().await;
                    }
                }
                result = self.run_connection() => {
                    if !self.running.load(Ordering::Relaxed) {
                        break;
                    }

                    result?;
                }
            }
        }

        Ok(())
    }

    /// Ends the connection to a backup broker so that the next one tries the primary first.
    async fn leave_backup(&self) {
        info!(
            "Bridge '{}' failing back to the primary broker",
            self.config.name
        );
        self.failing_back.store(true, Ordering::Relaxed);
        if let Err(e) = self.client.disconnect().await {
            debug!(bridge = %self.config.name, error = %e, "bridge client disconnect failed during failback");
        }
        *self.current_broker.write().await = None;
        let mut stats = self.stats.write().await;
        stats.connected = false;
        stats.connected_since = None;
    }

    /// Runs a single connection until disconnected
    async fn run_connection(&self) -> Result<()> {
        if !self.client.is_connected().await {
            self.register_ingress_callbacks().await?;
            Box::pin(self.connect()).await?;
            self.setup_subscriptions().await?;
        }

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

            if !self.client.is_connected().await {
                warn!(
                    "Bridge '{}' disconnected from remote broker",
                    self.config.name
                );
                self.stop_health_check().await;
                *self.current_broker.write().await = None;
                let mut stats = self.stats.write().await;
                stats.connected = false;
                stats.connected_since = None;
                break;
            }

            if !self.running.load(Ordering::Relaxed) {
                break;
            }
        }

        Ok(())
    }
}
