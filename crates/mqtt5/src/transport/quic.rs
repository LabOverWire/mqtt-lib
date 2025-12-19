use crate::crypto::NoVerification;
use crate::error::{MqttError, Result};
use crate::time::Duration;
use crate::transport::flow::FlowFlags;
use crate::transport::packet_io::{encode_packet_to_buffer, PacketReader, PacketWriter};
use crate::Transport;
use bytes::{BufMut, BytesMut};
use mqtt5_protocol::packet::{FixedHeader, Packet, PacketType};
use quinn::{ClientConfig, Connection, Endpoint, RecvStream, SendStream};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig as RustlsClientConfig, RootCertStore};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{debug, instrument};

// [MQoQ§5] Multi-stream modes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamStrategy {
    #[default]
    ControlOnly,
    DataPerPublish,
    DataPerTopic,
    DataPerSubscription,
}

#[derive(Debug, Clone)]
pub struct ClientTransportConfig {
    pub insecure_tls: bool,
    pub stream_strategy: StreamStrategy,
    pub flow_headers: bool,
    pub flow_expire: Duration,
    pub max_streams: Option<usize>,
    pub datagrams: bool,
    pub connect_timeout: Duration,
}

impl Default for ClientTransportConfig {
    fn default() -> Self {
        Self {
            insecure_tls: false,
            stream_strategy: StreamStrategy::default(),
            flow_headers: false,
            flow_expire: Duration::from_secs(300),
            max_streams: None,
            datagrams: false,
            connect_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct QuicConfig {
    pub addr: SocketAddr,
    pub server_name: String,
    pub connect_timeout: Duration,
    pub client_cert: Option<Vec<CertificateDer<'static>>>,
    pub client_key: Option<PrivateKeyDer<'static>>,
    pub root_certs: Option<Vec<CertificateDer<'static>>>,
    pub use_system_roots: bool,
    pub verify_server_cert: bool,
    pub stream_strategy: StreamStrategy,
    pub max_concurrent_streams: Option<usize>,
    pub enable_datagrams: bool,
    pub datagram_send_buffer_size: usize,
    pub datagram_receive_buffer_size: usize,
    // [MQoQ§4] Flow header toggle
    pub enable_flow_headers: bool,
    pub flow_expire_interval: u64,
    pub flow_flags: FlowFlags,
}

const DEFAULT_DATAGRAM_BUFFER_SIZE: usize = 65536;
const DEFAULT_FLOW_EXPIRE_INTERVAL: u64 = 300;

impl QuicConfig {
    #[must_use]
    pub fn new(addr: SocketAddr, server_name: impl Into<String>) -> Self {
        Self {
            addr,
            server_name: server_name.into(),
            connect_timeout: Duration::from_secs(30),
            client_cert: None,
            client_key: None,
            root_certs: None,
            use_system_roots: true,
            verify_server_cert: true,
            stream_strategy: StreamStrategy::default(),
            max_concurrent_streams: None,
            enable_datagrams: false,
            datagram_send_buffer_size: DEFAULT_DATAGRAM_BUFFER_SIZE,
            datagram_receive_buffer_size: DEFAULT_DATAGRAM_BUFFER_SIZE,
            enable_flow_headers: false,
            flow_expire_interval: DEFAULT_FLOW_EXPIRE_INTERVAL,
            flow_flags: FlowFlags::default(),
        }
    }

    #[must_use]
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    #[must_use]
    pub fn with_client_cert(
        mut self,
        cert: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> Self {
        self.client_cert = Some(cert);
        self.client_key = Some(key);
        self
    }

    #[must_use]
    pub fn with_root_certs(mut self, certs: Vec<CertificateDer<'static>>) -> Self {
        self.root_certs = Some(certs);
        self.use_system_roots = false;
        self
    }

    #[must_use]
    pub fn with_verify_server_cert(mut self, verify: bool) -> Self {
        self.verify_server_cert = verify;
        self
    }

    #[must_use]
    pub fn with_stream_strategy(mut self, strategy: StreamStrategy) -> Self {
        self.stream_strategy = strategy;
        self
    }

    #[must_use]
    pub fn with_max_concurrent_streams(mut self, max: usize) -> Self {
        self.max_concurrent_streams = Some(max);
        self
    }

    #[must_use]
    pub fn with_datagrams(mut self, enable: bool) -> Self {
        self.enable_datagrams = enable;
        self
    }

    #[must_use]
    pub fn with_datagram_buffer_sizes(mut self, send: usize, receive: usize) -> Self {
        self.datagram_send_buffer_size = send;
        self.datagram_receive_buffer_size = receive;
        self
    }

    #[must_use]
    pub fn with_flow_headers(mut self, enable: bool) -> Self {
        self.enable_flow_headers = enable;
        self
    }

    #[must_use]
    pub fn with_flow_expire_interval(mut self, seconds: u64) -> Self {
        self.flow_expire_interval = seconds;
        self
    }

    #[must_use]
    pub fn with_flow_flags(mut self, flags: FlowFlags) -> Self {
        self.flow_flags = flags;
        self
    }

    fn build_client_config(&self) -> Result<ClientConfig> {
        let crypto_provider = Arc::new(rustls::crypto::ring::default_provider());

        let mut crypto = if self.verify_server_cert {
            let mut root_store = RootCertStore::empty();

            if self.use_system_roots {
                root_store.extend(webpki_roots::TLS_SERVER_ROOTS.to_vec());
            }

            if let Some(ref certs) = self.root_certs {
                for cert in certs {
                    root_store.add(cert.clone()).map_err(|e| {
                        MqttError::ConnectionError(format!("Failed to add root cert: {e}"))
                    })?;
                }
            }

            if let (Some(ref cert_chain), Some(ref key)) = (&self.client_cert, &self.client_key) {
                RustlsClientConfig::builder_with_provider(crypto_provider)
                    .with_safe_default_protocol_versions()
                    .map_err(|e| {
                        MqttError::ConnectionError(format!("Failed to set protocol versions: {e}"))
                    })?
                    .with_root_certificates(root_store)
                    .with_client_auth_cert(cert_chain.clone(), key.clone_key())
                    .map_err(|e| {
                        MqttError::ConnectionError(format!("Failed to configure client cert: {e}"))
                    })?
            } else {
                RustlsClientConfig::builder_with_provider(crypto_provider)
                    .with_safe_default_protocol_versions()
                    .map_err(|e| {
                        MqttError::ConnectionError(format!("Failed to set protocol versions: {e}"))
                    })?
                    .with_root_certificates(root_store)
                    .with_no_client_auth()
            }
        } else if let (Some(ref cert_chain), Some(ref key)) = (&self.client_cert, &self.client_key)
        {
            RustlsClientConfig::builder_with_provider(crypto_provider.clone())
                .with_safe_default_protocol_versions()
                .map_err(|e| {
                    MqttError::ConnectionError(format!("Failed to set protocol versions: {e}"))
                })?
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerification))
                .with_client_auth_cert(cert_chain.clone(), key.clone_key())
                .map_err(|e| {
                    MqttError::ConnectionError(format!("Failed to configure client cert: {e}"))
                })?
        } else {
            RustlsClientConfig::builder_with_provider(crypto_provider.clone())
                .with_safe_default_protocol_versions()
                .map_err(|e| {
                    MqttError::ConnectionError(format!("Failed to set protocol versions: {e}"))
                })?
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerification))
                .with_no_client_auth()
        };

        crypto.alpn_protocols = vec![b"mqtt".to_vec()];

        let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
            .map_err(|e| MqttError::ConnectionError(format!("Failed to build QUIC config: {e}")))?;

        let mut client_config = ClientConfig::new(Arc::new(quic_crypto));

        if self.enable_datagrams {
            let mut transport_config = quinn::TransportConfig::default();
            transport_config.datagram_send_buffer_size(self.datagram_send_buffer_size);
            transport_config.datagram_receive_buffer_size(Some(self.datagram_receive_buffer_size));
            client_config.transport_config(Arc::new(transport_config));
        }

        Ok(client_config)
    }
}

#[derive(Debug)]
pub struct QuicTransport {
    config: QuicConfig,
    endpoint: Option<Endpoint>,
    connection: Option<Connection>,
    control_stream: Option<(SendStream, RecvStream)>,
}

impl QuicTransport {
    #[must_use]
    pub fn new(config: QuicConfig) -> Self {
        Self {
            config,
            endpoint: None,
            connection: None,
            control_stream: None,
        }
    }

    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connection.is_some() && self.control_stream.is_some()
    }

    /// Splits the transport into separate send/recv components.
    ///
    /// # Errors
    /// Returns an error if the transport is not connected.
    pub fn into_split(
        mut self,
    ) -> Result<(SendStream, RecvStream, Connection, StreamStrategy, bool)> {
        let (send, recv) = self.control_stream.take().ok_or(MqttError::NotConnected)?;
        let conn = self.connection.take().ok_or(MqttError::NotConnected)?;
        let strategy = self.config.stream_strategy;
        let datagrams_enabled = self.config.enable_datagrams;
        Ok((send, recv, conn, strategy, datagrams_enabled))
    }

    pub fn datagrams_enabled(&self) -> bool {
        self.config.enable_datagrams
    }

    pub fn max_datagram_size(&self) -> Option<usize> {
        self.connection
            .as_ref()
            .and_then(Connection::max_datagram_size)
    }

    /// Sends a datagram over the QUIC connection.
    ///
    /// # Errors
    /// Returns an error if not connected or the send fails.
    pub fn send_datagram(&self, data: bytes::Bytes) -> Result<()> {
        let conn = self.connection.as_ref().ok_or(MqttError::NotConnected)?;
        conn.send_datagram(data)
            .map_err(|e| MqttError::ConnectionError(format!("Datagram send failed: {e}")))
    }

    /// Sends a datagram and waits for acknowledgment.
    ///
    /// # Errors
    /// Returns an error if not connected or the send fails.
    pub async fn send_datagram_wait(&self, data: bytes::Bytes) -> Result<()> {
        let conn = self.connection.as_ref().ok_or(MqttError::NotConnected)?;
        conn.send_datagram_wait(data)
            .await
            .map_err(|e| MqttError::ConnectionError(format!("Datagram send failed: {e}")))
    }

    /// Reads a datagram from the QUIC connection.
    ///
    /// # Errors
    /// Returns an error if not connected or the read fails.
    pub async fn read_datagram(&self) -> Result<bytes::Bytes> {
        let conn = self.connection.as_ref().ok_or(MqttError::NotConnected)?;
        conn.read_datagram()
            .await
            .map_err(|e| MqttError::ConnectionError(format!("Datagram read failed: {e}")))
    }
}

// [RFC9000§7] QUIC connection establishment
impl Transport for QuicTransport {
    #[instrument(skip(self), fields(server_name = %self.config.server_name, addr = %self.config.addr))]
    async fn connect(&mut self) -> Result<()> {
        if self.connection.is_some() {
            return Err(MqttError::AlreadyConnected);
        }

        let client_config = self.config.build_client_config()?;

        let mut endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap())
            .map_err(|e| MqttError::ConnectionError(format!("Failed to create endpoint: {e}")))?;
        endpoint.set_default_client_config(client_config);

        let connecting = endpoint
            .connect(self.config.addr, &self.config.server_name)
            .map_err(|e| MqttError::ConnectionError(format!("QUIC connect failed: {e}")))?;

        let connection = tokio::time::timeout(self.config.connect_timeout, connecting)
            .await
            .map_err(|_| MqttError::Timeout)?
            .map_err(|e| MqttError::ConnectionError(format!("QUIC handshake failed: {e}")))?;

        tracing::info!(
            remote_addr = %connection.remote_address(),
            "QUIC connection established"
        );

        // [MQoQ§4] Control flow (mandatory first bidirectional stream)
        let (send, recv) = connection.open_bi().await.map_err(|e| {
            MqttError::ConnectionError(format!("Failed to open control stream: {e}"))
        })?;

        tracing::debug!("QUIC control stream opened");

        self.endpoint = Some(endpoint);
        self.connection = Some(connection);
        self.control_stream = Some((send, recv));

        Ok(())
    }

    #[instrument(skip(self, buf), fields(buf_len = buf.len()), level = "debug")]
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let (_, recv) = self
            .control_stream
            .as_mut()
            .ok_or(MqttError::NotConnected)?;

        let n = recv
            .read(buf)
            .await
            .map_err(|e| MqttError::ConnectionError(format!("QUIC read error: {e}")))?
            .ok_or(MqttError::ClientClosed)?;
        debug!(bytes_read = n, "QUIC read complete");
        Ok(n)
    }

    #[instrument(skip(self, buf), fields(buf_len = buf.len()), level = "debug")]
    async fn write(&mut self, buf: &[u8]) -> Result<()> {
        let (send, _) = self
            .control_stream
            .as_mut()
            .ok_or(MqttError::NotConnected)?;

        send.write_all(buf)
            .await
            .map_err(|e| MqttError::ConnectionError(format!("QUIC write error: {e}")))?;
        debug!(bytes_written = buf.len(), "QUIC write complete");
        Ok(())
    }

    #[instrument(skip(self))]
    async fn close(&mut self) -> Result<()> {
        debug!("Closing QUIC connection");
        if let Some((mut send, _)) = self.control_stream.take() {
            let _ = send.finish();
        }
        if let Some(conn) = self.connection.take() {
            conn.close(0u32.into(), b"normal close");
        }
        if let Some(endpoint) = self.endpoint.take() {
            endpoint.wait_idle().await;
        }
        debug!("QUIC connection closed");
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.is_connected()
    }
}

// [MQTT5§2] Fixed header parsing
impl PacketReader for RecvStream {
    async fn read_packet(&mut self, protocol_version: u8) -> Result<Packet> {
        let mut header_buf = BytesMut::with_capacity(5);

        let mut byte = [0u8; 1];
        let n = self
            .read(&mut byte)
            .await
            .map_err(|e| MqttError::ConnectionError(format!("QUIC read error: {e}")))?
            .ok_or(MqttError::ClientClosed)?;
        if n == 0 {
            return Err(MqttError::ClientClosed);
        }
        header_buf.put_u8(byte[0]);

        loop {
            let n = self
                .read(&mut byte)
                .await
                .map_err(|e| MqttError::ConnectionError(format!("QUIC read error: {e}")))?
                .ok_or(MqttError::ClientClosed)?;
            if n == 0 {
                return Err(MqttError::ClientClosed);
            }
            header_buf.put_u8(byte[0]);

            if (byte[0] & crate::constants::masks::CONTINUATION_BIT) == 0 {
                break;
            }

            if header_buf.len() > 4 {
                return Err(MqttError::MalformedPacket(
                    "Invalid remaining length encoding".to_string(),
                ));
            }
        }

        let mut header_buf = header_buf.freeze();
        let fixed_header = FixedHeader::decode(&mut header_buf)?;

        let mut payload = vec![0u8; fixed_header.remaining_length as usize];
        let mut bytes_read = 0;
        while bytes_read < payload.len() {
            let n = self
                .read(&mut payload[bytes_read..])
                .await
                .map_err(|e| MqttError::ConnectionError(format!("QUIC read error: {e}")))?
                .ok_or(MqttError::ClientClosed)?;
            if n == 0 {
                return Err(MqttError::ClientClosed);
            }
            bytes_read += n;
        }

        let mut payload_buf = BytesMut::from(&payload[..]);
        let packet = Packet::decode_from_body_with_version(
            fixed_header.packet_type,
            &fixed_header,
            &mut payload_buf,
            protocol_version,
        )?;
        debug!(
            packet_type = ?fixed_header.packet_type,
            packet_size = fixed_header.remaining_length,
            "QUIC packet read complete"
        );
        Ok(packet)
    }
}

fn packet_to_type(packet: &Packet) -> PacketType {
    match packet {
        Packet::Connect(_) => PacketType::Connect,
        Packet::ConnAck(_) => PacketType::ConnAck,
        Packet::Publish(_) => PacketType::Publish,
        Packet::PubAck(_) => PacketType::PubAck,
        Packet::PubRec(_) => PacketType::PubRec,
        Packet::PubRel(_) => PacketType::PubRel,
        Packet::PubComp(_) => PacketType::PubComp,
        Packet::Subscribe(_) => PacketType::Subscribe,
        Packet::SubAck(_) => PacketType::SubAck,
        Packet::Unsubscribe(_) => PacketType::Unsubscribe,
        Packet::UnsubAck(_) => PacketType::UnsubAck,
        Packet::PingReq => PacketType::PingReq,
        Packet::PingResp => PacketType::PingResp,
        Packet::Disconnect(_) => PacketType::Disconnect,
        Packet::Auth(_) => PacketType::Auth,
    }
}

// [MQTT5§2] Packet encoding
impl PacketWriter for SendStream {
    async fn write_packet(&mut self, packet: Packet) -> Result<()> {
        let packet_type = packet_to_type(&packet);
        let mut buf = BytesMut::with_capacity(1024);
        encode_packet_to_buffer(&packet, &mut buf)?;
        let packet_size = buf.len();
        self.write_all(&buf)
            .await
            .map_err(|e| MqttError::ConnectionError(format!("QUIC write error: {e}")))?;
        debug!(
            packet_type = ?packet_type,
            packet_size = packet_size,
            "QUIC packet written"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn test_quic_config() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 14567);
        let config = QuicConfig::new(addr, "localhost")
            .with_connect_timeout(Duration::from_secs(10))
            .with_verify_server_cert(false);

        assert_eq!(config.addr, addr);
        assert_eq!(config.server_name, "localhost");
        assert_eq!(config.connect_timeout, Duration::from_secs(10));
        assert!(!config.verify_server_cert);
    }

    #[test]
    fn test_quic_transport_creation() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 14567);
        let config = QuicConfig::new(addr, "localhost");
        let transport = QuicTransport::new(config);

        assert!(!transport.is_connected());
    }

    #[test]
    fn test_quic_config_defaults() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 14567);
        let config = QuicConfig::new(addr, "localhost");

        assert!(!config.enable_datagrams);
        assert_eq!(
            config.datagram_send_buffer_size,
            DEFAULT_DATAGRAM_BUFFER_SIZE
        );
        assert_eq!(
            config.datagram_receive_buffer_size,
            DEFAULT_DATAGRAM_BUFFER_SIZE
        );
    }

    #[test]
    fn test_quic_config_with_datagrams() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 14567);
        let config = QuicConfig::new(addr, "localhost")
            .with_datagrams(true)
            .with_datagram_buffer_sizes(32768, 16384);

        assert!(config.enable_datagrams);
        assert_eq!(config.datagram_send_buffer_size, 32768);
        assert_eq!(config.datagram_receive_buffer_size, 16384);
    }

    #[test]
    fn test_quic_transport_datagrams_disabled_by_default() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 14567);
        let config = QuicConfig::new(addr, "localhost");
        let transport = QuicTransport::new(config);

        assert!(!transport.datagrams_enabled());
        assert!(transport.max_datagram_size().is_none());
    }

    #[test]
    fn test_quic_transport_datagrams_enabled() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 14567);
        let config = QuicConfig::new(addr, "localhost").with_datagrams(true);
        let transport = QuicTransport::new(config);

        assert!(transport.datagrams_enabled());
    }

    #[test]
    fn test_quic_config_flow_headers_disabled_by_default() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 14567);
        let config = QuicConfig::new(addr, "localhost");

        assert!(!config.enable_flow_headers);
        assert_eq!(config.flow_expire_interval, DEFAULT_FLOW_EXPIRE_INTERVAL);
    }

    #[test]
    fn test_quic_config_with_flow_headers() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 14567);
        let flags = FlowFlags {
            persistent_qos: 1,
            persistent_subscriptions: 1,
            ..Default::default()
        };
        let config = QuicConfig::new(addr, "localhost")
            .with_flow_headers(true)
            .with_flow_expire_interval(600)
            .with_flow_flags(flags);

        assert!(config.enable_flow_headers);
        assert_eq!(config.flow_expire_interval, 600);
        assert_eq!(config.flow_flags.persistent_qos, 1);
        assert_eq!(config.flow_flags.persistent_subscriptions, 1);
    }
}
