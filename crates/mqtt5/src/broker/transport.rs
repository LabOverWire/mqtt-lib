use crate::error::Result;
use crate::Transport;
use std::fmt::Debug;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;

#[cfg(feature = "transport-quic")]
use super::quic_acceptor::QuicStreamWrapper;
use super::tls_acceptor::TlsStreamWrapper;
#[cfg(feature = "transport-websocket")]
use super::websocket_server::WebSocketStreamWrapper;

#[cfg(feature = "transport-websocket")]
pub trait WebSocketTransport:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Sync + Unpin
{
    /// # Errors
    ///
    /// Returns an error if the peer address cannot be determined.
    fn peer_addr(&self) -> Result<SocketAddr>;
}

#[cfg(feature = "transport-websocket")]
impl<S> WebSocketTransport for WebSocketStreamWrapper<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Sync + Unpin,
{
    fn peer_addr(&self) -> Result<SocketAddr> {
        Err(crate::error::MqttError::InvalidState(
            "peer_addr not available for generic WebSocket stream".to_string(),
        ))
    }
}

pub enum BrokerTransport {
    Tcp(TcpStream),
    Tls(Box<TlsStreamWrapper>),
    #[cfg(feature = "transport-websocket")]
    WebSocket(Box<dyn WebSocketTransport>),
    #[cfg(feature = "transport-quic")]
    Quic(Box<QuicStreamWrapper>),
}

impl BrokerTransport {
    pub fn tcp(stream: TcpStream) -> Self {
        Self::Tcp(stream)
    }

    pub fn tls(stream: TlsStreamWrapper) -> Self {
        Self::Tls(Box::new(stream))
    }

    #[cfg(feature = "transport-websocket")]
    pub fn websocket<S>(stream: WebSocketStreamWrapper<S>) -> Self
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Sync + Unpin + 'static,
    {
        Self::WebSocket(Box::new(stream))
    }

    #[cfg(feature = "transport-quic")]
    #[must_use]
    pub fn quic(stream: QuicStreamWrapper) -> Self {
        Self::Quic(Box::new(stream))
    }

    /// # Errors
    ///
    /// Returns an error if the peer address cannot be determined.
    pub fn peer_addr(&self) -> Result<SocketAddr> {
        match self {
            Self::Tcp(stream) => Ok(stream.peer_addr()?),
            Self::Tls(stream) => stream.peer_addr(),
            #[cfg(feature = "transport-websocket")]
            Self::WebSocket(stream) => stream.peer_addr(),
            #[cfg(feature = "transport-quic")]
            Self::Quic(stream) => Ok(stream.peer_addr()),
        }
    }

    pub fn transport_type(&self) -> &'static str {
        match self {
            Self::Tcp(_) => "TCP",
            Self::Tls(_) => "TLS",
            #[cfg(feature = "transport-websocket")]
            Self::WebSocket(_) => "WebSocket",
            #[cfg(feature = "transport-quic")]
            Self::Quic(_) => "QUIC",
        }
    }

    pub fn is_secure(&self) -> bool {
        match self {
            Self::Tls(_) => true,
            #[cfg(feature = "transport-quic")]
            Self::Quic(_) => true,
            Self::Tcp(_) => false,
            #[cfg(feature = "transport-websocket")]
            Self::WebSocket(_) => false,
        }
    }

    pub fn client_cert_info(&self) -> Option<String> {
        match self {
            Self::Tls(stream) => {
                if stream.has_client_cert() {
                    Some("Client certificate provided".to_string())
                } else {
                    None
                }
            }
            Self::Tcp(_) => None,
            #[cfg(feature = "transport-websocket")]
            Self::WebSocket(_) => None,
            #[cfg(feature = "transport-quic")]
            Self::Quic(_) => None,
        }
    }
}

impl Debug for BrokerTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tcp(_) => write!(f, "BrokerTransport::Tcp"),
            Self::Tls(_) => write!(f, "BrokerTransport::Tls"),
            #[cfg(feature = "transport-websocket")]
            Self::WebSocket(_) => write!(f, "BrokerTransport::WebSocket"),
            #[cfg(feature = "transport-quic")]
            Self::Quic(_) => write!(f, "BrokerTransport::Quic"),
        }
    }
}

impl AsyncRead for BrokerTransport {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_read(cx, buf),
            #[cfg(feature = "transport-websocket")]
            Self::WebSocket(stream) => Pin::new(stream).poll_read(cx, buf),
            #[cfg(feature = "transport-quic")]
            Self::Quic(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for BrokerTransport {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_write(cx, buf),
            #[cfg(feature = "transport-websocket")]
            Self::WebSocket(stream) => Pin::new(stream).poll_write(cx, buf),
            #[cfg(feature = "transport-quic")]
            Self::Quic(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream).poll_flush(cx),
            #[cfg(feature = "transport-websocket")]
            Self::WebSocket(stream) => Pin::new(stream).poll_flush(cx),
            #[cfg(feature = "transport-quic")]
            Self::Quic(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream).poll_shutdown(cx),
            #[cfg(feature = "transport-websocket")]
            Self::WebSocket(stream) => Pin::new(stream).poll_shutdown(cx),
            #[cfg(feature = "transport-quic")]
            Self::Quic(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

impl Transport for BrokerTransport {
    fn connect(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        Ok(AsyncReadExt::read(self, buf).await?)
    }

    async fn write(&mut self, buf: &[u8]) -> Result<()> {
        AsyncWriteExt::write_all(self, buf).await?;
        AsyncWriteExt::flush(self).await?;
        Ok(())
    }

    async fn close(&mut self) -> Result<()> {
        AsyncWriteExt::shutdown(self).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_transport_type() {}
}
