use crate::broker::auth::AuthProvider;
use crate::broker::client_handler::ClientHandler;
use crate::broker::config::BrokerConfig;
use crate::broker::resource_monitor::ResourceMonitor;
use crate::broker::router::MessageRouter;
use crate::broker::storage::DynamicStorage;
use crate::broker::sys_topics::BrokerStats;
use crate::broker::transport::BrokerTransport;
use crate::error::{MqttError, Result};
use crate::packet::{FixedHeader, Packet};
use crate::session::quic_flow::{FlowRegistry, FlowState};
use crate::transport::flow::{
    FlowFlags, FlowHeader, FlowId, FLOW_TYPE_CLIENT_DATA, FLOW_TYPE_CONTROL, FLOW_TYPE_SERVER_DATA,
};
use bytes::{Buf, Bytes, BytesMut};
use quinn::{Connection, Endpoint, RecvStream, SendStream, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::RootCertStore;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{broadcast, mpsc, Mutex};
use tracing::{debug, error, instrument, trace, warn};

use super::tls_acceptor::TlsAcceptorConfig;

pub struct QuicAcceptorConfig {
    pub cert_chain: Vec<CertificateDer<'static>>,
    pub private_key: PrivateKeyDer<'static>,
    pub client_ca_certs: Option<Vec<CertificateDer<'static>>>,
    pub require_client_cert: bool,
    pub alpn_protocols: Vec<Vec<u8>>,
    pub enable_early_data: bool,
    pub max_concurrent_streams: Option<u32>,
    pub stream_receive_window: Option<u32>,
    pub disable_segmentation_offload: bool,
}

impl QuicAcceptorConfig {
    #[must_use]
    pub fn new(
        cert_chain: Vec<CertificateDer<'static>>,
        private_key: PrivateKeyDer<'static>,
    ) -> Self {
        Self {
            cert_chain,
            private_key,
            client_ca_certs: None,
            require_client_cert: false,
            alpn_protocols: vec![b"MQTT-next".to_vec(), b"mqtt".to_vec()],
            enable_early_data: false,
            max_concurrent_streams: None,
            stream_receive_window: None,
            disable_segmentation_offload: false,
        }
    }

    /// Loads a certificate chain from a PEM file.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read or parsed as PEM certificates.
    pub async fn load_cert_chain_from_file(
        path: impl AsRef<std::path::Path>,
    ) -> Result<Vec<CertificateDer<'static>>> {
        TlsAcceptorConfig::load_cert_chain_from_file(path).await
    }

    /// Loads a private key from a PEM file.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read or parsed as a PEM private key.
    pub async fn load_private_key_from_file(
        path: impl AsRef<std::path::Path>,
    ) -> Result<PrivateKeyDer<'static>> {
        TlsAcceptorConfig::load_private_key_from_file(path).await
    }

    #[must_use]
    pub fn with_client_ca_certs(mut self, certs: Vec<CertificateDer<'static>>) -> Self {
        self.client_ca_certs = Some(certs);
        self
    }

    #[must_use]
    pub fn with_require_client_cert(mut self, require: bool) -> Self {
        self.require_client_cert = require;
        self
    }

    #[must_use]
    pub fn with_alpn_protocols(mut self, protocols: Vec<Vec<u8>>) -> Self {
        self.alpn_protocols = protocols;
        self
    }

    #[must_use]
    pub fn with_early_data(mut self, enable: bool) -> Self {
        self.enable_early_data = enable;
        self
    }

    #[must_use]
    pub fn with_max_concurrent_streams(mut self, max: u32) -> Self {
        self.max_concurrent_streams = Some(max);
        self
    }

    #[must_use]
    pub fn with_stream_receive_window(mut self, bytes: u32) -> Self {
        self.stream_receive_window = Some(bytes);
        self
    }

    #[must_use]
    pub fn with_disable_segmentation_offload(mut self, disable: bool) -> Self {
        self.disable_segmentation_offload = disable;
        self
    }

    /// Builds a rustls `ServerConfig` from this QUIC acceptor configuration.
    ///
    /// # Errors
    /// Returns an error if certificate loading fails or TLS configuration is invalid.
    pub fn build_server_config(&self) -> Result<ServerConfig> {
        let crypto_provider = Arc::new(rustls::crypto::ring::default_provider());

        let mut tls_config = if let Some(ref client_ca_certs) = self.client_ca_certs {
            let mut root_store = RootCertStore::empty();
            for cert in client_ca_certs {
                root_store.add(cert.clone()).map_err(|e| {
                    MqttError::Configuration(format!("Failed to add client CA cert: {e}"))
                })?;
            }

            let verifier_builder = WebPkiClientVerifier::builder(Arc::new(root_store));

            let client_verifier = if self.require_client_cert {
                verifier_builder.build()
            } else {
                verifier_builder.allow_unauthenticated().build()
            }
            .map_err(|e| {
                MqttError::Configuration(format!("Failed to build client verifier: {e}"))
            })?;

            rustls::ServerConfig::builder_with_provider(crypto_provider)
                .with_safe_default_protocol_versions()
                .map_err(|e| {
                    MqttError::Configuration(format!("Failed to set protocol versions: {e}"))
                })?
                .with_client_cert_verifier(client_verifier)
                .with_single_cert(self.cert_chain.clone(), self.private_key.clone_key())
                .map_err(|e| {
                    MqttError::Configuration(format!("Failed to configure server cert: {e}"))
                })?
        } else {
            rustls::ServerConfig::builder_with_provider(crypto_provider)
                .with_safe_default_protocol_versions()
                .map_err(|e| {
                    MqttError::Configuration(format!("Failed to set protocol versions: {e}"))
                })?
                .with_no_client_auth()
                .with_single_cert(self.cert_chain.clone(), self.private_key.clone_key())
                .map_err(|e| {
                    MqttError::Configuration(format!("Failed to configure server cert: {e}"))
                })?
        };

        tls_config.alpn_protocols.clone_from(&self.alpn_protocols);

        if self.enable_early_data {
            tls_config.max_early_data_size = u32::MAX;
            tls_config.send_half_rtt_data = true;
        }

        let quic_config =
            quinn::crypto::rustls::QuicServerConfig::try_from(tls_config).map_err(|e| {
                MqttError::Configuration(format!("Failed to create QUIC server config: {e}"))
            })?;

        let mut server_config = ServerConfig::with_crypto(Arc::new(quic_config));

        let mut transport_config = quinn::TransportConfig::default();
        transport_config.max_idle_timeout(Some(quinn::IdleTimeout::from(quinn::VarInt::from_u32(
            60_000,
        ))));
        transport_config.datagram_receive_buffer_size(Some(65536));
        transport_config.datagram_send_buffer_size(65536);

        transport_config
            .stream_receive_window(self.stream_receive_window.unwrap_or(262_144).into());
        transport_config.receive_window(1_048_576u32.into());
        transport_config.send_window(1_048_576);

        if let Some(max) = self.max_concurrent_streams {
            transport_config.max_concurrent_uni_streams(max.into());
            transport_config.max_concurrent_bidi_streams(max.into());
        }

        if self.disable_segmentation_offload {
            transport_config.enable_segmentation_offload(false);
        }

        server_config.transport_config(Arc::new(transport_config));

        Ok(server_config)
    }

    /// Builds a QUIC endpoint bound to the specified address.
    ///
    /// # Errors
    /// Returns an error if the server config is invalid or the address cannot be bound.
    pub fn build_endpoint(&self, bind_addr: SocketAddr) -> Result<Endpoint> {
        let server_config = self.build_server_config()?;
        Endpoint::server(server_config, bind_addr)
            .map_err(|e| MqttError::ConnectionError(format!("Failed to bind QUIC endpoint: {e}")))
    }
}

pub struct QuicStreamWrapper {
    send: SendStream,
    recv: RecvStream,
    peer_addr: SocketAddr,
}

impl QuicStreamWrapper {
    #[must_use]
    pub fn new(send: SendStream, recv: RecvStream, peer_addr: SocketAddr) -> Self {
        Self {
            send,
            recv,
            peer_addr,
        }
    }

    #[must_use]
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    #[must_use]
    pub fn split(self) -> (SendStream, RecvStream) {
        (self.send, self.recv)
    }
}

impl AsyncRead for QuicStreamWrapper {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.recv).poll_read(cx, buf)
    }
}

impl AsyncWrite for QuicStreamWrapper {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match Pin::new(&mut self.send).poll_write(cx, buf) {
            Poll::Ready(Ok(n)) => Poll::Ready(Ok(n)),
            Poll::Ready(Err(e)) => Poll::Ready(Err(std::io::Error::other(e))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.send).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.send).poll_shutdown(cx)
    }
}

/// Accepts an incoming QUIC connection from the endpoint.
///
/// # Errors
/// Returns an error if the endpoint is closed or the handshake fails.
#[instrument(skip(endpoint))]
pub async fn accept_quic_connection(
    endpoint: &Endpoint,
) -> Result<(quinn::Connection, SocketAddr)> {
    let incoming = endpoint.accept().await.ok_or(MqttError::ConnectionError(
        "QUIC endpoint closed".to_string(),
    ))?;

    let peer_addr = incoming.remote_address();
    debug!("Incoming QUIC connection from {}", peer_addr);

    let connection = incoming.await.map_err(|e| {
        error!("QUIC connection failed from {}: {}", peer_addr, e);
        MqttError::ConnectionError(format!("QUIC handshake failed: {e}"))
    })?;

    debug!(
        "QUIC connection established with {} (RTT: {:?})",
        peer_addr,
        connection.rtt()
    );

    Ok((connection, peer_addr))
}

/// Accepts a bidirectional QUIC stream from an established connection.
///
/// # Errors
/// Returns an error if stream acceptance fails or the connection is closed.
#[instrument(skip(connection), fields(peer_addr = %peer_addr))]
pub async fn accept_quic_stream(
    connection: &quinn::Connection,
    peer_addr: SocketAddr,
) -> Result<QuicStreamWrapper> {
    let (send, recv) = connection.accept_bi().await.map_err(|e| {
        let reason = crate::transport::quic_error::parse_connection_error(&e);
        error!("Failed to accept bidirectional stream from {peer_addr}: {reason}");
        MqttError::ConnectionError(format!("Failed to accept QUIC stream: {reason}"))
    })?;

    debug!("QUIC bidirectional stream accepted from {}", peer_addr);

    Ok(QuicStreamWrapper::new(send, recv, peer_addr))
}

fn is_flow_header_byte(b: u8) -> bool {
    matches!(
        b,
        FLOW_TYPE_CONTROL | FLOW_TYPE_CLIENT_DATA | FLOW_TYPE_SERVER_DATA
    )
}

struct FlowHeaderResult {
    flow_id: Option<FlowId>,
    flags: Option<FlowFlags>,
    expire: Option<Duration>,
    leftover: BytesMut,
}

#[instrument(skip(recv), level = "debug")]
async fn try_read_flow_header(recv: &mut RecvStream) -> Result<FlowHeaderResult> {
    let chunk = recv
        .read_chunk(1, true)
        .await
        .map_err(|e| MqttError::ConnectionError(format!("Failed to read stream: {e}")))?;

    let Some(chunk) = chunk else {
        return Ok(FlowHeaderResult {
            flow_id: None,
            flags: None,
            expire: None,
            leftover: BytesMut::new(),
        });
    };

    if chunk.bytes.is_empty() {
        return Ok(FlowHeaderResult {
            flow_id: None,
            flags: None,
            expire: None,
            leftover: BytesMut::new(),
        });
    }

    let first_byte = chunk.bytes[0];

    if !is_flow_header_byte(first_byte) {
        let mut leftover = BytesMut::with_capacity(chunk.bytes.len());
        leftover.extend_from_slice(&chunk.bytes);
        return Ok(FlowHeaderResult {
            flow_id: None,
            flags: None,
            expire: None,
            leftover,
        });
    }

    let mut header_buf = Vec::with_capacity(32);
    header_buf.extend_from_slice(&chunk.bytes);

    let (flow_header, leftover) = loop {
        let mut bytes = Bytes::copy_from_slice(&header_buf);
        match FlowHeader::decode(&mut bytes) {
            Ok(header) => {
                let consumed = header_buf.len() - bytes.remaining();
                let mut lo = BytesMut::with_capacity(bytes.remaining());
                if consumed < header_buf.len() {
                    lo.extend_from_slice(&header_buf[consumed..]);
                }
                break (header, lo);
            }
            Err(_) if header_buf.len() >= 32 => {
                return Err(MqttError::ProtocolError("flow header too large".into()));
            }
            Err(_) => match recv.read_chunk(32 - header_buf.len(), true).await {
                Ok(Some(c)) if !c.bytes.is_empty() => {
                    header_buf.extend_from_slice(&c.bytes);
                }
                Ok(_) => {
                    return Err(MqttError::ProtocolError("incomplete flow header".into()));
                }
                Err(e) => {
                    return Err(MqttError::ConnectionError(format!(
                        "Failed to read flow header: {e}"
                    )));
                }
            },
        }
    };

    Ok(build_flow_header_result(flow_header, leftover))
}

fn build_flow_header_result(flow_header: FlowHeader, leftover: BytesMut) -> FlowHeaderResult {
    match flow_header {
        FlowHeader::Control(h) => {
            trace!(flow_id = ?h.flow_id, "Parsed control flow header");
            FlowHeaderResult {
                flow_id: Some(h.flow_id),
                flags: Some(h.flags),
                expire: None,
                leftover,
            }
        }
        FlowHeader::ClientData(h) | FlowHeader::ServerData(h) => {
            let expire = if h.expire_interval > 0 {
                Some(Duration::from_secs(h.expire_interval))
            } else {
                None
            };
            trace!(flow_id = ?h.flow_id, expire = ?expire, "Parsed data flow header");
            FlowHeaderResult {
                flow_id: Some(h.flow_id),
                flags: Some(h.flags),
                expire,
                leftover,
            }
        }
        FlowHeader::UserDefined(_) => {
            trace!("Ignoring user-defined flow header");
            FlowHeaderResult {
                flow_id: None,
                flags: None,
                expire: None,
                leftover,
            }
        }
    }
}

pub(super) async fn read_packet_with_buffer(
    recv: &mut RecvStream,
    buffer: &mut BytesMut,
) -> Result<Packet> {
    while buffer.len() < 2 {
        let mut tmp = [0u8; 64];
        let n = recv
            .read(&mut tmp)
            .await
            .map_err(|e| MqttError::ConnectionError(format!("QUIC read error: {e}")))?
            .ok_or(MqttError::ClientClosed)?;
        if n == 0 {
            return Err(MqttError::ClientClosed);
        }
        buffer.extend_from_slice(&tmp[..n]);
    }

    let mut remaining_length = 0u32;
    let mut multiplier = 1u32;
    let mut remaining_length_bytes = 1usize;

    for i in 1..5 {
        if i >= buffer.len() {
            let mut tmp = [0u8; 64];
            let n = recv
                .read(&mut tmp)
                .await
                .map_err(|e| MqttError::ConnectionError(format!("QUIC read error: {e}")))?
                .ok_or(MqttError::ClientClosed)?;
            if n == 0 {
                return Err(MqttError::ClientClosed);
            }
            buffer.extend_from_slice(&tmp[..n]);
        }

        let byte = buffer[i];
        remaining_length += u32::from(byte & 0x7F) * multiplier;
        multiplier *= 128;
        remaining_length_bytes = i;

        if (byte & 0x80) == 0 {
            break;
        }

        if i == 4 {
            return Err(MqttError::MalformedPacket(
                "Invalid remaining length encoding".to_string(),
            ));
        }
    }

    let header_len = 1 + remaining_length_bytes;
    let total_len = header_len + remaining_length as usize;

    while buffer.len() < total_len {
        let mut tmp = [0u8; 1024];
        let n = recv
            .read(&mut tmp)
            .await
            .map_err(|e| MqttError::ConnectionError(format!("QUIC read error: {e}")))?
            .ok_or(MqttError::ClientClosed)?;
        if n == 0 {
            return Err(MqttError::ClientClosed);
        }
        buffer.extend_from_slice(&tmp[..n]);
    }

    let packet_bytes = buffer.split_to(total_len);
    let mut header_buf = packet_bytes.clone().freeze();
    let fixed_header = FixedHeader::decode(&mut header_buf)?;

    let mut payload_buf = BytesMut::from(&packet_bytes[header_len..]);
    Packet::decode_from_body(fixed_header.packet_type, &fixed_header, &mut payload_buf)
}

pub struct QuicHandlerContext {
    pub config: Arc<BrokerConfig>,
    pub router: Arc<MessageRouter>,
    pub auth_provider: Arc<dyn AuthProvider>,
    pub storage: Option<Arc<DynamicStorage>>,
    pub stats: Arc<BrokerStats>,
    pub resource_monitor: Arc<ResourceMonitor>,
    pub shutdown_rx: broadcast::Receiver<()>,
    pub connection_alive: mpsc::Sender<()>,
}

#[instrument(skip(connection, context), fields(peer_addr = %peer_addr))]
pub async fn run_quic_connection_handler(
    connection: Arc<Connection>,
    peer_addr: SocketAddr,
    context: QuicHandlerContext,
) {
    run_quic_handler_inner(connection, peer_addr, context, false, "QUIC").await;
}

pub async fn run_quic_cluster_connection_handler(
    connection: Arc<Connection>,
    peer_addr: SocketAddr,
    context: QuicHandlerContext,
) {
    run_quic_handler_inner(connection, peer_addr, context, true, "Cluster QUIC").await;
}

async fn run_quic_handler_inner(
    connection: Arc<Connection>,
    peer_addr: SocketAddr,
    context: QuicHandlerContext,
    skip_bridge_forwarding: bool,
    label: &'static str,
) {
    let (packet_tx, packet_rx) = mpsc::channel::<(Packet, Option<u64>)>(100);
    let (flow_closed_tx, flow_closed_rx) = mpsc::channel::<u64>(32);
    let flow_registry = Arc::new(Mutex::new(FlowRegistry::new(256)));

    let (send, recv) = match connection.accept_bi().await {
        Ok(streams) => streams,
        Err(e) => {
            let reason = crate::transport::quic_error::parse_connection_error(&e);
            error!("Failed to accept control stream from {peer_addr}: {reason}");
            return;
        }
    };

    debug!(
        "{} control stream accepted from {}, starting handler",
        label, peer_addr
    );

    let stream = QuicStreamWrapper::new(send, recv, peer_addr);
    let transport = BrokerTransport::quic(stream);

    let delivery_strategy = context.config.server_delivery_strategy;
    let connection_alive = context.connection_alive;
    let handler = ClientHandler::new_with_external_packets(
        transport,
        peer_addr,
        context.config,
        context.router,
        context.auth_provider,
        context.storage,
        context.stats,
        context.resource_monitor,
        context.shutdown_rx,
        Some(packet_rx),
    )
    .with_quic_connection(connection.clone())
    .with_server_delivery_strategy(delivery_strategy)
    .with_quic_packet_tx(packet_tx.clone())
    .with_skip_bridge_forwarding(skip_bridge_forwarding)
    .with_flow_closed_rx(flow_closed_rx)
    .with_flow_registry(flow_registry.clone());

    let handler_label = label;
    let handler_connection = connection.clone();
    tokio::spawn(async move {
        let outcome = handler.run().await;
        drop(connection_alive);
        if let Err(e) = &outcome {
            if e.is_normal_disconnect() {
                debug!("{} client handler finished", handler_label);
            } else {
                warn!("{} client handler error: {e}", handler_label);
            }
        }
        close_after_control_flow(&handler_connection, peer_addr, &outcome).await;
    });

    spawn_datagram_reader(connection.clone(), packet_tx.clone(), peer_addr, label);
    spawn_bi_accept_loop(connection.clone(), flow_registry.clone(), peer_addr, label);
    spawn_quic_stats_sampler(connection.clone(), peer_addr);
    spawn_uni_accept_loop(
        connection,
        packet_tx,
        peer_addr,
        flow_registry,
        flow_closed_tx,
        label,
    );
}

const QUIC_STATS_DIR_ENV: &str = "MQTT5_QUIC_STATS_DIR";
const QUIC_STATS_HEADER: &str = "timestamp_ns,rtt_us,cwnd,lost_packets,congestion_events,sent_packets,stream_data_blocked,data_blocked,streams_blocked_uni\n";
const QUIC_STATS_INTERVAL: Duration = Duration::from_millis(100);

fn spawn_quic_stats_sampler(connection: Arc<Connection>, peer_addr: SocketAddr) {
    let Some(dir) = std::env::var_os(QUIC_STATS_DIR_ENV).filter(|dir| !dir.is_empty()) else {
        return;
    };
    tokio::spawn(async move {
        match sample_quic_stats(&connection, std::path::Path::new(&dir), peer_addr).await {
            Ok(rows) => debug!(%peer_addr, rows, "QUIC stats sampling finished"),
            Err(e) => warn!(%peer_addr, error = %e, "QUIC stats sampling stopped"),
        }
    });
}

async fn sample_quic_stats(
    connection: &Connection,
    dir: &std::path::Path,
    peer_addr: SocketAddr,
) -> std::io::Result<u64> {
    use tokio::io::AsyncWriteExt;

    tokio::fs::create_dir_all(dir).await?;
    let name = peer_addr.to_string().replace([':', '.', '[', ']'], "-");
    let mut out = tokio::fs::File::create(dir.join(format!("broker_quic_{name}.csv"))).await?;
    out.write_all(QUIC_STATS_HEADER.as_bytes()).await?;

    let mut rows = 0u64;
    let mut ticker = tokio::time::interval(QUIC_STATS_INTERVAL);
    loop {
        let closed = tokio::select! {
            _ = connection.closed() => true,
            _ = ticker.tick() => false,
        };
        out.write_all(quic_stats_row(connection).as_bytes()).await?;
        rows += 1;
        if closed {
            return Ok(rows);
        }
    }
}

fn quic_stats_row(connection: &Connection) -> String {
    let stats = connection.stats();
    let timestamp_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
        });
    let rtt_us = u64::try_from(stats.path.rtt.as_micros()).unwrap_or(u64::MAX);
    format!(
        "{timestamp_ns},{rtt_us},{},{},{},{},{},{},{}\n",
        stats.path.cwnd,
        stats.path.lost_packets,
        stats.path.congestion_events,
        stats.path.sent_packets,
        stats.frame_rx.stream_data_blocked,
        stats.frame_rx.data_blocked,
        stats.frame_rx.streams_blocked_uni,
    )
}

fn spawn_datagram_reader(
    connection: Arc<Connection>,
    packet_tx: mpsc::Sender<(Packet, Option<u64>)>,
    peer_addr: SocketAddr,
    label: &'static str,
) {
    tokio::spawn(async move {
        loop {
            match connection.read_datagram().await {
                Ok(datagram) => {
                    trace!(
                        len = datagram.len(),
                        "Received {} datagram from {}",
                        label,
                        peer_addr
                    );
                    let packet = match decode_datagram_packet(&datagram) {
                        Some(Ok(packet)) => packet,
                        Some(Err(e)) => {
                            close_on_malformed_packet(&connection, peer_addr, "datagram", &e);
                            break;
                        }
                        None => continue,
                    };
                    if packet_tx.send((packet, None)).await.is_err() {
                        debug!("Datagram packet channel closed for {}", peer_addr);
                        break;
                    }
                }
                Err(e) => {
                    debug!("Datagram read ended for {}: {}", peer_addr, e);
                    break;
                }
            }
        }
    });
}

fn spawn_bi_accept_loop(
    connection: Arc<Connection>,
    flow_registry: Arc<Mutex<FlowRegistry>>,
    peer_addr: SocketAddr,
    label: &'static str,
) {
    tokio::spawn(async move {
        loop {
            match connection.accept_bi().await {
                Ok((send, recv)) => {
                    debug!("{} bidirectional stream accepted from {}", label, peer_addr);
                    spawn_discard_handler(
                        send,
                        recv,
                        peer_addr,
                        flow_registry.clone(),
                        connection.clone(),
                    );
                }
                Err(e) => {
                    let reason = crate::transport::quic_error::parse_connection_error(&e);
                    debug!("{label} bi stream accept loop ended for {peer_addr}: {reason}");
                    break;
                }
            }
        }
    });
}

fn spawn_uni_accept_loop(
    connection: Arc<Connection>,
    packet_tx: mpsc::Sender<(Packet, Option<u64>)>,
    peer_addr: SocketAddr,
    flow_registry: Arc<Mutex<FlowRegistry>>,
    flow_closed_tx: mpsc::Sender<u64>,
    label: &'static str,
) {
    tokio::spawn(async move {
        loop {
            match connection.accept_uni().await {
                Ok(recv) => {
                    debug!(
                        "Additional {} data stream accepted from {}",
                        label, peer_addr
                    );
                    spawn_data_stream_reader(
                        connection.clone(),
                        recv,
                        packet_tx.clone(),
                        peer_addr,
                        flow_registry.clone(),
                        flow_closed_tx.clone(),
                    );
                }
                Err(e) => {
                    let reason = crate::transport::quic_error::parse_connection_error(&e);
                    debug!("{label} uni stream accept loop ended for {peer_addr}: {reason}");
                    break;
                }
            }
        }
    });
}

fn decode_datagram_packet(data: &Bytes) -> Option<Result<Packet>> {
    if data.is_empty() || data[0] == 0x00 {
        return None;
    }

    let mut buf = BytesMut::from(data.as_ref());
    let fixed_header = match FixedHeader::decode(&mut buf) {
        Ok(h) => h,
        Err(e) => return Some(Err(e)),
    };
    Some(Packet::decode_from_body(
        fixed_header.packet_type,
        &fixed_header,
        &mut buf,
    ))
}

fn handle_stream_error(
    send: &mut SendStream,
    connection: &Connection,
    peer_addr: SocketAddr,
    code: mqtt5_protocol::QuicStreamCode,
    err_tolerance: u8,
) {
    let error_level = code.error_level().unwrap_or(0);

    if error_level == 0 || err_tolerance == 0 {
        warn!(
            ?code,
            error_level, err_tolerance, "Level 0 error from {peer_addr}, closing connection"
        );
        connection.close(
            quinn::VarInt::from_u32(mqtt5_protocol::QuicConnectionCode::ProtocolLevel0.code()),
            code.to_string().as_bytes(),
        );
        return;
    }

    if error_level <= 1 || err_tolerance >= error_level {
        warn!(
            ?code,
            error_level, err_tolerance, "Stream error from {peer_addr}, resetting stream"
        );
        if let Err(e) = send.reset(quinn::VarInt::from_u32(code.code())) {
            debug!("stream from {peer_addr} already closed before reset: {e}");
        }
        return;
    }

    warn!(
        ?code,
        error_level, err_tolerance, "Error exceeds tolerance for {peer_addr}, closing connection"
    );
    connection.close(
        quinn::VarInt::from_u32(mqtt5_protocol::QuicConnectionCode::ProtocolLevel0.code()),
        code.to_string().as_bytes(),
    );
}

fn spawn_discard_handler(
    mut send: SendStream,
    mut recv: RecvStream,
    peer_addr: SocketAddr,
    flow_registry: Arc<Mutex<FlowRegistry>>,
    connection: Arc<Connection>,
) {
    tokio::spawn(async move {
        let result = match try_read_flow_header(&mut recv).await {
            Ok(result) => result,
            Err(e) => {
                warn!("failed to read flow header on bi stream from {peer_addr}: {e}");
                return;
            }
        };

        let (Some(flow_id), Some(flags)) = (result.flow_id, result.flags) else {
            handle_stream_error(
                &mut send,
                &connection,
                peer_addr,
                mqtt5_protocol::QuicStreamCode::NoFlowState,
                0,
            );
            return;
        };

        if !flags.is_discard_signal() {
            handle_stream_error(
                &mut send,
                &connection,
                peer_addr,
                mqtt5_protocol::QuicStreamCode::NotFlowOwner,
                flags.err_tolerance,
            );
            return;
        }

        {
            let mut registry = flow_registry.lock().await;
            if let Some(state) = registry.remove(flow_id) {
                debug!(
                    flow_id = ?flow_id,
                    "discarded flow state for {peer_addr}: {state:?}"
                );
            } else {
                debug!(
                    flow_id = ?flow_id,
                    "no flow state to discard for {peer_addr} (already expired or unknown)"
                );
            }
        }

        if let Err(e) = send.finish() {
            debug!(flow_id = ?flow_id, "discard stream from {peer_addr} already closed: {e}");
        }

        debug!(flow_id = ?flow_id, "completed discard handshake for {peer_addr}");
    });
}

const CONTROL_FLOW_CLOSE_GRACE: Duration = Duration::from_secs(1);

fn close_code(outcome: &Result<()>) -> mqtt5_protocol::QuicConnectionCode {
    match outcome {
        Ok(()) => mqtt5_protocol::QuicConnectionCode::NoError,
        Err(e) if e.is_normal_disconnect() => mqtt5_protocol::QuicConnectionCode::NoError,
        Err(
            MqttError::MalformedPacket(_)
            | MqttError::ProtocolError(_)
            | MqttError::InvalidQoS(_)
            | MqttError::InvalidPacketType(_)
            | MqttError::InvalidPropertyId(_)
            | MqttError::DuplicatePropertyId(_)
            | MqttError::InvalidReasonCode(_)
            | MqttError::StringTooLong(_)
            | MqttError::InvalidTopicName(_)
            | MqttError::PacketTooLarge { .. },
        ) => mqtt5_protocol::QuicConnectionCode::ProtocolLevel0,
        Err(_) => mqtt5_protocol::QuicConnectionCode::Unspecified,
    }
}

async fn close_after_control_flow(
    connection: &Connection,
    peer_addr: SocketAddr,
    outcome: &Result<()>,
) {
    if tokio::time::timeout(CONTROL_FLOW_CLOSE_GRACE, connection.closed())
        .await
        .is_ok()
    {
        return;
    }
    let code = close_code(outcome);
    debug!(%code, "closing QUIC connection from {peer_addr} after its control flow ended");
    close_connection(connection, code);
}

fn close_on_malformed_packet(
    connection: &Connection,
    peer_addr: SocketAddr,
    source: &str,
    error: &MqttError,
) {
    warn!("malformed packet on {source} from {peer_addr}, closing connection: {error}");
    close_connection(
        connection,
        mqtt5_protocol::QuicConnectionCode::ProtocolLevel0,
    );
}

fn close_connection(connection: &Connection, code: mqtt5_protocol::QuicConnectionCode) {
    connection.close(
        quinn::VarInt::from_u32(code.code()),
        code.to_string().as_bytes(),
    );
}

fn spawn_data_stream_reader(
    connection: Arc<Connection>,
    mut recv: RecvStream,
    packet_tx: mpsc::Sender<(Packet, Option<u64>)>,
    peer_addr: SocketAddr,
    flow_registry: Arc<Mutex<FlowRegistry>>,
    flow_closed_tx: mpsc::Sender<u64>,
) {
    tokio::spawn(async move {
        let (flow_id, mut buffer) = match try_read_flow_header(&mut recv).await {
            Ok(result) => {
                let flow_id = if let (Some(id), Some(flags)) = (result.flow_id, result.flags) {
                    let state = FlowState::new_client_data(id, flags, result.expire);
                    let mut registry = flow_registry.lock().await;
                    if registry.register_flow(state) {
                        debug!(flow_id = ?id, "Registered flow from {}", peer_addr);
                    } else {
                        warn!(flow_id = ?id, "Failed to register flow (registry full)");
                    }
                    Some(id)
                } else {
                    trace!("No flow header on data stream from {}", peer_addr);
                    None
                };
                (flow_id, result.leftover)
            }
            Err(e) => {
                warn!("Error parsing flow header from {}: {}", peer_addr, e);
                return;
            }
        };

        loop {
            match read_packet_with_buffer(&mut recv, &mut buffer).await {
                Ok(packet) => {
                    if let Some(id) = flow_id {
                        let mut registry = flow_registry.lock().await;
                        registry.touch(id);
                    }
                    let raw_flow_id = flow_id.map(|f| f.raw());
                    debug!(flow_id = ?flow_id, packet_type = %packet.packet_type_name(), "Read packet from QUIC data stream");
                    if packet_tx.send((packet, raw_flow_id)).await.is_err() {
                        debug!("Packet channel closed, stopping data stream reader");
                        break;
                    }
                }
                Err(MqttError::ClientClosed) => {
                    debug!(flow_id = ?flow_id, "QUIC data stream closed from {}", peer_addr);
                    break;
                }
                Err(MqttError::ConnectionError(reason)) => {
                    debug!(flow_id = ?flow_id, "QUIC data stream from {peer_addr} ended: {reason}");
                    break;
                }
                Err(e) => {
                    close_on_malformed_packet(&connection, peer_addr, "data stream", &e);
                    break;
                }
            }
        }

        if let Some(id) = flow_id {
            let mut registry = flow_registry.lock().await;
            let should_keep = registry
                .get(id)
                .is_some_and(|state| state.flags.err_tolerance >= 2);
            if should_keep {
                debug!(flow_id = ?id, "Preserving flow state (err_tolerance >= 2) for recovery");
            } else if registry.remove(id).is_some() {
                debug!(flow_id = ?id, "Removed flow from registry");
                if flow_closed_tx.send(id.raw()).await.is_err() {
                    debug!(flow_id = ?id, "client handler ended before the flow closed");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quic_acceptor_config() {
        let cert = CertificateDer::from(vec![0x30, 0x82, 0x01, 0x00]);
        let key = PrivateKeyDer::from(rustls::pki_types::PrivatePkcs8KeyDer::from(vec![
            0x30, 0x48, 0x02, 0x01,
        ]));

        let config = QuicAcceptorConfig::new(vec![cert.clone()], key.clone_key())
            .with_require_client_cert(true);

        assert!(config.require_client_cert);
        assert_eq!(config.alpn_protocols.len(), 2);
        assert_eq!(config.alpn_protocols[0], b"MQTT-next");
        assert_eq!(config.alpn_protocols[1], b"mqtt");
        assert_eq!(config.cert_chain.len(), 1);
    }

    #[test]
    fn test_quic_acceptor_custom_alpn() {
        let cert = CertificateDer::from(vec![0x30, 0x82, 0x01, 0x00]);
        let key = PrivateKeyDer::from(rustls::pki_types::PrivatePkcs8KeyDer::from(vec![
            0x30, 0x48, 0x02, 0x01,
        ]));

        let config = QuicAcceptorConfig::new(vec![cert.clone()], key.clone_key())
            .with_alpn_protocols(vec![b"mqtt".to_vec()]);

        assert_eq!(config.alpn_protocols.len(), 1);
        assert_eq!(config.alpn_protocols[0], b"mqtt");
    }
}
