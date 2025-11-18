//! Simple wrapper for `TcpStream` to implement Transport trait

use crate::error::Result;
use crate::transport::Transport;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Wrapper for `TcpStream` that implements Transport
pub struct TcpStreamWrapper {
    stream: TcpStream,
}

impl TcpStreamWrapper {
    /// Creates a new wrapper
    pub fn new(stream: TcpStream) -> Self {
        Self { stream }
    }
}

impl Transport for TcpStreamWrapper {
    async fn connect(&mut self) -> Result<()> {
        // Already connected
        Ok(())
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        Ok(self.stream.read(buf).await?)
    }

    async fn write(&mut self, buf: &[u8]) -> Result<()> {
        self.stream.write_all(buf).await?;
        self.stream.flush().await?;
        Ok(())
    }

    async fn close(&mut self) -> Result<()> {
        self.stream.shutdown().await?;
        Ok(())
    }
}
