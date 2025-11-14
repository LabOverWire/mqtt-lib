use crate::error::Result;
use crate::transport::Transport;
use async_trait::async_trait;

pub struct WasmWebSocketTransport {
    url: String,
    connected: bool,
}

impl WasmWebSocketTransport {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            connected: false,
        }
    }
}

#[async_trait(?Send)]
impl Transport for WasmWebSocketTransport {
    async fn connect(&mut self) -> Result<()> {
        self.connected = true;
        Ok(())
    }

    async fn read(&mut self, _buf: &mut [u8]) -> Result<usize> {
        todo!("WasmWebSocketTransport::read - Sprint 2")
    }

    async fn write(&mut self, _buf: &[u8]) -> Result<()> {
        todo!("WasmWebSocketTransport::write - Sprint 2")
    }

    async fn close(&mut self) -> Result<()> {
        self.connected = false;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }
}
