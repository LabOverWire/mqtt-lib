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

pub mod harness;
pub mod manifest;
pub mod raw_client;
pub mod report;
