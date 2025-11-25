use crate::error::{MqttError, Result};
use crate::time::Duration;
use crate::transport::packet_io::{encode_packet_to_buffer, PacketReader, PacketWriter};
use crate::Transport;
use bytes::{BufMut, BytesMut};
use mqtt5_protocol::packet::{FixedHeader, Packet};
use quinn::{ClientConfig, Connection, Endpoint, RecvStream, SendStream};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig as RustlsClientConfig, RootCertStore};
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamStrategy {
    ControlOnly,
    DataPerPublish,
    DataPerTopic,
    DataPerSubscription,
}

impl Default for StreamStrategy {
    fn default() -> Self {
        Self::ControlOnly
    }
}

#[derive(Debug)]
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
}

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

    fn build_client_config(&self) -> Result<ClientConfig> {
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

        let crypto_provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut crypto =
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
            };

        crypto.alpn_protocols = vec![b"mqtt".to_vec()];

        Ok(ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto).map_err(|e| {
                MqttError::ConnectionError(format!("Failed to build QUIC config: {e}"))
            })?,
        )))
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

    pub fn into_split(mut self) -> Result<(SendStream, RecvStream, Connection, StreamStrategy)> {
        let (send, recv) = self.control_stream.take().ok_or(MqttError::NotConnected)?;
        let conn = self.connection.take().ok_or(MqttError::NotConnected)?;
        let strategy = self.config.stream_strategy;
        Ok((send, recv, conn, strategy))
    }
}

impl Transport for QuicTransport {
    async fn connect(&mut self) -> Result<()> {
        if self.connection.is_some() {
            return Err(MqttError::AlreadyConnected);
        }

        let client_config = self.config.build_client_config()?;

        let mut endpoint = Endpoint::client("[::]:0".parse().unwrap())
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

        let (send, recv) = connection.open_bi().await.map_err(|e| {
            MqttError::ConnectionError(format!("Failed to open control stream: {e}"))
        })?;

        tracing::debug!("QUIC control stream opened");

        self.endpoint = Some(endpoint);
        self.connection = Some(connection);
        self.control_stream = Some((send, recv));

        Ok(())
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let (_, recv) = self
            .control_stream
            .as_mut()
            .ok_or(MqttError::NotConnected)?;

        recv.read(buf)
            .await
            .map_err(|e| MqttError::ConnectionError(format!("QUIC read error: {e}")))?
            .ok_or(MqttError::ClientClosed)
    }

    async fn write(&mut self, buf: &[u8]) -> Result<()> {
        let (send, _) = self
            .control_stream
            .as_mut()
            .ok_or(MqttError::NotConnected)?;

        send.write_all(buf)
            .await
            .map_err(|e| MqttError::ConnectionError(format!("QUIC write error: {e}")))?;

        Ok(())
    }

    async fn close(&mut self) -> Result<()> {
        if let Some((mut send, _)) = self.control_stream.take() {
            let _ = send.finish();
        }
        if let Some(conn) = self.connection.take() {
            conn.close(0u32.into(), b"normal close");
        }
        if let Some(endpoint) = self.endpoint.take() {
            endpoint.wait_idle().await;
        }
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.is_connected()
    }
}

impl PacketReader for RecvStream {
    async fn read_packet(&mut self) -> Result<Packet> {
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
        Packet::decode_from_body(fixed_header.packet_type, &fixed_header, &mut payload_buf)
    }
}

impl PacketWriter for SendStream {
    async fn write_packet(&mut self, packet: Packet) -> Result<()> {
        let mut buf = BytesMut::with_capacity(1024);
        encode_packet_to_buffer(&packet, &mut buf)?;
        self.write_all(&buf)
            .await
            .map_err(|e| MqttError::ConnectionError(format!("QUIC write error: {e}")))?;
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
}
