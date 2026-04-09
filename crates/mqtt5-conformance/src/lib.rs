//! MQTT v5.0 OASIS specification conformance test suite.
//!
//! Tests every normative statement from the
//! [OASIS MQTT Version 5.0 specification](https://docs.oasis-open.org/mqtt/mqtt/v5.0/mqtt-v5.0.html)
//! against the real broker running in-process on loopback TCP.
//!
//! Each test maps to a specific `[MQTT-x.x.x-y]` conformance identifier.
//! The [`manifest`] module tracks coverage via a structured TOML manifest,
//! and [`report`] generates human-readable and machine-readable coverage reports.

#![warn(clippy::pedantic)]

extern crate self as mqtt5_conformance;

pub mod assertions;
pub mod capabilities;
#[cfg(feature = "inprocess-fixture")]
pub mod conformance_tests;
#[cfg(feature = "inprocess-fixture")]
pub mod harness;
pub mod manifest;
pub mod packet_parser;
pub mod raw_client;
pub mod registry;
pub mod report;
pub mod skip;
pub mod sut;
pub mod test_client;
pub mod transport;

pub use mqtt5_conformance_macros::conformance_test;
