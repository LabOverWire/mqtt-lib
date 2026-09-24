//! Unified reader and writer types for all transport types

use crate::error::{MqttError, Result};
use crate::packet::Packet;
use crate::transport::packet_io::read_packet_from_stream;
use crate::transport::tls::{TlsReadHalf, TlsWriteHalf};
use crate::transport::PacketWriter;
use bytes::BytesMut;
use tokio::io::AsyncWriteExt;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

#[cfg(feature = "transport-websocket")]
use crate::transport::websocket::{WebSocketReadHandle, WebSocketWriteHandle};
#[cfg(feature = "transport-quic")]
use quinn::{Connection, RecvStream, SendStream};
#[cfg(feature = "transport-quic")]
use std::sync::Arc;
#[cfg(feature = "transport-quic")]
use std::time::Duration;

#[cfg(feature = "transport-quic")]
const QUIC_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

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
    read_buffer: BytesMut,
    max_packet_size: usize,
}

impl UnifiedReader {
    pub fn tcp(reader: OwnedReadHalf, protocol_version: u8) -> Self {
        Self {
            inner: UnifiedReaderInner::Tcp(reader),
            protocol_version,
            read_buffer: BytesMut::new(),
            max_packet_size: usize::MAX,
        }
    }

    pub fn tls(reader: TlsReadHalf, protocol_version: u8) -> Self {
        Self {
            inner: UnifiedReaderInner::Tls(reader),
            protocol_version,
            read_buffer: BytesMut::new(),
            max_packet_size: usize::MAX,
        }
    }

    #[cfg(feature = "transport-websocket")]
    pub fn websocket(reader: WebSocketReadHandle, protocol_version: u8) -> Self {
        Self {
            inner: UnifiedReaderInner::WebSocket(reader),
            protocol_version,
            read_buffer: BytesMut::new(),
            max_packet_size: usize::MAX,
        }
    }

    #[cfg(feature = "transport-quic")]
    pub fn quic(reader: RecvStream, protocol_version: u8) -> Self {
        Self {
            inner: UnifiedReaderInner::Quic(reader),
            protocol_version,
            read_buffer: BytesMut::new(),
            max_packet_size: usize::MAX,
        }
    }

    #[must_use]
    pub fn with_maximum_packet_size(mut self, maximum_packet_size: Option<u32>) -> Self {
        self.max_packet_size = maximum_packet_size
            .and_then(|size| usize::try_from(size).ok())
            .unwrap_or(usize::MAX);
        self
    }

    pub async fn read_packet(&mut self) -> Result<Packet> {
        match &mut self.inner {
            UnifiedReaderInner::Tcp(reader) => {
                read_packet_from_stream(
                    reader,
                    self.protocol_version,
                    &mut self.read_buffer,
                    self.max_packet_size,
                )
                .await
            }
            UnifiedReaderInner::Tls(reader) => {
                read_packet_from_stream(
                    reader,
                    self.protocol_version,
                    &mut self.read_buffer,
                    self.max_packet_size,
                )
                .await
            }
            #[cfg(feature = "transport-websocket")]
            UnifiedReaderInner::WebSocket(reader) => {
                reader
                    .read_packet_limited(self.protocol_version, self.max_packet_size)
                    .await
            }
            #[cfg(feature = "transport-quic")]
            UnifiedReaderInner::Quic(reader) => {
                read_packet_from_stream(
                    reader,
                    self.protocol_version,
                    &mut self.read_buffer,
                    self.max_packet_size,
                )
                .await
            }
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
    #[cfg(feature = "transport-quic")]
    QuicControl(SendStream, Arc<Connection>),
    Closed,
}

impl UnifiedWriter {
    pub async fn close(&mut self, final_packet: Option<Packet>) -> Result<()> {
        let mut current = std::mem::replace(self, Self::Closed);
        let failed = final_packet.as_ref().is_some_and(is_error_disconnect);
        let written = match final_packet {
            Some(packet) => current.write_packet(packet).await,
            None => Ok(()),
        };
        let shutdown = current.shutdown(failed).await;
        written.and(shutdown)
    }

    async fn shutdown(&mut self, failed: bool) -> Result<()> {
        tracing::debug!(failed, "Shutting down network connection");
        match self {
            Self::Tcp(writer) => Ok(writer.shutdown().await?),
            Self::Tls(writer) => Ok(writer.shutdown().await?),
            #[cfg(feature = "transport-websocket")]
            Self::WebSocket(writer) => writer.close().await,
            #[cfg(feature = "transport-quic")]
            Self::Quic(writer) => finish_quic_stream(writer),
            #[cfg(feature = "transport-quic")]
            Self::QuicControl(writer, connection) => {
                let finished = finish_quic_stream(writer);
                if finished.is_ok()
                    && tokio::time::timeout(QUIC_DRAIN_TIMEOUT, writer.stopped())
                        .await
                        .is_err()
                {
                    tracing::debug!("QUIC control stream not acknowledged before close");
                }
                let code = if failed {
                    mqtt5_protocol::QuicConnectionCode::Unspecified
                } else {
                    mqtt5_protocol::QuicConnectionCode::NoError
                };
                connection.close(quinn::VarInt::from_u32(code.code()), b"disconnect");
                finished
            }
            Self::Closed => Ok(()),
        }
    }
}

fn is_error_disconnect(packet: &Packet) -> bool {
    matches!(packet, Packet::Disconnect(disconnect) if disconnect.reason_code.is_error())
}

#[cfg(feature = "transport-quic")]
fn finish_quic_stream(writer: &mut SendStream) -> Result<()> {
    writer
        .finish()
        .map_err(|e| MqttError::ConnectionError(format!("QUIC stream finish: {e}")))
}

impl PacketWriter for UnifiedWriter {
    async fn write_packet(&mut self, packet: Packet) -> Result<()> {
        match self {
            Self::Tcp(writer) => writer.write_packet(packet).await,
            Self::Tls(writer) => writer.write_packet(packet).await,
            #[cfg(feature = "transport-websocket")]
            Self::WebSocket(writer) => writer.write_packet(packet).await,
            #[cfg(feature = "transport-quic")]
            Self::Quic(writer) | Self::QuicControl(writer, _) => writer.write_packet(packet).await,
            Self::Closed => Err(MqttError::NotConnected),
        }
    }
}
