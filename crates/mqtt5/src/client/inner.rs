use crate::error::{MqttError, Result};
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::types::{ConnectOptions, ConnectResult};
use crate::Transport;
use tracing::instrument;

#[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
use crate::transport::quic::QuicConfig;
#[cfg(not(target_arch = "wasm32"))]
use crate::transport::tcp::TcpConfig;
#[cfg(not(target_arch = "wasm32"))]
use crate::transport::tls::TlsConfig;
#[cfg(all(not(target_arch = "wasm32"), feature = "transport-websocket"))]
use crate::transport::websocket::{WebSocketConfig, WebSocketTransport};
#[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
use crate::transport::QuicTransport;
#[cfg(not(target_arch = "wasm32"))]
use crate::transport::{TcpTransport, TlsTransport, TransportType};

use super::connection::ConnectionEvent;
use super::direct::{AutomaticReconnectLifecycle, DirectClientInner, StoredSubscription};
#[cfg(feature = "transport-websocket")]
use super::state::WebSocketSettings;
use super::state::{ClientTransportType, Reconnection};
use super::MqttClient;

/// The settings a QUIC connection's configuration was built from: the
/// configuration kept for early data is used again only with the same ones,
/// so that a quics:// connection is never made with one built for quic://,
/// for insecure TLS or for another stored TLS configuration, and a change to
/// early data, flow headers, datagrams or the stream limit takes effect
#[cfg(feature = "transport-quic")]
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct QuicBuiltFor {
    tls: QuicTlsBuiltFor,
    early_data: bool,
    flow_headers: bool,
    datagrams: bool,
    max_streams: Option<usize>,
}

#[cfg(feature = "transport-quic")]
#[derive(Clone, PartialEq, Eq)]
enum QuicTlsBuiltFor {
    Unverified,
    Secure {
        insecure_tls: bool,
        tls_config_times_stored: u64,
    },
}

/// A connected transport, with what its configuration was built from if it is
/// QUIC's
struct Connected {
    transport: TransportType,
    #[cfg(feature = "transport-quic")]
    quic_built_for: Option<QuicBuiltFor>,
}

impl From<TransportType> for Connected {
    fn from(transport: TransportType) -> Self {
        Self {
            transport,
            #[cfg(feature = "transport-quic")]
            quic_built_for: None,
        }
    }
}

pub(crate) struct ReplacedByConnect {
    options: ConnectOptions,
    auth_method: Option<String>,
    reconnection: Option<Reconnection>,
    lifecycle: AutomaticReconnectLifecycle,
    reconnect_attempt: u32,
    monitored: bool,
    stored_tls: StoredTlsBeforeConnect,
}

enum StoredTlsBeforeConnect {
    Untouched,
    Was {
        config: Box<Option<TlsConfig>>,
        times_stored: u64,
    },
}

struct ConnectInProgress {
    client: MqttClient,
    connect: u64,
    replaced: Option<ReplacedByConnect>,
}

impl ConnectInProgress {
    fn replaced_a_reconnection(&self) -> bool {
        self.replaced.as_ref().is_some_and(|replaced| {
            replaced.monitored && replaced.lifecycle == AutomaticReconnectLifecycle::Armed
        })
    }

    fn finish(mut self) {
        self.replaced = None;
    }
}

impl Drop for ConnectInProgress {
    fn drop(&mut self) {
        let Some(replaced) = self.replaced.take() else {
            return;
        };
        *self.client.cancelled_connect.lock() = Some(replaced);
        let client = self.client.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    let connection_guard = client.connection_mutex.lock().await;
                    client.settle_cancelled_connect().await;
                    drop(connection_guard);
                });
            }
            Err(_) => {
                tracing::warn!(
                    "Connect cancelled outside a runtime: it is undone by the next connect"
                );
            }
        }
    }
}

impl MqttClient {
    /// The configuration kept for early data, if it was built for `built_for`
    #[cfg(feature = "transport-quic")]
    async fn kept_quic_client_config(
        &self,
        built_for: &QuicBuiltFor,
    ) -> Option<quinn::ClientConfig> {
        let (kept_for, config) = self.quic_client_config.read().await.clone()?;
        (kept_for == *built_for).then_some(config)
    }

    async fn on_successful_connect(
        &self,
        stored_subs: Vec<StoredSubscription>,
        session_present: bool,
        keep_alive: std::time::Duration,
    ) {
        self.trigger_connection_event(ConnectionEvent::Connected {
            session_present,
            keep_alive,
        })
        .await;
        #[cfg(feature = "transport-quic")]
        self.recover_quic_flows().await;
        self.restore_subscriptions_after_connect(stored_subs, session_present)
            .await;
        if !session_present {
            self.restore_ack_subscriptions_after_lost_session().await;
        }
    }

    #[cfg(any(not(feature = "transport-websocket"), not(feature = "transport-quic")))]
    fn unsupported_transport_feature(transport: &str, feature: &str) -> MqttError {
        MqttError::Configuration(format!(
            "{transport} transport is disabled at compile time; enable the `{feature}` feature"
        ))
    }

    pub(crate) fn is_aws_iot_endpoint(hostname: &str) -> bool {
        hostname.contains(".iot.") && hostname.ends_with(".amazonaws.com")
    }

    /// Parses an address string to determine transport type and components
    ///
    /// # Errors
    ///
    /// Returns an error if the address format is invalid
    pub(crate) fn parse_address(address: &str) -> Result<(ClientTransportType, &str, u16)> {
        if let Some(rest) = address.strip_prefix("mqtt://") {
            let (host, port) = Self::split_host_port(rest, 1883)?;
            Ok((ClientTransportType::Tcp, host, port))
        } else if let Some(rest) = address.strip_prefix("mqtts://") {
            let (host, port) = Self::split_host_port(rest, 8883)?;
            Ok((ClientTransportType::Tls, host, port))
        } else if let Some(rest) = address.strip_prefix("ws://") {
            #[cfg(feature = "transport-websocket")]
            {
                let (host, port) = Self::split_host_port(rest, 80)?;
                Ok((
                    ClientTransportType::WebSocket(
                        address.to_string(),
                        WebSocketSettings::Configured,
                    ),
                    host,
                    port,
                ))
            }
            #[cfg(not(feature = "transport-websocket"))]
            {
                Self::split_host_port(rest, 80).and(Err(Self::unsupported_transport_feature(
                    "WebSocket",
                    "transport-websocket",
                )))
            }
        } else if let Some(rest) = address.strip_prefix("wss://") {
            #[cfg(feature = "transport-websocket")]
            {
                let (host, port) = Self::split_host_port(rest, 443)?;
                Ok((
                    ClientTransportType::WebSocketSecure(
                        address.to_string(),
                        WebSocketSettings::Configured,
                    ),
                    host,
                    port,
                ))
            }
            #[cfg(not(feature = "transport-websocket"))]
            {
                Self::split_host_port(rest, 443).and(Err(Self::unsupported_transport_feature(
                    "WebSocket",
                    "transport-websocket",
                )))
            }
        } else if let Some(rest) = address.strip_prefix("tcp://") {
            let (host, port) = Self::split_host_port(rest, 1883)?;
            Ok((ClientTransportType::Tcp, host, port))
        } else if let Some(rest) = address.strip_prefix("ssl://") {
            let (host, port) = Self::split_host_port(rest, 8883)?;
            Ok((ClientTransportType::Tls, host, port))
        } else if let Some(rest) = address.strip_prefix("quic://") {
            #[cfg(feature = "transport-quic")]
            {
                let (host, port) = Self::split_host_port(rest, 14567)?;
                Ok((ClientTransportType::Quic, host, port))
            }
            #[cfg(not(feature = "transport-quic"))]
            {
                Self::split_host_port(rest, 14567).and(Err(Self::unsupported_transport_feature(
                    "QUIC",
                    "transport-quic",
                )))
            }
        } else if let Some(rest) = address.strip_prefix("quics://") {
            #[cfg(feature = "transport-quic")]
            {
                let (host, port) = Self::split_host_port(rest, 14567)?;
                Ok((ClientTransportType::QuicSecure, host, port))
            }
            #[cfg(not(feature = "transport-quic"))]
            {
                Self::split_host_port(rest, 14567).and(Err(Self::unsupported_transport_feature(
                    "QUIC",
                    "transport-quic",
                )))
            }
        } else {
            let (host, port) = Self::split_host_port(address, 1883)?;
            Ok((ClientTransportType::Tcp, host, port))
        }
    }

    /// The address a server's redirect (`UseAnotherServer`, `ServerMoved`)
    /// sends a connection to `from` to. A reference with no scheme, as
    /// `host:port` usually is, means TCP, made TLS (mqtts://) when `from` is
    /// over TLS (mqtts://, wss://, quics://); and a connection over TLS is
    /// not redirected to one without.
    ///
    /// # Errors
    ///
    /// Returns an error if the reference is not an address, or would take a
    /// connection over TLS to one without
    pub(crate) fn redirect_address(from: &str, to: &str) -> Result<String> {
        let (from_transport, _, _) = Self::parse_address(from)?;
        if !to.contains("://") {
            return Ok(if from_transport.is_secure() {
                format!("mqtts://{to}")
            } else {
                to.to_string()
            });
        }
        let (to_transport, _, _) = Self::parse_address(to).map_err(|e| match e {
            MqttError::Configuration(reason) => {
                MqttError::ConnectionError(format!("Server redirect to {to} refused: {reason}"))
            }
            other => other,
        })?;
        if from_transport.is_secure() && !to_transport.is_secure() {
            return Err(MqttError::ConnectionError(format!(
                "Server redirect to {to} refused: it is not over TLS, as the connection redirected was"
            )));
        }
        Ok(to.to_string())
    }

    /// The name a certificate is checked against for `host`: an IPv6 address
    /// without the brackets it is written with in a URL
    fn server_name(host: &str) -> &str {
        host.strip_prefix('[')
            .and_then(|address| address.strip_suffix(']'))
            .unwrap_or(host)
    }

    /// Splits a host:port string
    ///
    /// # Errors
    ///
    /// Returns an error if the port is invalid
    pub(crate) fn split_host_port(address: &str, default_port: u16) -> Result<(&str, u16)> {
        let address_without_path = address.split('/').next().unwrap_or(address);

        if address_without_path.ends_with(']') {
            return Ok((address_without_path, default_port));
        }
        if let Some(colon_pos) = address_without_path.rfind(':') {
            let host = &address_without_path[..colon_pos];
            let port_str = &address_without_path[colon_pos + 1..];
            let port = port_str
                .parse::<u16>()
                .map_err(|_| MqttError::ConnectionError(format!("Invalid port: {port_str}")))?;
            Ok((host, port))
        } else {
            Ok((address_without_path, default_port))
        }
    }

    pub(crate) async fn resolve_addresses(
        host: &str,
        port: u16,
    ) -> Result<Vec<std::net::SocketAddr>> {
        let addr_str = format!("{host}:{port}");
        tracing::debug!(addr_str = %addr_str, "🌐 DNS RESOLUTION - Starting address resolution");

        let addrs: Vec<_> = tokio::net::lookup_host(addr_str.as_str())
            .await
            .map_err(|e| {
                tracing::error!(addr_str = %addr_str, error = %e, "🌐 DNS RESOLUTION - Failed to resolve address");
                MqttError::ConnectionError(format!("Failed to resolve address: {e}"))
            })?
            .collect();

        tracing::debug!(addr_str = %addr_str, resolved_count = addrs.len(), "🌐 DNS RESOLUTION - Address resolved successfully");

        if addrs.is_empty() {
            return Err(MqttError::ConnectionError(
                "No valid address found".to_string(),
            ));
        }

        Ok(addrs)
    }

    pub(crate) fn select_addresses_for_connection<'a>(
        addrs: &'a [std::net::SocketAddr],
        host: &str,
    ) -> &'a [std::net::SocketAddr] {
        if Self::is_aws_iot_endpoint(host) {
            tracing::debug!("AWS IoT endpoint detected, limiting to first resolved address");
            &addrs[0..1]
        } else {
            addrs
        }
    }

    /// Connects to `addr`, over TLS with `given_tls` if a connection made with
    /// `connect_with_tls` is redirected
    async fn try_connect_address(
        &self,
        addr: std::net::SocketAddr,
        client_transport_type: ClientTransportType,
        host: &str,
        given_tls: Option<&TlsConfig>,
    ) -> Result<Connected> {
        match client_transport_type {
            ClientTransportType::Tcp => Self::connect_tcp(addr).await.map(Connected::from),
            ClientTransportType::Tls => match given_tls {
                Some(given) => {
                    let mut config = given.clone();
                    config.addr = addr;
                    config.hostname = Self::server_name(host).to_string();
                    Self::open_tls(config).await.map(Connected::from)
                }
                None => self.connect_tls(addr, host).await.map(Connected::from),
            },
            #[cfg(feature = "transport-websocket")]
            ClientTransportType::WebSocket(url, settings) => self
                .connect_websocket(&url, settings)
                .await
                .map(Connected::from),
            #[cfg(feature = "transport-websocket")]
            ClientTransportType::WebSocketSecure(url, settings) => self
                .connect_websocket_secure(addr, host, &url, settings)
                .await
                .map(Connected::from),
            #[cfg(feature = "transport-quic")]
            ClientTransportType::Quic => self.connect_quic(addr, host).await,
            #[cfg(feature = "transport-quic")]
            ClientTransportType::QuicSecure => self.connect_quic_secure(addr, host).await,
        }
    }

    async fn connect_tcp(addr: std::net::SocketAddr) -> Result<TransportType> {
        let config = TcpConfig::new(addr);
        let mut tcp_transport = TcpTransport::new(config);
        tcp_transport
            .connect()
            .await
            .map_err(|e| MqttError::ConnectionError(format!("TCP connect failed: {e}")))?;
        Ok(TransportType::Tcp(tcp_transport))
    }

    async fn connect_tls(&self, addr: std::net::SocketAddr, host: &str) -> Result<TransportType> {
        let insecure = self.transport_config.read().await.insecure_tls;
        let tls_config_lock = self.tls_config.read().await;
        let config = if let Some(existing_config) = &tls_config_lock.config {
            tracing::debug!(
                "Using stored TLS config - use_system_roots: {}, has_ca: {}, has_cert: {}",
                existing_config.use_system_roots,
                existing_config.root_certs.is_some(),
                existing_config.client_cert.is_some()
            );
            let mut cfg = existing_config.clone();
            cfg.addr = addr;
            cfg.hostname = Self::server_name(host).to_string();
            cfg.verify_server_cert = !insecure;
            cfg
        } else {
            tracing::debug!("No stored TLS config, using default");
            TlsConfig::new(addr, Self::server_name(host)).with_verify_server_cert(!insecure)
        };
        drop(tls_config_lock);
        Self::open_tls(config).await
    }

    async fn open_tls(config: TlsConfig) -> Result<TransportType> {
        let mut tls_transport = TlsTransport::new(config);
        tls_transport
            .connect()
            .await
            .map_err(|e| MqttError::ConnectionError(format!("TLS connect failed: {e}")))?;
        Ok(TransportType::Tls(Box::new(tls_transport)))
    }

    /// The WebSocket configuration for a connection to `url`: the one set with
    /// `ConnectOptions::with_websocket_config`, read on every attempt so
    /// reconnects use it too, or a default one. A server redirect gets a
    /// default one, with that configuration's TLS configuration if the
    /// connection started over wss://.
    #[cfg(feature = "transport-websocket")]
    async fn websocket_config_for(
        &self,
        url: &str,
        settings: WebSocketSettings,
    ) -> Result<WebSocketConfig> {
        let configured = self.inner.read().await.options.websocket_config.clone();
        match (configured, settings) {
            (Some(config), WebSocketSettings::Configured) => {
                WebSocketConfig::clone(&config).with_url(url)
            }
            (Some(config), WebSocketSettings::Redirected { keeps_tls: true }) => {
                WebSocketConfig::new(url).map(|redirected| match config.tls_config.clone() {
                    Some(tls_config) => redirected.with_tls_config(tls_config),
                    None => redirected,
                })
            }
            (Some(_), WebSocketSettings::Redirected { keeps_tls: false }) => {
                WebSocketConfig::new(url)
            }
            (None, _) => WebSocketConfig::new(url),
        }
        .map_err(|e| MqttError::ConnectionError(format!("Invalid WebSocket URL: {e}")))
    }

    /// Refuses a redirect from a wss:// connection whose WebSocket
    /// configuration has a TLS configuration of its own to anything but
    /// wss://, which is all that TLS configuration goes with
    ///
    /// # Errors
    ///
    /// Returns an error if the redirect is refused
    #[cfg(feature = "transport-websocket")]
    async fn check_redirect_keeps_websocket_tls(&self, from: &str, to: &str) -> Result<()> {
        let (from_transport, _, _) = Self::parse_address(from)?;
        let (to_transport, _, _) = Self::parse_address(to)?;
        let own_tls = self
            .inner
            .read()
            .await
            .options
            .websocket_config
            .as_ref()
            .is_some_and(|config| config.tls_config.is_some());
        if own_tls
            && matches!(from_transport, ClientTransportType::WebSocketSecure(..))
            && !matches!(to_transport, ClientTransportType::WebSocketSecure(..))
        {
            return Err(MqttError::ConnectionError(format!(
                "Server redirect to {to} refused: the WebSocket configuration's TLS configuration is for wss:// alone"
            )));
        }
        Ok(())
    }

    #[cfg(feature = "transport-websocket")]
    async fn connect_websocket(
        &self,
        url: &str,
        settings: WebSocketSettings,
    ) -> Result<TransportType> {
        let config = self.websocket_config_for(url, settings).await?;
        Self::open_websocket(config).await
    }

    #[cfg(feature = "transport-websocket")]
    async fn connect_websocket_secure(
        &self,
        addr: std::net::SocketAddr,
        host: &str,
        url: &str,
        settings: WebSocketSettings,
    ) -> Result<TransportType> {
        let insecure = self.transport_config.read().await.insecure_tls;
        let mut config = self.websocket_config_for(url, settings).await?;

        if let Some(tls_config) = config.tls_config.as_mut() {
            if insecure {
                tls_config.verify_server_cert = false;
            }
            return Self::open_websocket(config).await;
        }

        // As in connect_tls: a stored TLS config applies to wss:// too. With
        // none, an insecure connection still needs one to disable
        // verification; a secure one keeps the WebSocket transport's default.
        // The stored ALPN protocols are left out: they name the protocol for
        // MQTT directly over TLS (mqtts://, e.g. AWS IoT's x-amzn-mqtt-ca),
        // while a WebSocket server negotiates HTTP.
        let stored = self.tls_config.read().await.config.clone();
        let tls_config = match stored {
            Some(mut cfg) => {
                cfg.addr = addr;
                cfg.hostname = Self::server_name(host).to_string();
                cfg.verify_server_cert = !insecure;
                cfg.alpn_protocols = None;
                Some(cfg)
            }
            None if insecure => {
                Some(TlsConfig::new(addr, Self::server_name(host)).with_verify_server_cert(false))
            }
            None => None,
        };
        if let Some(tls_config) = tls_config {
            config = config.with_tls_config(tls_config);
        }

        Self::open_websocket(config).await
    }

    #[cfg(feature = "transport-websocket")]
    async fn open_websocket(config: WebSocketConfig) -> Result<TransportType> {
        let mut ws_transport = WebSocketTransport::new(config);
        ws_transport.connect().await?;
        Ok(TransportType::WebSocket(Box::new(ws_transport)))
    }

    #[cfg(feature = "transport-quic")]
    async fn connect_quic(&self, addr: std::net::SocketAddr, host: &str) -> Result<Connected> {
        let (config, built_for) = self.quic_config(addr, host).await;
        Self::open_quic(config, built_for).await
    }

    /// The configuration of a quic:// connection to `addr`, which verifies
    /// nothing, so is built from none of the TLS settings
    #[cfg(feature = "transport-quic")]
    async fn quic_config(
        &self,
        addr: std::net::SocketAddr,
        host: &str,
    ) -> (QuicConfig, QuicBuiltFor) {
        let qc = self.transport_config.read().await;
        let built_for = QuicBuiltFor {
            tls: QuicTlsBuiltFor::Unverified,
            early_data: qc.enable_early_data,
            flow_headers: qc.flow_headers,
            datagrams: qc.datagrams,
            max_streams: qc.max_streams,
        };
        let server_name = if host.parse::<std::net::IpAddr>().is_ok() {
            "localhost"
        } else {
            host
        };
        let mut config = QuicConfig::new(addr, server_name)
            .with_verify_server_cert(false)
            .with_stream_strategy(qc.stream_strategy)
            .with_flow_headers(qc.flow_headers)
            .with_flow_expire_interval(qc.flow_expire.as_secs())
            .with_datagrams(qc.datagrams)
            .with_connect_timeout(qc.connect_timeout)
            .with_early_data(qc.enable_early_data);
        if let Some(max) = qc.max_streams {
            config = config.with_max_concurrent_streams(max);
        }
        let enable_early_data = qc.enable_early_data;
        drop(qc);
        if enable_early_data {
            if let Some(cached) = self.kept_quic_client_config(&built_for).await {
                config = config.with_cached_client_config(cached);
            }
        }
        (config, built_for)
    }

    #[cfg(feature = "transport-quic")]
    async fn open_quic(config: QuicConfig, built_for: QuicBuiltFor) -> Result<Connected> {
        let mut quic_transport = QuicTransport::new(config);
        quic_transport
            .connect()
            .await
            .map_err(|e| MqttError::ConnectionError(format!("QUIC connect failed: {e}")))?;
        Ok(Connected {
            transport: TransportType::Quic(Box::new(quic_transport)),
            quic_built_for: Some(built_for),
        })
    }

    #[cfg(feature = "transport-quic")]
    async fn connect_quic_secure(
        &self,
        addr: std::net::SocketAddr,
        host: &str,
    ) -> Result<Connected> {
        let (config, built_for) = self.quic_secure_config(addr, host).await;
        Self::open_quic(config, built_for).await
    }

    /// The configuration of a quics:// connection to `addr`, and what it was
    /// built from: the client's transport settings and a stored TLS
    /// configuration's certificates, each read once
    #[cfg(feature = "transport-quic")]
    async fn quic_secure_config(
        &self,
        addr: std::net::SocketAddr,
        host: &str,
    ) -> (QuicConfig, QuicBuiltFor) {
        let qc: crate::transport::ClientTransportConfig =
            (*self.transport_config.read().await).clone();
        let tls_config_lock = self.tls_config.read().await;
        let server_name = if host.parse::<std::net::IpAddr>().is_ok() {
            "localhost"
        } else {
            host
        };
        let mut config = QuicConfig::new(addr, server_name)
            .with_verify_server_cert(!qc.insecure_tls)
            .with_stream_strategy(qc.stream_strategy)
            .with_flow_headers(qc.flow_headers)
            .with_flow_expire_interval(qc.flow_expire.as_secs())
            .with_datagrams(qc.datagrams)
            .with_connect_timeout(qc.connect_timeout)
            .with_early_data(qc.enable_early_data);

        if let Some(max) = qc.max_streams {
            config = config.with_max_concurrent_streams(max);
        }

        if let Some(existing_config) = &tls_config_lock.config {
            tracing::debug!(
                "Using stored TLS config for QUIC - use_system_roots: {}, has_ca: {}, has_cert: {}",
                existing_config.use_system_roots,
                existing_config.root_certs.is_some(),
                existing_config.client_cert.is_some()
            );

            if let (Some(ref cert_chain), Some(ref key)) =
                (&existing_config.client_cert, &existing_config.client_key)
            {
                config = config.with_client_cert(cert_chain.clone(), key.clone_key());
            }

            if let Some(ref certs) = existing_config.root_certs {
                config = config.with_root_certs(certs.clone());
            }
        } else {
            tracing::debug!("No stored TLS config for QUIC, using default");
        }
        let built_for = QuicBuiltFor {
            tls: QuicTlsBuiltFor::Secure {
                insecure_tls: qc.insecure_tls,
                tls_config_times_stored: tls_config_lock.times_stored,
            },
            early_data: qc.enable_early_data,
            flow_headers: qc.flow_headers,
            datagrams: qc.datagrams,
            max_streams: qc.max_streams,
        };
        drop(tls_config_lock);

        if qc.enable_early_data {
            if let Some(cached) = self.kept_quic_client_config(&built_for).await {
                config = config.with_cached_client_config(cached);
            }
        }
        (config, built_for)
    }

    /// Refuses to send a CONNECT once `disconnect` has been called since the
    /// connect or reconnection began, which it does without waiting for one
    /// that is still opening its transport
    fn check_not_disconnected(inner: &DirectClientInner) -> Result<()> {
        if inner.automatic_reconnect_lifecycle == AutomaticReconnectLifecycle::Stopped {
            return Err(MqttError::ConnectionError(
                "disconnect was called while the connection was being opened".to_string(),
            ));
        }
        Ok(())
    }

    pub(crate) async fn try_connect_to_addresses(
        &self,
        addresses: &[std::net::SocketAddr],
        transport_type: ClientTransportType,
        host: &str,
        given_tls: Option<&TlsConfig>,
    ) -> Result<ConnectResult> {
        let mut last_error = None;

        for addr in addresses {
            tracing::debug!("Trying to connect to address: {}", addr);

            let connected = match self
                .try_connect_address(*addr, transport_type.clone(), host, given_tls)
                .await
            {
                Ok(connected) => connected,
                Err(e) => {
                    tracing::debug!("Failed to connect to {}: {}", addr, e);
                    last_error = Some(e);
                    continue;
                }
            };

            let mut inner = self.inner.write().await;
            Self::check_not_disconnected(&inner)?;
            match inner.connect(connected.transport).await {
                Ok(result) => {
                    inner.reconnect_attempt = 0;
                    #[cfg(feature = "transport-quic")]
                    if let (Some(cached), Some(built_for)) = (
                        inner.cached_quic_client_config.take(),
                        connected.quic_built_for,
                    ) {
                        *self.quic_client_config.write().await = Some((built_for, cached));
                    }
                    let stored_subs = inner.stored_subscriptions.lock().clone();
                    let session_present = result.session_present;
                    let keep_alive = inner.negotiated_keep_alive();
                    drop(inner);

                    self.on_successful_connect(stored_subs, session_present, keep_alive)
                        .await;

                    return Ok(result);
                }
                Err(
                    e @ MqttError::ConnectionRefused(
                        ReasonCode::UseAnotherServer | ReasonCode::ServerMoved,
                    ),
                ) => {
                    drop(inner);
                    return Err(e);
                }
                Err(e) => {
                    drop(inner);
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            MqttError::ConnectionError("Failed to connect to any address".to_string())
        }))
    }

    pub(crate) async fn connect_internal(&self, address: &str) -> Result<ConnectResult> {
        Box::pin(self.connect_following_redirects(address, 0, None)).await
    }

    /// Connects to `address`, following the server's redirects. A connection
    /// made with `connect_with_tls` is redirected with its TLS configuration,
    /// `given_tls`, which is for TLS over TCP alone.
    async fn connect_following_redirects(
        &self,
        address: &str,
        redirects_followed: u8,
        given_tls: Option<&TlsConfig>,
    ) -> Result<ConnectResult> {
        const MAX_REDIRECTS: u8 = 3;
        self.inner.read().await.options.validate_deferred_ack()?;
        let mut current_address = address.to_string();
        let mut redirect_count = redirects_followed;
        let started_over_wss = Self::parse_address(address)?.0.is_websocket_secure();

        loop {
            let client_id = self.inner.read().await.options.client_id.clone();
            tracing::debug!(
                address = %current_address,
                client_id = %client_id,
                redirect_count,
                "Connection attempt"
            );

            let (requested_transport_type, host, port) = Self::parse_address(&current_address)?;
            if given_tls.is_some() && !matches!(requested_transport_type, ClientTransportType::Tls)
            {
                return Err(MqttError::ConnectionError(format!(
                    "Server redirect to {current_address} refused: a connection made with connect_with_tls is only redirected to mqtts://, which its TLS configuration is for"
                )));
            }
            let client_transport_type = if redirect_count == 0 {
                requested_transport_type
            } else {
                requested_transport_type.redirected(started_over_wss)
            };
            let addrs = if client_transport_type.dials_by_url() {
                vec![std::net::SocketAddr::new(
                    std::net::Ipv4Addr::UNSPECIFIED.into(),
                    port,
                )]
            } else {
                Self::resolve_addresses(host, port).await?
            };
            let addresses_to_try = Self::select_addresses_for_connection(&addrs, host);

            match self
                .try_connect_to_addresses(addresses_to_try, client_transport_type, host, given_tls)
                .await
            {
                Ok(result) => return Ok(result),
                Err(MqttError::ConnectionRefused(
                    reason @ (ReasonCode::UseAnotherServer | ReasonCode::ServerMoved),
                )) => {
                    let redirect_url = self.inner.write().await.server_redirect.take();
                    if let Some(url) = redirect_url {
                        if redirect_count < MAX_REDIRECTS {
                            tracing::info!(
                                from = %current_address,
                                to = %url,
                                reason = ?reason,
                                "Following server redirect"
                            );
                            redirect_count += 1;
                            let next_address = Self::redirect_address(&current_address, &url)?;
                            #[cfg(feature = "transport-websocket")]
                            self.check_redirect_keeps_websocket_tls(address, &next_address)
                                .await?;
                            current_address = next_address;
                            continue;
                        }
                    }
                    return Err(MqttError::ConnectionError(
                        "Server redirect failed: too many redirects or missing server reference"
                            .to_string(),
                    ));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Connects with `tls_config` as it is given, as `connect_with_tls` and
    /// its reconnections do, and follows a redirect with it
    ///
    /// # Errors
    ///
    /// Returns an error if TLS connection fails
    pub(crate) async fn connect_with_tls_config(
        &self,
        tls_config: TlsConfig,
    ) -> Result<ConnectResult> {
        let transport = Self::open_tls(tls_config.clone()).await?;

        let mut inner = self.inner.write().await;
        Self::check_not_disconnected(&inner)?;
        match inner.connect(transport).await {
            Ok(result) => {
                inner.reconnect_attempt = 0;
                let stored_subs = inner.stored_subscriptions.lock().clone();
                let session_present = result.session_present;
                let keep_alive = inner.negotiated_keep_alive();
                drop(inner);

                self.on_successful_connect(stored_subs, session_present, keep_alive)
                    .await;

                Ok(result)
            }
            Err(MqttError::ConnectionRefused(
                reason @ (ReasonCode::UseAnotherServer | ReasonCode::ServerMoved),
            )) => {
                let redirect_url = inner.server_redirect.take();
                drop(inner);
                if let Some(url) = redirect_url {
                    tracing::info!(
                        to = %url,
                        reason = ?reason,
                        "Following server redirect from TLS connection"
                    );
                    let url = Self::redirect_address("mqtts://", &url)?;
                    return Box::pin(self.connect_following_redirects(&url, 1, Some(&tls_config)))
                        .await;
                }
                Err(MqttError::ConnectionError(
                    "Server redirect missing server reference".to_string(),
                ))
            }
            Err(e) => {
                drop(inner);
                Err(e)
            }
        }
    }

    #[instrument(skip(self, options), fields(client_id = %options.client_id, clean_start = %options.clean_start), level = "debug")]
    pub(crate) async fn connect_with_options_internal(
        &self,
        address: &str,
        options: ConnectOptions,
    ) -> Result<ConnectResult> {
        if self.is_connected().await {
            return Err(MqttError::AlreadyConnected);
        }

        let in_progress = self
            .begin_connect(
                &options,
                Reconnection::Address(address.to_string()),
                options.properties.authentication_method.clone(),
            )
            .await;

        self.trigger_connection_event(ConnectionEvent::Connecting)
            .await;
        let result = self.connect_internal(address).await;
        self.monitor_after_connect(
            &result,
            &options,
            in_progress.connect,
            in_progress.replaced_a_reconnection(),
        );
        in_progress.finish();
        result
    }

    async fn begin_connect(
        &self,
        options: &ConnectOptions,
        reconnection: Reconnection,
        auth_method: Option<String>,
    ) -> ConnectInProgress {
        let mut inner = self.inner.write().await;
        let replaced = ReplacedByConnect {
            options: std::mem::replace(&mut inner.options, options.clone()),
            auth_method: std::mem::replace(&mut inner.auth_method, auth_method),
            reconnection: inner.reconnection.replace(reconnection),
            lifecycle: inner.automatic_reconnect_lifecycle,
            reconnect_attempt: inner.reconnect_attempt,
            monitored: *self.monitored_connect.lock() == Some(inner.connects),
            stored_tls: StoredTlsBeforeConnect::Untouched,
        };
        inner.reconnect_attempt = 0;
        inner.connects += 1;
        inner.automatic_reconnect_lifecycle = AutomaticReconnectLifecycle::Armed;
        ConnectInProgress {
            client: self.clone(),
            connect: inner.connects,
            replaced: Some(replaced),
        }
    }

    pub(crate) async fn settle_cancelled_connect(&self) {
        if self.cancelled_connect.lock().is_none() {
            return;
        }
        let mut inner = self.inner.write().await;
        let mut stored = self.tls_config.write().await;
        let Some(replaced) = self.cancelled_connect.lock().take() else {
            return;
        };
        let armed = inner.automatic_reconnect_lifecycle == AutomaticReconnectLifecycle::Armed;
        if inner.is_connected() {
            if armed && inner.options.reconnect_config.enabled {
                let connect = inner.connects;
                *self.monitored_connect.lock() = Some(connect);
                self.spawn_monitor(connect);
            }
            let stored_subs = inner.stored_subscriptions.lock().clone();
            drop(stored);
            drop(inner);
            let client = self.clone();
            tokio::spawn(async move {
                client
                    .restore_subscriptions_after_connect(stored_subs, false)
                    .await;
                client.restore_ack_subscriptions_after_lost_session().await;
            });
            return;
        }
        let stays_armed = armed && replaced.lifecycle == AutomaticReconnectLifecycle::Armed;
        inner.options = replaced.options;
        inner.auth_method = replaced.auth_method;
        inner.reconnection = replaced.reconnection;
        inner.reconnect_attempt = replaced.reconnect_attempt;
        inner.connects += 1;
        inner.automatic_reconnect_lifecycle = if stays_armed {
            AutomaticReconnectLifecycle::Armed
        } else {
            AutomaticReconnectLifecycle::Stopped
        };
        let restored_connect = inner.connects;
        let monitors = stays_armed && replaced.monitored;
        if monitors {
            *self.monitored_connect.lock() = Some(restored_connect);
        }
        drop(inner);
        if let StoredTlsBeforeConnect::Was {
            config,
            times_stored,
        } = replaced.stored_tls
        {
            stored.restore(*config, times_stored);
        }
        drop(stored);
        if monitors {
            self.spawn_monitor(restored_connect);
        }
    }

    fn spawn_monitor(&self, connect: u64) {
        let client = self.clone();
        tokio::spawn(async move {
            client.monitor_connection(connect).await;
        });
    }

    /// Whether another attempt can't help: the server refused this client or its credentials,
    /// or the options are wrong
    pub(super) fn refused_for_good(error: &MqttError) -> bool {
        matches!(
            error,
            MqttError::Configuration(_)
                | MqttError::ConnectionRefused(
                    ReasonCode::NotAuthorized
                        | ReasonCode::BadUsernameOrPassword
                        | ReasonCode::Banned
                        | ReasonCode::BadAuthenticationMethod
                        | ReasonCode::ClientIdentifierNotValid
                        | ReasonCode::UnsupportedProtocolVersion
                )
        )
    }

    /// Starts the task that reconnects when the connection made by connect
    /// number `connect` is lost, if the options ask for it, once it is made or
    /// has failed in a way that a later attempt may not
    fn monitor_after_connect(
        &self,
        result: &Result<ConnectResult>,
        options: &ConnectOptions,
        connect: u64,
        replaced_a_reconnection: bool,
    ) {
        if let Err(ref error) = result {
            let error_recovery_config =
                crate::client::error_recovery::ErrorRecoveryConfig::default();
            if (replaced_a_reconnection && !Self::refused_for_good(error))
                || crate::client::error_recovery::is_recoverable(error, &error_recovery_config)
                    .is_some()
            {
                if options.reconnect_config.enabled {
                    tracing::debug!(
                        error = %error,
                        "Initial connection failed with recoverable error, starting background reconnection"
                    );
                    *self.monitored_connect.lock() = Some(connect);
                    self.spawn_monitor(connect);
                } else {
                    tracing::debug!(error = %error, "Initial connection failed with recoverable error, but automatic reconnection is disabled");
                }
            } else {
                tracing::debug!(error = %error, "Initial connection failed with non-recoverable error, not starting background reconnection");
            }
        } else if result.is_ok() && options.reconnect_config.enabled {
            tracing::debug!(
                "Successful connection, starting monitor task for future disconnections"
            );
            *self.monitored_connect.lock() = Some(connect);
            self.spawn_monitor(connect);
        }
    }

    /// Internal TLS connection method (no mutex guard)
    ///
    /// # Errors
    ///
    /// Returns an error if connection fails or configuration is invalid
    pub(crate) async fn connect_with_tls_and_options_internal(
        &self,
        tls_config: TlsConfig,
        options: ConnectOptions,
    ) -> Result<ConnectResult> {
        if self.is_connected().await {
            return Err(MqttError::AlreadyConnected);
        }

        let auth_method = options.properties.authentication_method.clone();
        let mut in_progress = self
            .begin_connect(
                &options,
                Reconnection::Tls(Box::new(tls_config.clone())),
                auth_method,
            )
            .await;
        {
            let mut stored = self.tls_config.write().await;
            let config = stored.config.clone();
            stored.store(tls_config.clone());
            if let Some(replaced) = in_progress.replaced.as_mut() {
                replaced.stored_tls = StoredTlsBeforeConnect::Was {
                    config: Box::new(config),
                    times_stored: stored.times_stored,
                };
            }
        }

        self.trigger_connection_event(ConnectionEvent::Connecting)
            .await;
        let result = self.connect_with_tls_config(tls_config).await;
        self.monitor_after_connect(
            &result,
            &options,
            in_progress.connect,
            in_progress.replaced_a_reconnection(),
        );
        in_progress.finish();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::MqttClient;
    use std::future::Future;
    use std::task::{Context, Waker};

    async fn cancelled_connect_with_tls(client: &MqttClient) -> tokio::net::TcpListener {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config =
            crate::transport::tls::TlsConfig::new(silent.local_addr().unwrap(), "localhost");
        let outcome = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            client.connect_with_tls(config),
        )
        .await;
        assert!(outcome.is_err(), "the connect was to be cancelled");
        silent
    }

    #[tokio::test]
    async fn a_connect_with_tls_restarts_the_attempt_count() {
        let client = MqttClient::new("tls-attempts");
        client.inner.write().await.reconnect_attempt = 3;
        let refusing = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = refusing.local_addr().unwrap();
        drop(refusing);
        let config = crate::transport::tls::TlsConfig::new(addr, "localhost");
        assert!(client.connect_with_tls(config).await.is_err());
        assert_eq!(client.inner.read().await.reconnect_attempt, 0);
    }

    #[tokio::test]
    async fn a_cancelled_connect_puts_back_the_auth_method_and_the_attempt_count() {
        let client = MqttClient::new("cancelled-state");
        {
            let mut inner = client.inner.write().await;
            inner.auth_method = Some("first".to_string());
            inner.reconnect_attempt = 3;
        }
        let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let options = crate::types::ConnectOptions::new("cancelled-state")
            .with_authentication_method("second");
        let outcome = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            client
                .connect_with_options(&format!("mqtt://{}", silent.local_addr().unwrap()), options),
        )
        .await;
        assert!(outcome.is_err(), "the connect was to be cancelled");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let inner = client.inner.read().await;
        assert_eq!(inner.auth_method.as_deref(), Some("first"));
        assert_eq!(inner.reconnect_attempt, 3);
    }

    #[tokio::test]
    async fn a_connect_cancelled_while_it_settles_an_earlier_one_loses_nothing() {
        let client = MqttClient::new("settle-cancelled");
        client.inner.write().await.options = crate::types::ConnectOptions::new("first");
        let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("mqtt://{}", silent.local_addr().unwrap());
        let wait = std::time::Duration::from_millis(100);
        let options = |id| crate::types::ConnectOptions::new(id);
        let cancelled =
            tokio::time::timeout(wait, client.connect_with_options(&url, options("second"))).await;
        assert!(cancelled.is_err());
        let held = client.inner.read().await;
        let cancelled =
            tokio::time::timeout(wait, client.connect_with_options(&url, options("third"))).await;
        assert!(cancelled.is_err());
        drop(held);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(client.inner.read().await.options.client_id, "first");
    }

    #[tokio::test]
    async fn cancelled_connects_with_tls_leave_nothing_stored() {
        let client = MqttClient::new("cancelled-tls");
        let first = cancelled_connect_with_tls(&client).await;
        let second = cancelled_connect_with_tls(&client).await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        drop((first, second));
        assert!(client.tls_config.read().await.config.is_none());
    }

    #[tokio::test]
    async fn a_tls_connect_cancelled_before_it_stores_leaves_nothing_stored() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = MqttClient::new("cancelled-early");
        let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config =
            crate::transport::tls::TlsConfig::new(silent.local_addr().unwrap(), "localhost");
        let held = client.inner.read().await;
        let outcome = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            client.connect_with_tls(config),
        )
        .await;
        drop(held);
        assert!(outcome.is_err(), "the connect was to be cancelled");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(client.tls_config.read().await.config.is_none());
    }

    #[tokio::test]
    async fn a_tls_configuration_set_after_a_cancelled_connect_stays() {
        let client = MqttClient::new("cancelled-then-set");
        let silent = cancelled_connect_with_tls(&client).await;
        let ca = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../test_certs/ca.pem"
        ))
        .unwrap();
        client.set_tls_config(None, None, Some(ca)).await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        drop(silent);
        assert!(client.tls_config.read().await.config.is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hostname_resolution_yields_to_the_runtime() {
        let mut resolution = Box::pin(MqttClient::resolve_addresses("localhost", 1883));
        let first_poll = resolution
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()));
        assert!(
            first_poll.is_pending(),
            "resolving a hostname must not block the runtime thread"
        );

        let addrs = resolution.await.unwrap();
        assert!(!addrs.is_empty());
        assert!(addrs.iter().all(|addr| addr.port() == 1883));
        assert!(addrs.iter().all(|addr| addr.ip().is_loopback()));
    }

    #[test]
    fn a_redirect_without_a_scheme_is_tcp_made_tls_from_a_secure_connection() {
        assert_eq!(
            MqttClient::redirect_address("mqtt://lb:1883", "backend:1883").unwrap(),
            "backend:1883"
        );
        assert_eq!(
            MqttClient::redirect_address("mqtts://lb:8883", "backend:8883").unwrap(),
            "mqtts://backend:8883"
        );
        #[cfg(feature = "transport-websocket")]
        {
            assert_eq!(
                MqttClient::redirect_address("ws://lb/mqtt", "backend:1883").unwrap(),
                "backend:1883"
            );
            assert_eq!(
                MqttClient::redirect_address("wss://lb/mqtt", "backend:8883").unwrap(),
                "mqtts://backend:8883"
            );
        }
        #[cfg(feature = "transport-quic")]
        {
            assert_eq!(
                MqttClient::redirect_address("quic://lb:14567", "backend:1883").unwrap(),
                "backend:1883"
            );
            assert_eq!(
                MqttClient::redirect_address("quics://lb:14567", "backend:8883").unwrap(),
                "mqtts://backend:8883"
            );
        }
    }

    #[test]
    fn a_redirect_from_a_secure_connection_to_a_plain_one_is_refused() {
        for plain in ["mqtt://backend", "tcp://backend"] {
            assert!(MqttClient::redirect_address("mqtts://lb", plain).is_err());
        }
        #[cfg(feature = "transport-websocket")]
        {
            assert!(MqttClient::redirect_address("wss://lb/mqtt", "ws://backend/mqtt").is_err());
            assert!(MqttClient::redirect_address("mqtts://lb", "ws://backend/mqtt").is_err());
        }
        #[cfg(feature = "transport-quic")]
        assert!(MqttClient::redirect_address("quics://lb", "quic://backend").is_err());
        assert_eq!(
            MqttClient::redirect_address("mqtt://lb", "mqtts://backend").unwrap(),
            "mqtts://backend"
        );
    }
}
