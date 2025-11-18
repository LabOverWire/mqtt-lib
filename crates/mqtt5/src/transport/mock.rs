//! Mock transport implementation for testing

use crate::error::{MqttError, Result};
use crate::transport::Transport;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Mock transport behavior configuration
#[derive(Debug, Clone)]
pub struct MockBehavior {
    /// Should the next connect attempt fail?
    pub fail_connect: bool,
    /// Should the next read attempt fail?
    pub fail_read: bool,
    /// Should the next write attempt fail?
    pub fail_write: bool,
    /// Delay for connect operation (milliseconds)
    pub connect_delay_ms: u64,
    /// Delay for read operations (milliseconds)
    pub read_delay_ms: u64,
    /// Delay for write operations (milliseconds)
    pub write_delay_ms: u64,
    /// Maximum bytes to read per call
    pub read_chunk_size: usize,
}

impl Default for MockBehavior {
    fn default() -> Self {
        Self {
            fail_connect: false,
            fail_read: false,
            fail_write: false,
            connect_delay_ms: 0,
            read_delay_ms: 0,
            write_delay_ms: 0,
            read_chunk_size: 1024,
        }
    }
}

/// Mock transport for testing
pub struct MockTransport {
    /// Current connection state
    connected: bool,
    /// Behavior configuration
    behavior: Arc<Mutex<MockBehavior>>,
    /// Incoming data buffer (what the transport will read)
    incoming: Arc<Mutex<VecDeque<u8>>>,
    /// Outgoing data buffer (what was written to the transport)
    outgoing: Arc<Mutex<Vec<u8>>>,
    /// Packets to be injected on read
    inject_packets: Arc<Mutex<VecDeque<Vec<u8>>>>,
}

impl MockTransport {
    /// Creates a new mock transport
    #[must_use]
    pub fn new() -> Self {
        Self {
            connected: false,
            behavior: Arc::new(Mutex::new(MockBehavior::default())),
            incoming: Arc::new(Mutex::new(VecDeque::new())),
            outgoing: Arc::new(Mutex::new(Vec::new())),
            inject_packets: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Sets the behavior for the mock transport
    pub async fn set_behavior(&self, behavior: MockBehavior) {
        *self.behavior.lock().await = behavior;
    }

    /// Adds data to be read by the transport
    pub async fn add_incoming_data(&self, data: &[u8]) {
        self.incoming.lock().await.extend(data);
    }

    /// Injects a complete packet to be read
    pub async fn inject_packet(&self, packet_data: Vec<u8>) {
        self.inject_packets.lock().await.push_back(packet_data);
    }

    /// Gets all data that was written to the transport
    pub async fn get_written_data(&self) -> Vec<u8> {
        self.outgoing.lock().await.clone()
    }

    /// Clears the written data buffer
    pub async fn clear_written_data(&self) {
        self.outgoing.lock().await.clear();
    }

    /// Simulates a connection drop
    pub fn drop_connection(&mut self) {
        self.connected = false;
    }
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for MockTransport {
    async fn connect(&mut self) -> Result<()> {
        let behavior = self.behavior.lock().await;

        if behavior.connect_delay_ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(
                behavior.connect_delay_ms,
            ))
            .await;
        }

        if behavior.fail_connect {
            return Err(MqttError::ConnectionError(
                "Mock connect failure".to_string(),
            ));
        }

        if self.connected {
            return Err(MqttError::AlreadyConnected);
        }

        self.connected = true;
        Ok(())
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if !self.connected {
            return Err(MqttError::NotConnected);
        }

        let behavior = self.behavior.lock().await;

        if behavior.read_delay_ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(behavior.read_delay_ms)).await;
        }

        if behavior.fail_read {
            return Err(MqttError::ConnectionError("Mock read failure".to_string()));
        }

        // First check if we have injected packets
        let mut packets = self.inject_packets.lock().await;
        if let Some(packet_data) = packets.pop_front() {
            let to_read = packet_data.len().min(buf.len());
            buf[..to_read].copy_from_slice(&packet_data[..to_read]);

            // If we couldn't read the whole packet, put the rest back
            if to_read < packet_data.len() {
                let remaining = packet_data[to_read..].to_vec();
                self.incoming.lock().await.extend(&remaining);
            }

            return Ok(to_read);
        }
        drop(packets);

        // Otherwise read from incoming buffer
        let mut incoming = self.incoming.lock().await;
        if incoming.is_empty() {
            return Ok(0); // No data available
        }

        let to_read = incoming.len().min(buf.len()).min(behavior.read_chunk_size);
        for byte in buf.iter_mut().take(to_read) {
            *byte = incoming.pop_front().unwrap();
        }

        Ok(to_read)
    }

    async fn write(&mut self, buf: &[u8]) -> Result<()> {
        if !self.connected {
            return Err(MqttError::NotConnected);
        }

        let behavior = self.behavior.lock().await;

        if behavior.write_delay_ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(behavior.write_delay_ms)).await;
        }

        if behavior.fail_write {
            return Err(MqttError::ConnectionError("Mock write failure".to_string()));
        }

        self.outgoing.lock().await.extend_from_slice(buf);
        Ok(())
    }

    async fn close(&mut self) -> Result<()> {
        self.connected = false;
        Ok(())
    }
}

/// Builder for creating mock transports with predefined behaviors
pub struct MockTransportBuilder {
    transport: MockTransport,
}

impl Default for MockTransportBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl MockTransportBuilder {
    /// Creates a new builder
    #[must_use]
    pub fn new() -> Self {
        Self {
            transport: MockTransport::new(),
        }
    }

    /// Sets the transport to fail on next connect
    ///
    /// # Panics
    ///
    /// Panics if unable to create a tokio runtime
    #[must_use]
    pub fn fail_on_connect(self) -> Self {
        let transport = self.transport;
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut behavior = transport.behavior.lock().await;
            behavior.fail_connect = true;
        });
        Self { transport }
    }

    /// Sets the transport to fail on next read
    ///
    /// # Panics
    ///
    /// Panics if unable to create a tokio runtime
    #[must_use]
    pub fn fail_on_read(self) -> Self {
        let transport = self.transport;
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut behavior = transport.behavior.lock().await;
            behavior.fail_read = true;
        });
        Self { transport }
    }

    /// Sets delays for operations
    ///
    /// # Panics
    ///
    /// Panics if unable to create a tokio runtime
    #[must_use]
    pub fn with_delays(self, connect_ms: u64, read_ms: u64, write_ms: u64) -> Self {
        let transport = self.transport;
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut behavior = transport.behavior.lock().await;
            behavior.connect_delay_ms = connect_ms;
            behavior.read_delay_ms = read_ms;
            behavior.write_delay_ms = write_ms;
        });
        Self { transport }
    }

    /// Builds the mock transport
    #[must_use]
    pub fn build(self) -> MockTransport {
        self.transport
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_connect_disconnect() {
        let mut transport = MockTransport::new();

        // Initially not connected
        assert!(transport.read(&mut [0u8; 10]).await.is_err());
        assert!(transport.write(b"test").await.is_err());

        // Connect
        assert!(transport.connect().await.is_ok());

        // Can't connect again
        assert!(transport.connect().await.is_err());

        // Disconnect
        assert!(transport.close().await.is_ok());
    }

    #[tokio::test]
    async fn test_mock_read_write() {
        let mut transport = MockTransport::new();
        transport.connect().await.unwrap();

        // Add incoming data
        transport.add_incoming_data(b"hello").await;

        // Read it
        let mut buf = [0u8; 10];
        let n = transport.read(&mut buf).await.unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf[..5], b"hello");

        // Write data
        transport.write(b"world").await.unwrap();
        let written = transport.get_written_data().await;
        assert_eq!(written, b"world");
    }

    #[tokio::test]
    async fn test_mock_failures() {
        let mut transport = MockTransport::new();

        // Set to fail on connect
        transport
            .set_behavior(MockBehavior {
                fail_connect: true,
                ..Default::default()
            })
            .await;

        assert!(transport.connect().await.is_err());

        // Reset and connect successfully
        transport.set_behavior(MockBehavior::default()).await;
        transport.connect().await.unwrap();

        // Set to fail on read
        transport
            .set_behavior(MockBehavior {
                fail_read: true,
                ..Default::default()
            })
            .await;

        let mut buf = [0u8; 10];
        assert!(transport.read(&mut buf).await.is_err());
    }

    #[tokio::test]
    async fn test_mock_packet_injection() {
        let mut transport = MockTransport::new();
        transport.connect().await.unwrap();

        // Inject a packet (PUBLISH with QoS 1)
        transport
            .inject_packet(vec![
                crate::constants::fixed_header::PUBLISH_BASE | 0x02,
                0x0A,
            ])
            .await;

        // Read it
        let mut buf = [0u8; 2];
        let n = transport.read(&mut buf).await.unwrap();
        assert_eq!(n, 2);
        assert_eq!(buf[0], crate::constants::fixed_header::PUBLISH_BASE | 0x02);
        assert_eq!(buf[1], 0x0A);
    }
}
