---
title: 'mqtt5: A Rust MQTT v5.0 Client and Broker Library with QUIC Transport and Specification Conformance Testing'
tags:
  - Rust
  - MQTT
  - IoT
  - QUIC
  - WebAssembly
  - publish-subscribe
authors:
  - name: Fabrício Bracht
    orcid: 0000-0002-4758-0159
    corresponding: true
    affiliation: 1
affiliations:
  - name: LabOverWire
    index: 1
date: 9 March 2026
bibliography: paper.bib
---

# Summary

`mqtt5` is an open-source Rust library that provides a complete MQTT v5.0 [@mqtt-v5-spec] client and broker implementation across four transports—TCP, TLS 1.3, WebSocket, and QUIC—and three platform targets: native, WebAssembly, and bare-metal embedded. The library is organized as a Cargo workspace of five crates totaling approximately 47,000 lines of code and 33,000 lines of tests. A conformance test suite tracks all 247 normative statements from the OASIS MQTT v5.0 standard, with 174 directly tested. The QUIC transport, built on Quinn [@quinn], supports three configurable stream mapping strategies (control-only, per-topic, per-publish) and an unreliable datagram mode, enabling researchers to study how different QUIC stream-to-MQTT mappings affect head-of-line blocking, throughput, and latency under varying network conditions.

# Statement of need

MQTT v5.0 is a widely adopted publish-subscribe protocol for IoT deployments [@bansal2020iot-mqtt-survey], but existing implementations leave gaps that affect researchers and developers working at the intersection of IoT protocols and transport-layer innovation.

No existing MQTT library provides configurable QUIC stream mapping strategies. EMQX [@emqx] added QUIC support using a single-stream mapping, but does not expose alternative strategies. Researchers studying how QUIC's multiplexed streams interact with MQTT's topic-based message routing must either modify broker internals or build custom implementations. `mqtt5` makes this a configuration option: control-only (single stream), per-topic (one stream per topic), and per-publish (one stream per message), plus an unreliable datagram mode for loss-tolerant use cases.

No existing MQTT implementation provides a conformance test suite with per-statement traceability against the OASIS specification. The 247 normative statements in MQTT v5.0 are identified by a machine-readable pattern (`[MQTT-x.x.x-y]`), yet no published broker maps each statement to a test function. `mqtt5` maintains a manifest (`conformance.toml`) that classifies every statement and links it to the test functions that verify it, providing a reusable methodology for protocol conformance tracking.

Finally, no existing MQTT library compiles from a single codebase to native, WebAssembly, and bare-metal embedded targets. The `no_std` protocol codec enables use on ARM Cortex-M4 and RISC-V microcontrollers, while the WASM crate provides JavaScript bindings for browser-based MQTT clients and an optional in-browser broker.

# State of the field

Mosquitto [@mosquitto] is a widely deployed C broker and client library supporting TCP, TLS, and WebSocket, but not QUIC, and without a published conformance suite. EMQX [@emqx] is an Erlang/OTP broker designed for horizontal scaling with QUIC support via a single-stream mapping, but without configurable stream strategies, datagram transport, or WebAssembly deployment. HiveMQ [@hivemq] is a Java broker with an open-source community edition and proprietary enterprise features, without QUIC support. rumqtt [@rumqtt] is a Rust MQTT library with a client and broker supporting TCP, TLS, and WebSocket, with partial MQTT v5.0 support but no QUIC, WebAssembly, or embedded targets. paho-mqtt [@paho-mqtt] is an Eclipse Foundation client library in C, C++, Java, Python, and other languages, supporting v5.0 over TCP, TLS, and WebSocket, but is client-only.

To our knowledge, `mqtt5` is the only MQTT implementation that combines a specification conformance suite with per-statement traceability, configurable QUIC stream mapping strategies with datagram transport, and multi-target support spanning native, WebAssembly, and bare-metal embedded from a single codebase.

# Software design

The library separates concerns across five crates. `mqtt5-protocol` implements the MQTT v5.0 packet codec, topic validation, session state management, and QoS flow control under `no_std` with the `alloc` crate, enabling bare-metal deployment. `mqtt5` adds async networking via Tokio [@tokio], transport implementations, the broker's message router, authentication subsystem, and persistent storage. `mqtt5-wasm` wraps the client and broker with `wasm-bindgen` for browser environments. `mqttv5-cli` provides a command-line interface for publishing, subscribing, broker operation, and benchmarking. `mqtt5-conformance` contains the specification test suite.

All transports implement a common `Transport` trait. The broker supports simultaneous listeners on all four transports. The QUIC transport's three stream mapping strategies allow the same broker and client to operate with different stream-to-topic relationships by changing a configuration option rather than modifying code.

The broker is structured around four subsystems: a message router with exact-match and wildcard subscription tables supporting shared subscriptions; session management with persistent storage via pluggable backends (in-memory for testing, file-based with atomic writes for production); a pluggable authentication system with implementations for passwords, SCRAM-SHA-256, JWT, and X.509 certificates; and a security layer that injects verifiable sender identity properties on every published message.

The conformance suite uses three tiers: raw protocol tests that send hand-crafted byte sequences to test broker behavior against malformed packets, structural validation tests that inspect response packet structure, and behavioral tests that exercise end-to-end message flows through the library's own client. The conformance process exposed 18 broker implementation gaps—9 missing validations and 9 incorrect behaviors—all documented in a development diary with dates, normative statement references, and fixing commits.

Beyond conformance, the library is verified through 483 integration tests covering all transports and MQTT features, property-based testing with proptest [@proptest] for topic validation and session state invariants, and deterministic network simulation with Turmoil [@turmoil] for partition and latency testing. A multi-project validation approach uses downstream consumers—a web frontend importing the npm package published from the WASM crate, and the CLI tool—as implicit integration test harnesses that surface defects at runtime-target boundaries. The CI pipeline runs seven parallel jobs on every push: formatting, linting with pedantic Clippy, unit tests, MSRV verification, documentation completeness, WASM compilation, and embedded cross-compilation for ARM Cortex-M4 and RISC-V.

# Research impact statement

`mqtt5` is, to our knowledge, the first MQTT implementation to offer configurable QUIC stream mapping strategies and QUIC datagram transport for MQTT messaging. These capabilities enable controlled experimentation on how QUIC's multiplexed streams interact with MQTT's topic-based routing—a research question that previously required building custom implementations from scratch. The library's built-in benchmarking tool (`mqttv5 bench`) has been used in a series of controlled experiments comparing TCP and QUIC transport performance under varying network conditions, measuring connection latency, head-of-line blocking isolation across stream strategies, throughput scaling, datagram-versus-stream trade-offs, and resource overhead at scale. The configurable stream mapping strategies provide a single codebase where the only variable is the QUIC stream strategy, eliminating confounds from differing MQTT implementations.

The conformance methodology—a machine-readable manifest linking every normative statement to its test functions—is reusable for other protocol implementations. The 18 implementation gaps discovered demonstrate the value of systematic per-statement conformance testing for protocol libraries.

The library is published on crates.io as the `mqtt5` crate and as an npm package for browser-based use.

# AI usage disclosure

Development used Claude Code (Anthropic) as a code generation and automation tool operating under developer direction. The developer drove all architectural decisions, reviewed all generated code, performed quality assurance, and analyzed defects. The AI tool executed specific tasks on command: writing and rewriting code, running build and test commands, writing automation scripts, and editing text. All AI-generated code passed through the same CI pipeline, conformance verification, and pedantic linting as manually written code. This paper was drafted with AI assistance and reviewed and edited by the author.

# Acknowledgements

The QUIC transport implementation uses Quinn [@quinn], and the async runtime is provided by Tokio [@tokio].

# References
