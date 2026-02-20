//! MQTT v5.0 Broker Server

use crate::broker::auth::{
    AllowAllAuthProvider, AuthProvider, AuthRateLimiter, RateLimitedAuthProvider,
};
use crate::broker::binding::{bind_tcp_addresses, format_binding_error};
use crate::broker::bridge::BridgeManager;
use crate::broker::client_handler::ClientHandler;
use crate::broker::config::{BrokerConfig, StorageBackend as StorageBackendType};
use crate::broker::hot_reload::HotReloadManager;
use crate::broker::quic_acceptor::{
    accept_quic_connection, run_quic_cluster_connection_handler, run_quic_connection_handler,
    QuicAcceptorConfig,
};
use crate::broker::resource_monitor::{ResourceLimits, ResourceMonitor};
use crate::broker::router::MessageRouter;
use crate::broker::storage::{DynamicStorage, FileBackend, MemoryBackend, StorageBackend};
use crate::broker::sys_topics::{BrokerStats, SysTopicsProvider};
use crate::broker::tls_acceptor::{accept_tls_connection, TlsAcceptorConfig};
use crate::broker::transport::BrokerTransport;
use crate::broker::websocket_server::{accept_websocket_connection, WebSocketServerConfig};
use crate::error::{MqttError, Result};
use quinn::Endpoint;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio_rustls::TlsAcceptor;
use tracing::{debug, error, info, warn};

#[cfg(feature = "opentelemetry")]
use crate::telemetry;

#[derive(Clone)]
struct AcceptLoopState {
    config_rx: watch::Receiver<Arc<BrokerConfig>>,
    router: Arc<MessageRouter>,
    auth_rx: watch::Receiver<Arc<dyn AuthProvider>>,
    storage: Option<Arc<DynamicStorage>>,
    stats: Arc<BrokerStats>,
    resource_monitor: Arc<ResourceMonitor>,
    shutdown_tx: tokio::sync::broadcast::Sender<()>,
}

impl AcceptLoopState {
    fn snapshot_config(&self) -> Arc<BrokerConfig> {
        Arc::clone(&self.config_rx.borrow())
    }

    fn snapshot_auth(&self) -> Arc<dyn AuthProvider> {
        Arc::clone(&self.auth_rx.borrow())
    }
}

/// MQTT v5.0 Broker
pub struct MqttBroker {
    config: Arc<BrokerConfig>,
    router: Arc<MessageRouter>,
    auth_provider: Arc<dyn AuthProvider>,
    storage: Option<Arc<DynamicStorage>>,
    stats: Arc<BrokerStats>,
    resource_monitor: Arc<ResourceMonitor>,
    bridge_manager: Option<Arc<BridgeManager>>,
    listeners: Vec<TcpListener>,
    tls_listeners: Vec<TcpListener>,
    tls_acceptor: Option<TlsAcceptor>,
    ws_listeners: Vec<TcpListener>,
    ws_config: Option<WebSocketServerConfig>,
    ws_tls_listeners: Vec<TcpListener>,
    ws_tls_config: Option<WebSocketServerConfig>,
    ws_tls_acceptor: Option<TlsAcceptor>,
    quic_endpoints: Vec<Endpoint>,
    cluster_listeners: Vec<TcpListener>,
    cluster_tls_acceptor: Option<TlsAcceptor>,
    cluster_quic_endpoints: Vec<Endpoint>,
    shutdown_tx: Option<tokio::sync::broadcast::Sender<()>>,
    ready_tx: Option<watch::Sender<bool>>,
    ready_rx: watch::Receiver<bool>,
    config_watch_tx: watch::Sender<Arc<BrokerConfig>>,
    config_watch_rx: watch::Receiver<Arc<BrokerConfig>>,
    auth_watch_tx: watch::Sender<Arc<dyn AuthProvider>>,
    auth_watch_rx: watch::Receiver<Arc<dyn AuthProvider>>,
    hot_reload_manager: Option<HotReloadManager>,
    reload_tx: Option<mpsc::Sender<()>>,
    reload_rx: Option<mpsc::Receiver<()>>,
}

#[allow(clippy::too_many_lines)]
async fn create_auth_provider(
    config: &crate::broker::config::AuthConfig,
) -> Result<Arc<dyn AuthProvider>> {
    use crate::broker::acl::AclManager;
    use crate::broker::auth::{ComprehensiveAuthProvider, PasswordAuthProvider};
    use crate::broker::auth_mechanisms::{
        FederatedJwtAuthProvider, FileBasedScramCredentialStore, JwtAuthProvider,
        ScramSha256AuthProvider,
    };
    use crate::broker::config::{AuthMethod, JwtAlgorithm};

    let rate_limiter = if config.rate_limit.enabled {
        Some(Arc::new(AuthRateLimiter::new(
            config.rate_limit.max_attempts,
            config.rate_limit.window_secs,
            config.rate_limit.lockout_secs,
        )))
    } else {
        None
    };

    let provider: Arc<dyn AuthProvider> = match config.auth_method {
        AuthMethod::ScramSha256 => {
            let Some(scram_file) = &config.scram_file else {
                return Err(MqttError::Configuration(
                    "SCRAM-SHA-256 authentication requires scram_file".to_string(),
                ));
            };
            let store = FileBasedScramCredentialStore::load_from_file(scram_file)
                .map_err(|e| MqttError::Configuration(format!("Failed to load SCRAM file: {e}")))?;
            let provider = ScramSha256AuthProvider::new(Arc::new(store));
            info!("SCRAM-SHA-256 authentication enabled");
            Arc::new(provider) as Arc<dyn AuthProvider>
        }
        AuthMethod::Jwt => {
            let Some(jwt_config) = &config.jwt_config else {
                return Err(MqttError::Configuration(
                    "JWT authentication requires jwt_config".to_string(),
                ));
            };
            let key_data = std::fs::read(&jwt_config.secret_or_key_file).map_err(|e| {
                MqttError::Configuration(format!("Failed to read JWT key file: {e}"))
            })?;
            let mut provider = match jwt_config.algorithm {
                JwtAlgorithm::HS256 => {
                    let secret = String::from_utf8_lossy(&key_data);
                    let trimmed = secret.trim();
                    JwtAuthProvider::with_hs256_secret(trimmed.as_bytes())
                }
                JwtAlgorithm::RS256 => JwtAuthProvider::with_rs256_public_key(&key_data),
                JwtAlgorithm::ES256 => JwtAuthProvider::with_es256_public_key(&key_data),
            };
            provider = provider.with_clock_skew(jwt_config.clock_skew_secs);
            if let Some(ref issuer) = jwt_config.issuer {
                provider = provider.with_issuer(issuer);
            }
            if let Some(ref audience) = jwt_config.audience {
                provider = provider.with_audience(audience);
            }
            info!(algorithm = ?jwt_config.algorithm, "JWT authentication enabled");
            Arc::new(provider) as Arc<dyn AuthProvider>
        }
        AuthMethod::JwtFederated => {
            let Some(federated_config) = &config.federated_jwt_config else {
                return Err(MqttError::Configuration(
                    "Federated JWT authentication requires federated_jwt_config".to_string(),
                ));
            };

            let provider = FederatedJwtAuthProvider::new(federated_config.issuers.clone())
                .map_err(|e| {
                    MqttError::Configuration(format!(
                        "Failed to create federated JWT provider: {e}"
                    ))
                })?
                .with_clock_skew(federated_config.clock_skew_secs);

            if let Some(acl_file) = &config.acl_file {
                let acl_manager = AclManager::from_file(acl_file).await?;
                let provider =
                    provider.with_acl_manager(Arc::new(tokio::sync::RwLock::new(acl_manager)));
                provider.initial_fetch().await?;
                drop(provider.start_background_refresh());
                info!(
                    issuers = federated_config.issuers.len(),
                    "Federated JWT authentication enabled with ACL"
                );
                Arc::new(provider) as Arc<dyn AuthProvider>
            } else {
                provider.initial_fetch().await?;
                drop(provider.start_background_refresh());
                info!(
                    issuers = federated_config.issuers.len(),
                    "Federated JWT authentication enabled"
                );
                Arc::new(provider) as Arc<dyn AuthProvider>
            }
        }
        AuthMethod::Password | AuthMethod::None => {
            match (&config.password_file, &config.acl_file) {
                (Some(password_file), Some(acl_file)) => {
                    let password_provider = PasswordAuthProvider::from_file(password_file)
                        .await?
                        .with_anonymous(config.allow_anonymous);
                    let acl_manager = AclManager::from_file(acl_file).await?;
                    let provider =
                        ComprehensiveAuthProvider::with_providers(password_provider, acl_manager);
                    info!(
                        "Comprehensive authentication enabled (password + ACL, anonymous: {})",
                        config.allow_anonymous
                    );
                    Arc::new(provider) as Arc<dyn AuthProvider>
                }
                (Some(password_file), None) => {
                    let provider = PasswordAuthProvider::from_file(password_file)
                        .await?
                        .with_anonymous(config.allow_anonymous);
                    info!(
                        "Password authentication enabled (anonymous: {})",
                        config.allow_anonymous
                    );
                    Arc::new(provider) as Arc<dyn AuthProvider>
                }
                (None, Some(acl_file)) => {
                    let password_provider =
                        PasswordAuthProvider::new().with_anonymous(config.allow_anonymous);
                    let acl_manager = AclManager::from_file(acl_file).await?;
                    let provider =
                        ComprehensiveAuthProvider::with_providers(password_provider, acl_manager);
                    info!(
                        "ACL authorization enabled (anonymous: {})",
                        config.allow_anonymous
                    );
                    Arc::new(provider) as Arc<dyn AuthProvider>
                }
                (None, None) if config.allow_anonymous => {
                    info!("Anonymous authentication enabled");
                    Arc::new(AllowAllAuthProvider) as Arc<dyn AuthProvider>
                }
                (None, None) => {
                    return Err(MqttError::Configuration(
                        "Authentication required but no password or ACL file specified".to_string(),
                    ));
                }
            }
        }
    };

    if let Some(rate_limiter) = rate_limiter {
        info!(
            max_attempts = config.rate_limit.max_attempts,
            window_secs = config.rate_limit.window_secs,
            lockout_secs = config.rate_limit.lockout_secs,
            "Authentication rate limiting enabled"
        );
        Ok(Arc::new(RateLimitedAuthProvider::new(
            provider,
            rate_limiter,
        )))
    } else {
        Ok(provider)
    }
}

impl MqttBroker {
    /// Creates a new broker with default configuration
    ///
    /// # Errors
    ///
    /// Returns an error if binding fails
    pub async fn bind(addr: impl AsRef<str>) -> Result<Self> {
        let addr = addr
            .as_ref()
            .parse::<std::net::SocketAddr>()
            .map_err(|e| MqttError::Configuration(format!("Invalid address: {e}")))?;

        let config = BrokerConfig::default().with_bind_address(addr);
        Self::with_config(config).await
    }

    /// Creates a new broker with custom configuration
    ///
    /// # Errors
    ///
    /// Returns an error if configuration is invalid or binding fails
    pub async fn with_config(config: BrokerConfig) -> Result<Self> {
        config.validate()?;

        let bind_result = bind_tcp_addresses(&config.bind_addresses, "TCP").await;
        if bind_result.is_empty() {
            let error_msg =
                format_binding_error("TCP", &bind_result.failures, &config.bind_addresses);
            return Err(MqttError::Configuration(error_msg));
        }
        bind_result.warn_partial_failures("TCP");
        let listeners = bind_result.successful;

        let (ws_listeners, ws_config) = Self::setup_websocket(&config).await?;
        let (ws_tls_listeners, ws_tls_config, ws_tls_acceptor) =
            Self::setup_websocket_tls(&config).await?;
        let (tls_listeners, tls_acceptor) = Self::setup_tls(&config).await?;
        let quic_endpoints = Self::setup_quic(&config).await?;
        let (cluster_listeners, cluster_tls_acceptor, cluster_quic_endpoints) =
            Self::setup_cluster_listener(&config).await?;

        let storage = if config.storage_config.enable_persistence {
            Some(Self::create_storage_backend(&config.storage_config).await?)
        } else {
            None
        };

        let router = Self::build_router(&config, storage.as_ref());

        let auth_provider = create_auth_provider(&config.auth_config).await?;
        let stats = Arc::new(BrokerStats::new());
        let resource_monitor = Arc::new(ResourceMonitor::new(Self::default_resource_limits(
            config.max_clients,
        )));
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
        let (ready_tx, ready_rx) = watch::channel(false);

        #[cfg(feature = "opentelemetry")]
        if let Some(ref otel_config) = config.opentelemetry_config {
            telemetry::init_tracing_subscriber(otel_config)?;
        }

        let bridge_manager = if config.bridges.is_empty() {
            None
        } else {
            info!("Initializing {} bridge(s)", config.bridges.len());

            let manager = Arc::new(BridgeManager::new(Arc::clone(&router)));
            router.set_bridge_manager(Arc::clone(&manager)).await;

            for bridge_config in &config.bridges {
                info!("Adding bridge '{}'", bridge_config.name);
                if let Err(e) = manager.add_bridge(bridge_config.clone()) {
                    error!("Failed to add bridge '{}': {}", bridge_config.name, e);
                }
            }

            Some(manager)
        };

        let config = Arc::new(config);
        let (config_watch_tx, config_watch_rx) = watch::channel(Arc::clone(&config));
        let (auth_watch_tx, auth_watch_rx) = watch::channel(Arc::clone(&auth_provider));

        Ok(Self {
            config,
            router,
            auth_provider,
            storage,
            stats,
            resource_monitor,
            bridge_manager,
            listeners,
            tls_listeners,
            tls_acceptor,
            ws_listeners,
            ws_config,
            ws_tls_listeners,
            ws_tls_config,
            ws_tls_acceptor,
            quic_endpoints,
            cluster_listeners,
            cluster_tls_acceptor,
            cluster_quic_endpoints,
            shutdown_tx: Some(shutdown_tx),
            ready_tx: Some(ready_tx),
            ready_rx,
            config_watch_tx,
            config_watch_rx,
            auth_watch_tx,
            auth_watch_rx,
            hot_reload_manager: None,
            reload_tx: None,
            reload_rx: None,
        })
    }

    async fn setup_websocket(
        config: &BrokerConfig,
    ) -> Result<(Vec<TcpListener>, Option<WebSocketServerConfig>)> {
        if let Some(ref ws_config) = config.websocket_config {
            let bind_result = bind_tcp_addresses(&ws_config.bind_addresses, "WebSocket").await;
            if bind_result.is_empty() {
                let error_msg = format_binding_error(
                    "WebSocket",
                    &bind_result.failures,
                    &ws_config.bind_addresses,
                );
                warn!("{}, WebSocket disabled", error_msg);
                return Ok((Vec::new(), None));
            }
            bind_result.warn_partial_failures("WebSocket");
            let server_config = WebSocketServerConfig::new()
                .with_path(ws_config.path.clone())
                .with_subprotocol(ws_config.subprotocol.clone());
            Ok((bind_result.successful, Some(server_config)))
        } else {
            Ok((Vec::new(), None))
        }
    }

    async fn setup_websocket_tls(
        config: &BrokerConfig,
    ) -> Result<(
        Vec<TcpListener>,
        Option<WebSocketServerConfig>,
        Option<TlsAcceptor>,
    )> {
        if let Some(ref ws_tls_config) = config.websocket_tls_config {
            if let Some(ref tls_config) = config.tls_config {
                let cert_chain =
                    TlsAcceptorConfig::load_cert_chain_from_file(&tls_config.cert_file).await?;
                let private_key =
                    TlsAcceptorConfig::load_private_key_from_file(&tls_config.key_file).await?;

                let mut acceptor_config = TlsAcceptorConfig::new(cert_chain, private_key);

                if let Some(ref ca_file) = tls_config.ca_file {
                    let ca_certs = TlsAcceptorConfig::load_cert_chain_from_file(ca_file).await?;
                    acceptor_config = acceptor_config.with_client_ca_certs(ca_certs);
                }

                acceptor_config =
                    acceptor_config.with_require_client_cert(tls_config.require_client_cert);
                let acceptor = acceptor_config.build_acceptor()?;

                let bind_result =
                    bind_tcp_addresses(&ws_tls_config.bind_addresses, "WebSocket TLS").await;
                if bind_result.is_empty() {
                    let error_msg = format_binding_error(
                        "WebSocket TLS",
                        &bind_result.failures,
                        &ws_tls_config.bind_addresses,
                    );
                    warn!("{}, WebSocket TLS disabled", error_msg);
                    return Ok((Vec::new(), None, None));
                }
                bind_result.warn_partial_failures("WebSocket TLS");

                let server_config = WebSocketServerConfig::new()
                    .with_path(ws_tls_config.path.clone())
                    .with_subprotocol(ws_tls_config.subprotocol.clone());

                Ok((bind_result.successful, Some(server_config), Some(acceptor)))
            } else {
                Err(MqttError::Configuration(
                    "WebSocket TLS requires TLS configuration (cert/key)".to_string(),
                ))
            }
        } else {
            Ok((Vec::new(), None, None))
        }
    }

    async fn setup_tls(config: &BrokerConfig) -> Result<(Vec<TcpListener>, Option<TlsAcceptor>)> {
        if let Some(ref tls_config) = config.tls_config {
            let cert_chain =
                TlsAcceptorConfig::load_cert_chain_from_file(&tls_config.cert_file).await?;
            let private_key =
                TlsAcceptorConfig::load_private_key_from_file(&tls_config.key_file).await?;

            let mut acceptor_config = TlsAcceptorConfig::new(cert_chain, private_key);

            if let Some(ref ca_file) = tls_config.ca_file {
                let ca_certs = TlsAcceptorConfig::load_cert_chain_from_file(ca_file).await?;
                acceptor_config = acceptor_config.with_client_ca_certs(ca_certs);
            }

            acceptor_config =
                acceptor_config.with_require_client_cert(tls_config.require_client_cert);
            let acceptor = acceptor_config.build_acceptor()?;

            let bind_result = bind_tcp_addresses(&tls_config.bind_addresses, "TLS").await;
            if bind_result.is_empty() {
                let error_msg =
                    format_binding_error("TLS", &bind_result.failures, &tls_config.bind_addresses);
                warn!("{}, TLS disabled", error_msg);
                return Ok((Vec::new(), None));
            }
            bind_result.warn_partial_failures("TLS");

            Ok((bind_result.successful, Some(acceptor)))
        } else {
            Ok((Vec::new(), None))
        }
    }

    async fn setup_quic(config: &BrokerConfig) -> Result<Vec<Endpoint>> {
        if let Some(ref quic_config) = config.quic_config {
            let cert_chain =
                QuicAcceptorConfig::load_cert_chain_from_file(&quic_config.cert_file).await?;
            let private_key =
                QuicAcceptorConfig::load_private_key_from_file(&quic_config.key_file).await?;

            let mut acceptor_config = QuicAcceptorConfig::new(cert_chain, private_key);

            if let Some(ref ca_file) = quic_config.ca_file {
                let ca_certs = QuicAcceptorConfig::load_cert_chain_from_file(ca_file).await?;
                acceptor_config = acceptor_config.with_client_ca_certs(ca_certs);
            }

            acceptor_config =
                acceptor_config.with_require_client_cert(quic_config.require_client_cert);

            let mut endpoints = Vec::new();
            for addr in &quic_config.bind_addresses {
                match acceptor_config.build_endpoint(*addr) {
                    Ok(endpoint) => {
                        info!("QUIC endpoint bound to {}", addr);
                        endpoints.push(endpoint);
                    }
                    Err(e) => {
                        warn!("Failed to bind QUIC endpoint to {}: {}", addr, e);
                    }
                }
            }

            if endpoints.is_empty() {
                warn!("No QUIC endpoints could be bound, QUIC disabled");
            }

            Ok(endpoints)
        } else {
            Ok(Vec::new())
        }
    }

    async fn setup_cluster_listener(
        config: &BrokerConfig,
    ) -> Result<(Vec<TcpListener>, Option<TlsAcceptor>, Vec<Endpoint>)> {
        if let Some(ref cluster_config) = config.cluster_listener_config {
            if let Err(e) = cluster_config.validate() {
                return Err(MqttError::Configuration(e));
            }

            if cluster_config.is_quic() {
                let cert_file = cluster_config.cert_file.as_ref().ok_or_else(|| {
                    MqttError::Configuration("QUIC cluster listener requires cert_file".to_string())
                })?;
                let key_file = cluster_config.key_file.as_ref().ok_or_else(|| {
                    MqttError::Configuration("QUIC cluster listener requires key_file".to_string())
                })?;

                let cert_chain = QuicAcceptorConfig::load_cert_chain_from_file(cert_file).await?;
                let private_key = QuicAcceptorConfig::load_private_key_from_file(key_file).await?;

                let mut acceptor_config = QuicAcceptorConfig::new(cert_chain, private_key);

                if let Some(ref ca_file) = cluster_config.ca_file {
                    let ca_certs = QuicAcceptorConfig::load_cert_chain_from_file(ca_file).await?;
                    acceptor_config = acceptor_config.with_client_ca_certs(ca_certs);
                }

                acceptor_config =
                    acceptor_config.with_require_client_cert(cluster_config.require_client_cert);

                let mut endpoints = Vec::new();
                for addr in &cluster_config.bind_addresses {
                    match acceptor_config.build_endpoint(*addr) {
                        Ok(endpoint) => {
                            info!("Cluster QUIC endpoint bound to {}", addr);
                            endpoints.push(endpoint);
                        }
                        Err(e) => {
                            warn!("Failed to bind Cluster QUIC endpoint to {}: {}", addr, e);
                        }
                    }
                }

                if endpoints.is_empty() {
                    warn!("No Cluster QUIC endpoints could be bound, Cluster listener disabled");
                }

                return Ok((Vec::new(), None, endpoints));
            }

            let bind_result = bind_tcp_addresses(&cluster_config.bind_addresses, "Cluster").await;
            if bind_result.is_empty() {
                let error_msg = format_binding_error(
                    "Cluster",
                    &bind_result.failures,
                    &cluster_config.bind_addresses,
                );
                warn!("{}, Cluster listener disabled", error_msg);
                return Ok((Vec::new(), None, Vec::new()));
            }
            bind_result.warn_partial_failures("Cluster");

            let acceptor = if cluster_config.uses_tls() {
                let cert_file = cluster_config.cert_file.as_ref().unwrap();
                let key_file = cluster_config.key_file.as_ref().unwrap();

                let cert_chain = TlsAcceptorConfig::load_cert_chain_from_file(cert_file).await?;
                let private_key = TlsAcceptorConfig::load_private_key_from_file(key_file).await?;

                let mut acceptor_config = TlsAcceptorConfig::new(cert_chain, private_key);

                if let Some(ref ca_file) = cluster_config.ca_file {
                    let ca_certs = TlsAcceptorConfig::load_cert_chain_from_file(ca_file).await?;
                    acceptor_config = acceptor_config.with_client_ca_certs(ca_certs);
                }

                acceptor_config =
                    acceptor_config.with_require_client_cert(cluster_config.require_client_cert);
                Some(acceptor_config.build_acceptor()?)
            } else {
                None
            };

            Ok((bind_result.successful, acceptor, Vec::new()))
        } else {
            Ok((Vec::new(), None, Vec::new()))
        }
    }

    fn build_router(
        config: &BrokerConfig,
        storage: Option<&Arc<DynamicStorage>>,
    ) -> Arc<MessageRouter> {
        let base = if let Some(storage) = storage {
            MessageRouter::with_storage(Arc::clone(storage))
        } else {
            MessageRouter::new()
        };
        let with_handler = if let Some(ref handler) = config.event_handler {
            base.with_event_handler(Arc::clone(handler))
        } else {
            base
        };
        let router = if config.echo_suppression_config.enabled {
            with_handler
                .with_echo_suppression_key(config.echo_suppression_config.property_key.clone())
        } else {
            with_handler
        };
        Arc::new(router)
    }

    fn default_resource_limits(max_clients: usize) -> ResourceLimits {
        ResourceLimits {
            max_connections: max_clients,
            max_connections_per_ip: 10_000,
            max_memory_bytes: 1024 * 1024 * 1024,
            max_message_rate_per_client: 10_000_000,
            max_bandwidth_per_client: 1024 * 1024 * 1024,
            max_connection_rate: 10_000,
            rate_limit_window: crate::time::Duration::from_secs(1),
        }
    }

    /// Create storage backend based on configuration
    async fn create_storage_backend(
        storage_config: &crate::broker::config::StorageConfig,
    ) -> Result<Arc<DynamicStorage>> {
        match storage_config.backend {
            StorageBackendType::File => {
                let backend = FileBackend::new(&storage_config.base_dir).await?;
                Ok(Arc::new(DynamicStorage::File(backend)))
            }
            StorageBackendType::Memory => {
                let backend = MemoryBackend::new();
                Ok(Arc::new(DynamicStorage::Memory(backend)))
            }
        }
    }

    /// Creates a broker with hot-reload support from a config file path
    ///
    /// # Errors
    ///
    /// Returns an error if the config is invalid, binding fails, or the hot-reload manager
    /// cannot be created.
    pub async fn with_config_file(config: BrokerConfig, path: PathBuf) -> Result<Self> {
        let manager = HotReloadManager::new(config.clone(), path)?;
        let (reload_tx, reload_rx) = mpsc::channel(1);
        let mut broker = Self::with_config(config).await?;
        broker.hot_reload_manager = Some(manager);
        broker.reload_tx = Some(reload_tx);
        broker.reload_rx = Some(reload_rx);
        Ok(broker)
    }

    #[must_use]
    pub fn manual_reload_sender(&self) -> Option<mpsc::Sender<()>> {
        self.reload_tx.clone()
    }

    #[must_use]
    pub fn with_auth_provider(mut self, provider: Arc<dyn AuthProvider>) -> Self {
        let _ = self.auth_watch_tx.send(Arc::clone(&provider));
        self.auth_provider = provider;
        self
    }

    async fn initialize_storage(
        &self,
        shutdown_tx: &tokio::sync::broadcast::Sender<()>,
        task_handles: &mut Vec<tokio::task::JoinHandle<()>>,
    ) -> Result<()> {
        if let Some(ref storage) = self.storage {
            storage.cleanup_expired().await?;

            let storage_clone = Arc::clone(storage);
            let cleanup_interval = self.config.storage_config.cleanup_interval;
            let mut shutdown_rx = shutdown_tx.subscribe();

            task_handles.push(tokio::spawn(async move {
                let mut interval = tokio::time::interval(cleanup_interval);
                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            if let Err(e) = storage_clone.cleanup_expired().await {
                                error!("Storage cleanup error: {e}");
                            }
                        }
                        _ = shutdown_rx.recv() => {
                            debug!("Storage cleanup task shutting down");
                            break;
                        }
                    }
                }
            }));

            let storage_clone = Arc::clone(storage);
            let mut shutdown_rx = shutdown_tx.subscribe();
            let flush_interval = std::time::Duration::from_secs(5);

            task_handles.push(tokio::spawn(async move {
                let mut interval = tokio::time::interval(flush_interval);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            if let Err(e) = storage_clone.flush_sessions().await {
                                error!("Session flush error: {e}");
                            }
                        }
                        _ = shutdown_rx.recv() => {
                            debug!("Flushing sessions before shutdown");
                            if let Err(e) = storage_clone.shutdown().await {
                                error!("Session shutdown flush error: {e}");
                            }
                            break;
                        }
                    }
                }
            }));
        }
        Ok(())
    }

    /// Runs the broker until shutdown
    ///
    /// This accepts incoming connections and spawns handlers for each.
    ///
    /// # Errors
    ///
    /// Returns an error if the accept loop fails
    #[allow(clippy::too_many_lines)]
    pub async fn run(&mut self) -> Result<()> {
        info!("Starting MQTT broker");

        if self.listeners.is_empty() {
            return Err(MqttError::InvalidState(
                "Broker already running".to_string(),
            ));
        }

        let listeners = std::mem::take(&mut self.listeners);
        let tls_listeners = std::mem::take(&mut self.tls_listeners);
        let tls_acceptor = self.tls_acceptor.take();
        let ws_listeners = std::mem::take(&mut self.ws_listeners);
        let ws_config = self.ws_config.take();
        let ws_tls_listeners = std::mem::take(&mut self.ws_tls_listeners);
        let ws_tls_config = self.ws_tls_config.take();
        let ws_tls_acceptor = self.ws_tls_acceptor.take();
        let quic_endpoints = std::mem::take(&mut self.quic_endpoints);
        let cluster_listeners = std::mem::take(&mut self.cluster_listeners);
        let cluster_tls_acceptor = self.cluster_tls_acceptor.take();
        let cluster_quic_endpoints = std::mem::take(&mut self.cluster_quic_endpoints);

        let Some(shutdown_tx) = self.shutdown_tx.take() else {
            return Err(MqttError::InvalidState(
                "Broker already running".to_string(),
            ));
        };

        let mut task_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

        self.initialize_storage(&shutdown_tx, &mut task_handles)
            .await?;
        self.router.initialize().await?;

        let sys_provider =
            SysTopicsProvider::new(Arc::clone(&self.router), Arc::clone(&self.stats));
        let sys_handle = sys_provider.start();

        info!("Router initialized, starting resource monitor cleanup task");

        let resource_monitor_clone = Arc::clone(&self.resource_monitor);
        let mut shutdown_rx_cleanup = shutdown_tx.subscribe();
        task_handles.push(tokio::spawn(async move {
            let mut interval = tokio::time::interval(crate::time::Duration::from_secs(60));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        resource_monitor_clone.cleanup_expired_windows().await;
                    }
                    _ = shutdown_rx_cleanup.recv() => {
                        debug!("Resource monitor cleanup task shutting down");
                        break;
                    }
                }
            }
        }));

        let mut shutdown_rx = shutdown_tx.subscribe();

        let accept_state = AcceptLoopState {
            config_rx: self.config_watch_rx.clone(),
            router: Arc::clone(&self.router),
            auth_rx: self.auth_watch_rx.clone(),
            storage: self.storage.clone(),
            stats: Arc::clone(&self.stats),
            resource_monitor: Arc::clone(&self.resource_monitor),
            shutdown_tx: shutdown_tx.clone(),
        };

        // Spawn WebSocket accept tasks (one per listener)
        if let Some(ws_config) = ws_config {
            for ws_listener in ws_listeners {
                let ws_cfg = ws_config.clone();
                let state = accept_state.clone();
                let mut shutdown_rx_ws = state.shutdown_tx.subscribe();

                task_handles.push(tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            accept_result = ws_listener.accept() => {
                                match accept_result {
                                    Ok((tcp_stream, addr)) => {
                                        debug!("New WebSocket connection from {}", addr);

                                        if !state.resource_monitor.can_accept_connection(addr.ip()).await {
                                            warn!("WebSocket connection rejected from {}: resource limits exceeded", addr);
                                            continue;
                                        }

                                        match accept_websocket_connection(tcp_stream, &ws_cfg, addr).await {
                                            Ok(ws_stream) => {
                                                let transport = BrokerTransport::websocket(ws_stream);

                                                let handler = ClientHandler::new(
                                                    transport,
                                                    addr,
                                                    state.snapshot_config(),
                                                    Arc::clone(&state.router),
                                                    state.snapshot_auth(),
                                                    state.storage.clone(),
                                                    Arc::clone(&state.stats),
                                                    Arc::clone(&state.resource_monitor),
                                                    state.shutdown_tx.subscribe(),
                                                );

                                                tokio::spawn(async move {
                                                    if let Err(e) = handler.run().await {
                                                        if e.is_normal_disconnect() {
                                                            debug!("Client handler finished");
                                                        } else {
                                                            warn!("Client handler error: {e}");
                                                        }
                                                    }
                                                });
                                            }
                                            Err(e) => {
                                                error!("WebSocket handshake failed: {e}");
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        error!("WebSocket accept error: {e}");
                                    }
                                }
                            }

                            _ = shutdown_rx_ws.recv() => {
                                debug!("WebSocket accept task shutting down");
                                break;
                            }
                        }
                    }
                }));
            }
        }

        if let (Some(ws_tls_config), Some(ws_tls_acceptor)) = (ws_tls_config, ws_tls_acceptor) {
            let acceptor = Arc::new(ws_tls_acceptor);
            for ws_tls_listener in ws_tls_listeners {
                let ws_cfg = ws_tls_config.clone();
                let acceptor = Arc::clone(&acceptor);
                let state = accept_state.clone();
                let mut shutdown_rx_wss = state.shutdown_tx.subscribe();

                task_handles.push(tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            accept_result = ws_tls_listener.accept() => {
                                match accept_result {
                                    Ok((tcp_stream, addr)) => {
                                        debug!("New WebSocket TLS connection from {}", addr);

                                        if !state.resource_monitor.can_accept_connection(addr.ip()).await {
                                            warn!("WebSocket TLS connection rejected from {}: resource limits exceeded", addr);
                                            continue;
                                        }

                                        let acc_clone = acceptor.clone();
                                        let cfg_clone = ws_cfg.clone();
                                        let state_clone = state.clone();

                                        tokio::spawn(async move {
                                            let cfg = state_clone.snapshot_config();
                                            let auth = state_clone.snapshot_auth();
                                            match accept_tls_connection(&acc_clone, tcp_stream, addr).await {
                                                Ok(tls_stream) => {
                                                    match accept_websocket_connection(tls_stream, &cfg_clone, addr).await {
                                                        Ok(ws_stream) => {
                                                            let transport = BrokerTransport::websocket(ws_stream);

                                                            let handler = ClientHandler::new(
                                                                transport,
                                                                addr,
                                                                cfg,
                                                                state_clone.router,
                                                                auth,
                                                                state_clone.storage,
                                                                state_clone.stats,
                                                                state_clone.resource_monitor,
                                                                state_clone.shutdown_tx.subscribe(),
                                                            );

                                                            if let Err(e) = handler.run().await {
                                                                if e.to_string().contains("Connection closed") {
                                                                    info!("Client handler finished: {e}");
                                                                } else {
                                                                    warn!("Client handler error: {e}");
                                                                }
                                                            }
                                                        }
                                                        Err(e) => {
                                                            error!("WebSocket TLS handshake failed: {e}");
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    error!("TLS handshake failed for WebSocket: {e}");
                                                }
                                            }
                                        });
                                    }
                                    Err(e) => {
                                        error!("WebSocket TLS accept error: {e}");
                                    }
                                }
                            }

                            _ = shutdown_rx_wss.recv() => {
                                debug!("WebSocket TLS accept task shutting down");
                                break;
                            }
                        }
                    }
                }));
            }
        }

        if let Some(tls_acceptor) = tls_acceptor {
            let acceptor = Arc::new(tls_acceptor);
            for tls_listener in tls_listeners {
                let acceptor = Arc::clone(&acceptor);
                let state = accept_state.clone();
                let mut shutdown_rx_tls = state.shutdown_tx.subscribe();

                task_handles.push(tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            accept_result = tls_listener.accept() => {
                                match accept_result {
                                    Ok((tcp_stream, addr)) => {
                                        debug!("New TLS connection from {}", addr);

                                        if !state.resource_monitor.can_accept_connection(addr.ip()).await {
                                            warn!("TLS connection rejected from {}: resource limits exceeded", addr);
                                            continue;
                                        }

                                        match accept_tls_connection(&acceptor, tcp_stream, addr).await {
                                            Ok(tls_stream) => {
                                                let transport = BrokerTransport::tls(tls_stream);

                                                let handler = ClientHandler::new(
                                                    transport,
                                                    addr,
                                                    state.snapshot_config(),
                                                    Arc::clone(&state.router),
                                                    state.snapshot_auth(),
                                                    state.storage.clone(),
                                                    Arc::clone(&state.stats),
                                                    Arc::clone(&state.resource_monitor),
                                                    state.shutdown_tx.subscribe(),
                                                );

                                                tokio::spawn(async move {
                                                    if let Err(e) = handler.run().await {
                                                        if e.is_normal_disconnect() {
                                                            debug!("Client handler finished");
                                                        } else {
                                                            warn!("Client handler error: {e}");
                                                        }
                                                    }
                                                });
                                            }
                                            Err(e) => {
                                                error!("TLS handshake failed: {e}");
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        error!("TLS accept error: {e}");
                                    }
                                }
                            }

                            _ = shutdown_rx_tls.recv() => {
                                debug!("TLS accept task shutting down");
                                break;
                            }
                        }
                    }
                }));
            }
        }

        info!(
            "Number of QUIC endpoints to start: {}",
            quic_endpoints.len()
        );
        for quic_endpoint in quic_endpoints {
            let state = accept_state.clone();
            let mut shutdown_rx_quic = state.shutdown_tx.subscribe();

            let local_addr = quic_endpoint.local_addr();
            task_handles.push(tokio::spawn(async move {
                debug!("QUIC accept loop starting for {:?}", local_addr);
                loop {
                    tokio::select! {
                        accept_result = accept_quic_connection(&quic_endpoint) => {
                            match accept_result {
                                Ok((connection, peer_addr)) => {
                                    debug!("New QUIC connection from {}", peer_addr);

                                    if !state.resource_monitor.can_accept_connection(peer_addr.ip()).await {
                                        warn!("QUIC connection rejected from {}: resource limits exceeded", peer_addr);
                                        connection.close(0u32.into(), b"resource limit");
                                        continue;
                                    }

                                    let conn = Arc::new(connection);
                                    let state_clone = state.clone();

                                    let cfg = state_clone.snapshot_config();
                                    let auth = state_clone.snapshot_auth();
                                    tokio::spawn(async move {
                                        run_quic_connection_handler(
                                            conn,
                                            peer_addr,
                                            cfg,
                                            state_clone.router,
                                            auth,
                                            state_clone.storage,
                                            state_clone.stats,
                                            state_clone.resource_monitor,
                                            state_clone.shutdown_tx.subscribe(),
                                        )
                                        .await;
                                    });
                                }
                                Err(e) => {
                                    if !e.to_string().contains("endpoint closed") {
                                        error!("QUIC accept error: {e}");
                                    }
                                }
                            }
                        }

                        _ = shutdown_rx_quic.recv() => {
                            debug!("QUIC accept task shutting down");
                            break;
                        }
                    }
                }
            }));
        }

        if !cluster_listeners.is_empty() {
            info!(
                "Starting Cluster accept tasks for {} listeners",
                cluster_listeners.len()
            );
        }

        let cluster_tls_acceptor = cluster_tls_acceptor.map(Arc::new);
        for cluster_listener in cluster_listeners {
            let state = accept_state.clone();
            let mut shutdown_rx_cluster = state.shutdown_tx.subscribe();
            let acceptor = cluster_tls_acceptor.clone();

            task_handles.push(tokio::spawn(async move {
                loop {
                    tokio::select! {
                        accept_result = cluster_listener.accept() => {
                            match accept_result {
                                Ok((tcp_stream, addr)) => {
                                    debug!(addr = %addr, "New Cluster connection");

                                    if !state.resource_monitor.can_accept_connection(addr.ip()).await {
                                        warn!("Cluster connection rejected from {}: resource limits exceeded", addr);
                                        continue;
                                    }

                                    if let Some(ref tls_acceptor) = acceptor {
                                        let acc_clone = Arc::clone(tls_acceptor);
                                        let state_clone = state.clone();

                                        tokio::spawn(async move {
                                            let cfg = state_clone.snapshot_config();
                                            let auth = state_clone.snapshot_auth();
                                            match accept_tls_connection(&acc_clone, tcp_stream, addr).await {
                                                Ok(tls_stream) => {
                                                    let transport = BrokerTransport::tls(tls_stream);

                                                    let handler = ClientHandler::new(
                                                        transport,
                                                        addr,
                                                        cfg,
                                                        state_clone.router,
                                                        auth,
                                                        state_clone.storage,
                                                        state_clone.stats,
                                                        state_clone.resource_monitor,
                                                        state_clone.shutdown_tx.subscribe(),
                                                    )
                                                    .with_skip_bridge_forwarding(true);

                                                    if let Err(e) = handler.run().await {
                                                        if e.is_normal_disconnect() {
                                                            debug!("Cluster client handler finished");
                                                        } else {
                                                            warn!("Cluster client handler error: {e}");
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    error!("Cluster TLS handshake failed: {e}");
                                                }
                                            }
                                        });
                                    } else {
                                        let transport = BrokerTransport::tcp(tcp_stream);

                                        let handler = ClientHandler::new(
                                            transport,
                                            addr,
                                            state.snapshot_config(),
                                            Arc::clone(&state.router),
                                            state.snapshot_auth(),
                                            state.storage.clone(),
                                            Arc::clone(&state.stats),
                                            Arc::clone(&state.resource_monitor),
                                            state.shutdown_tx.subscribe(),
                                        )
                                        .with_skip_bridge_forwarding(true);

                                        tokio::spawn(async move {
                                            if let Err(e) = handler.run().await {
                                                if e.is_normal_disconnect() {
                                                    debug!("Cluster client handler finished");
                                                } else {
                                                    warn!("Cluster client handler error: {e}");
                                                }
                                            }
                                        });
                                    }
                                }
                                Err(e) => {
                                    error!("Cluster accept error: {e}");
                                }
                            }
                        }

                        _ = shutdown_rx_cluster.recv() => {
                            debug!("Cluster accept task shutting down");
                            break;
                        }
                    }
                }
            }));
        }

        if !cluster_quic_endpoints.is_empty() {
            info!(
                "Starting Cluster QUIC accept tasks for {} endpoints",
                cluster_quic_endpoints.len()
            );
        }

        for cluster_quic_endpoint in cluster_quic_endpoints {
            let state = accept_state.clone();
            let mut shutdown_rx_quic_cluster = state.shutdown_tx.subscribe();

            let local_addr = cluster_quic_endpoint.local_addr();
            task_handles.push(tokio::spawn(async move {
                debug!("Cluster QUIC accept loop starting for {:?}", local_addr);
                loop {
                    tokio::select! {
                        accept_result = accept_quic_connection(&cluster_quic_endpoint) => {
                            match accept_result {
                                Ok((connection, peer_addr)) => {
                                    debug!("New Cluster QUIC connection from {}", peer_addr);

                                    if !state.resource_monitor.can_accept_connection(peer_addr.ip()).await {
                                        warn!("Cluster QUIC connection rejected from {}: resource limits exceeded", peer_addr);
                                        connection.close(0u32.into(), b"resource limit");
                                        continue;
                                    }

                                    let conn = Arc::new(connection);
                                    let state_clone = state.clone();

                                    let cfg = state_clone.snapshot_config();
                                    let auth = state_clone.snapshot_auth();
                                    tokio::spawn(async move {
                                        run_quic_cluster_connection_handler(
                                            conn,
                                            peer_addr,
                                            cfg,
                                            state_clone.router,
                                            auth,
                                            state_clone.storage,
                                            state_clone.stats,
                                            state_clone.resource_monitor,
                                            state_clone.shutdown_tx.subscribe(),
                                        )
                                        .await;
                                    });
                                }
                                Err(e) => {
                                    if !e.to_string().contains("endpoint closed") {
                                        error!("Cluster QUIC accept error: {e}");
                                    }
                                }
                            }
                        }

                        _ = shutdown_rx_quic_cluster.recv() => {
                            debug!("Cluster QUIC accept task shutting down");
                            break;
                        }
                    }
                }
            }));
        }

        info!(
            "Starting TCP accept tasks for {} listeners",
            listeners.len()
        );
        for listener in listeners {
            let state = accept_state.clone();
            let mut shutdown_rx_tcp = state.shutdown_tx.subscribe();

            task_handles.push(tokio::spawn(async move {
                loop {
                    tokio::select! {
                        accept_result = listener.accept() => {
                            match accept_result {
                                Ok((stream, addr)) => {
                                    debug!(addr = %addr, "New TCP connection");

                                    if !state.resource_monitor.can_accept_connection(addr.ip()).await {
                                        warn!("Connection rejected from {}: resource limits exceeded", addr);
                                        continue;
                                    }

                                    let transport = BrokerTransport::tcp(stream);

                                    let handler = ClientHandler::new(
                                        transport,
                                        addr,
                                        state.snapshot_config(),
                                        Arc::clone(&state.router),
                                        state.snapshot_auth(),
                                        state.storage.clone(),
                                        Arc::clone(&state.stats),
                                        Arc::clone(&state.resource_monitor),
                                        state.shutdown_tx.subscribe(),
                                    );

                                    tokio::spawn(async move {
                                        if let Err(e) = handler.run().await {
                                            if e.is_normal_disconnect() {
                                                debug!("Client handler finished");
                                            } else {
                                                warn!("Client handler error: {e}");
                                            }
                                        }
                                    });
                                }
                                Err(e) => {
                                    error!("TCP accept error: {e}");
                                }
                            }
                        }
                        _ = shutdown_rx_tcp.recv() => {
                            debug!("TCP accept task shutting down");
                            break;
                        }
                    }
                }
            }));
        }

        if let Some(mut manager) = self.hot_reload_manager.take() {
            if let Err(e) = manager.start().await {
                error!("Failed to start hot-reload watcher: {e}");
            } else {
                let mut change_rx = manager.subscribe_to_changes();
                let config_handle = manager.current_config_handle();
                let config_path = manager.config_path().to_path_buf();
                let config_watch_tx = self.config_watch_tx.clone();
                let auth_watch_tx = self.auth_watch_tx.clone();
                let resource_monitor = Arc::clone(&self.resource_monitor);
                let router = Arc::clone(&self.router);
                let mut old_config = Arc::clone(&self.config);
                let mut shutdown_rx_reload = shutdown_tx.subscribe();
                let mut reload_rx = self.reload_rx.take();

                task_handles.push(tokio::spawn(async move {
                    loop {
                        let reload_trigger = async {
                            if let Some(ref mut rx) = reload_rx {
                                rx.recv().await;
                            } else {
                                std::future::pending::<()>().await;
                            }
                        };

                        tokio::select! {
                            change_result = change_rx.recv() => {
                                match change_result {
                                    Ok(_event) => {}
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                        debug!("Config change channel closed");
                                        break;
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                        warn!("Config change listener lagged, skipped {n} events");
                                        continue;
                                    }
                                }
                            }
                            () = reload_trigger => {
                                info!("Manual config reload triggered");
                                match HotReloadManager::reload_config_file(&config_path).await {
                                    Ok(new_cfg) => {
                                        if let Err(e) = new_cfg.validate() {
                                            error!("Reloaded config is invalid, ignoring: {e}");
                                            continue;
                                        }
                                        *config_handle.write().await = new_cfg;
                                    }
                                    Err(e) => {
                                        error!("Failed to read config file: {e}");
                                        continue;
                                    }
                                }
                            }
                            _ = shutdown_rx_reload.recv() => {
                                debug!("Config change listener shutting down");
                                break;
                            }
                        }

                        let new_config = config_handle.read().await.clone();

                        Self::warn_non_reloadable_changes(&old_config, &new_config);

                        resource_monitor
                            .update_limits(Self::default_resource_limits(new_config.max_clients));

                        match create_auth_provider(&new_config.auth_config).await {
                            Ok(new_auth) => {
                                let _ = auth_watch_tx.send(new_auth);
                                info!("Auth provider recreated from reloaded config");

                                let echo_key = if new_config.echo_suppression_config.enabled {
                                    Some(new_config.echo_suppression_config.property_key.clone())
                                } else {
                                    None
                                };
                                router.update_echo_suppression_key(echo_key).await;

                                let new_config = Arc::new(new_config);
                                let _ = config_watch_tx.send(Arc::clone(&new_config));
                                old_config = new_config;
                                info!("Config watch updated for new connections");
                            }
                            Err(e) => {
                                error!("Failed to recreate auth provider, keeping old: {e}");
                            }
                        }
                    }
                }));
            }
        }

        info!("Broker ready - accepting connections");

        if let Some(ready_tx) = self.ready_tx.take() {
            let _ = ready_tx.send(true);
        }

        shutdown_rx.recv().await.ok();
        info!("Broker shutting down");

        sys_handle.abort();

        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for handle in task_handles {
                let _ = handle.await;
            }
        })
        .await;

        Ok(())
    }

    /// Shuts down the broker gracefully
    ///
    /// # Errors
    ///
    /// Returns an error if no receivers are available for shutdown signal
    pub async fn shutdown(&self) -> Result<()> {
        if let Some(ref shutdown_tx) = self.shutdown_tx {
            shutdown_tx.send(()).map_err(|_| {
                MqttError::InvalidState("No receivers for shutdown signal".to_string())
            })?;
        }

        if let Some(ref bridge_manager) = self.bridge_manager {
            info!("Stopping all bridges");
            if let Err(e) = bridge_manager.stop_all().await {
                error!("Error stopping bridges: {e}");
            }
        }

        // Give clients time to disconnect gracefully
        tokio::time::sleep(crate::time::Duration::from_millis(100)).await;

        info!("Broker shutdown complete");
        Ok(())
    }

    /// Gets broker statistics
    #[must_use]
    pub fn stats(&self) -> Arc<BrokerStats> {
        Arc::clone(&self.stats)
    }

    /// Gets resource monitor
    #[must_use]
    pub fn resource_monitor(&self) -> Arc<ResourceMonitor> {
        Arc::clone(&self.resource_monitor)
    }

    #[must_use]
    pub fn auth_provider(&self) -> Arc<dyn AuthProvider> {
        Arc::clone(&self.auth_provider)
    }

    #[must_use]
    pub fn router(&self) -> Arc<MessageRouter> {
        Arc::clone(&self.router)
    }

    #[must_use]
    pub fn local_addr(&self) -> Option<std::net::SocketAddr> {
        self.listeners.first()?.local_addr().ok()
    }

    #[must_use]
    pub fn ws_local_addr(&self) -> Option<std::net::SocketAddr> {
        self.ws_listeners.first()?.local_addr().ok()
    }

    /// Returns a receiver that signals when the broker is ready to accept connections
    ///
    /// Call this before spawning `run()` to get a receiver. The broker sends `true`
    /// when it starts accepting connections. Use `changed().await` to wait.
    #[must_use]
    pub fn ready_receiver(&self) -> watch::Receiver<bool> {
        self.ready_rx.clone()
    }

    fn warn_non_reloadable_changes(old: &BrokerConfig, new: &BrokerConfig) {
        if old.bind_addresses != new.bind_addresses {
            warn!("bind_addresses changed but requires restart to take effect");
        }
        if old.tls_config != new.tls_config {
            warn!("tls_config changed but requires restart to take effect");
        }
        if old.websocket_config != new.websocket_config {
            warn!("websocket_config changed but requires restart to take effect");
        }
        if old.websocket_tls_config != new.websocket_tls_config {
            warn!("websocket_tls_config changed but requires restart to take effect");
        }
        if old.quic_config != new.quic_config {
            warn!("quic_config changed but requires restart to take effect");
        }
        if old.cluster_listener_config != new.cluster_listener_config {
            warn!("cluster_listener_config changed but requires restart to take effect");
        }
        if old.storage_config != new.storage_config {
            warn!("storage_config changed but requires restart to take effect");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_broker_bind() {
        // Use random port to avoid conflicts
        let broker = MqttBroker::bind("127.0.0.1:0").await;
        assert!(broker.is_ok());
    }

    #[tokio::test]
    async fn test_broker_with_config() {
        let config = BrokerConfig::default()
            .with_bind_address(([127, 0, 0, 1], 0))
            .with_max_clients(100);

        let broker = MqttBroker::with_config(config).await;
        assert!(broker.is_ok());
    }

    #[tokio::test]
    async fn test_broker_shutdown() {
        let mut broker = MqttBroker::bind("127.0.0.1:0").await.unwrap();

        // Start broker in background
        let broker_handle = tokio::spawn(async move { broker.run().await });

        // Give broker time to start
        tokio::time::sleep(crate::time::Duration::from_millis(10)).await;

        // Now test shutdown - but we can't call it because broker was moved
        // Just ensure the broker starts without error for now
        broker_handle.abort();
    }

    #[tokio::test]
    async fn test_broker_stats() {
        let broker = MqttBroker::bind("127.0.0.1:0").await.unwrap();
        let stats = broker.stats();

        assert_eq!(
            stats
                .clients_connected
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }
}
