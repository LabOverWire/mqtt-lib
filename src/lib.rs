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
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::unused_self)]
#![allow(clippy::needless_borrows_for_generic_args)]
#![allow(clippy::writeln_empty_string)]
#![allow(clippy::unnecessary_map_or)]
#![allow(clippy::if_not_else)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::doc_markdown)]
#![allow(dead_code)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::single_component_path_imports)]

#[cfg(any(not(target_arch = "wasm32"), feature = "wasm-broker"))]
pub mod broker;
pub mod callback;
#[cfg(not(target_arch = "wasm32"))]
pub mod client;
pub mod constants;
pub mod encoding;
pub mod error;
pub mod flags;
pub mod packet;
pub mod packet_id;
pub mod qos2;
pub mod protocol;
pub mod session;
#[cfg(not(target_arch = "wasm32"))]
pub mod tasks;
pub mod telemetry;
#[cfg(not(target_arch = "wasm32"))]
pub mod test_utils;
#[cfg(any(test, feature = "turmoil-testing"))]
pub mod testing;
pub mod time;
pub mod topic_matching;
pub mod transport;
pub mod types;
pub mod validation;

#[cfg(all(
    target_arch = "wasm32",
    any(feature = "wasm-client", feature = "wasm-broker")
))]
pub mod wasm;

#[cfg(not(target_arch = "wasm32"))]
pub use client::{
    ConnectionEvent, DisconnectReason, MockCall, MockMqttClient, MqttClient, MqttClientTrait,
};
pub use error::{MqttError, Result};
pub use packet::publish::PublishPacket;
pub use packet::{FixedHeader, Packet, PacketType};
pub use protocol::v5::properties::{Properties, PropertyId, PropertyValue, PropertyValueType};
pub use types::{
    ConnectOptions, ConnectProperties, ConnectResult, ConnectionStats, Message, MessageProperties,
    PublishOptions, PublishProperties, PublishResult, RetainHandling, SubscribeOptions,
    WillMessage, WillProperties,
};
pub use validation::{
    is_valid_client_id, is_valid_topic_filter, is_valid_topic_name, topic_matches_filter,
    validate_client_id, validate_topic_filter, validate_topic_name, RestrictiveValidator,
    StandardValidator, TopicValidator,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum QoS {
    AtMostOnce = 0,
    AtLeastOnce = 1,
    ExactlyOnce = 2,
}

impl From<u8> for QoS {
    fn from(value: u8) -> Self {
        match value {
            1 => QoS::AtLeastOnce,
            2 => QoS::ExactlyOnce,
            _ => QoS::AtMostOnce, // Default to QoS 0 for invalid values (including 0)
        }
    }
}

impl From<QoS> for u8 {
    fn from(qos: QoS) -> Self {
        qos as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qos_values() {
        assert_eq!(QoS::AtMostOnce as u8, 0);
        assert_eq!(QoS::AtLeastOnce as u8, 1);
        assert_eq!(QoS::ExactlyOnce as u8, 2);
    }

    #[test]
    fn test_qos_from_u8() {
        assert_eq!(QoS::from(0), QoS::AtMostOnce);
        assert_eq!(QoS::from(1), QoS::AtLeastOnce);
        assert_eq!(QoS::from(2), QoS::ExactlyOnce);

        // Invalid values default to AtMostOnce
        assert_eq!(QoS::from(3), QoS::AtMostOnce);
        assert_eq!(QoS::from(255), QoS::AtMostOnce);
    }

    #[test]
    fn test_qos_into_u8() {
        assert_eq!(u8::from(QoS::AtMostOnce), 0);
        assert_eq!(u8::from(QoS::AtLeastOnce), 1);
        assert_eq!(u8::from(QoS::ExactlyOnce), 2);
    }
}
