use crate::error::Result;
use crate::transport::Transport;

pub struct BroadcastChannelTransport {
    connected: bool,
}

impl BroadcastChannelTransport {
    pub fn new() -> Self {
        Self { connected: false }
    }
}

impl Transport for BroadcastChannelTransport {
    fn connect(&mut self) -> impl std::future::Future<Output = Result<()>> {
        self.connected = true;
        async { Ok(()) }
    }

    fn read(&mut self, _buf: &mut [u8]) -> impl std::future::Future<Output = Result<usize>> {
        async { todo!("BroadcastChannelTransport::read - Sprint 2") }
    }

    fn write(&mut self, _buf: &[u8]) -> impl std::future::Future<Output = Result<()>> {
        async { todo!("BroadcastChannelTransport::write - Sprint 2") }
    }

    fn close(&mut self) -> impl std::future::Future<Output = Result<()>> {
        self.connected = false;
        async { Ok(()) }
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
