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
pub use websocket::{RequestModifier, RequestModifierError, WebSocketConfig, WebSocketTransport};

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn unbracketed_host(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|address| address.strip_suffix(']'))
        .unwrap_or(host)
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn certificate_name(host: &str) -> &str {
    match host
        .strip_prefix('[')
        .and_then(|address| address.strip_suffix(']'))
    {
        Some(address) => address.split('%').next().unwrap_or(address),
        None => host,
    }
}

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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::{certificate_name, unbracketed_host};

    #[test]
    fn a_certificate_is_checked_against_an_ipv6_address_without_brackets_or_zone() {
        assert_eq!(certificate_name("[::1]"), "::1");
        assert_eq!(certificate_name("[fe80::1%1]"), "fe80::1");
        assert_eq!(certificate_name("[fe80::1%lo0]"), "fe80::1");
        assert_eq!(certificate_name("[fe80::1%25eth0]"), "fe80::1");
    }

    #[test]
    fn a_certificate_is_checked_against_any_other_host_as_written() {
        assert_eq!(certificate_name("127.0.0.1"), "127.0.0.1");
        assert_eq!(certificate_name("broker.example.com"), "broker.example.com");
        assert_eq!(certificate_name("odd%name"), "odd%name");
    }

    #[test]
    fn name_resolution_takes_an_ipv6_address_with_its_zone_and_without_brackets() {
        assert_eq!(unbracketed_host("[fe80::1%lo0]"), "fe80::1%lo0");
        assert_eq!(unbracketed_host("[::1]"), "::1");
        assert_eq!(unbracketed_host("localhost"), "localhost");
    }
}
