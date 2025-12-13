//! MQTT v5.0 Broker Implementation
//!
//! A high-performance, async MQTT broker using direct async/await patterns.
//!
//! # Example
//!
//! ```rust,no_run
//! use mqtt5::broker::{MqttBroker, BrokerConfig};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Start a simple broker
//!     let broker = MqttBroker::bind("0.0.0.0:1883").await?;
//!     
//!     // Run until shutdown signal
//!     tokio::signal::ctrl_c().await?;
//!     broker.shutdown().await?;
//!     
//!     Ok(())
//! }
//! ```

pub mod acl;
pub mod auth;
#[cfg(not(target_arch = "wasm32"))]
mod binding;
#[cfg(not(target_arch = "wasm32"))]
pub mod bridge;
#[cfg(not(target_arch = "wasm32"))]
pub mod client_handler;
pub mod config;
pub mod connection_pool;
#[cfg(not(target_arch = "wasm32"))]
pub mod hot_reload;
#[cfg(not(target_arch = "wasm32"))]
pub mod quic_acceptor;
pub mod resource_monitor;
pub mod router;
#[cfg(not(target_arch = "wasm32"))]
pub mod server;
pub mod storage;
pub mod sys_topics;
#[cfg(not(target_arch = "wasm32"))]
mod tcp_stream_wrapper;
#[cfg(not(target_arch = "wasm32"))]
pub mod tls_acceptor;
#[cfg(not(target_arch = "wasm32"))]
pub mod transport;
#[cfg(not(target_arch = "wasm32"))]
pub mod websocket_server;

pub use acl::{AclManager, AclRule, Permission, Role, RoleRule};
pub use auth::{
    AllowAllAuthProvider, AuthProvider, AuthResult, CertificateAuthProvider,
    ComprehensiveAuthProvider, PasswordAuthProvider,
};
pub use config::{BrokerConfig, StorageBackend as StorageBackendType, StorageConfig};
pub use resource_monitor::{ResourceLimits, ResourceMonitor, ResourceStats};
#[cfg(not(target_arch = "wasm32"))]
pub use server::MqttBroker;
#[cfg(not(target_arch = "wasm32"))]
pub use storage::{DynamicStorage, FileBackend, MemoryBackend, Storage, StorageBackend};
#[cfg(target_arch = "wasm32")]
pub use storage::{DynamicStorage, MemoryBackend, Storage, StorageBackend};

// Re-export key types for convenience
pub use crate::QoS;
