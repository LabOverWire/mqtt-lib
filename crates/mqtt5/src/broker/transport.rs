//! Unified transport wrapper for broker connections
//!
//! This module provides a unified interface for different transport types
//! (TCP, TLS, WebSocket) used by the broker's client handler.

use crate::error::Result;
use crate::transport::Transport;
use std::fmt::Debug;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;

use super::tls_acceptor::TlsStreamWrapper;
use super::websocket_server::WebSocketStreamWrapper;

/// Trait for streams that can be used as WebSocket transport
pub trait WebSocketTransport:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Sync + Unpin
{
    /// Gets the peer address (if available)
    fn peer_addr(&self) -> Result<SocketAddr>;
}

/// Blanket implementation for all types that implement the required traits
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

/// Unified transport enum for different connection types
pub enum BrokerTransport {
    /// Plain TCP connection
    Tcp(TcpStream),
    /// TLS-encrypted connection
    Tls(Box<TlsStreamWrapper>),
    /// WebSocket connection (can be over TCP or TLS)
    WebSocket(Box<dyn WebSocketTransport>),
}

impl BrokerTransport {
    /// Creates a new TCP transport
    pub fn tcp(stream: TcpStream) -> Self {
        Self::Tcp(stream)
    }

    /// Creates a new TLS transport
    pub fn tls(stream: TlsStreamWrapper) -> Self {
        Self::Tls(Box::new(stream))
    }

    /// Creates a new WebSocket transport
    pub fn websocket<S>(stream: WebSocketStreamWrapper<S>) -> Self
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Sync + Unpin + 'static,
    {
        Self::WebSocket(Box::new(stream))
    }

    /// Gets the peer address
    pub fn peer_addr(&self) -> Result<SocketAddr> {
        match self {
            Self::Tcp(stream) => Ok(stream.peer_addr()?),
            Self::Tls(stream) => stream.peer_addr(),
            Self::WebSocket(stream) => stream.peer_addr(),
        }
    }

    /// Gets the transport type as a string for logging
    pub fn transport_type(&self) -> &'static str {
        match self {
            Self::Tcp(_) => "TCP",
            Self::Tls(_) => "TLS",
            Self::WebSocket(_) => "WebSocket",
        }
    }

    /// Checks if this is a secure connection
    pub fn is_secure(&self) -> bool {
        matches!(self, Self::Tls(_))
    }

    /// Gets client certificate info if available (for TLS connections)
    pub fn client_cert_info(&self) -> Option<String> {
        match self {
            Self::Tls(stream) => {
                if stream.has_client_cert() {
                    Some("Client certificate provided".to_string())
                } else {
                    None
                }
            }
            Self::Tcp(_) | Self::WebSocket(_) => None,
        }
    }
}

impl Debug for BrokerTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tcp(_) => write!(f, "BrokerTransport::Tcp"),
            Self::Tls(_) => write!(f, "BrokerTransport::Tls"),
            Self::WebSocket(_) => write!(f, "BrokerTransport::WebSocket"),
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
            Self::WebSocket(stream) => Pin::new(stream).poll_read(cx, buf),
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
            Self::WebSocket(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream).poll_flush(cx),
            Self::WebSocket(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::WebSocket(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

// Implement the Transport trait for BrokerTransport
impl Transport for BrokerTransport {
    async fn connect(&mut self) -> Result<()> {
        // Server-side transports are already connected
        Ok(())
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
    fn test_transport_type() {
        // We can't test with actual streams in unit tests
        // This would be better as an integration test
    }
}
