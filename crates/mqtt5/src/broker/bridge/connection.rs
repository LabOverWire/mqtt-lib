//! Bridge connection implementation
//!
//! Manages a single bridge connection to a remote broker using our existing
//! `MqttClient` implementation.

use crate::broker::bridge::{BridgeConfig, BridgeError, BridgeProtocol, BridgeStats, Result};
use crate::broker::router::MessageRouter;
use crate::client::MqttClient;
use crate::packet::publish::PublishPacket;
use crate::time::{Duration, Instant};
use crate::transport::tls::TlsConfig;
use crate::types::ConnectOptions;
use mqtt5_protocol::bridge::{evaluate_forwarding, TopicMappingCore};
use rand::Rng;
use std::collections::VecDeque;
use std::net::ToSocketAddrs;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

fn apply_jitter(delay: Duration, enable: bool) -> Duration {
    if !enable {
        return delay;
    }
    let secs = delay.as_secs_f64();
    let jitter_range = secs * 0.25;
    let jitter = (rand::rng().random::<f64>() - 0.5) * 2.0 * jitter_range;
    Duration::from_secs_f64((secs + jitter).max(0.1))
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
}

impl BridgeConnection {
    /// Creates a new bridge connection.
    ///
    /// # Errors
    /// Returns an error if the configuration is invalid.
    pub fn new(config: BridgeConfig, router: Arc<MessageRouter>) -> Result<Self> {
        // Validate configuration
        config
            .validate()
            .map_err(|e| BridgeError::ConfigurationError(e.to_string()))?;

        // Create MQTT client for bridge
        let client = Arc::new(MqttClient::new(&config.client_id));

        // Create shutdown channel
        let (shutdown_tx, _) = broadcast::channel(1);

        Ok(Self {
            config,
            client,
            router,
            stats: Arc::new(RwLock::new(BridgeStats::default())),
            running: Arc::new(AtomicBool::new(false)),
            shutdown_tx,
            messages_sent: Arc::new(AtomicU64::new(0)),
            messages_received: Arc::new(AtomicU64::new(0)),
            bytes_sent: Arc::new(AtomicU64::new(0)),
            bytes_received: Arc::new(AtomicU64::new(0)),
            current_broker: Arc::new(RwLock::new(None)),
            health_check_handle: Arc::new(RwLock::new(None)),
            pending_messages: Arc::new(Mutex::new(VecDeque::new())),
        })
    }

    /// Starts the bridge connection.
    pub fn start(&self) {
        if self.running.load(Ordering::Relaxed) {
            return;
        }

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

        let _ = self.client.disconnect().await;

        let mut stats = self.stats.write().await;
        stats.connected = false;
        stats.connected_since = None;
        *self.current_broker.write().await = None;

        Ok(())
    }

    /// Builds TLS configuration for a broker address
    fn build_tls_config(&self, address: &str) -> Result<TlsConfig> {
        let addr = address
            .to_socket_addrs()
            .map_err(|e| BridgeError::ConfigurationError(format!("Invalid address: {e}")))?
            .next()
            .ok_or_else(|| {
                BridgeError::ConfigurationError("Could not resolve address".to_string())
            })?;

        let hostname = if let Some(ref server_name) = self.config.tls_server_name {
            server_name.clone()
        } else {
            address
                .split(':')
                .next()
                .ok_or_else(|| {
                    BridgeError::ConfigurationError("Invalid address format".to_string())
                })?
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
        let mut options = ConnectOptions::new(&self.config.client_id);
        options.clean_start = self.config.clean_start;
        options.keep_alive = Duration::from_secs(u64::from(self.config.keepalive));

        if let Some(ref username) = self.config.username {
            options.username = Some(username.clone());
        }
        if let Some(ref password) = self.config.password {
            options.password = Some(password.clone().into_bytes());
        }

        if self.config.try_private {
            options
                .properties
                .user_properties
                .push(("bridge".to_string(), self.config.name.clone()));
        }

        options
    }

    /// Attempts to connect using TLS to primary and backup brokers
    async fn connect_tls(&self, options: &ConnectOptions) -> Result<ConnectedBroker> {
        let tls_config = self.build_tls_config(&self.config.remote_address)?;
        match self
            .client
            .connect_with_tls_and_options(tls_config, options.clone())
            .await
        {
            Ok(_) => {
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
                self.update_error_stats(e.to_string()).await;
            }
        }

        for (idx, backup) in self.config.backup_brokers.iter().enumerate() {
            let tls_config = self.build_tls_config(backup)?;
            match self
                .client
                .connect_with_tls_and_options(tls_config, options.clone())
                .await
            {
                Ok(_) => {
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
                    self.update_error_stats(e.to_string()).await;
                }
            }
        }

        Err(BridgeError::ConnectionFailed(
            "Failed to connect to any broker".to_string(),
        ))
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
            BridgeProtocol::Quic => Box::pin(self.connect_quic(options, false)).await,
            BridgeProtocol::QuicSecure => Box::pin(self.connect_quic(options, true)).await,
        }
    }

    fn get_fallback_protocols(&self) -> Vec<BridgeProtocol> {
        let tcp_fallback = self.config.fallback_tcp
            && !self
                .config
                .fallback_protocols
                .contains(&BridgeProtocol::Tcp);

        self.config
            .fallback_protocols
            .iter()
            .copied()
            .chain(tcp_fallback.then_some(BridgeProtocol::Tcp))
            .collect()
    }

    async fn try_all_protocols(&self, options: &ConnectOptions) -> Result<ConnectedBroker> {
        let max_retries = self.config.connection_retries.max(1);

        for attempt in 1..=max_retries {
            if let Ok(broker) = self
                .connect_with_protocol(self.config.protocol, options)
                .await
            {
                return Ok(broker);
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

        let fallbacks = self.get_fallback_protocols();
        if !fallbacks.is_empty() {
            warn!(
                "Bridge '{}' primary protocol {:?} failed after {} attempts, trying fallbacks",
                self.config.name, self.config.protocol, max_retries
            );
        }

        for fallback in fallbacks {
            if let Ok(broker) = self.connect_with_protocol(fallback, options).await {
                info!(
                    "Bridge '{}' connected via fallback protocol {:?}",
                    self.config.name, fallback
                );
                return Ok(broker);
            }
        }

        Err(BridgeError::ConnectionFailed(format!(
            "All protocols failed for bridge '{}'",
            self.config.name
        )))
    }

    async fn connect(&self) -> Result<ConnectedBroker> {
        let options = self.build_connect_options();
        let mut attempt = 0u32;

        loop {
            attempt += 1;

            {
                let mut stats = self.stats.write().await;
                stats.connection_attempts += 1;
            }

            match self.try_all_protocols(&options).await {
                Ok(broker) => {
                    if matches!(broker, ConnectedBroker::Backup(_)) && self.config.enable_failback {
                        self.start_health_check().await;
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
                        Duration::from_secs_f64(delay_secs).min(self.config.max_reconnect_delay)
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

    async fn start_health_check(&self) {
        self.stop_health_check().await;

        let config = self.config.clone();
        let running = self.running.clone();
        let shutdown_tx = self.shutdown_tx.clone();
        let current_broker = self.current_broker.clone();
        let stats = self.stats.clone();

        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(config.primary_health_check_interval);

            while running.load(Ordering::Relaxed) {
                interval.tick().await;

                if !running.load(Ordering::Relaxed) {
                    break;
                }

                if let Some(ConnectedBroker::Primary) = *current_broker.read().await {
                    debug!("Health check: already on primary, stopping");
                    break;
                }

                if Self::probe_broker(&config.remote_address, &config).await {
                    info!(
                        "Bridge '{}': primary broker {} is available, triggering failback",
                        config.name, config.remote_address
                    );

                    {
                        let mut stats = stats.write().await;
                        stats.failback_attempts += 1;
                    }

                    let _ = shutdown_tx.send(());
                    break;
                }
            }
        });

        *self.health_check_handle.write().await = Some(handle);
    }

    async fn stop_health_check(&self) {
        if let Some(handle) = self.health_check_handle.write().await.take() {
            handle.abort();
        }
    }

    async fn probe_broker(address: &str, config: &BridgeConfig) -> bool {
        use tokio::net::TcpStream;

        let addr = match address.to_socket_addrs() {
            Ok(mut addrs) => match addrs.next() {
                Some(addr) => addr,
                None => return false,
            },
            Err(_) => return false,
        };

        match tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr)).await {
            Ok(Ok(_stream)) => {
                debug!(
                    "Bridge '{}': primary broker {} responded to probe",
                    config.name, address
                );
                true
            }
            _ => false,
        }
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

    /// Sets up subscriptions for incoming topics
    async fn setup_subscriptions(&self) -> Result<()> {
        for mapping in &self.config.topics {
            let core_mapping = TopicMappingCore::from(mapping);
            if !core_mapping.direction.allows_incoming() {
                continue;
            }

            let remote_topic = core_mapping.apply_remote_prefix(&core_mapping.pattern);
            let local_prefix = core_mapping.local_prefix.clone();
            let qos = core_mapping.qos;

            let router = self.router.clone();
            let stats_received = self.messages_received.clone();
            let stats_bytes = self.bytes_received.clone();

            self.client
                .subscribe(&remote_topic, move |msg| {
                    let router = router.clone();
                    let local_prefix = local_prefix.clone();
                    let stats_received = stats_received.clone();
                    let stats_bytes = stats_bytes.clone();

                    stats_received.fetch_add(1, Ordering::Relaxed);
                    stats_bytes.fetch_add(msg.payload.len() as u64, Ordering::Relaxed);

                    let local_topic = match &local_prefix {
                        Some(prefix) => format!("{prefix}{}", msg.topic),
                        None => msg.topic.clone(),
                    };

                    let mut packet = PublishPacket::new(local_topic, msg.payload.clone(), msg.qos);
                    let pub_props: crate::types::PublishProperties = msg.properties.into();
                    packet.properties = pub_props.into();
                    packet.retain = msg.retain;

                    tokio::spawn(async move {
                        router.route_message_local_only(&packet, None).await;
                    });
                })
                .await?;

            info!(
                "Bridge '{}' subscribed to remote topic: {} (QoS: {:?})",
                self.config.name, remote_topic, qos
            );
        }

        Ok(())
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
        let options = crate::types::PublishOptions {
            qos: decision.qos,
            retain: packet.retain,
            properties: msg_props.into(),
        };

        let client = self.client.clone();
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
            match client
                .publish_with_options(&remote_topic, payload, options)
                .await
            {
                Ok(_) => {
                    debug!(
                        bridge = %bridge_name_clone,
                        topic = %remote_topic,
                        "publish succeeded"
                    );
                    messages_sent.fetch_add(1, Ordering::Relaxed);
                    bytes_sent.fetch_add(payload_len as u64, Ordering::Relaxed);
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

    /// Updates stats when connected
    async fn update_connected_stats(&self, broker: ConnectedBroker, address: &str) {
        let mut stats = self.stats.write().await;
        stats.connected = true;
        stats.connected_since = Some(Instant::now());
        stats.last_error = None;
        stats.current_broker = Some(address.to_string());
        stats.on_primary = matches!(broker, ConnectedBroker::Primary);

        // Store which broker we're connected to
        *self.current_broker.write().await = Some(broker);

        // Flush any pending messages that were queued while disconnected
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
                result = self.run_connection() => {
                    if !self.running.load(Ordering::Relaxed) {
                        break;
                    }

                    if let Err(e) = result {
                        error!("Bridge '{}' connection failed: {}", self.config.name, e);
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    /// Runs a single connection until disconnected
    async fn run_connection(&self) -> Result<()> {
        if !self.client.is_connected().await {
            let _ = Box::pin(self.connect()).await?;
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
                break;
            }

            if !self.running.load(Ordering::Relaxed) {
                break;
            }
        }

        Ok(())
    }
}
