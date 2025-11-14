use crate::error::Result;
use crate::transport::Transport;

pub struct MessagePortTransport {
    connected: bool,
}

impl MessagePortTransport {
    pub fn new() -> Self {
        Self { connected: false }
    }
}

impl Transport for MessagePortTransport {
    fn connect(&mut self) -> impl std::future::Future<Output = Result<()>> {
        self.connected = true;
        async { Ok(()) }
    }

    fn read(&mut self, _buf: &mut [u8]) -> impl std::future::Future<Output = Result<usize>> {
        async { todo!("MessagePortTransport::read - Sprint 2") }
    }

    fn write(&mut self, _buf: &[u8]) -> impl std::future::Future<Output = Result<()>> {
        async { todo!("MessagePortTransport::write - Sprint 2") }
    }

    fn close(&mut self) -> impl std::future::Future<Output = Result<()>> {
        self.connected = false;
        async { Ok(()) }
    }

    fn is_connected(&self) -> bool {
        self.connected
    }
}

impl Default for MessagePortTransport {
    fn default() -> Self {
        Self::new()
    }
}
