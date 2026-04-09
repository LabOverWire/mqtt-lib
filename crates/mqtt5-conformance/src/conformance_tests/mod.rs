//! Library-hosted conformance test bodies.
//!
//! Tests live inside the library crate (instead of under `tests/`) so they
//! are compiled into any downstream binary that depends on
//! `mqtt5-conformance`. The CLI runner walks the
//! [`crate::registry::CONFORMANCE_TESTS`] distributed slice to enumerate
//! every registered test at link time without runtime discovery.
//!
//! Each `#[conformance_test]` function emits both a `#[cfg(test)] #[tokio::test]`
//! wrapper (so `cargo test --lib --features inprocess-fixture` runs the
//! suite against the default in-process fixture during development) and a
//! runner function registered in the distributed slice (so the CLI runner
//! can execute the same test against any SUT declared in `sut.toml`).
//!
//! Phase G uses [`section3_ping`] as the proof-of-concept module while the
//! broader migration pulls the rest of the existing `tests/section*.rs`
//! files in alongside it.

pub mod section1_data_repr;
pub mod section3_connack;
pub mod section3_connect;
pub mod section3_connect_extended;
pub mod section3_disconnect;
pub mod section3_final_conformance;
pub mod section3_ping;
pub mod section3_publish;
pub mod section3_publish_advanced;
pub mod section3_publish_alias;
pub mod section3_publish_flow;
pub mod section3_qos_ack;
pub mod section3_subscribe;
pub mod section3_unsubscribe;
pub mod section4_enhanced_auth;
pub mod section4_error_handling;
pub mod section4_flow_control;
pub mod section4_qos;
pub mod section4_shared_sub;
pub mod section4_topic;
pub mod section6_websocket;
