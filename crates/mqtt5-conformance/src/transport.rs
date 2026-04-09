//! Transport abstraction for raw MQTT packet I/O.
//!
//! [`RawMqttClient`](crate::raw_client::RawMqttClient) is generic over any byte
//! stream that implements the [`Transport`] trait. Today this covers
//! [`TcpTransport`]; future phases add TLS and WebSocket implementations so the
//! conformance suite can drive a broker over every protocol from the same
//! test code.

#![allow(clippy::missing_errors_doc)]

use std::io;
use std::net::SocketAddr;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

/// A byte-oriented transport that speaks raw MQTT packets.
///
/// Implementations must be `Send + Unpin` so tests can move them across await
/// points and between tasks on the multi-threaded runtime.
pub trait Transport: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> Transport for T {}

/// A plain TCP transport backed by a tokio [`TcpStream`].
pub struct TcpTransport {
    stream: TcpStream,
}

impl TcpTransport {
    /// Opens a TCP connection to `addr`.
    pub async fn connect(addr: SocketAddr) -> io::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        Ok(Self { stream })
    }

    /// Consumes this transport and returns the underlying [`TcpStream`].
    #[must_use]
    pub fn into_inner(self) -> TcpStream {
        self.stream
    }
}

impl AsyncRead for TcpTransport {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for TcpTransport {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}
