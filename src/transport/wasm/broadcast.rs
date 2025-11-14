use crate::error::Result;
use crate::transport::Transport;
use async_trait::async_trait;

pub struct BroadcastChannelTransport {
    connected: bool,
}

impl BroadcastChannelTransport {
    pub fn new() -> Self {
        Self { connected: false }
    }
}

#[async_trait(?Send)]
impl Transport for BroadcastChannelTransport {
    async fn connect(&mut self) -> Result<()> {
        self.connected = true;
        Ok(())
    }

    async fn read(&mut self, _buf: &mut [u8]) -> Result<usize> {
        todo!("BroadcastChannelTransport::read - Sprint 2")
    }

    async fn write(&mut self, _buf: &[u8]) -> Result<()> {
        todo!("BroadcastChannelTransport::write - Sprint 2")
    }

    async fn close(&mut self) -> Result<()> {
        self.connected = false;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }
}

impl Default for BroadcastChannelTransport {
    fn default() -> Self {
        Self::new()
    }
}
