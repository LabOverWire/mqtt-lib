//! Unified reader and writer types for all transport types

use crate::error::Result;
use crate::packet::Packet;
use crate::transport::tls::{TlsReadHalf, TlsWriteHalf};
use crate::transport::websocket::{WebSocketReadHandle, WebSocketWriteHandle};
use crate::transport::{PacketReader, PacketWriter};
use quinn::{RecvStream, SendStream};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

enum UnifiedReaderInner {
    Tcp(OwnedReadHalf),
    Tls(TlsReadHalf),
    WebSocket(WebSocketReadHandle),
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

    pub fn websocket(reader: WebSocketReadHandle, protocol_version: u8) -> Self {
        Self {
            inner: UnifiedReaderInner::WebSocket(reader),
            protocol_version,
        }
    }

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
            UnifiedReaderInner::WebSocket(reader) => {
                reader.read_packet(self.protocol_version).await
            }
            UnifiedReaderInner::Quic(reader) => reader.read_packet(self.protocol_version).await,
        }
    }
}

pub enum UnifiedWriter {
    Tcp(OwnedWriteHalf),
    Tls(TlsWriteHalf),
    WebSocket(WebSocketWriteHandle),
    Quic(SendStream),
}

impl PacketWriter for UnifiedWriter {
    async fn write_packet(&mut self, packet: Packet) -> Result<()> {
        match self {
            Self::Tcp(writer) => writer.write_packet(packet).await,
            Self::Tls(writer) => writer.write_packet(packet).await,
            Self::WebSocket(writer) => writer.write_packet(packet).await,
            Self::Quic(writer) => writer.write_packet(packet).await,
        }
    }
}
