#![cfg(target_arch = "wasm32")]

pub mod bindings;
#[cfg(feature = "wasm-broker")]
pub mod broker;
pub mod client;
#[cfg(feature = "wasm-broker")]
mod client_handler;
pub mod decoder;
pub mod service_worker;

#[cfg(feature = "wasm-broker")]
pub use broker::WasmBroker;
pub use client::WasmMqttClient;
pub use service_worker::start_service_worker;
