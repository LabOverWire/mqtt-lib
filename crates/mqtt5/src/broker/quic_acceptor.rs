use crate::broker::auth::AuthProvider;
use crate::broker::client_handler::ClientHandler;
use crate::broker::config::BrokerConfig;
use crate::broker::resource_monitor::ResourceMonitor;
use crate::broker::router::MessageRouter;
use crate::broker::storage::DynamicStorage;
use crate::broker::sys_topics::BrokerStats;
use crate::broker::transport::BrokerTransport;
use crate::error::{MqttError, Result};
use crate::packet::Packet;
use crate::transport::packet_io::PacketReader;
use quinn::{Connection, Endpoint, RecvStream, SendStream, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::RootCertStore;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, warn};

use super::tls_acceptor::TlsAcceptorConfig;

pub struct QuicAcceptorConfig {
    pub cert_chain: Vec<CertificateDer<'static>>,
    pub private_key: PrivateKeyDer<'static>,
    pub client_ca_certs: Option<Vec<CertificateDer<'static>>>,
    pub require_client_cert: bool,
    pub alpn_protocols: Vec<Vec<u8>>,
}

impl QuicAcceptorConfig {
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

    pub async fn load_cert_chain_from_file(
        path: impl AsRef<std::path::Path>,
    ) -> Result<Vec<CertificateDer<'static>>> {
        TlsAcceptorConfig::load_cert_chain_from_file(path).await
    }

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

    #[allow(clippy::missing_panics_doc)]
    pub fn build_server_config(&self) -> Result<ServerConfig> {
        let crypto_provider = Arc::new(rustls::crypto::ring::default_provider());

        let mut tls_config = if let Some(ref client_ca_certs) = self.client_ca_certs {
            let mut root_store = RootCertStore::empty();
            for cert in client_ca_certs {
                root_store.add(cert.clone()).map_err(|e| {
                    MqttError::Configuration(format!("Failed to add client CA cert: {e}"))
                })?;
            }

            let client_verifier = WebPkiClientVerifier::builder(Arc::new(root_store))
                .build()
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
        server_config.transport_config(Arc::new(transport_config));

        Ok(server_config)
    }

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
    pub fn new(send: SendStream, recv: RecvStream, peer_addr: SocketAddr) -> Self {
        Self {
            send,
            recv,
            peer_addr,
        }
    }

    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

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

#[allow(clippy::too_many_arguments)]
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
    let (packet_tx, packet_rx) = mpsc::channel::<Packet>(100);

    let (send, recv) = match connection.accept_bi().await {
        Ok(streams) => streams,
        Err(e) => {
            error!("Failed to accept control stream from {}: {}", peer_addr, e);
            return;
        }
    };

    debug!(
        "QUIC control stream accepted from {}, starting handler",
        peer_addr
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
    );

    tokio::spawn(async move {
        if let Err(e) = handler.run().await {
            if e.is_normal_disconnect() {
                debug!("QUIC client handler finished");
            } else {
                warn!("QUIC client handler error: {}", e);
            }
        }
    });

    tokio::spawn(async move {
        loop {
            if let Ok((_send, recv)) = connection.accept_bi().await {
                debug!("Additional QUIC data stream accepted from {}", peer_addr);
                spawn_data_stream_reader(recv, packet_tx.clone(), peer_addr);
            } else {
                debug!("QUIC connection stream accept loop ended for {}", peer_addr);
                break;
            }
        }
    });
}

fn spawn_data_stream_reader(
    mut recv: RecvStream,
    packet_tx: mpsc::Sender<Packet>,
    peer_addr: SocketAddr,
) {
    tokio::spawn(async move {
        loop {
            match recv.read_packet().await {
                Ok(packet) => {
                    debug!("Read packet from QUIC data stream: {:?}", packet);
                    if packet_tx.send(packet).await.is_err() {
                        debug!("Packet channel closed, stopping data stream reader");
                        break;
                    }
                }
                Err(e) => {
                    if matches!(e, MqttError::ClientClosed) {
                        debug!("QUIC data stream closed from {}", peer_addr);
                    } else {
                        warn!("Error reading from QUIC data stream: {}", e);
                    }
                    break;
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
            .with_require_client_cert(true)
            .with_alpn_protocols(vec![b"mqtt".to_vec()]);

        assert!(config.require_client_cert);
        assert_eq!(config.alpn_protocols.len(), 1);
        assert_eq!(config.cert_chain.len(), 1);
    }
}
