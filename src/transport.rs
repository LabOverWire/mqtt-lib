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

#[cfg(all(
    target_arch = "wasm32",
    any(feature = "wasm-client", feature = "wasm-broker")
))]
pub mod wasm;

use crate::error::Result;

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

#[cfg(all(
    target_arch = "wasm32",
    any(feature = "wasm-client", feature = "wasm-broker")
))]
pub use wasm::{BroadcastChannelTransport, MessagePortTransport, WasmWebSocketTransport};

#[cfg(not(target_arch = "wasm32"))]
pub trait Transport: Send + Sync {
    /// Establishes a connection
    ///
    /// # Errors
    ///
    /// Returns an error if the connection cannot be established
    fn connect(&mut self) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Reads data into the provided buffer
    ///
    /// # Errors
    ///
    /// Returns an error if the read operation fails
    fn read(&mut self, buf: &mut [u8]) -> impl std::future::Future<Output = Result<usize>> + Send;

    /// Writes data from the provided buffer
    ///
    /// # Errors
    ///
    /// Returns an error if the write operation fails
    fn write(&mut self, buf: &[u8]) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Closes the connection
    ///
    /// # Errors
    ///
    /// Returns an error if the connection cannot be closed cleanly
    fn close(&mut self) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Checks if the transport is connected
    fn is_connected(&self) -> bool {
        false
    }
}

#[cfg(target_arch = "wasm32")]
pub trait Transport {
    /// Establishes a connection
    ///
    /// # Errors
    ///
    /// Returns an error if the connection cannot be established
    fn connect(&mut self) -> impl std::future::Future<Output = Result<()>>;

    /// Reads data into the provided buffer
    ///
    /// # Errors
    ///
    /// Returns an error if the read operation fails
    fn read(&mut self, buf: &mut [u8]) -> impl std::future::Future<Output = Result<usize>>;

    /// Writes data from the provided buffer
    ///
    /// # Errors
    ///
    /// Returns an error if the write operation fails
    fn write(&mut self, buf: &[u8]) -> impl std::future::Future<Output = Result<()>>;

    /// Closes the connection
    ///
    /// # Errors
    ///
    /// Returns an error if the connection cannot be closed cleanly
    fn close(&mut self) -> impl std::future::Future<Output = Result<()>>;

    /// Checks if the transport is connected
    fn is_connected(&self) -> bool {
        false
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub enum TransportType {
    Tcp(TcpTransport),
    Tls(Box<TlsTransport>),
    WebSocket(Box<WebSocketTransport>),
}

#[cfg(not(target_arch = "wasm32"))]
impl Transport for TransportType {
    async fn connect(&mut self) -> Result<()> {
        match self {
            Self::Tcp(t) => t.connect().await,
            Self::Tls(t) => t.connect().await,
            Self::WebSocket(t) => t.connect().await,
        }
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        match self {
            Self::Tcp(t) => t.read(buf).await,
            Self::Tls(t) => t.read(buf).await,
            Self::WebSocket(t) => t.read(buf).await,
        }
    }

    async fn write(&mut self, buf: &[u8]) -> Result<()> {
        match self {
            Self::Tcp(t) => t.write(buf).await,
            Self::Tls(t) => t.write(buf).await,
            Self::WebSocket(t) => t.write(buf).await,
        }
    }

    async fn close(&mut self) -> Result<()> {
        match self {
            Self::Tcp(t) => t.close().await,
            Self::Tls(t) => t.close().await,
            Self::WebSocket(t) => t.close().await,
        }
    }
}
