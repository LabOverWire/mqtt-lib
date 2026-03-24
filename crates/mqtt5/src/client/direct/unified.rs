//! Unified reader and writer types for all transport types

use crate::error::Result;
use crate::packet::Packet;
use crate::transport::tls::{TlsReadHalf, TlsWriteHalf};
use crate::transport::{PacketReader, PacketWriter};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

#[cfg(feature = "transport-websocket")]
use crate::transport::websocket::{WebSocketReadHandle, WebSocketWriteHandle};
#[cfg(feature = "transport-quic")]
use quinn::{RecvStream, SendStream};

enum UnifiedReaderInner {
    Tcp(OwnedReadHalf),
    Tls(TlsReadHalf),
    #[cfg(feature = "transport-websocket")]
    WebSocket(WebSocketReadHandle),
    #[cfg(feature = "transport-quic")]
    Quic(RecvStream),
}

pub struct UnifiedReader {
    inner: UnifiedReaderInner,
    protocol_version: u8,
}

impl UnifiedReader {
    pub fn tcp(reader: OwnedReadHalf, protocol_version: u8) -> Self {
        Self {
            inner: UnifiedReaderInner::Tcp(reader),
            protocol_version,
        }
    }

    pub fn tls(reader: TlsReadHalf, protocol_version: u8) -> Self {
        Self {
            inner: UnifiedReaderInner::Tls(reader),
            protocol_version,
        }
    }

    #[cfg(feature = "transport-websocket")]
    pub fn websocket(reader: WebSocketReadHandle, protocol_version: u8) -> Self {
        Self {
            inner: UnifiedReaderInner::WebSocket(reader),
            protocol_version,
        }
    }

    #[cfg(feature = "transport-quic")]
    pub fn quic(reader: RecvStream, protocol_version: u8) -> Self {
        Self {
            inner: UnifiedReaderInner::Quic(reader),
            protocol_version,
        }
    }

    pub async fn read_packet(&mut self) -> Result<Packet> {
        match &mut self.inner {
            UnifiedReaderInner::Tcp(reader) => reader.read_packet(self.protocol_version).await,
            UnifiedReaderInner::Tls(reader) => reader.read_packet(self.protocol_version).await,
            #[cfg(feature = "transport-websocket")]
            UnifiedReaderInner::WebSocket(reader) => {
                reader.read_packet(self.protocol_version).await
            }
            #[cfg(feature = "transport-quic")]
            UnifiedReaderInner::Quic(reader) => reader.read_packet(self.protocol_version).await,
        }
    }
}

pub enum UnifiedWriter {
    Tcp(OwnedWriteHalf),
    Tls(TlsWriteHalf),
    #[cfg(feature = "transport-websocket")]
    WebSocket(WebSocketWriteHandle),
    #[cfg(feature = "transport-quic")]
    Quic(SendStream),
}

impl PacketWriter for UnifiedWriter {
    async fn write_packet(&mut self, packet: Packet) -> Result<()> {
        match self {
            Self::Tcp(writer) => writer.write_packet(packet).await,
            Self::Tls(writer) => writer.write_packet(packet).await,
            #[cfg(feature = "transport-websocket")]
            Self::WebSocket(writer) => writer.write_packet(packet).await,
            #[cfg(feature = "transport-quic")]
            Self::Quic(writer) => writer.write_packet(packet).await,
        }
    }
}
