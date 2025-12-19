//! # Complete MQTT v5.0 Platform
//!
//! A complete MQTT v5.0 platform providing both high-performance async client library and full-featured broker implementation.
//! Features include certificate loading from bytes, multi-transport support (TCP, TLS, WebSocket), authentication, bridging, and comprehensive testing.
//!
//! ## Architecture
//!
//! This library uses Rust's native async/await patterns throughout:
//! - Direct async methods for all operations
//! - Background async tasks for continuous operations (packet reading, keepalive)
//! - The Tokio runtime for task scheduling
//!
//! For architectural details, see ARCHITECTURE.md.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use mqtt5::{MqttClient, ConnectOptions, QoS};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let client = MqttClient::new("test-client");
//!     
//!     // Direct async connect
//!     client.connect("mqtt://test.mosquitto.org:1883").await?;
//!     
//!     // Direct async subscribe with callback
//!     client.subscribe("sensors/+/data", |msg| {
//!         println!("Received {} on {}",
//!                  String::from_utf8_lossy(&msg.payload),
//!                  msg.topic);
//!     }).await?;
//!     
//!     // Direct async publish
//!     client.publish("sensors/temp/data", b"25.5").await?;
//!     
//!     Ok(())
//! }
//! ```
//!
//! ## Advanced Example
//!
//! ```rust,no_run
//! use mqtt5::{MqttClient, ConnectOptions, PublishOptions, QoS, ConnectionEvent};
//! use std::time::Duration;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Configure connection options
//!     let options = ConnectOptions::new("weather-station")
//!         .with_clean_start(false)  // Resume previous session
//!         .with_keep_alive(Duration::from_secs(30))
//!         .with_automatic_reconnect(true)
//!         .with_reconnect_delay(Duration::from_secs(5), Duration::from_secs(60));
//!     
//!     let client = MqttClient::with_options(options);
//!     
//!     // Monitor connection events
//!     client.on_connection_event(|event| {
//!         match event {
//!             ConnectionEvent::Connected { session_present } => {
//!                 println!("Connected! Session present: {}", session_present);
//!             }
//!             ConnectionEvent::Disconnected { reason } => {
//!                 println!("Disconnected: {:?}", reason);
//!             }
//!             ConnectionEvent::Reconnecting { attempt } => {
//!                 println!("Reconnecting... attempt {}", attempt);
//!             }
//!             ConnectionEvent::ReconnectFailed { error } => {
//!                 println!("Reconnection failed: {}", error);
//!             }
//!         }
//!     }).await?;
//!     
//!     // Connect to broker
//!     client.connect("mqtts://broker.example.com:8883").await?;
//!     
//!     // Subscribe with QoS 2 for critical data
//!     client.subscribe("weather/+/alerts", |msg| {
//!         if msg.retain {
//!             println!("Retained alert: {}", String::from_utf8_lossy(&msg.payload));
//!         } else {
//!             println!("New alert: {}", String::from_utf8_lossy(&msg.payload));
//!         }
//!     }).await?;
//!     
//!     // Publish with custom options
//!     let mut pub_opts = PublishOptions::default();
//!     pub_opts.qos = QoS::ExactlyOnce;
//!     pub_opts.retain = true;
//!     pub_opts.properties.message_expiry_interval = Some(3600); // 1 hour
//!     
//!     client.publish_with_options(
//!         "weather/station01/temperature",
//!         b"25.5",
//!         pub_opts
//!     ).await?;
//!     
//!     // Keep running
//!     tokio::time::sleep(Duration::from_secs(3600)).await;
//!     
//!     Ok(())
//! }
//! ```
//!
//! ## Broker Example
//!
//! This library also provides a complete MQTT broker implementation:
//!
//! ```rust,no_run
//! use mqtt5::broker::{BrokerConfig, MqttBroker};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {  
//!     // Create a basic broker
//!     let mut broker = MqttBroker::bind("0.0.0.0:1883").await?;
//!     
//!     println!("🚀 MQTT broker running on port 1883");
//!     
//!     // Run until shutdown
//!     broker.run().await?;
//!     Ok(())
//! }
//! ```
//!
//! ### Advanced Broker with Multi-Transport
//!
//! ```rust,no_run
//! use mqtt5::broker::{BrokerConfig, MqttBroker};
//! use mqtt5::broker::config::{TlsConfig, WebSocketConfig};
//! use std::net::SocketAddr;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let config = BrokerConfig::default()
//!         // TCP on port 1883
//!         .with_bind_address("0.0.0.0:1883".parse::<SocketAddr>()?)
//!         // TLS on port 8883
//!         .with_tls(
//!             TlsConfig::new("certs/server.crt".into(), "certs/server.key".into())
//!                 .with_bind_address("0.0.0.0:8883".parse::<SocketAddr>()?)
//!         )
//!         // WebSocket on port 8080
//!         .with_websocket(
//!             WebSocketConfig::default()
//!                 .with_bind_address("0.0.0.0:8080".parse::<SocketAddr>()?)
//!                 .with_path("/mqtt")
//!         );
//!
//!     let mut broker = MqttBroker::with_config(config).await?;
//!     
//!     println!("🚀 Multi-transport MQTT broker running");
//!     println!("  📡 TCP:       mqtt://localhost:1883");
//!     println!("  🔒 TLS:       mqtts://localhost:8883");  
//!     println!("  🌐 WebSocket: ws://localhost:8080/mqtt");
//!     
//!     broker.run().await?;
//!     Ok(())
//! }
//! ```

#![warn(clippy::pedantic)]
#![allow(clippy::uninlined_format_args)]
#![allow(clippy::unused_self)]
#![allow(clippy::needless_borrows_for_generic_args)]
#![allow(clippy::writeln_empty_string)]
#![allow(clippy::unnecessary_map_or)]
#![allow(clippy::if_not_else)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::single_component_path_imports)]

pub use mqtt5_protocol::{
    constants, encoding, error, flags, packet, packet_id, protocol, qos2, time, topic_matching,
    validation,
};

pub mod broker;
pub mod callback;
#[cfg(not(target_arch = "wasm32"))]
pub mod client;
#[cfg(not(target_arch = "wasm32"))]
pub mod crypto;
pub mod session;
#[cfg(not(target_arch = "wasm32"))]
pub mod tasks;
pub mod telemetry;
#[cfg(not(target_arch = "wasm32"))]
pub mod test_utils;
#[cfg(any(test, feature = "turmoil-testing"))]
pub mod testing;
pub mod transport;
pub mod types;

#[cfg(not(target_arch = "wasm32"))]
pub use client::{
    AuthHandler, AuthResponse, ConnectionEvent, DisconnectReason, JwtAuthHandler, MockCall,
    MockMqttClient, MqttClient, MqttClientTrait, PlainAuthHandler, ScramSha256AuthHandler,
};
pub use mqtt5_protocol::{
    is_valid_client_id, is_valid_topic_filter, is_valid_topic_name, parse_shared_subscription,
    strip_shared_subscription_prefix, topic_matches_filter, validate_client_id,
    validate_topic_filter, validate_topic_name, ConnectProperties, ConnectResult, FixedHeader,
    Message, MessageProperties, MqttError, Packet, PacketType, Properties, PropertyId,
    PropertyValue, PropertyValueType, ProtocolVersion, PublishOptions, PublishProperties,
    PublishResult, QoS, RestrictiveValidator, Result, RetainHandling, StandardValidator,
    SubscribeOptions, TopicValidator, Transport, WillMessage, WillProperties,
};
pub use types::{ConnectOptions, ConnectionStats};
