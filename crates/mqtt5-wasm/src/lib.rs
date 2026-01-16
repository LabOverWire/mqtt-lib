#![cfg(target_arch = "wasm32")]
#![warn(clippy::pedantic)]

pub mod bindings;
#[cfg(feature = "broker")]
pub mod bridge;
#[cfg(feature = "broker")]
pub mod broker;
pub mod client;
#[cfg(feature = "broker")]
mod client_handler;
pub mod config;
pub mod decoder;
pub mod transport;
mod utils;

#[cfg(feature = "broker")]
pub use bridge::{WasmBridgeConfig, WasmBridgeDirection, WasmTopicMapping};
#[cfg(feature = "broker")]
pub use broker::{WasmBroker, WasmBrokerConfig};
pub use client::WasmMqttClient;
pub use config::{
    WasmConnectOptions, WasmPublishOptions, WasmReconnectOptions, WasmSubscribeOptions,
    WasmWillMessage,
};
pub use mqtt5_protocol::RecoverableError;
