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

// [RFC9000§7] QUIC transport parameters
pub struct QuicAcceptorConfig {
    pub cert_chain: Vec<CertificateDer<'static>>,
    pub private_key: PrivateKeyDer<'static>,
    pub client_ca_certs: Option<Vec<CertificateDer<'static>>>,
    pub require_client_cert: bool,
    pub alpn_protocols: Vec<Vec<u8>>,
}

impl QuicAcceptorConfig {
    #[allow(clippy::must_use_candidate)]
    pub fn new(
        cert_chain: Vec<CertificateDer<'static>>,
        private_key: PrivateKeyDer<'static>,
    ) -> Self {
        Self {
            cert_chain,
            private_key,
            client_ca_certs: None,
            require_client_cert: false,
            alpn_protocols: vec![b"mqtt".to_vec()],
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

    /// Builds a rustls `ServerConfig` from this QUIC acceptor configuration.
    ///
    /// # Errors
    /// Returns an error if certificate loading fails or TLS configuration is invalid.
    ///
    /// # Panics
    /// Panics if the idle timeout duration conversion fails (should never happen).
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

        let quic_config =
            quinn::crypto::rustls::QuicServerConfig::try_from(tls_config).map_err(|e| {
                MqttError::Configuration(format!("Failed to create QUIC server config: {e}"))
            })?;

        let mut server_config = ServerConfig::with_crypto(Arc::new(quic_config));

        let mut transport_config = quinn::TransportConfig::default();
        transport_config.max_idle_timeout(Some(
            std::time::Duration::from_secs(60)
                .try_into()
                .expect("valid duration"),
        ));
        transport_config.datagram_receive_buffer_size(Some(65536));
        transport_config.datagram_send_buffer_size(65536);
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
    #[allow(clippy::must_use_candidate)]
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
        error!(
            "Failed to accept bidirectional stream from {}: {}",
            peer_addr, e
        );
        MqttError::ConnectionError(format!("Failed to accept QUIC stream: {e}"))
    })?;

    debug!("QUIC bidirectional stream accepted from {}", peer_addr);

    Ok(QuicStreamWrapper::new(send, recv, peer_addr))
}

// [MQoQ§4.1] Flow type detection
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

    while header_buf.len() < 32 {
        match recv.read_chunk(32 - header_buf.len(), true).await {
            Ok(Some(chunk)) if !chunk.bytes.is_empty() => {
                header_buf.extend_from_slice(&chunk.bytes);
            }
            Ok(_) => break,
            Err(e) => {
                return Err(MqttError::ConnectionError(format!(
                    "Failed to read flow header: {e}"
                )));
            }
        }
    }

    let mut bytes = Bytes::from(header_buf);
    let flow_header = FlowHeader::decode(&mut bytes)?;

    let mut leftover = BytesMut::with_capacity(bytes.remaining());
    if bytes.has_remaining() {
        leftover.extend_from_slice(&bytes);
    }

    match flow_header {
        FlowHeader::Control(h) => {
            trace!(flow_id = ?h.flow_id, "Parsed control flow header");
            Ok(FlowHeaderResult {
                flow_id: Some(h.flow_id),
                flags: Some(h.flags),
                expire: None,
                leftover,
            })
        }
        FlowHeader::ClientData(h) | FlowHeader::ServerData(h) => {
            let expire = if h.expire_interval > 0 {
                Some(Duration::from_secs(h.expire_interval))
            } else {
                None
            };
            trace!(flow_id = ?h.flow_id, expire = ?expire, "Parsed data flow header");
            Ok(FlowHeaderResult {
                flow_id: Some(h.flow_id),
                flags: Some(h.flags),
                expire,
                leftover,
            })
        }
        FlowHeader::UserDefined(_) => {
            trace!("Ignoring user-defined flow header");
            Ok(FlowHeaderResult {
                flow_id: None,
                flags: None,
                expire: None,
                leftover,
            })
        }
    }
}

async fn read_packet_with_buffer(recv: &mut RecvStream, buffer: &mut BytesMut) -> Result<Packet> {
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

// [MQoQ§4] QUIC connection handling with flow headers
#[allow(clippy::too_many_arguments)]
#[instrument(skip(connection, config, router, auth_provider, storage, stats, resource_monitor, shutdown_rx), fields(peer_addr = %peer_addr))]
pub async fn run_quic_connection_handler(
    connection: Arc<Connection>,
    peer_addr: SocketAddr,
    config: Arc<BrokerConfig>,
    router: Arc<MessageRouter>,
    auth_provider: Arc<dyn AuthProvider>,
    storage: Option<Arc<DynamicStorage>>,
    stats: Arc<BrokerStats>,
    resource_monitor: Arc<ResourceMonitor>,
    shutdown_rx: broadcast::Receiver<()>,
) {
    run_quic_handler_inner(
        connection,
        peer_addr,
        config,
        router,
        auth_provider,
        storage,
        stats,
        resource_monitor,
        shutdown_rx,
        false,
        "QUIC",
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
pub async fn run_quic_cluster_connection_handler(
    connection: Arc<Connection>,
    peer_addr: SocketAddr,
    config: Arc<BrokerConfig>,
    router: Arc<MessageRouter>,
    auth_provider: Arc<dyn AuthProvider>,
    storage: Option<Arc<DynamicStorage>>,
    stats: Arc<BrokerStats>,
    resource_monitor: Arc<ResourceMonitor>,
    shutdown_rx: broadcast::Receiver<()>,
) {
    run_quic_handler_inner(
        connection,
        peer_addr,
        config,
        router,
        auth_provider,
        storage,
        stats,
        resource_monitor,
        shutdown_rx,
        true,
        "Cluster QUIC",
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn run_quic_handler_inner(
    connection: Arc<Connection>,
    peer_addr: SocketAddr,
    config: Arc<BrokerConfig>,
    router: Arc<MessageRouter>,
    auth_provider: Arc<dyn AuthProvider>,
    storage: Option<Arc<DynamicStorage>>,
    stats: Arc<BrokerStats>,
    resource_monitor: Arc<ResourceMonitor>,
    shutdown_rx: broadcast::Receiver<()>,
    skip_bridge_forwarding: bool,
    label: &'static str,
) {
    let (packet_tx, packet_rx) = mpsc::channel::<Packet>(100);
    let flow_registry = Arc::new(Mutex::new(FlowRegistry::new(256)));

    let (send, recv) = match connection.accept_bi().await {
        Ok(streams) => streams,
        Err(e) => {
            error!("Failed to accept control stream from {}: {}", peer_addr, e);
            return;
        }
    };

    debug!(
        "{} control stream accepted from {}, starting handler",
        label, peer_addr
    );

    let stream = QuicStreamWrapper::new(send, recv, peer_addr);
    let transport = BrokerTransport::quic(stream);

    let handler = ClientHandler::new_with_external_packets(
        transport,
        peer_addr,
        config,
        router,
        auth_provider,
        storage,
        stats,
        resource_monitor,
        shutdown_rx,
        Some(packet_rx),
    )
    .with_skip_bridge_forwarding(skip_bridge_forwarding);

    let handler_label = label;
    tokio::spawn(async move {
        if let Err(e) = handler.run().await {
            if e.is_normal_disconnect() {
                debug!("{} client handler finished", handler_label);
            } else {
                warn!("{} client handler error: {e}", handler_label);
            }
        }
    });

    let datagram_connection = connection.clone();
    let datagram_packet_tx = packet_tx.clone();
    let datagram_label = label;
    tokio::spawn(async move {
        loop {
            match datagram_connection.read_datagram().await {
                Ok(datagram) => {
                    trace!(
                        len = datagram.len(),
                        "Received {} datagram from {}",
                        datagram_label,
                        peer_addr
                    );
                    match decode_datagram_packet(&datagram) {
                        Ok(packet) => {
                            if datagram_packet_tx.send(packet).await.is_err() {
                                debug!("Datagram packet channel closed for {}", peer_addr);
                                break;
                            }
                        }
                        Err(e) => {
                            warn!("Failed to decode datagram from {}: {}", peer_addr, e);
                        }
                    }
                }
                Err(e) => {
                    debug!("Datagram read ended for {}: {}", peer_addr, e);
                    break;
                }
            }
        }
    });

    let stream_label = label;
    tokio::spawn(async move {
        loop {
            if let Ok((_send, recv)) = connection.accept_bi().await {
                debug!(
                    "Additional {} data stream accepted from {}",
                    stream_label, peer_addr
                );
                spawn_data_stream_reader(recv, packet_tx.clone(), peer_addr, flow_registry.clone());
            } else {
                debug!(
                    "{} connection stream accept loop ended for {}",
                    stream_label, peer_addr
                );
                break;
            }
        }
    });
}

fn decode_datagram_packet(data: &Bytes) -> Result<Packet> {
    if data.is_empty() {
        return Err(MqttError::MalformedPacket(
            "Empty datagram received".to_string(),
        ));
    }

    let mut buf = BytesMut::from(data.as_ref());
    let fixed_header = FixedHeader::decode(&mut buf)?;
    Packet::decode_from_body(fixed_header.packet_type, &fixed_header, &mut buf)
}

// [MQoQ§5] Data stream processing
fn spawn_data_stream_reader(
    mut recv: RecvStream,
    packet_tx: mpsc::Sender<Packet>,
    peer_addr: SocketAddr,
    flow_registry: Arc<Mutex<FlowRegistry>>,
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
                    debug!(flow_id = ?flow_id, packet_type = %packet.packet_type_name(), "Read packet from QUIC data stream");
                    if packet_tx.send(packet).await.is_err() {
                        debug!("Packet channel closed, stopping data stream reader");
                        break;
                    }
                }
                Err(e) => {
                    if matches!(e, MqttError::ClientClosed) {
                        debug!(flow_id = ?flow_id, "QUIC data stream closed from {}", peer_addr);
                    } else {
                        warn!(flow_id = ?flow_id, "Error reading from QUIC data stream: {e}");
                    }
                    break;
                }
            }
        }

        if let Some(id) = flow_id {
            let mut registry = flow_registry.lock().await;
            if registry.remove(id).is_some() {
                debug!(flow_id = ?id, "Removed flow from registry");
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
            .with_require_client_cert(true)
            .with_alpn_protocols(vec![b"mqtt".to_vec()]);

        assert!(config.require_client_cert);
        assert_eq!(config.alpn_protocols.len(), 1);
        assert_eq!(config.cert_chain.len(), 1);
    }
}
