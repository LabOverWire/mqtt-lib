#[cfg(not(target_arch = "wasm32"))]
pub mod manager;
#[cfg(test)]
pub mod mock;
#[cfg(not(target_arch = "wasm32"))]
pub mod packet_io;
#[cfg(not(target_arch = "wasm32"))]
pub mod tcp;
#[cfg(not(target_arch = "wasm32"))]
pub mod tls;
#[cfg(not(target_arch = "wasm32"))]
pub mod websocket;
#[cfg(not(target_arch = "wasm32"))]
pub mod quic;
#[cfg(not(target_arch = "wasm32"))]
pub mod quic_stream_manager;

#[cfg(not(target_arch = "wasm32"))]
use crate::error::Result;
#[cfg(not(target_arch = "wasm32"))]
use crate::Transport;

#[cfg(not(target_arch = "wasm32"))]
pub use manager::{ConnectionState, ConnectionStats, ManagerConfig, TransportManager};
#[cfg(not(target_arch = "wasm32"))]
pub use packet_io::{PacketIo, PacketReader, PacketWriter};
#[cfg(not(target_arch = "wasm32"))]
pub use tcp::{TcpConfig, TcpTransport};
#[cfg(not(target_arch = "wasm32"))]
pub use tls::{TlsConfig, TlsTransport};
#[cfg(not(target_arch = "wasm32"))]
pub use websocket::{WebSocketConfig, WebSocketTransport};
#[cfg(not(target_arch = "wasm32"))]
pub use quic::{QuicConfig, QuicTransport, StreamStrategy};
#[cfg(not(target_arch = "wasm32"))]
pub use quic_stream_manager::QuicStreamManager;

#[cfg(not(target_arch = "wasm32"))]
pub enum TransportType {
    Tcp(TcpTransport),
    Tls(Box<TlsTransport>),
    WebSocket(Box<WebSocketTransport>),
    Quic(Box<QuicTransport>),
}

#[cfg(not(target_arch = "wasm32"))]
impl Transport for TransportType {
    async fn connect(&mut self) -> Result<()> {
        match self {
            Self::Tcp(t) => t.connect().await,
            Self::Tls(t) => t.connect().await,
            Self::WebSocket(t) => t.connect().await,
            Self::Quic(t) => t.connect().await,
        }
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        match self {
            Self::Tcp(t) => t.read(buf).await,
            Self::Tls(t) => t.read(buf).await,
            Self::WebSocket(t) => t.read(buf).await,
            Self::Quic(t) => t.read(buf).await,
        }
    }

    async fn write(&mut self, buf: &[u8]) -> Result<()> {
        match self {
            Self::Tcp(t) => t.write(buf).await,
            Self::Tls(t) => t.write(buf).await,
            Self::WebSocket(t) => t.write(buf).await,
            Self::Quic(t) => t.write(buf).await,
        }
    }

    async fn close(&mut self) -> Result<()> {
        match self {
            Self::Tcp(t) => t.close().await,
            Self::Tls(t) => t.close().await,
            Self::WebSocket(t) => t.close().await,
            Self::Quic(t) => t.close().await,
        }
    }
}
