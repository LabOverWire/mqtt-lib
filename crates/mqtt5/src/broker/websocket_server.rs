//! WebSocket server support for the MQTT broker
//!
//! This module provides WebSocket transport for MQTT connections,
//! allowing browsers and other WebSocket clients to connect to the broker.

use crate::error::{MqttError, Result};
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    accept_hdr_async, tungstenite::protocol::WebSocketConfig, WebSocketStream,
};
use tracing::{debug, error};

/// WebSocket server configuration
#[derive(Debug, Clone)]
pub struct WebSocketServerConfig {
    /// Path for WebSocket connections (e.g., "/mqtt")
    pub path: String,
    /// Subprotocol to use (e.g., "mqtt", "mqttv5.0")
    pub subprotocol: String,
    /// Maximum frame size
    pub max_frame_size: Option<usize>,
    /// Maximum message size
    pub max_message_size: Option<usize>,
}

impl Default for WebSocketServerConfig {
    fn default() -> Self {
        Self {
            path: "/mqtt".to_string(),
            subprotocol: "mqtt".to_string(),
            max_frame_size: Some(16 * 1024 * 1024),   // 16MB
            max_message_size: Some(64 * 1024 * 1024), // 64MB
        }
    }
}

impl WebSocketServerConfig {
    /// Creates a new WebSocket server configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the WebSocket path
    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    /// Sets the subprotocol
    #[must_use]
    pub fn with_subprotocol(mut self, subprotocol: impl Into<String>) -> Self {
        self.subprotocol = subprotocol.into();
        self
    }

    /// Builds the WebSocket configuration
    pub fn build_ws_config(&self) -> Option<WebSocketConfig> {
        // For now return None to use default config
        // TODO: Update when tokio-tungstenite exposes proper config
        None
    }
}

/// Wrapper for WebSocket streams that implements AsyncRead/AsyncWrite
pub struct WebSocketStreamWrapper<S = TcpStream>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    inner: WebSocketStream<S>,
    read_buffer: Vec<u8>,
    read_pos: usize,
}

impl<S> WebSocketStreamWrapper<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    /// Creates a new WebSocket stream wrapper
    pub fn new(stream: WebSocketStream<S>) -> Self {
        Self {
            inner: stream,
            read_buffer: Vec::new(),
            read_pos: 0,
        }
    }

    /// Gets the peer address (only works for TcpStream)
    pub fn peer_addr(&self) -> Result<SocketAddr>
    where
        S: std::ops::Deref<Target = TcpStream>,
    {
        self.inner
            .get_ref()
            .peer_addr()
            .map_err(|e| MqttError::Io(format!("Failed to get peer address: {e}")))
    }
}

/// Accepts a WebSocket connection with the given configuration
///
/// # Errors
///
/// Returns an error if the WebSocket handshake fails
///
/// # Panics
///
/// Panics if the subprotocol string cannot be parsed as an HTTP header value
pub async fn accept_websocket_connection<S>(
    stream: S,
    config: &WebSocketServerConfig,
    peer_addr: SocketAddr,
) -> Result<WebSocketStreamWrapper<S>>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    debug!("Starting WebSocket handshake with {}", peer_addr);

    // Create callback to handle WebSocket handshake
    let subprotocol = config.subprotocol.clone();
    let path = config.path.clone();

    let callback =
        |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
         response: tokio_tungstenite::tungstenite::handshake::server::Response| {
            // Check the path
            if req.uri().path() != path {
                debug!("WebSocket path mismatch: {} != {}", req.uri().path(), path);
                // We can't reject here, but we'll close the connection after handshake
            }

            // Check for the MQTT subprotocol
            let has_mqtt_subprotocol = req
                .headers()
                .get("Sec-WebSocket-Protocol")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|protocols| {
                    protocols
                        .split(',')
                        .any(|p| p.trim() == subprotocol.as_str())
                });

            if has_mqtt_subprotocol {
                // Add the subprotocol to the response
                let mut response = response;
                response.headers_mut().insert(
                    "Sec-WebSocket-Protocol",
                    subprotocol
                        .parse()
                        .unwrap_or_else(|_| "mqtt".parse().unwrap()),
                );
                Ok(response)
            } else {
                Ok(response)
            }
        };

    match accept_hdr_async(stream, callback).await {
        Ok(ws_stream) => {
            debug!("WebSocket handshake completed with {}", peer_addr);
            Ok(WebSocketStreamWrapper::new(ws_stream))
        }
        Err(e) => {
            error!("WebSocket handshake failed with {}: {}", peer_addr, e);
            Err(MqttError::ConnectionError(format!(
                "WebSocket handshake failed: {e}"
            )))
        }
    }
}

// Implement AsyncRead and AsyncWrite for WebSocketStreamWrapper
impl<S> AsyncRead for WebSocketStreamWrapper<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        use std::task::Poll;
        use tokio_tungstenite::tungstenite::Message;

        // If we have buffered data, return it first
        if self.read_pos < self.read_buffer.len() {
            let remaining = &self.read_buffer[self.read_pos..];
            let to_copy = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..to_copy]);
            self.read_pos += to_copy;

            // Clear buffer if we've read everything
            if self.read_pos >= self.read_buffer.len() {
                self.read_buffer.clear();
                self.read_pos = 0;
            }

            return Poll::Ready(Ok(()));
        }

        // Otherwise, try to read a new message
        let mut inner = std::pin::Pin::new(&mut self.inner);
        match inner.poll_next_unpin(cx) {
            Poll::Ready(Some(Ok(Message::Binary(data)))) => {
                // Store the data in our buffer
                self.read_buffer = data.to_vec();
                self.read_pos = 0;

                // Copy what we can to the output buffer
                let to_copy = self.read_buffer.len().min(buf.remaining());
                buf.put_slice(&self.read_buffer[..to_copy]);
                self.read_pos = to_copy;

                Poll::Ready(Ok(()))
            }
            Poll::Ready(Some(Ok(Message::Close(_)))) => Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "WebSocket closed",
            ))),
            Poll::Ready(Some(Ok(_))) => {
                // Ignore other message types (text, ping, pong)
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Err(std::io::Error::other(e.to_string()))),
            Poll::Ready(None) => Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "WebSocket stream ended",
            ))),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S> AsyncWrite for WebSocketStreamWrapper<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        use std::task::Poll;
        use tokio_tungstenite::tungstenite::Message;

        let message = Message::Binary(buf.to_vec().into());
        let mut inner = std::pin::Pin::new(&mut self.inner);

        match inner.poll_ready_unpin(cx) {
            Poll::Ready(Ok(())) => match inner.start_send_unpin(message) {
                Ok(()) => Poll::Ready(Ok(buf.len())),
                Err(e) => Poll::Ready(Err(std::io::Error::other(e.to_string()))),
            },
            Poll::Ready(Err(e)) => Poll::Ready(Err(std::io::Error::other(e.to_string()))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let mut inner = std::pin::Pin::new(&mut self.inner);
        match inner.poll_flush_unpin(cx) {
            std::task::Poll::Ready(Ok(())) => std::task::Poll::Ready(Ok(())),
            std::task::Poll::Ready(Err(e)) => {
                std::task::Poll::Ready(Err(std::io::Error::other(e.to_string())))
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let mut inner = std::pin::Pin::new(&mut self.inner);
        match inner.poll_close_unpin(cx) {
            std::task::Poll::Ready(Ok(())) => std::task::Poll::Ready(Ok(())),
            std::task::Poll::Ready(Err(e)) => {
                std::task::Poll::Ready(Err(std::io::Error::other(e.to_string())))
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_websocket_config_default() {
        let config = WebSocketServerConfig::default();
        assert_eq!(config.path, "/mqtt");
        assert_eq!(config.subprotocol, "mqtt");
        assert_eq!(config.max_frame_size, Some(16 * 1024 * 1024));
        assert_eq!(config.max_message_size, Some(64 * 1024 * 1024));
    }

    #[test]
    fn test_websocket_config_builder() {
        let config = WebSocketServerConfig::new()
            .with_path("/ws")
            .with_subprotocol("mqttv5.0");

        assert_eq!(config.path, "/ws");
        assert_eq!(config.subprotocol, "mqttv5.0");
    }

    #[test]
    fn test_ws_config_build() {
        let config = WebSocketServerConfig::default();
        let ws_config = config.build_ws_config();

        // For now, build_ws_config returns None as tokio-tungstenite doesn't expose full config
        assert!(ws_config.is_none());
    }
}
