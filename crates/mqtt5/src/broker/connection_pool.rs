//! High-performance connection pooling and buffer management for MQTT broker
//!
//! This module implements optimizations for connection handling:
//! - Connection object pooling to reduce allocations
//! - Buffer reuse for message parsing and serialization  
//! - Optimized memory management for high-throughput scenarios

use crate::packet::publish::PublishPacket;
use crate::packet::MqttPacket;
use bytes::{Bytes, BytesMut};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, trace};

/// Pool of reusable connection objects to minimize allocations
pub struct ConnectionPool {
    /// Pool of available connection contexts
    available_connections: Arc<Mutex<VecDeque<PooledConnection>>>,
    /// Pool of reusable byte buffers
    buffer_pool: Arc<BufferPool>,
    /// Pool configuration
    config: PoolConfig,
    /// Pool metrics
    metrics: Arc<PoolMetrics>,
}

/// Configuration for connection pooling
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Maximum number of pooled connections
    pub max_connections: usize,
    /// Maximum number of pooled buffers  
    pub max_buffers: usize,
    /// Initial buffer capacity in bytes
    pub initial_buffer_capacity: usize,
    /// Maximum buffer capacity before discarding
    pub max_buffer_capacity: usize,
    /// Enable connection keep-alive pooling
    pub enable_connection_pooling: bool,
    /// Enable buffer reuse
    pub enable_buffer_pooling: bool,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_connections: 1000,
            max_buffers: 500,
            initial_buffer_capacity: 1024,
            max_buffer_capacity: 64 * 1024, // 64KB max
            enable_connection_pooling: true,
            enable_buffer_pooling: true,
        }
    }
}

/// Metrics for pool performance monitoring
#[derive(Debug, Default)]
pub struct PoolMetrics {
    pub connections_created: std::sync::atomic::AtomicUsize,
    pub connections_reused: std::sync::atomic::AtomicUsize,
    pub buffers_created: std::sync::atomic::AtomicUsize,
    pub buffers_reused: std::sync::atomic::AtomicUsize,
    pub pool_hits: std::sync::atomic::AtomicUsize,
    pub pool_misses: std::sync::atomic::AtomicUsize,
}

/// A pooled connection object that can be reused
#[derive(Debug)]
pub struct PooledConnection {
    /// Unique connection ID
    pub id: String,
    /// Reusable read buffer
    pub read_buffer: BytesMut,
    /// Reusable write buffer  
    pub write_buffer: BytesMut,
    /// Connection state
    pub state: ConnectionState,
    /// Last activity timestamp
    pub last_used: std::time::Instant,
}

/// Connection state for pooling
#[derive(Debug, Clone, Copy)]
pub enum ConnectionState {
    /// Newly created connection
    Fresh,
    /// Connection ready for reuse
    Ready,
    /// Connection being used
    InUse,
    /// Connection marked for cleanup
    Cleanup,
}

/// High-performance buffer pool for message handling
pub struct BufferPool {
    /// Pool of read buffers
    read_buffers: Mutex<VecDeque<BytesMut>>,
    /// Pool of write buffers
    write_buffers: Mutex<VecDeque<BytesMut>>,
    /// Pool of message buffers
    message_buffers: Mutex<VecDeque<Vec<u8>>>,
    /// Pool configuration
    config: PoolConfig,
}

impl ConnectionPool {
    /// Creates a new connection pool with the given configuration
    pub fn new(config: PoolConfig) -> Self {
        Self {
            available_connections: Arc::new(Mutex::new(VecDeque::new())),
            buffer_pool: Arc::new(BufferPool::new(config.clone())),
            config,
            metrics: Arc::new(PoolMetrics::default()),
        }
    }

    /// Gets a connection from the pool or creates a new one
    pub async fn get_connection(&self, client_id: &str) -> PooledConnection {
        if self.config.enable_connection_pooling {
            let mut pool = self.available_connections.lock().await;

            if let Some(mut conn) = pool.pop_front() {
                conn.id = client_id.to_string();
                conn.state = ConnectionState::InUse;
                conn.last_used = std::time::Instant::now();

                // Clear buffers for reuse
                conn.read_buffer.clear();
                conn.write_buffer.clear();

                self.metrics
                    .connections_reused
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.metrics
                    .pool_hits
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                trace!("Reused pooled connection for client: {}", client_id);
                return conn;
            }
        }

        // Create new connection
        let connection = PooledConnection {
            id: client_id.to_string(),
            read_buffer: BytesMut::with_capacity(self.config.initial_buffer_capacity),
            write_buffer: BytesMut::with_capacity(self.config.initial_buffer_capacity),
            state: ConnectionState::Fresh,
            last_used: std::time::Instant::now(),
        };

        self.metrics
            .connections_created
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics
            .pool_misses
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        debug!("Created new connection for client: {}", client_id);
        connection
    }

    /// Returns a connection to the pool for reuse
    pub async fn return_connection(&self, mut connection: PooledConnection) {
        if self.config.enable_connection_pooling {
            connection.state = ConnectionState::Ready;

            // Don't pool oversized buffers
            if connection.read_buffer.capacity() > self.config.max_buffer_capacity {
                connection.read_buffer =
                    BytesMut::with_capacity(self.config.initial_buffer_capacity);
            }
            if connection.write_buffer.capacity() > self.config.max_buffer_capacity {
                connection.write_buffer =
                    BytesMut::with_capacity(self.config.initial_buffer_capacity);
            }

            let mut pool = self.available_connections.lock().await;

            // Maintain pool size limit
            if pool.len() < self.config.max_connections {
                let conn_id = connection.id.clone();
                pool.push_back(connection);
                trace!("Returned connection to pool: {}", conn_id);
            } else {
                debug!("Pool full, discarding connection: {}", connection.id);
            }
        }
    }

    /// Gets a reusable buffer from the buffer pool
    pub async fn get_read_buffer(&self) -> BytesMut {
        self.buffer_pool.get_read_buffer().await
    }

    /// Gets a reusable write buffer from the buffer pool
    pub async fn get_write_buffer(&self) -> BytesMut {
        self.buffer_pool.get_write_buffer().await
    }

    /// Returns a buffer to the pool for reuse
    pub async fn return_read_buffer(&self, buffer: BytesMut) {
        self.buffer_pool.return_read_buffer(buffer).await;
    }

    /// Returns a write buffer to the pool for reuse
    pub async fn return_write_buffer(&self, buffer: BytesMut) {
        self.buffer_pool.return_write_buffer(buffer).await;
    }

    /// Gets current pool metrics
    pub fn get_metrics(&self) -> (usize, usize, usize, usize, usize, usize) {
        let connections_created = self
            .metrics
            .connections_created
            .load(std::sync::atomic::Ordering::Relaxed);
        let connections_reused = self
            .metrics
            .connections_reused
            .load(std::sync::atomic::Ordering::Relaxed);
        let buffers_created = self
            .metrics
            .buffers_created
            .load(std::sync::atomic::Ordering::Relaxed);
        let buffers_reused = self
            .metrics
            .buffers_reused
            .load(std::sync::atomic::Ordering::Relaxed);
        let pool_hits = self
            .metrics
            .pool_hits
            .load(std::sync::atomic::Ordering::Relaxed);
        let pool_misses = self
            .metrics
            .pool_misses
            .load(std::sync::atomic::Ordering::Relaxed);

        (
            connections_created,
            connections_reused,
            buffers_created,
            buffers_reused,
            pool_hits,
            pool_misses,
        )
    }

    /// Cleans up expired connections from the pool
    pub async fn cleanup_expired(&self, max_age: std::time::Duration) {
        let mut pool = self.available_connections.lock().await;
        let now = std::time::Instant::now();

        let initial_size = pool.len();
        pool.retain(|conn| now.duration_since(conn.last_used) < max_age);
        let removed = initial_size - pool.len();

        if removed > 0 {
            debug!("Cleaned up {} expired connections from pool", removed);
        }
    }
}

impl BufferPool {
    /// Creates a new buffer pool
    pub fn new(config: PoolConfig) -> Self {
        Self {
            read_buffers: Mutex::new(VecDeque::new()),
            write_buffers: Mutex::new(VecDeque::new()),
            message_buffers: Mutex::new(VecDeque::new()),
            config,
        }
    }

    /// Gets a read buffer from the pool or creates a new one
    pub async fn get_read_buffer(&self) -> BytesMut {
        if self.config.enable_buffer_pooling {
            let mut pool = self.read_buffers.lock().await;
            if let Some(mut buffer) = pool.pop_front() {
                buffer.clear();
                return buffer;
            }
        }

        BytesMut::with_capacity(self.config.initial_buffer_capacity)
    }

    /// Gets a write buffer from the pool or creates a new one
    pub async fn get_write_buffer(&self) -> BytesMut {
        if self.config.enable_buffer_pooling {
            let mut pool = self.write_buffers.lock().await;
            if let Some(mut buffer) = pool.pop_front() {
                buffer.clear();
                return buffer;
            }
        }

        BytesMut::with_capacity(self.config.initial_buffer_capacity)
    }

    /// Returns a read buffer to the pool
    pub async fn return_read_buffer(&self, buffer: BytesMut) {
        if self.config.enable_buffer_pooling && buffer.capacity() <= self.config.max_buffer_capacity
        {
            let mut pool = self.read_buffers.lock().await;
            if pool.len() < self.config.max_buffers {
                pool.push_back(buffer);
            }
        }
    }

    /// Returns a write buffer to the pool
    pub async fn return_write_buffer(&self, buffer: BytesMut) {
        if self.config.enable_buffer_pooling && buffer.capacity() <= self.config.max_buffer_capacity
        {
            let mut pool = self.write_buffers.lock().await;
            if pool.len() < self.config.max_buffers {
                pool.push_back(buffer);
            }
        }
    }

    /// Gets a message buffer for packet serialization
    pub async fn get_message_buffer(&self) -> Vec<u8> {
        if self.config.enable_buffer_pooling {
            let mut pool = self.message_buffers.lock().await;
            if let Some(mut buffer) = pool.pop_front() {
                buffer.clear();
                return buffer;
            }
        }

        Vec::with_capacity(self.config.initial_buffer_capacity)
    }

    /// Returns a message buffer to the pool
    pub async fn return_message_buffer(&self, buffer: Vec<u8>) {
        if self.config.enable_buffer_pooling && buffer.capacity() <= self.config.max_buffer_capacity
        {
            let mut pool = self.message_buffers.lock().await;
            if pool.len() < self.config.max_buffers {
                pool.push_back(buffer);
            }
        }
    }
}

/// Optimized message serializer that reuses buffers
pub struct OptimizedMessageSerializer {
    buffer_pool: Arc<BufferPool>,
}

impl OptimizedMessageSerializer {
    pub fn new(buffer_pool: Arc<BufferPool>) -> Self {
        Self { buffer_pool }
    }

    /// Serializes a publish packet using pooled buffers
    pub async fn serialize_publish(
        &self,
        packet: &PublishPacket,
    ) -> Result<Bytes, crate::error::MqttError> {
        // Get a buffer from the pool
        let mut buffer = self.buffer_pool.get_write_buffer().await;

        // Serialize the packet
        packet.encode(&mut buffer)?;

        // Convert to Bytes for sending
        let result = buffer.freeze();

        // Note: We can't return the buffer here since it's been frozen
        // In a real implementation, we'd want to avoid this allocation

        Ok(result)
    }

    /// Batch serialize multiple packets efficiently
    pub async fn serialize_batch(
        &self,
        packets: &[PublishPacket],
    ) -> Result<Vec<Bytes>, crate::error::MqttError> {
        let mut results = Vec::with_capacity(packets.len());

        // Reuse a single buffer for all serializations
        let mut buffer = self.buffer_pool.get_write_buffer().await;

        for packet in packets {
            buffer.clear();
            packet.encode(&mut buffer)?;
            results.push(buffer.split().freeze());
        }

        // Return the buffer to the pool
        self.buffer_pool.return_write_buffer(buffer).await;

        Ok(results)
    }
}

/// Connection manager that uses pooling for high performance
pub struct PooledConnectionManager {
    /// Connection pool
    connection_pool: Arc<ConnectionPool>,
    /// Message serializer
    serializer: OptimizedMessageSerializer,
}

impl PooledConnectionManager {
    /// Creates a new pooled connection manager
    pub fn new(config: PoolConfig) -> Self {
        let connection_pool = Arc::new(ConnectionPool::new(config));
        let serializer = OptimizedMessageSerializer::new(connection_pool.buffer_pool.clone());

        Self {
            connection_pool,
            serializer,
        }
    }

    /// Creates a new connection context for a client
    pub async fn create_client_context(&self, client_id: &str) -> ClientContext {
        let connection = self.connection_pool.get_connection(client_id).await;

        ClientContext {
            connection_pool: self.connection_pool.clone(),
            connection: Some(connection),
        }
    }

    /// Gets performance metrics from the connection pool
    pub fn get_metrics(&self) -> (usize, usize, usize, usize, usize, usize) {
        self.connection_pool.get_metrics()
    }

    /// Performs maintenance on the connection pool
    pub async fn maintain_pools(&self) {
        // Clean up connections older than 5 minutes
        self.connection_pool
            .cleanup_expired(std::time::Duration::from_secs(300))
            .await;
    }
}

/// Client context that manages pooled resources for a single client
pub struct ClientContext {
    connection_pool: Arc<ConnectionPool>,
    connection: Option<PooledConnection>,
}

impl ClientContext {
    /// Gets a read buffer for this client
    pub async fn get_read_buffer(&self) -> BytesMut {
        self.connection_pool.get_read_buffer().await
    }

    /// Gets a write buffer for this client
    pub async fn get_write_buffer(&self) -> BytesMut {
        self.connection_pool.get_write_buffer().await
    }

    /// Returns buffers to the pool
    pub async fn return_buffers(&self, read_buf: Option<BytesMut>, write_buf: Option<BytesMut>) {
        if let Some(buf) = read_buf {
            self.connection_pool.return_read_buffer(buf).await;
        }
        if let Some(buf) = write_buf {
            self.connection_pool.return_write_buffer(buf).await;
        }
    }

    /// Gets the connection ID
    pub fn client_id(&self) -> Option<String> {
        self.connection.as_ref().map(|c| c.id.clone())
    }
}

impl Drop for ClientContext {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            let pool = self.connection_pool.clone();
            tokio::spawn(async move {
                pool.return_connection(connection).await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_connection_pooling() {
        let config = PoolConfig::default();
        let pool = ConnectionPool::new(config);

        // Get a connection
        let conn1 = pool.get_connection("client1").await;
        assert_eq!(conn1.id, "client1");

        // Return it to the pool
        pool.return_connection(conn1).await;

        // Get another connection - should reuse the first one
        let conn2 = pool.get_connection("client2").await;
        assert_eq!(conn2.id, "client2"); // ID should be updated

        let (created, reused, _, _, hits, misses) = pool.get_metrics();
        assert_eq!(created, 1); // Only one connection created
        assert_eq!(reused, 1); // One connection reused
        assert_eq!(hits, 1); // One pool hit
        assert_eq!(misses, 1); // One pool miss (initial creation)
    }

    #[tokio::test]
    async fn test_buffer_pooling() {
        let config = PoolConfig::default();
        let buffer_pool = BufferPool::new(config);

        // Get a buffer
        let buf1 = buffer_pool.get_read_buffer().await;
        assert!(buf1.capacity() >= 1024);

        // Return it
        buffer_pool.return_read_buffer(buf1).await;

        // Get another - should reuse the first
        let buf2 = buffer_pool.get_read_buffer().await;
        assert!(buf2.is_empty()); // Should be cleared
    }

    #[tokio::test]
    async fn test_pooled_connection_manager() {
        let config = PoolConfig::default();
        let manager = PooledConnectionManager::new(config);

        // Create client context
        let ctx = manager.create_client_context("test_client").await;
        assert_eq!(ctx.client_id(), Some("test_client".to_string()));

        // Test buffer operations
        let read_buf = ctx.get_read_buffer().await;
        let write_buf = ctx.get_write_buffer().await;

        ctx.return_buffers(Some(read_buf), Some(write_buf)).await;

        let (created, _reused, _, _, _, _) = manager.get_metrics();
        assert_eq!(created, 1);
    }

    #[tokio::test]
    async fn test_connection_cleanup() {
        let config = PoolConfig::default();
        let pool = ConnectionPool::new(config);

        // Create and return a connection
        let conn = pool.get_connection("client1").await;
        pool.return_connection(conn).await;

        // Clean up with very short timeout - should remove the connection
        pool.cleanup_expired(std::time::Duration::from_nanos(1))
            .await;

        // Next get should create a new connection
        let _conn2 = pool.get_connection("client2").await;
        let (created, reused, _, _, _, _) = pool.get_metrics();
        assert_eq!(created, 2); // Two connections created due to cleanup
        assert_eq!(reused, 0); // No reuse due to cleanup
    }
}
