#![cfg(target_arch = "wasm32")]

pub mod broadcast;
pub mod message_port;
pub mod websocket;

pub use broadcast::BroadcastChannelTransport;
pub use message_port::MessagePortTransport;
pub use websocket::WasmWebSocketTransport;

use mqtt5_protocol::error::Result;
use mqtt5_protocol::Transport;

pub enum WasmTransportType {
    WebSocket(WasmWebSocketTransport),
    MessagePort(MessagePortTransport),
    BroadcastChannel(BroadcastChannelTransport),
}

impl Transport for WasmTransportType {
    async fn connect(&mut self) -> Result<()> {
        match self {
            Self::WebSocket(t) => t.connect().await,
            Self::MessagePort(t) => t.connect().await,
            Self::BroadcastChannel(t) => t.connect().await,
        }
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        match self {
            Self::WebSocket(t) => t.read(buf).await,
            Self::MessagePort(t) => t.read(buf).await,
            Self::BroadcastChannel(t) => t.read(buf).await,
        }
    }

    async fn write(&mut self, buf: &[u8]) -> Result<()> {
        match self {
            Self::WebSocket(t) => t.write(buf).await,
            Self::MessagePort(t) => t.write(buf).await,
            Self::BroadcastChannel(t) => t.write(buf).await,
        }
    }

    async fn close(&mut self) -> Result<()> {
        match self {
            Self::WebSocket(t) => t.close().await,
            Self::MessagePort(t) => t.close().await,
            Self::BroadcastChannel(t) => t.close().await,
        }
    }

    fn is_connected(&self) -> bool {
        match self {
            Self::WebSocket(t) => t.is_connected(),
            Self::MessagePort(t) => t.is_connected(),
            Self::BroadcastChannel(t) => t.is_connected(),
        }
    }
}

pub enum WasmReader {
    WebSocket(websocket::WasmReader),
    BroadcastChannel(broadcast::BroadcastChannelReader),
    MessagePort(message_port::MessagePortReader),
}

pub enum WasmWriter {
    WebSocket(websocket::WasmWriter),
    BroadcastChannel(broadcast::BroadcastChannelWriter),
    MessagePort(message_port::MessagePortWriter),
}

impl WasmReader {
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        match self {
            Self::WebSocket(r) => r.read(buf).await,
            Self::BroadcastChannel(r) => r.read(buf).await,
            Self::MessagePort(r) => r.read(buf).await,
        }
    }

    pub fn is_connected(&self) -> bool {
        match self {
            Self::WebSocket(r) => r.is_connected(),
            Self::BroadcastChannel(r) => r.is_connected(),
            Self::MessagePort(r) => r.is_connected(),
        }
    }
}

impl WasmWriter {
    pub async fn write(&mut self, buf: &[u8]) -> Result<()> {
        match self {
            Self::WebSocket(w) => w.write(buf).await,
            Self::BroadcastChannel(w) => w.write(buf).await,
            Self::MessagePort(w) => w.write(buf).await,
        }
    }

    pub async fn close(&mut self) -> Result<()> {
        match self {
            Self::WebSocket(w) => w.close().await,
            Self::BroadcastChannel(w) => w.close().await,
            Self::MessagePort(w) => w.close().await,
        }
    }

    pub fn is_connected(&self) -> bool {
        match self {
            Self::WebSocket(w) => w.is_connected(),
            Self::BroadcastChannel(w) => w.is_connected(),
            Self::MessagePort(w) => w.is_connected(),
        }
    }
}

impl WasmTransportType {
    pub fn into_split(self) -> Result<(WasmReader, WasmWriter)> {
        match self {
            Self::WebSocket(t) => {
                let (r, w) = t.into_split()?;
                Ok((WasmReader::WebSocket(r), WasmWriter::WebSocket(w)))
            }
            Self::MessagePort(t) => {
                let (r, w) = t.into_split()?;
                Ok((WasmReader::MessagePort(r), WasmWriter::MessagePort(w)))
            }
            Self::BroadcastChannel(t) => {
                let (r, w) = t.into_split()?;
                Ok((
                    WasmReader::BroadcastChannel(r),
                    WasmWriter::BroadcastChannel(w),
                ))
            }
        }
    }
}
