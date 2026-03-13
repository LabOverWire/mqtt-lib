//! Builder patterns and factory methods for MQTT client

use crate::types::ConnectOptions;
use std::sync::Arc;
use tokio::sync::RwLock;

#[cfg(not(target_arch = "wasm32"))]
use crate::transport::tls::TlsConfig;

use super::connection::ConnectionEvent;
use super::direct::DirectClientInner;
use super::error_recovery::ErrorRecoveryConfig;

pub type ConnectionEventCallback = Arc<dyn Fn(ConnectionEvent) + Send + Sync>;

use super::MqttClient;

impl MqttClient {
    /// Creates a new MQTT client with default options
    ///
    /// # Examples
    ///
    /// ```
    /// use mqtt5::MqttClient;
    ///
    /// let client = MqttClient::new("my-device-001");
    /// ```
    pub fn new(client_id: impl Into<String>) -> Self {
        let client_id_str = client_id.into();
        tracing::trace!(client_id = %client_id_str, "MQTT CLIENT - new() method called");
        let options = ConnectOptions::new(client_id_str);
        Self::with_options(options)
    }

    /// Creates a new MQTT client with custom options
    ///
    /// # Examples
    ///
    /// ```
    /// use mqtt5::{MqttClient, ConnectOptions};
    /// use std::time::Duration;
    ///
    /// let options = ConnectOptions::new("client-001")
    ///     .with_clean_start(true)
    ///     .with_keep_alive(Duration::from_secs(60))
    ///     .with_credentials("mqtt_user", b"secret");
    ///
    /// let client = MqttClient::with_options(options);
    /// ```
    #[must_use]
    pub fn with_options(options: ConnectOptions) -> Self {
        tracing::trace!(client_id = %options.client_id, "MQTT CLIENT - with_options() method called");
        let inner = DirectClientInner::new(options);

        Self {
            inner: Arc::new(RwLock::new(inner)),
            connection_event_callbacks: Arc::new(RwLock::new(Vec::new())),
            error_callbacks: Arc::new(RwLock::new(Vec::new())),
            error_recovery_config: Arc::new(RwLock::new(ErrorRecoveryConfig::default())),
            connection_mutex: Arc::new(tokio::sync::Mutex::new(())),
            tls_config: Arc::new(RwLock::new(None)),
            transport_config: Arc::new(RwLock::new(
                crate::transport::ClientTransportConfig::default(),
            )),
        }
    }

    /// Set whether to skip TLS certificate verification
    ///
    /// # Safety
    ///
    /// This disables certificate verification and should only be used for testing
    /// with self-signed certificates. Never use in production.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use mqtt5::MqttClient;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = MqttClient::new("my-client");
    ///
    /// // Enable insecure mode for testing
    /// client.set_insecure_tls(true).await;
    ///
    /// // Connect will now skip certificate verification
    /// client.connect("mqtts://test-broker:8883").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn set_insecure_tls(&self, insecure: bool) {
        self.transport_config.write().await.insecure_tls = insecure;
    }

    pub async fn set_quic_stream_strategy(&self, strategy: crate::transport::StreamStrategy) {
        self.transport_config.write().await.stream_strategy = strategy;
    }

    pub async fn set_quic_flow_headers(&self, enable: bool) {
        self.transport_config.write().await.flow_headers = enable;
    }

    pub async fn set_quic_flow_expire(&self, duration: crate::time::Duration) {
        self.transport_config.write().await.flow_expire = duration;
    }

    pub async fn set_quic_max_streams(&self, max: Option<usize>) {
        self.transport_config.write().await.max_streams = max;
    }

    pub async fn set_quic_datagrams(&self, enable: bool) {
        self.transport_config.write().await.datagrams = enable;
    }

    pub async fn set_quic_connect_timeout(&self, timeout: crate::time::Duration) {
        self.transport_config.write().await.connect_timeout = timeout;
    }

    pub async fn set_tls_config(
        &self,
        cert_pem: Option<Vec<u8>>,
        key_pem: Option<Vec<u8>>,
        ca_cert_pem: Option<Vec<u8>>,
    ) {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        tracing::debug!(
            "set_tls_config called - cert: {}, key: {}, ca: {}",
            cert_pem.is_some(),
            key_pem.is_some(),
            ca_cert_pem.is_some()
        );

        let mut config_lock = self.tls_config.write().await;
        let placeholder_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);

        if cert_pem.is_some() || key_pem.is_some() || ca_cert_pem.is_some() {
            let mut config = TlsConfig::new(placeholder_addr, "placeholder");

            if let (Some(cert), Some(key)) = (cert_pem, key_pem) {
                if let Err(e) = config.load_client_cert_pem_bytes(&cert) {
                    tracing::error!("Failed to load client certificate: {e}");
                    return;
                }
                if let Err(e) = config.load_client_key_pem_bytes(&key) {
                    tracing::error!("Failed to load client key: {e}");
                    return;
                }
                tracing::debug!("Loaded client cert and key");
            }

            if let Some(ca_cert) = ca_cert_pem {
                if let Err(e) = config.load_ca_cert_pem_bytes(&ca_cert) {
                    tracing::error!("Failed to load CA certificate: {e}");
                    return;
                }
                config.use_system_roots = false;
                tracing::debug!(
                    "Loaded CA cert, use_system_roots=false, has {} certs",
                    config.root_certs.as_ref().map_or(0, Vec::len)
                );
            }

            *config_lock = Some(config);
            tracing::debug!("TLS config stored");
        }
    }
}
