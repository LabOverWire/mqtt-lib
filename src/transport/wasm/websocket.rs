use crate::error::Result;
use crate::transport::Transport;

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

impl Transport for WasmWebSocketTransport {
    fn connect(&mut self) -> impl std::future::Future<Output = Result<()>> {
        self.connected = true;
        async { Ok(()) }
    }

    fn read(&mut self, _buf: &mut [u8]) -> impl std::future::Future<Output = Result<usize>> {
        async { todo!("WasmWebSocketTransport::read - Sprint 2") }
    }

    fn write(&mut self, _buf: &[u8]) -> impl std::future::Future<Output = Result<()>> {
        async { todo!("WasmWebSocketTransport::write - Sprint 2") }
    }

    fn close(&mut self) -> impl std::future::Future<Output = Result<()>> {
        self.connected = false;
        async { Ok(()) }
    }

    fn is_connected(&self) -> bool {
        self.connected
    }
}
