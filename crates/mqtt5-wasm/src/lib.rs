#![cfg(target_arch = "wasm32")]
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

pub mod bindings;
#[cfg(feature = "broker")]
pub mod broker;
pub mod client;
#[cfg(feature = "broker")]
mod client_handler;
pub mod config;
pub mod decoder;
pub mod transport;

#[cfg(feature = "broker")]
pub use broker::{WasmBroker, WasmBrokerConfig};
pub use client::WasmMqttClient;
pub use config::{WasmConnectOptions, WasmPublishOptions, WasmSubscribeOptions, WasmWillMessage};
