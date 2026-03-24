#[cfg(not(target_arch = "wasm32"))]
pub mod client_config;
#[cfg(not(target_arch = "wasm32"))]
pub mod flow;
#[cfg(not(target_arch = "wasm32"))]
pub mod manager;
#[cfg(test)]
pub mod mock;
#[cfg(not(target_arch = "wasm32"))]
pub mod packet_io;
#[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
pub mod quic;
#[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
pub mod quic_error;
#[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
pub mod quic_stream_manager;
#[cfg(not(target_arch = "wasm32"))]
pub mod tcp;
#[cfg(not(target_arch = "wasm32"))]
pub mod tls;
#[cfg(all(not(target_arch = "wasm32"), feature = "transport-websocket"))]
pub mod websocket;

#[cfg(not(target_arch = "wasm32"))]
use crate::error::Result;
#[cfg(not(target_arch = "wasm32"))]
use crate::Transport;

#[cfg(not(target_arch = "wasm32"))]
pub use client_config::{ClientTransportConfig, StreamStrategy};
#[cfg(not(target_arch = "wasm32"))]
pub use flow::{FlowFlags, FlowId};
#[cfg(not(target_arch = "wasm32"))]
pub use manager::{ConnectionState, ConnectionStats, ManagerConfig, TransportManager};
#[cfg(not(target_arch = "wasm32"))]
pub use packet_io::{PacketIo, PacketReader, PacketWriter};
#[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
pub use quic::{QuicConfig, QuicSplitResult, QuicTransport, ALPN_MQTT, ALPN_MQTT_NEXT};
#[cfg(all(not(target_arch = "wasm32"), feature = "transport-quic"))]
pub use quic_stream_manager::QuicStreamManager;
#[cfg(not(target_arch = "wasm32"))]
pub use tcp::{TcpConfig, TcpTransport};
#[cfg(not(target_arch = "wasm32"))]
pub use tls::{TlsConfig, TlsTransport};
#[cfg(all(not(target_arch = "wasm32"), feature = "transport-websocket"))]
pub use websocket::{WebSocketConfig, WebSocketTransport};

#[cfg(not(target_arch = "wasm32"))]
pub enum TransportType {
    Tcp(TcpTransport),
    Tls(Box<TlsTransport>),
    #[cfg(feature = "transport-websocket")]
    WebSocket(Box<WebSocketTransport>),
    #[cfg(feature = "transport-quic")]
    Quic(Box<QuicTransport>),
}

#[cfg(not(target_arch = "wasm32"))]
impl Transport for TransportType {
    async fn connect(&mut self) -> Result<()> {
        match self {
            Self::Tcp(t) => t.connect().await,
            Self::Tls(t) => t.connect().await,
            #[cfg(feature = "transport-websocket")]
            Self::WebSocket(t) => t.connect().await,
            #[cfg(feature = "transport-quic")]
            Self::Quic(t) => t.connect().await,
        }
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        match self {
            Self::Tcp(t) => t.read(buf).await,
            Self::Tls(t) => t.read(buf).await,
            #[cfg(feature = "transport-websocket")]
            Self::WebSocket(t) => t.read(buf).await,
            #[cfg(feature = "transport-quic")]
            Self::Quic(t) => t.read(buf).await,
        }
    }

    async fn write(&mut self, buf: &[u8]) -> Result<()> {
        match self {
            Self::Tcp(t) => t.write(buf).await,
            Self::Tls(t) => t.write(buf).await,
            #[cfg(feature = "transport-websocket")]
            Self::WebSocket(t) => t.write(buf).await,
            #[cfg(feature = "transport-quic")]
            Self::Quic(t) => t.write(buf).await,
        }
    }

    async fn close(&mut self) -> Result<()> {
        match self {
            Self::Tcp(t) => t.close().await,
            Self::Tls(t) => t.close().await,
            #[cfg(feature = "transport-websocket")]
            Self::WebSocket(t) => t.close().await,
            #[cfg(feature = "transport-quic")]
            Self::Quic(t) => t.close().await,
        }
    }
}
