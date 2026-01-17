//! Internal client implementation details

use crate::error::{MqttError, Result};
use crate::types::{ConnectOptions, ConnectResult};
use crate::Transport;
use std::net::ToSocketAddrs;
use tracing::instrument;

#[cfg(not(target_arch = "wasm32"))]
use crate::transport::quic::QuicConfig;
#[cfg(not(target_arch = "wasm32"))]
use crate::transport::tcp::TcpConfig;
#[cfg(not(target_arch = "wasm32"))]
use crate::transport::tls::TlsConfig;
#[cfg(not(target_arch = "wasm32"))]
use crate::transport::websocket::{WebSocketConfig, WebSocketTransport};
#[cfg(not(target_arch = "wasm32"))]
use crate::transport::{QuicTransport, TcpTransport, TlsTransport, TransportType};

use super::connection::{ConnectionEvent, DisconnectReason};
use super::state::ClientTransportType;
use super::MqttClient;

impl MqttClient {
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
            let (host, port) = Self::split_host_port(rest, 80)?;
            Ok((
                ClientTransportType::WebSocket(address.to_string()),
                host,
                port,
            ))
        } else if let Some(rest) = address.strip_prefix("wss://") {
            let (host, port) = Self::split_host_port(rest, 443)?;
            Ok((
                ClientTransportType::WebSocketSecure(address.to_string()),
                host,
                port,
            ))
        } else if let Some(rest) = address.strip_prefix("tcp://") {
            let (host, port) = Self::split_host_port(rest, 1883)?;
            Ok((ClientTransportType::Tcp, host, port))
        } else if let Some(rest) = address.strip_prefix("ssl://") {
            let (host, port) = Self::split_host_port(rest, 8883)?;
            Ok((ClientTransportType::Tls, host, port))
        } else if let Some(rest) = address.strip_prefix("quic://") {
            let (host, port) = Self::split_host_port(rest, 14567)?;
            Ok((ClientTransportType::Quic, host, port))
        } else if let Some(rest) = address.strip_prefix("quics://") {
            let (host, port) = Self::split_host_port(rest, 14567)?;
            Ok((ClientTransportType::QuicSecure, host, port))
        } else {
            let (host, port) = Self::split_host_port(address, 1883)?;
            Ok((ClientTransportType::Tcp, host, port))
        }
    }

    /// Splits a host:port string
    ///
    /// # Errors
    ///
    /// Returns an error if the port is invalid
    pub(crate) fn split_host_port(address: &str, default_port: u16) -> Result<(&str, u16)> {
        let address_without_path = address.split('/').next().unwrap_or(address);

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

    pub(crate) fn resolve_addresses(host: &str, port: u16) -> Result<Vec<std::net::SocketAddr>> {
        let addr_str = format!("{host}:{port}");
        tracing::debug!(addr_str = %addr_str, "🌐 DNS RESOLUTION - Starting address resolution");

        let addrs: Vec<_> = addr_str
            .to_socket_addrs()
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
        let is_aws_iot = Self::is_aws_iot_endpoint(host);

        if is_aws_iot {
            tracing::debug!("AWS IoT endpoint detected, limiting to first resolved address");
            &addrs[0..1]
        } else {
            addrs
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) async fn try_connect_address(
        &self,
        addr: std::net::SocketAddr,
        client_transport_type: ClientTransportType,
        host: &str,
    ) -> Result<TransportType> {
        match client_transport_type {
            ClientTransportType::Tcp => {
                let config = TcpConfig::new(addr);
                let mut tcp_transport = TcpTransport::new(config);
                tcp_transport
                    .connect()
                    .await
                    .map_err(|e| MqttError::ConnectionError(format!("TCP connect failed: {e}")))?;
                Ok(TransportType::Tcp(tcp_transport))
            }
            ClientTransportType::Tls => {
                let insecure = self.transport_config.read().await.insecure_tls;
                let tls_config_lock = self.tls_config.read().await;
                let config = if let Some(existing_config) = &*tls_config_lock {
                    tracing::debug!(
                        "Using stored TLS config - use_system_roots: {}, has_ca: {}, has_cert: {}",
                        existing_config.use_system_roots,
                        existing_config.root_certs.is_some(),
                        existing_config.client_cert.is_some()
                    );
                    let mut cfg = existing_config.clone();
                    cfg.addr = addr;
                    cfg.hostname = host.to_string();
                    cfg.verify_server_cert = !insecure;
                    cfg
                } else {
                    tracing::debug!("No stored TLS config, using default");
                    TlsConfig::new(addr, host).with_verify_server_cert(!insecure)
                };
                drop(tls_config_lock);

                let mut tls_transport = TlsTransport::new(config);
                tls_transport
                    .connect()
                    .await
                    .map_err(|e| MqttError::ConnectionError(format!("TLS connect failed: {e}")))?;
                Ok(TransportType::Tls(Box::new(tls_transport)))
            }
            ClientTransportType::WebSocket(url) => {
                let config = WebSocketConfig::new(&url).map_err(|e| {
                    MqttError::ConnectionError(format!("Invalid WebSocket URL: {e}"))
                })?;
                let mut ws_transport = WebSocketTransport::new(config);
                ws_transport.connect().await.map_err(|e| {
                    MqttError::ConnectionError(format!("WebSocket connect failed: {e}"))
                })?;
                Ok(TransportType::WebSocket(Box::new(ws_transport)))
            }
            ClientTransportType::WebSocketSecure(url) => {
                let insecure = self.transport_config.read().await.insecure_tls;
                let mut config = WebSocketConfig::new(&url).map_err(|e| {
                    MqttError::ConnectionError(format!("Invalid WebSocket URL: {e}"))
                })?;

                if insecure {
                    let tls_config = TlsConfig::new(addr, host).with_verify_server_cert(false);
                    config = config.with_tls_config(tls_config);
                }

                let mut ws_transport = WebSocketTransport::new(config);
                ws_transport.connect().await.map_err(|e| {
                    MqttError::ConnectionError(format!("WebSocket connect failed: {e}"))
                })?;
                Ok(TransportType::WebSocket(Box::new(ws_transport)))
            }
            ClientTransportType::Quic => {
                let qc = self.transport_config.read().await;
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
                    .with_connect_timeout(qc.connect_timeout);
                if let Some(max) = qc.max_streams {
                    config = config.with_max_concurrent_streams(max);
                }
                drop(qc);
                let mut quic_transport = QuicTransport::new(config);
                quic_transport
                    .connect()
                    .await
                    .map_err(|e| MqttError::ConnectionError(format!("QUIC connect failed: {e}")))?;
                Ok(TransportType::Quic(Box::new(quic_transport)))
            }
            ClientTransportType::QuicSecure => {
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
                    .with_connect_timeout(qc.connect_timeout);

                if let Some(max) = qc.max_streams {
                    config = config.with_max_concurrent_streams(max);
                }

                if let Some(existing_config) = &*tls_config_lock {
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
                drop(tls_config_lock);

                let mut quic_transport = QuicTransport::new(config);
                quic_transport
                    .connect()
                    .await
                    .map_err(|e| MqttError::ConnectionError(format!("QUIC connect failed: {e}")))?;
                Ok(TransportType::Quic(Box::new(quic_transport)))
            }
        }
    }

    pub(crate) async fn try_connect_to_addresses(
        &self,
        addresses: &[std::net::SocketAddr],
        transport_type: ClientTransportType,
        host: &str,
    ) -> Result<ConnectResult> {
        let mut last_error = None;

        for addr in addresses {
            tracing::debug!("Trying to connect to address: {}", addr);

            let transport = match self
                .try_connect_address(*addr, transport_type.clone(), host)
                .await
            {
                Ok(t) => t,
                Err(e) => {
                    tracing::debug!("Failed to connect to {}: {}", addr, e);
                    last_error = Some(e);
                    continue;
                }
            };

            self.reset_reconnect_counter().await;

            let mut inner = self.inner.write().await;
            match inner.connect(transport).await {
                Ok(result) => {
                    let stored_subs = inner.stored_subscriptions.lock().clone();
                    let session_present = result.session_present;
                    drop(inner);

                    self.trigger_connection_event(ConnectionEvent::Connected { session_present })
                        .await;
                    self.recover_quic_flows().await;
                    self.restore_subscriptions_after_connect(stored_subs, session_present)
                        .await;

                    return Ok(result);
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
        let client_id = self.inner.read().await.options.client_id.clone();
        tracing::debug!(
            address = %address,
            client_id = %client_id,
            "🔄 CONNECTION ATTEMPT - Tracking source of connection attempt"
        );

        let (client_transport_type, host, port) = Self::parse_address(address)?;
        let addrs = Self::resolve_addresses(host, port)?;
        let addresses_to_try = Self::select_addresses_for_connection(&addrs, host);

        self.try_connect_to_addresses(addresses_to_try, client_transport_type, host)
            .await
    }

    /// Internal connection method using custom TLS configuration
    ///
    /// # Errors
    ///
    /// Returns an error if TLS connection fails
    pub(crate) async fn connect_internal_with_tls(
        &self,
        tls_config: crate::transport::tls::TlsConfig,
    ) -> Result<ConnectResult> {
        let mut tls_transport = crate::transport::tls::TlsTransport::new(tls_config);
        tls_transport
            .connect()
            .await
            .map_err(|e| MqttError::ConnectionError(format!("TLS connect failed: {e}")))?;

        let transport = TransportType::Tls(Box::new(tls_transport));

        {
            let mut inner = self.inner.write().await;
            inner.reconnect_attempt = 0;
        }

        let mut inner = self.inner.write().await;
        match inner.connect(transport).await {
            Ok(result) => {
                let stored_subs = inner.stored_subscriptions.lock().clone();
                let session_present = result.session_present;
                drop(inner);

                self.trigger_connection_event(ConnectionEvent::Connected { session_present })
                    .await;

                self.recover_quic_flows().await;

                if !stored_subs.is_empty() {
                    if session_present {
                        tracing::info!(
                            "Session resumed, restoring {} callbacks",
                            stored_subs.len()
                        );
                        let inner = self.inner.read().await;
                        for (topic, _, callback_id) in stored_subs {
                            if let Err(e) = inner.callback_manager.restore_callback(callback_id) {
                                tracing::warn!("Failed to restore callback for {}: {}", topic, e);
                            }
                        }
                    } else {
                        tracing::info!(
                            "Session not resumed, restoring {} subscriptions",
                            stored_subs.len()
                        );
                        for (topic, options, callback_id) in stored_subs {
                            if let Err(e) = self
                                .resubscribe_internal(&topic, options, callback_id)
                                .await
                            {
                                tracing::warn!(
                                    "Failed to restore subscription to {}: {}",
                                    topic,
                                    e
                                );
                            }
                        }
                    }
                }

                Ok(result)
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

        {
            let mut inner = self.inner.write().await;
            inner
                .auth_method
                .clone_from(&options.properties.authentication_method);
            inner.options = options.clone();
            inner.last_address = Some(address.to_string());
        }

        let result = self.connect_internal(address).await;

        if let Err(ref error) = result {
            let error_recovery_config =
                crate::client::error_recovery::ErrorRecoveryConfig::default();
            if let Some(_recoverable_error) =
                crate::client::error_recovery::is_recoverable(error, &error_recovery_config)
            {
                if options.reconnect_config.enabled {
                    tracing::debug!(error = %error, "🔄 SPAWN MONITOR - Initial connection failed with recoverable error, starting background reconnection");
                    let client = self.clone();
                    tokio::spawn(async move {
                        client.monitor_connection().await;
                    });
                } else {
                    tracing::debug!(error = %error, "Initial connection failed with recoverable error, but automatic reconnection is disabled");
                }
            } else {
                tracing::debug!(error = %error, "Initial connection failed with non-recoverable error, not starting background reconnection");
            }
        } else if result.is_ok() && options.reconnect_config.enabled {
            tracing::debug!("🔄 SPAWN MONITOR - Successful connection, starting monitor task for future disconnections");
            let client = self.clone();
            tokio::spawn(async move {
                client.monitor_connection().await;
            });
        }

        result
    }

    /// Internal TLS connection method (no mutex guard)
    ///
    /// # Errors
    ///
    /// Returns an error if connection fails or configuration is invalid
    pub(crate) async fn connect_with_tls_and_options_internal(
        &self,
        tls_config: crate::transport::tls::TlsConfig,
        options: ConnectOptions,
    ) -> Result<ConnectResult> {
        if self.is_connected().await {
            return Err(MqttError::AlreadyConnected);
        }

        {
            let mut inner = self.inner.write().await;
            inner.options = options.clone();
            inner.last_address = Some(format!(
                "{}:{}",
                tls_config.hostname,
                tls_config.addr.port()
            ));
        }

        let result = self.connect_internal_with_tls(tls_config).await;

        if let Err(ref error) = result {
            if options.reconnect_config.enabled {
                self.trigger_connection_event(ConnectionEvent::Disconnected {
                    reason: DisconnectReason::NetworkError(error.to_string()),
                })
                .await;

                tracing::warn!(
                    "Automatic reconnection with custom TLS config is not yet supported"
                );
            }
        }

        result
    }
}
